//! Deterministic process-crash/reopen evidence, NOT powerloss/tornwrite proof.
//! Only test-side trace_v2 and OS control pipes are used; no production hooks.
mod capture_journal_support;
mod support;

use std::{
    cell::RefCell,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use bif::storage;
use capture_journal_support::{
    assert_complete_journal, capture, capture_with_repository, persisted,
};
use rusqlite::trace::{TraceEvent, TraceEventCodes};
use support::OwnedTestDirectory;

struct Control {
    after_commit: bool,
    statements: Vec<String>,
    fired: bool,
}
thread_local! {
    static CONTROL: RefCell<Option<Control>> = const { RefCell::new(None) };
}

/// Block inside SQLite's COMMIT entry or completion callback, before the
/// repository can return a CaptureResult to its caller. Never reenter SQLite.
fn trace(event: TraceEvent<'_>) {
    CONTROL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(control) = slot.as_mut() else {
            return;
        };
        let boundary = match event {
            TraceEvent::Stmt(statement, _) => {
                let sql = statement.sql().into_owned();
                let boundary = !control.after_commit && sql == "COMMIT";
                control.statements.push(sql);
                boundary
            }
            TraceEvent::Profile(statement, _) => {
                control.after_commit && statement.sql() == "COMMIT"
            }
            _ => false,
        };
        if !boundary || control.fired {
            return;
        }
        control.fired = true;
        let position = |needle: &str| {
            control
                .statements
                .iter()
                .position(|sql| sql.contains(needle))
        };
        let ordered = match (
            position("INSERT INTO operations"),
            position("INSERT INTO mutation_receipts"),
            position("INSERT INTO change_journal"),
        ) {
            (Some(operation), Some(receipt), Some(journal)) => {
                operation < receipt && receipt < journal
            }
            _ => false,
        };
        // Errors/panics in FFI callbacks may be swallowed by the adapter. Report
        // ordering via the control pipe and assert in the parent instead.
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "CAPTURE_CONTROL:ordered={ordered}").unwrap();
        stdout.flush().unwrap();
        drop(stdout);
        let mut release = [0_u8];
        std::io::stdin().read_exact(&mut release).unwrap();
    });
}

#[test]
#[ignore = "child harness: invoked explicitly by process-crash tests"]
fn process_capture_child() {
    let path = std::env::var_os("BIF_CAPTURE_CRASH_DATABASE").unwrap();
    let mode = std::env::var("BIF_CAPTURE_CRASH_MODE").unwrap();
    let mut connection = storage::open(path).unwrap();
    // Handle construction uses a separate read-snapshot COMMIT. Trace only the
    // capture transaction, never that initialization boundary.
    connection.trace_v2(TraceEventCodes::all(), Some(trace));
    let mut repository = storage::CaptureRepository::new(&mut connection).unwrap();
    CONTROL.with(|slot| {
        *slot.borrow_mut() = Some(Control {
            after_commit: mode == "after",
            statements: vec![],
            fired: false,
        });
    });
    let result = capture_with_repository(&mut repository, "crash-key", "child", "bif").unwrap();
    // Parent never releases the pipe. Seeing this means the crash boundary was
    // missed and a domain response could already have been delivered.
    println!("DOMAIN_RESPONSE_DELIVERED:{}", result.replayed);
    panic!("child escaped its COMMIT control wait");
}

/// Always kill and reap on assertion failure/timeout as well as the happy path.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn kill_at_commit(path: &Path, after_commit: bool, observe: impl FnOnce()) {
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_capture_child",
                "--ignored",
                "--nocapture",
            ])
            .env("BIF_CAPTURE_CRASH_DATABASE", path)
            .env(
                "BIF_CAPTURE_CRASH_MODE",
                if after_commit { "after" } else { "before" },
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut lines = Vec::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.contains("CAPTURE_CONTROL:") {
                let _ = sender.send(line.clone());
            }
            lines.push(line);
        }
        lines
    });
    // A timeout is only a deadlock watchdog; no sleep or timing selects the kill.
    let notification = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(
        notification.contains("CAPTURE_CONTROL:ordered=true"),
        "operation/receipt/journal must precede COMMIT: {notification}"
    );
    observe();
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    let lines = reader.join().unwrap();
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("DOMAIN_RESPONSE_DELIVERED"))
    );
}

#[test]
fn process_crash_before_commit_reopens_with_full_rollback_and_retry_is_new() {
    // Cover creation of project/counter and an existing counter independently.
    for existing_counter in [false, true] {
        let root = OwnedTestDirectory::new();
        let path = root.path().join("before.sqlite");
        let mut observer = storage::open(&path).unwrap();
        if existing_counter {
            capture(&mut observer, "seed", "seed", "bif").unwrap();
        }
        let before = persisted(&observer);
        kill_at_commit(&path, false, || {
            assert_eq!(persisted(&observer), before, "uncommitted writes invisible");
            // Leave the child as the last open connection before killing it.
            drop(observer);
        });
        let mut connection = storage::open(&path).unwrap();
        assert_eq!(
            persisted(&connection),
            before,
            "full rollback after process death"
        );
        let retry = capture(&mut connection, "crash-key", "retry", "bif").unwrap();
        assert!(!retry.replayed);
        assert_eq!(
            retry.item.id().sequence(),
            if existing_counter { 2 } else { 1 }
        );
        assert_complete_journal(&connection, if existing_counter { &[1, 2] } else { &[1] });
    }
}

#[test]
fn process_crash_after_commit_before_response_retains_one_operation_and_retry_replays() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("after.sqlite");
    let observer = storage::open(&path).unwrap();
    let mut committed = None;
    kill_at_commit(&path, true, || {
        // COMMIT Profile is completion, not entry: another connection sees the
        // complete operation while the child still cannot deliver its result.
        assert_complete_journal(&observer, &[1]);
        committed = Some(persisted(&observer));
        // Do not let a surviving parent connection cleanly checkpoint after
        // the crash. The reopen below is the first connection after death.
        drop(observer);
    });
    let mut connection = storage::open(&path).unwrap();
    let before = persisted(&connection);
    assert_eq!(
        before,
        committed.unwrap(),
        "committed operation survives process death"
    );
    let replay = capture(&mut connection, "crash-key", "unused", "bif").unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.item.id().sequence(), 1);
    assert_eq!(
        persisted(&connection),
        before,
        "no extra row, H or allocation"
    );
    assert_complete_journal(&connection, &[1]);
}
