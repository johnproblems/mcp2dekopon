#![allow(clippy::unwrap_used)]

//! End-to-end generation against a loopback MCP mock: fetch (with paginated
//! and SSE answers), tighten, render, and the report.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

#[path = "snap.rs"]
mod snap;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(suite: &str) -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "mcp2dekopon-{}-{}-{}",
        suite,
        std::process::id(),
        id
    ))
}

/// A minimal MCP Streamable HTTP mock over a loopback socket.
struct Mock {
    url: String,
    port: u16,
    stop: Arc<AtomicBool>,
    problems: Arc<Mutex<Vec<String>>>,
    calls: Arc<Mutex<Vec<String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mock {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let problems: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = stop.clone();
        let worker_problems = problems.clone();
        let worker_calls = calls.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_stop.load(Ordering::Relaxed) {
                    break;
                }
                let mut stream = match stream {
                    Ok(stream) => stream,
                    Err(_) => continue,
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let request = read_request(&mut stream);
                let Some(request) = request else { continue };
                let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
                let method = body.get("method").and_then(Value::as_str).unwrap_or("");
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                worker_calls.lock().unwrap().push(method.to_string());
                check_protocol(&request, method, &worker_problems);
                let (status, content_type, extra, payload) = route(method, &body, id);
                write_response(&mut stream, status, &content_type, &extra, &payload);
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            port,
            stop,
            problems,
            calls,
            thread: Some(thread),
        }
    }

    fn check(&self) -> (Vec<String>, Vec<String>) {
        (
            self.calls.lock().unwrap().clone(),
            self.problems.lock().unwrap().clone(),
        )
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the blocking accept.
        let _ = std::net::TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct HttpRequest {
    headers: Vec<(String, String)>,
    body: String,
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<HttpRequest> {
    let mut raw = Vec::new();
    let mut byte = [0_u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).is_err() || raw.len() > 1_000_000 {
            return None;
        }
        raw.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&raw).into_owned();
    let mut length = 0_usize;
    let mut headers = Vec::new();
    for line in head.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let mut body = vec![0_u8; length.min(4_000_000)];
    if !body.is_empty() {
        stream.read_exact(&mut body).ok()?;
    }
    Some(HttpRequest {
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn check_protocol(request: &HttpRequest, method: &str, problems: &Arc<Mutex<Vec<String>>>) {
    let fail = |message: String| problems.lock().unwrap().push(message);
    let header = |name: &str| {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    if header("content-type").as_deref() != Some("application/json") {
        fail(format!("{method}: missing content-type"));
    }
    if !header("accept")
        .unwrap_or_default()
        .contains("text/event-stream")
    {
        fail(format!("{method}: missing SSE accept"));
    }
    if method != "initialize" {
        if header("mcp-session-id").as_deref() != Some("mock-session-1") {
            fail(format!("{method}: missing session echo"));
        }
        if header("mcp-protocol-version").as_deref() != Some("2025-06-18") {
            fail(format!("{method}: missing protocol version"));
        }
    }
}

fn route(method: &str, body: &Value, id: Value) -> (u16, String, Vec<(String, String)>, String) {
    let envelope =
        |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    match method {
        "initialize" => (
            200,
            "application/json".to_string(),
            vec![("mcp-session-id".to_string(), "mock-session-1".to_string())],
            envelope(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "serverInfo": {"name": "mock-mcp", "version": "0.0.0"},
            })),
        ),
        "notifications/initialized" => (202, "application/json".to_string(), vec![], String::new()),
        "tools/list" => {
            let cursor = body
                .get("params")
                .and_then(|p| p.get("cursor"))
                .and_then(Value::as_str);
            match cursor {
                None => (
                    200,
                    "application/json".to_string(),
                    vec![],
                    envelope(json!({
                        "tools": [search_tool(), details_tool()],
                        "nextCursor": "page2",
                    })),
                ),
                // Second page arrives as SSE, exercising the buffered reader.
                _ => {
                    let payload = envelope(json!({
                        "tools": [enum_tool(), recursive_tool(), union_tool(), remap_tool()],
                    }));
                    (
                        200,
                        "text/event-stream".to_string(),
                        vec![],
                        format!("event: message\ndata: {payload}\n\n"),
                    )
                }
            }
        }
        "tools/call" => (
            200,
            "application/json".to_string(),
            vec![],
            envelope(json!({
                "content": [{"type": "text", "text": "mock-ok"}],
            })),
        ),
        _ => (
            200,
            "application/json".to_string(),
            vec![],
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "unknown"}})
                .to_string(),
        ),
    }
}

fn write_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    content_type: &str,
    extra: &[(String, String)],
    payload: &str,
) {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n",
        payload.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
}

fn search_tool() -> Value {
    json!({
        "name": "search",
        "description": "Search the mock index.",
        "annotations": {"readOnlyHint": true},
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "The question."},
                "limit": {"type": "integer"},
            },
            "required": ["query"],
        },
    })
}

fn details_tool() -> Value {
    json!({
        "name": "get_details",
        "description": "Fetch one record with options.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "key": {"type": "string"},
                "options": {
                    "type": "object",
                    "properties": {
                        "tags": {"type": "array", "items": {"type": "string"}},
                        "labels": {"type": "object", "additionalProperties": {"type": "string"}},
                    },
                    "required": ["tags"],
                },
            },
            "required": ["key"],
        },
    })
}

fn enum_tool() -> Value {
    json!({
        "name": "locate",
        "description": "Locate a repo or a gist.",
        "annotations": {"destructiveHint": true},
        "inputSchema": {
            "type": "object",
            "properties": {
                "target": {
                    "oneOf": [
                        {"title": "Repo", "type": "object", "properties": {"repo": {"type": "string"}}, "required": ["repo"]},
                        {"title": "Gist", "type": "object", "properties": {"gist": {"type": "string"}}},
                    ],
                },
            },
            "required": ["target"],
        },
    })
}

fn recursive_tool() -> Value {
    json!({
        "name": "recurse",
        "description": "Never inlines.",
        "inputSchema": {
            "type": "object",
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {"child": {"$ref": "#/$defs/Node"}},
                },
            },
            "properties": {"node": {"$ref": "#/$defs/Node"}},
        },
    })
}

fn union_tool() -> Value {
    json!({
        "name": "either",
        "description": "Takes a value or a list of values.",
        "inputSchema": {
            "type": "object",
            "properties": {"value": {"oneOf": [{"type": "string"}, {"type": "integer"}]}},
        },
    })
}

fn remap_tool() -> Value {
    json!({
        "name": "remap",
        "description": "Takes free-form arguments.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": true,
        },
    })
}

fn gen_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_mcp2dekopon"))
}

#[test]
fn gen_from_mock_writes_a_provider_and_a_report() {
    let mock = Mock::start();
    let out = temp_dir("gen");
    let status = std::process::Command::new(gen_binary())
        .args([
            "gen",
            "--url",
            &mock.url,
            "--id",
            "mockmcp",
            "--out",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let provider = out.join("mockmcp");
    for file in [
        "Cargo.toml",
        "src/lib.rs",
        "build.sh",
        "tests/native_e2e.rs",
        "tests/conformance.rs",
        "wit/provider.wit",
        "wit/deps/provider.wit",
        "wit/deps/stdio.wit",
        "wit/deps/http.wit",
        "wit/deps/settings.wit",
        "broker.d/mockmcp.yaml",
        "mockmcp.cedar",
        "README.md",
    ] {
        assert!(provider.join(file).is_file(), "missing {file}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(provider.join("build.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "build.sh is not executable");
    }

    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["providers"][0]["id"], "mockmcp");
    assert_eq!(
        report["providers"][0]["capabilities"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(report["server"]["name"], "mock-mcp");
    let skipped = report["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["reason"], "recursive-schema");

    let capabilities = report["providers"][0]["capabilities"].as_array().unwrap();
    let remap = capabilities.iter().find(|c| c["name"] == "remap").unwrap();
    assert!(
        remap["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "free-map-as-string"),
        "{remap}"
    );
    let search = capabilities.iter().find(|c| c["name"] == "search").unwrap();
    assert_eq!(search["effect"], "read-only");
    assert_eq!(search["risk"], "Low");
    let locate = capabilities.iter().find(|c| c["name"] == "locate").unwrap();
    assert_eq!(locate["effect"], "external-write");
    assert_eq!(locate["risk"], "High");
    let details = capabilities
        .iter()
        .find(|c| c["name"] == "get_details")
        .unwrap();
    assert!(
        details["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "free-map-as-string"),
        "{details}"
    );

    let lib = std::fs::read_to_string(provider.join("src/lib.rs")).unwrap();
    assert!(lib.contains("dekopon_provider_sdk::export!(McpProvider);"));
    assert!(lib.contains("const NAME: &'static str = \"search\";"));
    assert!(lib.contains("parse_json_field_path(&mut arguments, &[\"options\", \"labels\"])?;"));

    let broker = std::fs::read_to_string(provider.join("broker.d/mockmcp.yaml")).unwrap();
    assert!(broker.contains(&format!("allowedHosts: [127.0.0.1:{}]", mock.port)));
    assert!(broker.contains("allowPlaintextLoopback: true"));

    // The mock saw the full lifecycle, paginated, with session discipline.
    let (calls, problems) = mock.check();
    assert!(
        calls.iter().filter(|c| c.as_str() == "tools/list").count() >= 2,
        "{calls:?}"
    );
    assert!(calls.contains(&"initialize".to_string()), "{calls:?}");
    assert!(problems.is_empty(), "{problems:?}");

    // Golden outputs with the ephemeral port redacted.
    let origin = mock.url.trim_end_matches("/mcp");
    let scope = format!("127.0.0.1:{}", mock.port);
    let redact = |text: &str| {
        text.replace(&mock.url, "http://127.0.0.1:PORT/mcp")
            .replace(origin, "http://127.0.0.1:PORT")
            .replace(&scope, "127.0.0.1:PORT")
    };
    snap::assert_snapshot("gen_mock_lib", &redact(&lib));
    snap::assert_snapshot("gen_mock_broker", &redact(&broker));

    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn gen_refuses_to_overwrite_without_force() {
    let mock = Mock::start();
    let out = temp_dir("force");
    let args = [
        "gen".to_string(),
        "--url".to_string(),
        mock.url.clone(),
        "--id".to_string(),
        "mockmcp".to_string(),
        "--out".to_string(),
        out.to_str().unwrap().to_string(),
    ];
    assert!(std::process::Command::new(gen_binary())
        .args(&args)
        .status()
        .unwrap()
        .success());
    let second = std::process::Command::new(gen_binary())
        .args(&args)
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("--force"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(std::process::Command::new(gen_binary())
        .args(args.iter().chain(["--force".to_string()].iter()))
        .status()
        .unwrap()
        .success());
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn gen_rejects_bad_identity_and_endpoints() {
    let out = temp_dir("bad");
    let binary = gen_binary();
    for (url, id) in [
        ("https://mcp.example.test/mcp", "BadId"),
        ("ftp://mcp.example.test/mcp", "mockmcp"),
        ("https://mcp.example.test/mcp?a=b", "mockmcp"),
    ] {
        let output = std::process::Command::new(&binary)
            .args([
                "gen",
                "--url",
                url,
                "--id",
                id,
                "--out",
                out.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{url} {id}");
    }
    let _ = std::fs::remove_dir_all(&out);
}
