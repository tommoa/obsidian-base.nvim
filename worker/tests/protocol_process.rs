use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};

use serde_json::{Value, json};
use tempfile::TempDir;

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"id": id, "request": {"method": method, "params": params}})
}

#[test]
fn framing_pipeline_event_order_and_shutdown() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("Host.md"), "# Host\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_obsidian-base-worker"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, "not json").unwrap();
    writeln!(
        stdin,
        "{}",
        request(1, "initialize", json!({"vault_root": root.path()}))
    )
    .unwrap();
    writeln!(stdin, "{}", request(2, "inspect", json!({}))).unwrap();
    writeln!(
        stdin,
        "{}",
        request(
            3,
            "overlay_upsert",
            json!({"path": "Draft.md", "contents": "# Draft\n"})
        )
    )
    .unwrap();
    writeln!(stdin, "{}", request(4, "shutdown", json!({}))).unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let mut envelopes = Vec::new();
    let mut line = String::new();
    while stdout.read_line(&mut line).unwrap() != 0 {
        envelopes.push(serde_json::from_str::<Value>(&line).unwrap());
        line.clear();
    }
    assert!(child.wait().unwrap().success());
    assert_eq!(envelopes[0]["response"]["type"], "error");
    assert_eq!(envelopes[0]["response"]["error"]["code"], "invalid_request");
    assert_eq!(envelopes[1]["id"], 1);
    assert_eq!(envelopes[2]["id"], 2);
    assert_eq!(envelopes[3]["event"]["type"], "index_changed");
    assert_eq!(envelopes[3]["event"]["origin"], "overlay");
    assert_eq!(envelopes[4]["id"], 3);
    assert_eq!(envelopes[5]["response"]["result"]["method"], "shutdown");
}

#[test]
fn rejects_unknown_request_fields() {
    let output = run(&[
        json!({"id": 7, "request": {"method": "inspect", "params": {}}, "extra": true}),
        json!({"id": 8, "request": {"method": "inspect", "params": {}, "extra": true}}),
        request(9, "shutdown", json!({})),
    ]);
    assert_eq!(output[0]["id"], 0);
    assert_eq!(output[0]["response"]["error"]["code"], "invalid_request");
    assert_eq!(output[1]["id"], 0);
    assert_eq!(output[1]["response"]["error"]["code"], "invalid_request");
}

#[test]
fn query_before_initialize_is_a_structured_error() {
    let output = run(&[
        request(
            1,
            "query",
            json!({
                "source": {"kind": "inline", "text": "views: []"},
                "host_path": "Host.md"
            }),
        ),
        request(2, "shutdown", json!({})),
    ]);
    assert_eq!(output[0]["id"], 1);
    assert_eq!(output[0]["response"]["error"]["code"], "not_initialized");
}

#[test]
fn query_error_includes_recovery_views() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("Host.md"), "# Host\n").unwrap();
    let output = run(&[
        request(1, "initialize", json!({"vault_root": root.path()})),
        request(
            2,
            "query",
            json!({
                "source": {
                    "kind": "inline",
                    "text": "views:\n  - type: table\n    name: Current\n    order: []\n"
                },
                "host_path": "Host.md",
                "view_name": "Removed"
            }),
        ),
        request(3, "shutdown", json!({})),
    ]);
    assert_eq!(output[1]["response"]["error"]["code"], "unknown_view");
    assert_eq!(
        output[1]["response"]["error"]["available_views"],
        json!([{"name": "Current", "type": "table"}])
    );
}

#[test]
fn rejects_zero_limit_overrides() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("Host.md"), "# Host\n").unwrap();
    let output = run(&[
        request(
            1,
            "initialize",
            json!({"vault_root": root.path(), "limits": {"source_bytes": 0}}),
        ),
        request(2, "shutdown", json!({})),
    ]);
    assert_eq!(output[0]["id"], 0);
    assert_eq!(output[0]["response"]["error"]["code"], "invalid_request");
}

fn run(requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_obsidian-base-worker"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
