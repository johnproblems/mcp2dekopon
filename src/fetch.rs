//! Stage 1: fetch the tool list over MCP Streamable HTTP.
//!
//! One endpoint, three calls: `initialize`, the `notifications/initialized`
//! notice (best effort), then `tools/list` following `nextCursor`.

use reqwest::blocking::{Client, Response};
use serde_json::{json, Value};

use crate::rpc::{self, RpcError};

/// One MCP tool as listed by the server.
#[derive(Clone, Debug)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    /// The raw listed object, kept so `--save-tools` round-trips faithfully.
    pub raw: Value,
}

/// Who the server claims to be.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

/// A tool list saved with `--save-tools` and replayed with `--from-file`.
/// Tools stay raw MCP tool objects so loading reuses the fetch parser.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedTools {
    pub format_version: u32,
    pub server: ServerInfo,
    pub tools: Vec<Value>,
}

impl SavedTools {
    pub fn save(
        path: &std::path::Path,
        server: &ServerInfo,
        tools: &[McpTool],
    ) -> Result<(), String> {
        let saved = Self {
            format_version: 1,
            server: server.clone(),
            tools: tools.iter().map(|tool| tool.raw.clone()).collect(),
        };
        let text = serde_json::to_string_pretty(&saved)
            .map_err(|e| format!("cannot encode tool list: {e}"))?;
        std::fs::write(path, format!("{text}\n"))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    pub fn load(path: &std::path::Path) -> Result<(ServerInfo, Vec<McpTool>), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let saved: Self = serde_json::from_str(&text)
            .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        if saved.format_version != 1 {
            return Err(format!(
                "unsupported tool list version {} in {}",
                saved.format_version,
                path.display()
            ));
        }
        let mut tools = Vec::with_capacity(saved.tools.len());
        for raw in &saved.tools {
            tools.push(parse_tool(raw).map_err(|e| e.to_string())?);
        }
        Ok((saved.server, tools))
    }
}

/// Why a fetch failed. Carries status codes and server messages, never secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    Transport(String),
    Status(u16),
    Unauthorized(u16),
    Body(String),
    Rpc(RpcError),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(formatter, "request failed: {message}"),
            Self::Status(status) => write!(formatter, "server returned status {status}"),
            Self::Unauthorized(status) => write!(
                formatter,
                "server returned status {status}; if the server needs auth, pass --header \"Authorization: ...\""
            ),
            Self::Body(message) => write!(formatter, "bad response: {message}"),
            Self::Rpc(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for FetchError {}

const MAX_PAGES: usize = 100;
const MAX_TOOLS: usize = 1000;
const MAX_ATTEMPTS: u32 = 3;

/// Validated `--url`: the vendor base, its origin and the broker scope entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The URL without a trailing slash, baked in as the vendor default.
    pub vendor_base: String,
    /// `scheme://host[:port]` for display and the broker fragment.
    pub origin: String,
    /// The `allowedHosts` entry: bare host for HTTPS on 443, else `host:port`.
    pub host_scope: String,
    /// Whether the broker grant needs `allowPlaintextLoopback`.
    pub plaintext_loopback: bool,
    /// Set when the fragment alone cannot authorize this destination.
    pub plaintext_note: Option<String>,
}

/// Validates a `--url` the way the SDK's `Base` would: `http(s)`, a host, no
/// userinfo, query or fragment.
pub fn validate_endpoint(raw: &str) -> Result<Endpoint, String> {
    let url = url::Url::parse(raw).map_err(|e| format!("invalid --url: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("invalid --url: the scheme must be http:// or https://".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("invalid --url: userinfo is not allowed".to_string());
    }
    if url.query().is_some() {
        return Err("invalid --url: a query string is not allowed".to_string());
    }
    if url.fragment().is_some() {
        return Err("invalid --url: a fragment is not allowed".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "invalid --url: a host is required".to_string())?;
    if raw.chars().any(char::is_whitespace) {
        return Err("invalid --url: whitespace is not allowed".to_string());
    }
    let default_port = if url.scheme() == "https" { 443 } else { 80 };
    let port = url.port().unwrap_or(default_port);
    let host_scope = if url.scheme() == "https" && port == 443 {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    let origin = format!("{}://{host_scope}", url.scheme());
    let vendor_base = raw.trim_end_matches('/').to_string();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
        || host.starts_with("127.")
        || host == "[::1]";
    let plaintext = url.scheme() == "http";
    let plaintext_note = if plaintext && !loopback {
        Some(
            "this origin speaks plaintext HTTP outside loopback; the broker also needs \
             an `http.plaintextHosts` entry in host.yaml (see the generated README)"
                .to_string(),
        )
    } else {
        None
    };
    Ok(Endpoint {
        vendor_base,
        origin,
        host_scope,
        plaintext_loopback: plaintext && loopback,
        plaintext_note,
    })
}

/// Fetches every tool the server lists: `initialize`, the
/// `notifications/initialized` notice (best effort), then `tools/list`
/// following `nextCursor`. Transient failures are retried; an expired session
/// (400/404) re-initializes once per page.
pub fn fetch_tools(
    endpoint: &str,
    extra_headers: &[(String, String)],
    timeout_secs: u64,
    protocol_version: &str,
    client_name: &str,
    client_version: &str,
    verbose: bool,
) -> Result<(ServerInfo, Vec<McpTool>), FetchError> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs.max(1)))
        .build()
        .map_err(|e| FetchError::Transport(trim_error(e.to_string())))?;
    let session = FetchSession {
        client: &client,
        endpoint,
        extra_headers,
        offered: protocol_version,
        client_name,
        client_version,
        verbose,
        next_id: std::cell::Cell::new(0_u64),
    };

    let (info, mut session_id, mut negotiated) = session.initialize()?;

    // Best effort: some servers require it before tools/list, others 404 it.
    let notice = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    });
    let _ignored = session.post(session_id.as_deref(), Some(&negotiated), &notice);

    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params = serde_json::Map::new();
        if let Some(next) = &cursor {
            params.insert("cursor".to_string(), Value::String(next.clone()));
        }
        let id = session.claim_id();
        let list = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": params,
        });
        let response = match session.post(session_id.as_deref(), Some(&negotiated), &list) {
            Err(FetchError::Status(400 | 404)) => {
                session.log("session rejected; re-initializing once");
                (_, session_id, negotiated) = session.initialize()?;
                session.post(session_id.as_deref(), Some(&negotiated), &list)?
            }
            other => other?,
        };
        let payload = rpc_body(response)?;
        check_id(&payload, id)?;
        let result = rpc::rpc_result(&payload).map_err(FetchError::Rpc)?;
        let listed = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| FetchError::Body("tools/list result had no tools array".to_string()))?;
        for tool in listed {
            tools.push(parse_tool(tool)?);
            if tools.len() > MAX_TOOLS {
                return Err(FetchError::Body(format!(
                    "server listed more than {MAX_TOOLS} tools"
                )));
            }
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|next| !next.is_empty())
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    if cursor.is_some() {
        return Err(FetchError::Body(format!(
            "tools/list pagination did not end after {MAX_PAGES} pages"
        )));
    }
    Ok((info, tools))
}

struct FetchSession<'a> {
    client: &'a Client,
    endpoint: &'a str,
    extra_headers: &'a [(String, String)],
    offered: &'a str,
    client_name: &'a str,
    client_version: &'a str,
    verbose: bool,
    next_id: std::cell::Cell<u64>,
}

impl FetchSession<'_> {
    fn log(&self, message: &str) {
        if self.verbose {
            eprintln!("fetch: {message}");
        }
    }

    fn claim_id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    fn initialize(&self) -> Result<(ServerInfo, Option<String>, String), FetchError> {
        let id = self.claim_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": self.offered,
                "capabilities": {},
                "clientInfo": {"name": self.client_name, "version": self.client_version},
            },
        });
        let response = self.post(None, None, &body)?;
        let session = session_id(&response);
        let payload = rpc_body(response)?;
        check_id(&payload, id)?;
        let result = rpc::rpc_result(&payload).map_err(FetchError::Rpc)?;
        let negotiated = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(self.offered)
            .to_owned();
        let server = result.get("serverInfo");
        let info = ServerInfo {
            name: server
                .and_then(|s| s.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            version: server
                .and_then(|s| s.get("version"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            protocol_version: negotiated.clone(),
        };
        Ok((info, session, negotiated))
    }

    /// POSTs once, retrying transient failures with bounded backoff.
    fn post(
        &self,
        session: Option<&str>,
        protocol_version: Option<&str>,
        body: &Value,
    ) -> Result<Response, FetchError> {
        let mut attempt = 0_u32;
        loop {
            attempt += 1;
            match post_once(
                self.client,
                self.endpoint,
                self.extra_headers,
                session,
                protocol_version,
                body,
            ) {
                Ok(response) => return Ok(response),
                Err((error, retryable)) if retryable && attempt < MAX_ATTEMPTS => {
                    self.log(&format!("attempt {attempt} failed ({error}); retrying"));
                    std::thread::sleep(std::time::Duration::from_millis(200 * u64::from(attempt)));
                }
                Err((error, _)) => return Err(error),
            }
        }
    }
}

/// Verifies a JSON-RPC response carries the request's id.
fn check_id(payload: &Value, expected: u64) -> Result<(), FetchError> {
    let matches = payload.get("id").is_some_and(|id| {
        id.as_u64() == Some(expected)
            || id
                .as_i64()
                .is_some_and(|got| got >= 0 && got as u64 == expected)
    });
    if matches {
        Ok(())
    } else {
        Err(FetchError::Body(format!(
            "response id did not match request {expected}"
        )))
    }
}

fn parse_tool(tool: &Value) -> Result<McpTool, FetchError> {
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| FetchError::Body("a listed tool had no name".to_string()))?
        .to_owned();
    let annotations = tool.get("annotations");
    Ok(McpTool {
        name,
        description: tool
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_schema: tool.get("inputSchema").cloned().unwrap_or(Value::Null),
        read_only: annotations
            .and_then(|a| a.get("readOnlyHint"))
            .and_then(Value::as_bool),
        destructive: annotations
            .and_then(|a| a.get("destructiveHint"))
            .and_then(Value::as_bool),
        raw: tool.clone(),
    })
}

/// One POST attempt. The flag says whether the failure is worth retrying.
fn post_once(
    client: &Client,
    endpoint: &str,
    extra_headers: &[(String, String)],
    session: Option<&str>,
    protocol_version: Option<&str>,
    body: &Value,
) -> Result<Response, (FetchError, bool)> {
    let mut request = client
        .post(endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (name, value) in extra_headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    if let Some(version) = protocol_version {
        request = request.header("mcp-protocol-version", version);
    }
    let response = match request.json(body).send() {
        Ok(response) => response,
        Err(error) => {
            let retryable = error.is_timeout() || error.is_connect();
            return Err((
                FetchError::Transport(trim_error(error.to_string())),
                retryable,
            ));
        }
    };
    let status = response.status().as_u16();
    if response.status().is_success() {
        return Ok(response);
    }
    if matches!(status, 401 | 403 | 407) {
        return Err((FetchError::Unauthorized(status), false));
    }
    let retryable = matches!(status, 500 | 502 | 503 | 504);
    Err((FetchError::Status(status), retryable))
}

fn rpc_body(response: Response) -> Result<Value, FetchError> {
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .bytes()
        .map_err(|e| FetchError::Transport(trim_error(e.to_string())))?;
    rpc::parse_rpc_body(&bytes, content_type.as_deref()).map_err(FetchError::Body)
}

fn session_id(response: &Response) -> Option<String> {
    response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn trim_error(message: String) -> String {
    let mut short: String = message.chars().take(300).collect();
    if short.len() != message.len() {
        short.push_str("...");
    }
    short
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn response_ids_must_match() {
        assert!(check_id(&json!({"id": 3}), 3).is_ok());
        assert!(check_id(&json!({"id": 3}), 4).is_err());
        assert!(check_id(&json!({}), 0).is_err());
        assert!(check_id(&json!({"id": "3"}), 3).is_err());
    }

    #[test]
    fn endpoints_validate_like_base() {
        let https = validate_endpoint("https://mcp.deepwiki.com/mcp/").unwrap();
        assert_eq!(https.vendor_base, "https://mcp.deepwiki.com/mcp");
        assert_eq!(https.origin, "https://mcp.deepwiki.com");
        assert_eq!(https.host_scope, "mcp.deepwiki.com");
        assert!(!https.plaintext_loopback);
        assert!(https.plaintext_note.is_none());

        let loopback = validate_endpoint("http://127.0.0.1:8787/mcp").unwrap();
        assert_eq!(loopback.host_scope, "127.0.0.1:8787");
        assert!(loopback.plaintext_loopback);
        assert!(loopback.plaintext_note.is_none());

        let lan = validate_endpoint("http://rpi.lan:8080/mcp").unwrap();
        assert!(!lan.plaintext_loopback);
        assert!(lan.plaintext_note.is_some());

        assert!(validate_endpoint("ftp://x.test/mcp").is_err());
        assert!(validate_endpoint("https://user@x.test/mcp").is_err());
        assert!(validate_endpoint("https://x.test/mcp?a=b").is_err());
        assert!(validate_endpoint("https://x.test/mcp#top").is_err());
        assert!(validate_endpoint("https://x.test/m cp").is_err());
        assert!(validate_endpoint("not a url").is_err());
    }
}
