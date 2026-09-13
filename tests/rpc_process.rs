use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

fn root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("bif-rpc-{name}-{nonce}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn bif(arguments: &[&str], input: Option<&str>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bif"));
    command
        .args(arguments)
        .env_remove("BIF_CONFIG")
        .env_remove("BIF_ROOT")
        .env_remove("BIF_REQUESTER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

#[test]
fn rpc_process_handles_one_mutation_then_one_read_with_clean_stdout() {
    let root = root("round-trip");
    let root_text = root.to_str().unwrap();
    let config = root.join("config.toml");
    let initialized = bif(
        &[
            "init",
            "--root",
            root_text,
            "--requester",
            "DAVIS",
            "--config",
            config.to_str().unwrap(),
        ],
        None,
    );
    assert!(initialized.status.success());

    let capture = r#"{"protocol_version":1,"request_id":"capture-1","operation":"capture","params":{"idempotency_key":"key-1","requester":"DAVIS","project":"widgets","title":"RPC task"}}"#;
    let output = bif(
        &["rpc", "--root", root_text, "--requester", "DAVIS"],
        Some(capture),
    );
    assert_eq!(output.status.code(), Some(0));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["request_id"], "capture-1");
    assert_eq!(response["result"]["item"]["id"], "DAVIS:widgets:001");
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );

    let get = r#"{"protocol_version":1,"request_id":"get-1","operation":"get","params":{"item_id":"DAVIS:widgets:001"}}"#;
    let output = bif(
        &["rpc", "--root", root_text, "--requester", "DAVIS"],
        Some(get),
    );
    assert_eq!(output.status.code(), Some(0));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["item"]["title"], "RPC task");
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rpc_process_returns_protocol_error_and_exit_code_when_not_initialized() {
    let root = root("not-initialized");
    let request = r#"{"protocol_version":1,"request_id":"missing","operation":"list","params":{}}"#;
    let output = bif(
        &[
            "rpc",
            "--root",
            root.to_str().unwrap(),
            "--requester",
            "DAVIS",
        ],
        Some(request),
    );
    assert_eq!(output.status.code(), Some(9));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["request_id"], "missing");
    assert_eq!(response["error"]["code"], "not_initialized");
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    assert!(!String::from_utf8_lossy(&output.stderr).is_empty());

    fs::remove_dir_all(root).unwrap();
}
