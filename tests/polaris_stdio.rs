use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_workspace() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("i-agent-polaris-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(dir.join("assets/skills")).unwrap();
    dir
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&request);
        let Some(header_end) = text.find("\r\n\r\n") else {
            continue;
        };
        let content_length = text[..header_end]
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + content_length {
            break;
        }
    }
    String::from_utf8(request).unwrap()
}

fn write_sse(stream: &mut std::net::TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
}

#[test]
fn polaris_stdio_reads_stdin_emits_ndjson_and_keeps_workspace_stateless() {
    let workspace = temp_workspace();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":1}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"收到\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        write_sse(&mut stream, body);
        request
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_i-agent"))
        .args(["--polaris-stdio", "-C"])
        .arg(&workspace)
        .env("ANTHROPIC_AUTH_TOKEN", "test-token")
        .env("ANTHROPIC_BASE_URL", format!("http://{address}"))
        .env("ANTHROPIC_MODEL", "test-model")
        .env("I_AGENT_ASSETS", workspace.join("assets"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("只回复收到".as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdout must contain NDJSON only"))
        .collect();
    assert!(events
        .iter()
        .any(|event| { event["type"] == "delta" && event["text"] == "收到" }));
    let result = events
        .iter()
        .find(|event| event["type"] == "result")
        .expect("result event missing");
    assert_eq!(result["ok"], true);
    assert_eq!(result["usage"]["input_tokens"], 5);
    assert_eq!(result["usage"]["cached_input_tokens"], 1);
    assert_eq!(result["usage"]["output_tokens"], 2);
    let usage = events
        .iter()
        .find(|event| event["type"] == "usage")
        .expect("dedicated usage event missing");
    assert_eq!(usage["input_tokens"], 5);
    assert_eq!(usage["cached_input_tokens"], 1);
    assert_eq!(usage["output_tokens"], 2);
    assert_eq!(usage["requests"], 1);
    assert!(
        !workspace.join(".i-agent").exists(),
        "Polaris stdio mode must not write i-agent session state"
    );

    let request = server.join().unwrap();
    assert!(request.contains("只回复收到"));
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn polaris_stdio_emits_structured_tool_events() {
    let workspace = temp_workspace();
    std::fs::write(workspace.join("note.txt"), "tool worked").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        let tool_body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"read\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"note.txt\\\"}\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let final_body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"完成\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        for body in [tool_body, final_body] {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            write_sse(&mut stream, body);
        }
        requests
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_i-agent"))
        .args(["--polaris-stdio", "-C"])
        .arg(&workspace)
        .env("ANTHROPIC_AUTH_TOKEN", "test-token")
        .env("ANTHROPIC_BASE_URL", format!("http://{address}"))
        .env("ANTHROPIC_MODEL", "test-model")
        .env("I_AGENT_ASSETS", workspace.join("assets"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("读取 note.txt".as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdout must contain NDJSON only"))
        .collect();
    let tool = events
        .iter()
        .find(|event| event["type"] == "tool")
        .expect("tool event missing");
    assert_eq!(tool["name"], "read");
    assert!(tool["detail"].as_str().unwrap().contains("note.txt"));
    assert!(events
        .iter()
        .any(|event| event["type"] == "result" && event["ok"] == true));
    assert_eq!(server.join().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn polaris_stdio_rejects_incompatible_or_unknown_flags_with_ndjson_only() {
    for args in [
        vec!["--polaris-stdio", "--branches"],
        vec!["--unknown-machine-flag", "--polaris-stdio"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_i-agent"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stderr.is_empty());
        let lines: Vec<_> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 1);
        let event: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(event["type"], "result");
        assert_eq!(event["ok"], false);
    }
}

#[test]
fn polaris_stdio_rejects_every_missing_option_value_with_ndjson_only() {
    for flag in [
        "--variants",
        "--prepare",
        "--branch",
        "--from",
        "-C",
        "--dir",
        "--provider",
        "-m",
        "--model",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_i-agent"))
            .args(["--polaris-stdio", flag])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "flag {flag}");
        assert!(output.stderr.is_empty(), "flag {flag}");
        let lines: Vec<_> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 1, "flag {flag}");
        let event: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(event["type"], "result", "flag {flag}");
        assert_eq!(event["ok"], false, "flag {flag}");
        assert!(
            event["error"].as_str().unwrap().contains(flag),
            "flag {flag}"
        );
    }
}
