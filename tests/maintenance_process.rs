//! Deterministic process-crash evidence for Online Backup and activation.
//! Test-only child controls; no production hooks, timing sleeps, or power-loss claims.
mod maintenance_support;

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
};

use bif::storage::{self, maintenance};
use maintenance_support::{Fixture, capture, generation, inventory, read};
use rusqlite::backup::{Backup, StepResult};

#[test]
fn committed_activation_without_response_survives_kill_inspection_and_retry() {
    use bif::storage::journal::Generation;
    let fixture = Fixture::new();
    let generation = generation(&read(&fixture.store));
    let mut expected = inventory(&read(&fixture.store));
    expected
        .iter_mut()
        .find(|(name, _)| name == "journal_metadata")
        .unwrap()
        .1[0][4] = 1_i64.into();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "activation_boundary_child",
            "--ignored",
            "--nocapture",
        ])
        .env("BIF_TEST_TARGET", &fixture.store)
        .env("BIF_TEST_GENERATION", &generation)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut child = ReapedChild::new(child);
    let mut output = BufReader::new(child.process.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            output.read_line(&mut line).unwrap() > 0,
            "child exited before commit boundary"
        );
        if line.trim() == "ACTIVATION_COMMITTED_BEFORE_RESPONSE" {
            break;
        }
    }
    assert!(forced_termination(child.kill_and_reap().unwrap()));
    // Inspect first: missing acknowledgement must not be interpreted as rollback.
    assert_eq!(inventory(&read(&fixture.store)), expected);
    maintenance::activate(&fixture.store, &Generation::parse(&generation).unwrap()).unwrap();
    assert_eq!(inventory(&storage::open(&fixture.store).unwrap()), expected);
}

#[test]
#[ignore = "test-only child invoked at activation commit-before-response boundary"]
fn activation_boundary_child() {
    let target = std::env::var_os("BIF_TEST_TARGET").expect("test target");
    let generation = bif::storage::journal::Generation::parse(
        &std::env::var("BIF_TEST_GENERATION").expect("test generation"),
    )
    .unwrap();
    maintenance::activate(std::path::Path::new(&target), &generation).unwrap();
    // Test-only handshake, NOT the operation response. The real API has committed;
    // the parent kills/reaps us before we can publish a simulated success response.
    println!("ACTIVATION_COMMITTED_BEFORE_RESPONSE");
    std::io::stdout().flush().unwrap();
    let mut release = [0];
    std::io::stdin().read_exact(&mut release).unwrap();
    panic!("parent should kill/reap before a response");
}

#[test]
fn killed_backup_process_reopens_as_unchanged_or_fully_restored() {
    for phase in ["before", "incomplete", "done"] {
        let fixture = Fixture::new();
        maintenance::backup(&fixture.store, &fixture.backup).unwrap();
        // Prepare a real maintenance-produced stage with its independent generation.
        let stage = fixture.path("stage.sqlite");
        maintenance::backup(&fixture.backup, &stage).unwrap();
        maintenance::restore(&stage, &fixture.backup, fixture.path("stage-preserved")).unwrap();
        let restored = inventory(&read(&stage));
        let mut current = storage::open(&fixture.store).unwrap();
        capture(&mut current, "later", "later committed write");
        let before = inventory(&current);
        drop(current);
        let preserved = fixture.path("preserved.sqlite");
        maintenance::backup(&fixture.store, &preserved).unwrap();
        let backup_bytes = std::fs::read(&fixture.backup).unwrap();
        let preservation_bytes = std::fs::read(&preserved).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "backup_boundary_child",
                "--ignored",
                "--nocapture",
            ])
            .env("BIF_TEST_STAGE", &stage)
            .env("BIF_TEST_TARGET", &fixture.store)
            .env("BIF_TEST_PHASE", phase)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut child = ReapedChild::new(child);
        let mut output = BufReader::new(child.process.stdout.take().unwrap());
        let mut ready = String::new();
        loop {
            ready.clear();
            assert!(
                output.read_line(&mut ready).unwrap() > 0,
                "child exited before boundary"
            );
            if ready.trim() == "BACKUP_BOUNDARY_READY" {
                break;
            }
        }
        let status = child.kill_and_reap().unwrap();
        assert!(
            forced_termination(status),
            "expected forced termination, got {status}"
        );
        // Kill skips Backup::drop and Connection::drop: this is SQLite WAL crash
        // recovery rather than just the graceful finish rollback test.
        let reopened = read(&fixture.store);
        let actual = inventory(&reopened);
        assert_eq!(actual, if phase == "done" { restored } else { before });
        assert_eq!(
            reopened
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(
            reopened
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read(&fixture.backup).unwrap(), backup_bytes);
        assert_eq!(std::fs::read(&preserved).unwrap(), preservation_bytes);
    }
}

/// Keep stdin separate: Child::wait otherwise closes it before waiting, which
/// can let EOF trigger graceful Backup::drop before the kill takes effect.
struct ReapedChild {
    process: Child,
    stdin: Option<ChildStdin>,
    reaped: bool,
}

impl ReapedChild {
    fn new(mut process: Child) -> Self {
        Self {
            stdin: process.stdin.take(),
            process,
            reaped: false,
        }
    }

    fn reap(&mut self) -> io::Result<ExitStatus> {
        let status = self.process.wait()?;
        self.reaped = true;
        Ok(status)
    }

    fn kill_and_reap(&mut self) -> io::Result<ExitStatus> {
        self.process.kill()?;
        self.reap()
    }
}

impl Drop for ReapedChild {
    fn drop(&mut self) {
        if !self.reaped {
            // Readiness/assertion/I/O failures must also terminate and reap.
            // stdin remains alive throughout this cleanup, including wait.
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}

#[cfg(unix)]
fn forced_termination(status: ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    status.signal() == Some(9) // SIGKILL, as sent by Child::kill on Unix.
}

#[cfg(windows)]
fn forced_termination(status: ExitStatus) -> bool {
    // Rust's Child::kill uses TerminateProcess with exit code 1 on Windows.
    status.code() == Some(1)
}

#[cfg(not(any(unix, windows)))]
fn forced_termination(_status: ExitStatus) -> bool {
    false // No verified forced-termination status on other platforms.
}

#[test]
fn graceful_child_panic_is_not_forced_termination() {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "graceful_panic_child",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = ReapedChild::new(child);
    // Deterministic EOF causes a normal Rust panic/unwind, not a signal kill.
    drop(child.stdin.take());
    let status = child.reap().unwrap();
    assert_eq!(
        status.code(),
        Some(101),
        "control must exit through test-harness panic"
    );
    assert!(
        !forced_termination(status),
        "graceful panic must be rejected as crash evidence"
    );
}

#[test]
#[ignore = "test-only graceful-unwind control"]
fn graceful_panic_child() {
    let mut release = [0];
    std::io::stdin().read_exact(&mut release).unwrap();
}

#[test]
#[ignore = "test-only child invoked with explicit disposable paths"]
fn backup_boundary_child() {
    let stage = std::env::var_os("BIF_TEST_STAGE").expect("test stage");
    let target = std::env::var_os("BIF_TEST_TARGET").expect("test target");
    let phase = std::env::var("BIF_TEST_PHASE").expect("test phase");
    let source = read(std::path::Path::new(&stage));
    let mut destination = storage::open(std::path::Path::new(&target)).unwrap();
    destination
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    let transfer = Backup::new(&source, &mut destination).unwrap();
    match phase.as_str() {
        "before" => {}
        "incomplete" => {
            assert_eq!(transfer.step(1).unwrap(), StepResult::More);
            assert!(transfer.progress().remaining > 0);
        }
        "done" => assert_eq!(transfer.step(-1).unwrap(), StepResult::Done),
        _ => panic!("unknown test boundary"),
    }
    println!("BACKUP_BOUNDARY_READY");
    std::io::stdout().flush().unwrap();
    let mut release = [0];
    std::io::stdin().read_exact(&mut release).unwrap();
    panic!("parent should terminate/reap this child, never release it");
}
