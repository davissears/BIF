//! Deterministic process-crash/reopen evidence, NOT powerloss/tornwrite proof.
//! Test-only trace_v2 and OS pipes control the actual mutation COMMIT boundary.
mod mutation_journal_support;
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

use bif::{
    domain::{ItemId, ProjectId, RequesterId},
    storage,
};
use mutation_journal_support::{
    assert_coverage, assert_mutation, compound, mutate, mutate_with_repository, persisted, request,
    seed,
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

/// Block before a domain response can escape, never reenter SQLite.
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
            position("INSERT INTO events"),
            position("INSERT INTO mutation_receipts"),
            position("INSERT INTO change_journal"),
            position("COMMIT"),
        ) {
            (Some(operation), Some(event), Some(receipt), Some(journal), Some(commit)) => {
                operation < event
                    && event < receipt
                    && receipt < journal
                    && journal < commit
                    && control
                        .statements
                        .iter()
                        .filter(|sql| sql.contains("INSERT INTO events"))
                        .count()
                        == 4
                    && control
                        .statements
                        .iter()
                        .enumerate()
                        .filter(|(_, sql)| sql.contains("INSERT INTO events"))
                        .all(|(index, _)| operation < index && index < receipt)
            }
            _ => false,
        };
        // FFI callback panics may be swallowed: report and assert in the parent.
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "MUTATION_CONTROL:ordered={ordered}").unwrap();
        stdout.flush().unwrap();
        drop(stdout);
        let mut release = [0_u8];
        std::io::stdin().read_exact(&mut release).unwrap();
    });
}

fn child_request() -> bif::application::MutationRequest {
    let id = ItemId::new(
        RequesterId::new("Davis").unwrap(),
        ProjectId::new("bif").unwrap(),
        1,
    )
    .unwrap();
    request(&id, "crash-key", 1, compound())
}

#[test]
#[ignore = "child harness: invoked explicitly by process-crash tests"]
fn process_mutation_child() {
    let path = std::env::var_os("BIF_MUTATION_CRASH_DATABASE").unwrap();
    let mode = std::env::var("BIF_MUTATION_CRASH_MODE").unwrap();
    let mut connection = storage::open(path).unwrap();
    // Registration precedes the exclusive repository borrow; control is armed
    // AFTER construction so its read-snapshot COMMIT cannot select the kill.
    connection.trace_v2(TraceEventCodes::all(), Some(trace));
    let mut repository = storage::MutationRepository::new(&mut connection).unwrap();
    CONTROL.with(|slot| {
        *slot.borrow_mut() = Some(Control {
            after_commit: mode == "after",
            statements: vec![],
            fired: false,
        });
    });
    let result = mutate_with_repository(&mut repository, child_request(), "child", 4).unwrap();
    println!("DOMAIN_RESPONSE_DELIVERED:{}", result.replayed);
    panic!("child escaped its COMMIT control wait");
}

/// Always kill/reap, including assertion failure and watchdog timeout.
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
                "process_mutation_child",
                "--ignored",
                "--nocapture",
            ])
            .env("BIF_MUTATION_CRASH_DATABASE", path)
            .env(
                "BIF_MUTATION_CRASH_MODE",
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
            if line.contains("MUTATION_CONTROL:") {
                let _ = sender.send(line.clone());
            }
            lines.push(line);
        }
        lines
    });
    // Watchdog only; no sleep or timing selects the kill.
    let notification = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(
        notification.contains("MUTATION_CONTROL:ordered=true"),
        "operation/events/receipt/journal must precede COMMIT: {notification}"
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
fn process_crash_before_commit_rolls_back_every_value_and_same_key_retry_is_new() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("before.sqlite");
    let mut observer = storage::open(&path).unwrap();
    seed(&mut observer);
    let before = persisted(&observer);
    kill_at_commit(&path, false, || {
        assert_eq!(persisted(&observer), before, "uncommitted writes invisible");
        drop(observer); // Child is the last connection before it is killed.
    });
    let mut connection = storage::open(&path).unwrap();
    assert_eq!(
        persisted(&connection),
        before,
        "full rollback after process death"
    );
    let request = child_request();
    let retry = mutate(&mut connection, request.clone(), "child", 4).unwrap();
    assert!(!retry.replayed);
    assert_eq!(retry.item.revision().get(), 2);
    assert_mutation(
        &connection,
        &request,
        "child",
        2,
        &[
            "approved",
            "priority_changed",
            "assignee_changed",
            "note_added",
        ],
    );
    assert_coverage(&connection, &[1, 2]);
}

#[test]
fn committed_response_lost_retains_one_operation_and_retry_does_not_allocate() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("after.sqlite");
    let mut observer = storage::open(&path).unwrap();
    seed(&mut observer);
    let mut committed = None;
    let request = child_request();
    kill_at_commit(&path, true, || {
        // Profile means completion: the whole operation is visible before response.
        assert_mutation(
            &observer,
            &request,
            "child",
            2,
            &[
                "approved",
                "priority_changed",
                "assignee_changed",
                "note_added",
            ],
        );
        assert_coverage(&observer, &[1, 2]);
        committed = Some(persisted(&observer));
        drop(observer); // No surviving parent may checkpoint after the crash.
    });
    let mut connection = storage::open(&path).unwrap();
    let before = persisted(&connection);
    assert_eq!(
        before,
        committed.unwrap(),
        "committed mutation survives process death"
    );
    let replay = mutate(&mut connection, request, "unused", 0).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.item.revision().get(), 2);
    assert_eq!(
        persisted(&connection),
        before,
        "no extra operation, H or allocation"
    );
    assert_coverage(&connection, &[1, 2]);
}
