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
}

/// Who the server claims to be.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

/// Why a fetch failed. Carries status codes and server messages, never secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    Transport(String),
    Status(u16),
    Body(String),
    Rpc(RpcError),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(formatter, "request failed: {message}"),
            Self::Status(status) => write!(formatter, "server returned status {status}"),
            Self::Body(message) => write!(formatter, "bad response: {message}"),
            Self::Rpc(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for FetchError {}

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

/// Fetches every tool the server lists.
pub fn fetch_tools(
    endpoint: &str,
    extra_headers: &[(String, String)],
    timeout_secs: u64,
    protocol_version: &str,
    client_name: &str,
    client_version: &str,
) -> Result<(ServerInfo, Vec<McpTool>), FetchError> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs.max(1)))
        .build()
        .map_err(|e| FetchError::Transport(trim_error(e.to_string())))?;

    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": protocol_version,
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": client_version},
        },
    });
    let response = post(&client, endpoint, extra_headers, None, None, &initialize)?;
    let session = session_id(&response);
    let payload = rpc_body(response)?;
    let result = rpc::rpc_result(&payload).map_err(FetchError::Rpc)?;
    let negotiated = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(protocol_version)
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

    // Best effort: some servers require it before tools/list, others 404 it.
    let notice = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    });
    let _ignored = post(
        &client,
        endpoint,
        extra_headers,
        session.as_deref(),
        Some(&negotiated),
        &notice,
    );

    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..100 {
        let mut params = serde_json::Map::new();
        if let Some(next) = &cursor {
            params.insert("cursor".to_string(), Value::String(next.clone()));
        }
        let list = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": params,
        });
        let response = post(
            &client,
            endpoint,
            extra_headers,
            session.as_deref(),
            Some(&negotiated),
            &list,
        )?;
        let payload = rpc_body(response)?;
        let result = rpc::rpc_result(&payload).map_err(FetchError::Rpc)?;
        let listed = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| FetchError::Body("tools/list result had no tools array".to_string()))?;
        for tool in listed {
            tools.push(parse_tool(tool)?);
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok((info, tools))
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
    })
}

fn post(
    client: &Client,
    endpoint: &str,
    extra_headers: &[(String, String)],
    session: Option<&str>,
    protocol_version: Option<&str>,
    body: &Value,
) -> Result<Response, FetchError> {
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
    let response = request
        .json(body)
        .send()
        .map_err(|e| FetchError::Transport(trim_error(e.to_string())))?;
    if !response.status().is_success() {
        return Err(FetchError::Status(response.status().as_u16()));
    }
    Ok(response)
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
