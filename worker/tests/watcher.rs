use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn watches_new_nested_directories_and_publishes_after_rescan() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("Host.md"), "# Host\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_obsidian-base-worker"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender
                .send(serde_json::from_str::<Value>(&line.unwrap()).unwrap())
                .is_err()
            {
                break;
            }
        }
    });
    writeln!(
        stdin,
        "{}",
        json!({"id": 1, "request": {"method": "initialize", "params": {"vault_root": root.path()}}})
    )
    .unwrap();
    stdin.flush().unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap()["id"],
        1
    );
    std::fs::create_dir(root.path().join("Nested")).unwrap();
    std::fs::write(root.path().join("Nested/Observed.md"), "# Observed\n").unwrap();
    let event = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(event["event"]["type"], "index_changed");
    writeln!(
        stdin,
        "{}",
        json!({"id": 2, "request": {"method": "inspect", "params": {}}})
    )
    .unwrap();
    stdin.flush().unwrap();
    let inspect = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(inspect["response"]["result"]["data"]["files"], 2);
    writeln!(
        stdin,
        "{}",
        json!({"id": 3, "request": {"method": "shutdown", "params": {}}})
    )
    .unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
