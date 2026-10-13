//! JSON-RPC response parsing for MCP Streamable HTTP.
//!
//! A server answers a request with plain JSON or with a buffered
//! `text/event-stream` body; both carry the one JSON-RPC message. The same
//! logic is embedded into generated providers (see `codegen`); keep the two
//! in sync.

use serde_json::Value;

/// Parses one response body into its JSON-RPC message.
pub fn parse_rpc_body(body: &[u8], content_type: Option<&str>) -> Result<Value, String> {
    let text = std::str::from_utf8(body).map_err(|_| "response body was not UTF-8".to_string())?;
    let looks_sse = content_type
        .is_some_and(|kind| kind.to_ascii_lowercase().contains("text/event-stream"))
        || text.trim_start().starts_with("data:")
        || text.trim_start().starts_with("event:");
    if looks_sse {
        sse_json(text).or_else(|_| parse_json(text))
    } else {
        parse_json(text)
    }
}

fn parse_json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|_| "response body was not JSON".to_string())
}

/// Extracts the last data payload that looks like a JSON-RPC response.
fn sse_json(text: &str) -> Result<Value, String> {
    let mut payloads: Vec<String> = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            if !data.is_empty() {
                payloads.push(data.join("\n"));
                data.clear();
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if !data.is_empty() {
        payloads.push(data.join("\n"));
    }
    for payload in payloads.iter().rev() {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(payload) {
            if map.contains_key("result") || map.contains_key("error") {
                return Ok(Value::Object(map));
            }
        }
    }
    Err("no JSON-RPC message in SSE response".to_string())
}

/// Splits a JSON-RPC message into its result or its error.
pub fn rpc_result(payload: &Value) -> Result<&Value, RpcError> {
    if let Some(error) = payload.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("mcp error")
            .to_owned();
        return Err(RpcError { code, message });
    }
    payload
        .get("result")
        .filter(|result| result.is_object())
        .ok_or_else(|| RpcError {
            code: 0,
            message: "JSON-RPC response had no result".to_string(),
        })
}

/// A JSON-RPC error object, or a malformed response described as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "rpc error {}: {}", self.code, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_json_parses() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        assert_eq!(
            parse_rpc_body(body, Some("application/json")).unwrap(),
            json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}})
        );
    }

    #[test]
    fn sse_body_yields_the_last_rpc_message() {
        let body = b"event: ping\ndata: {\"envelope\":\"noise\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        assert_eq!(
            parse_rpc_body(body, Some("text/event-stream")).unwrap(),
            json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}})
        );
    }

    #[test]
    fn mislabeled_sse_still_parses() {
        let body = b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        assert!(parse_rpc_body(body, Some("application/json")).is_ok());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_rpc_body(b"nope", Some("application/json")).is_err());
        assert!(parse_rpc_body(b": comment only\n\n", Some("text/event-stream")).is_err());
        assert!(parse_rpc_body(&[0xff, 0xfe], None).is_err());
    }

    #[test]
    fn result_or_error_split() {
        let ok = json!({"result": {"a": 1}});
        assert_eq!(rpc_result(&ok).unwrap(), &json!({"a": 1}));
        let missing = json!({"result": [1]});
        assert!(rpc_result(&missing).is_err());
        let error = json!({"error": {"code": -32601, "message": "nope"}});
        assert_eq!(
            rpc_result(&error).unwrap_err(),
            RpcError {
                code: -32601,
                message: "nope".to_string()
            }
        );
    }
}
