//! Stage 3: render a buildable provider crate plus a starter broker config.
//!
//! One MCP tool becomes one capability; one command word per provider, one
//! subcommand per tool. Every generated input schema is closed by
//! construction (`#[serde(deny_unknown_fields)]` on every struct, no `$ref`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::fetch::{Endpoint, ServerInfo};
use crate::names;
use crate::tighten::{EnumShape, Field, FieldType, ObjectShape, TightenedTool, VariantKind};

const WIT_WORLDS: &str = include_str!("../wit-0461/worlds-provider.wit");
const WIT_STDIO: &str = include_str!("../wit-0461/stdio.wit");
const WIT_HTTP: &str = include_str!("../wit-0461/http.wit");
const WIT_SETTINGS: &str = include_str!("../wit-0461/settings.wit");

/// Everything the renderer needs for one provider.
pub struct ProviderSpec<'a> {
    pub id: String,
    pub tools: &'a [TightenedTool],
    pub endpoint: &'a Endpoint,
    pub server: &'a ServerInfo,
    pub generator_version: String,
    /// Per-request fetch timeout, mirrored into the broker fragment.
    pub timeout_secs: u64,
}

/// One file to write, relative to the provider directory.
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
    pub executable: bool,
}

/// Renders every file of one provider crate.
pub fn render_provider(spec: &ProviderSpec<'_>) -> Vec<GeneratedFile> {
    let naming = Naming::assign(spec.tools);
    let lib = render_lib(spec, &naming);
    let crate_name = names::crate_name(&spec.id);
    vec![
        GeneratedFile {
            path: "Cargo.toml".to_string(),
            content: render_cargo(spec, &crate_name),
            executable: false,
        },
        GeneratedFile {
            path: "src/lib.rs".to_string(),
            content: lib,
            executable: false,
        },
        GeneratedFile {
            path: "build.sh".to_string(),
            content: render_build_sh(spec, &crate_name),
            executable: true,
        },
        GeneratedFile {
            path: "tests/native_e2e.rs".to_string(),
            content: render_native_e2e(spec, &naming, &crate_name),
            executable: false,
        },
        GeneratedFile {
            path: "tests/conformance.rs".to_string(),
            content: render_conformance(spec, &crate_name),
            executable: false,
        },
        GeneratedFile {
            path: "wit/provider.wit".to_string(),
            content: render_world(spec),
            executable: false,
        },
        GeneratedFile {
            path: "wit/deps/provider.wit".to_string(),
            content: WIT_WORLDS.to_string(),
            executable: false,
        },
        GeneratedFile {
            path: "wit/deps/stdio.wit".to_string(),
            content: WIT_STDIO.to_string(),
            executable: false,
        },
        GeneratedFile {
            path: "wit/deps/http.wit".to_string(),
            content: WIT_HTTP.to_string(),
            executable: false,
        },
        GeneratedFile {
            path: "wit/deps/settings.wit".to_string(),
            content: WIT_SETTINGS.to_string(),
            executable: false,
        },
        GeneratedFile {
            path: format!("broker.d/{}.yaml", spec.id),
            content: render_broker(spec),
            executable: false,
        },
        GeneratedFile {
            path: format!("{}.cedar", spec.id),
            content: render_cedar(spec),
            executable: false,
        },
        GeneratedFile {
            path: "README.md".to_string(),
            content: render_readme(spec),
            executable: false,
        },
        GeneratedFile {
            path: ".gitignore".to_string(),
            content: "/target\n*.wasm\nbuild-target.log\n".to_string(),
            executable: false,
        },
    ]
}

/// Writes one file atomically: the content lands in a sibling temp file
/// first, then renames over the target, so a failure never leaves a
/// half-written file behind.
pub fn write_file_atomic(path: &Path, content: String) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&tmp, content).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {}: {e}", path.display())
    })
}

/// Writes one provider directory. Refuses to overwrite unless `force` is set.
/// Files render into a sibling staging directory first and swap into place,
/// so a failure leaves no half-written crate; a `--force` regeneration
/// replaces the directory wholesale, so stale artifacts (a previous
/// `<id>.wasm` included) cannot survive to confuse `conformance()`.
pub fn write_provider(dir: &Path, files: &[GeneratedFile], force: bool) -> Result<(), String> {
    if dir.exists() && !force {
        return Err(format!(
            "refusing to overwrite {}; pass --force to regenerate",
            dir.display()
        ));
    }
    let staging = PathBuf::from(format!("{}.tmp-{}", dir.display(), std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let staged = (|| -> Result<(), String> {
        for file in files {
            let path = staging.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(&path, &file.content)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            #[cfg(unix)]
            if file.executable {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&path)
                    .map_err(|e| format!("cannot stat {}: {e}", path.display()))?
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&path, permissions)
                    .map_err(|e| format!("cannot chmod {}: {e}", path.display()))?;
            }
        }
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if force {
        let _ = std::fs::remove_dir_all(dir);
    }
    if let Err(error) = std::fs::rename(&staging, dir) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("cannot write {}: {error}", dir.display()));
    }
    Ok(())
}

/// Per-tool Rust names, unique across the provider.
struct ToolNames {
    cap_struct: String,
    input_struct: String,
    args_struct: String,
    variant: String,
    test_name: String,
}

struct Naming {
    tools: Vec<ToolNames>,
}

impl Naming {
    fn assign(tools: &[TightenedTool]) -> Self {
        let mut used = HashSet::new();
        // Reserve the fixed names first so no tool type collides with them.
        for fixed in [
            "McpProvider",
            "McpSettings",
            "McpError",
            "Command",
            "Action",
            "Fetch",
        ] {
            used.insert(fixed.to_string());
        }
        let mut used_tests = HashSet::new();
        let tools = tools
            .iter()
            .map(|tool| {
                let cap_struct = names::rust_type_name(&[&tool.cap_name, "cap"], &mut used);
                let input_struct = names::rust_type_name(&[&tool.cap_name, "input"], &mut used);
                let args_struct = names::rust_type_name(&[&tool.cap_name, "args"], &mut used);
                let variant = names::rust_type_name(&[&tool.cap_name], &mut used);
                let base: String = tool
                    .cap_name
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                let mut test_name = base.clone();
                let mut counter = 2_u32;
                while used_tests.contains(&test_name) {
                    test_name = format!("{base}_{counter}");
                    counter += 1;
                }
                used_tests.insert(test_name.clone());
                ToolNames {
                    cap_struct,
                    input_struct,
                    args_struct,
                    variant,
                    test_name,
                }
            })
            .collect();
        Self { tools }
    }
}

/// Renders a Rust string literal, quoted and escaped.
fn lit(value: &str) -> String {
    format!("{value:?}")
}

/// Escapes one Markdown table cell: pipes would split the column, and
/// newlines would break the row layout.
fn md_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

/// Renders description lines as doc comments.
fn docs(description: &str, indent: &str, out: &mut String) {
    for line in description.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            out.push_str(&format!("{indent}///\n"));
        } else {
            out.push_str(&format!("{indent}/// {line}\n"));
        }
    }
}

/// Renders a lowered type as Rust tokens.
fn rust_type(field_type: &FieldType) -> String {
    match field_type {
        FieldType::String | FieldType::JsonString => "String".to_string(),
        FieldType::Integer => "i64".to_string(),
        FieldType::Number => "f64".to_string(),
        FieldType::Boolean => "bool".to_string(),
        FieldType::Any => "Value".to_string(),
        FieldType::Array { item } => format!("Vec<{}>", rust_type(item)),
        FieldType::Nullable { inner } => format!("Option<{}>", rust_type(inner)),
        FieldType::Object(shape) => shape.type_name.clone(),
        FieldType::Enum(shape) => shape.type_name.clone(),
    }
}

/// Collects every free-map path (MCP names) under a field; arrays are
/// descended automatically by the generated helper, so they add no segment.
fn free_map_paths(field: &Field) -> Vec<Vec<String>> {
    let mut paths = Vec::new();
    collect_paths(
        &field.field_type,
        &mut vec![field.mcp_name.clone()],
        &mut paths,
    );
    paths
}

fn collect_paths(field_type: &FieldType, current: &mut Vec<String>, paths: &mut Vec<Vec<String>>) {
    match field_type {
        FieldType::JsonString => paths.push(current.clone()),
        FieldType::Object(shape) => {
            for field in &shape.fields {
                current.push(field.mcp_name.clone());
                collect_paths(&field.field_type, current, paths);
                current.pop();
            }
        }
        FieldType::Enum(shape) => {
            for variant in &shape.variants {
                let VariantKind::Struct(shape) = &variant.kind else {
                    continue;
                };
                for field in &shape.fields {
                    current.push(field.mcp_name.clone());
                    collect_paths(&field.field_type, current, paths);
                    current.pop();
                }
            }
        }
        FieldType::Array { item } | FieldType::Nullable { inner: item } => {
            collect_paths(item, current, paths);
        }
        FieldType::String
        | FieldType::Integer
        | FieldType::Number
        | FieldType::Boolean
        | FieldType::Any => {}
    }
}

fn render_lib(spec: &ProviderSpec<'_>, naming: &Naming) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "//! {} — Dekopon provider generated from {}.\n\
         //!\n\
         //! Generated by mcp2dekopon {} from {} {} ({}) for\n\
         //! dekopon-provider-sdk ={}. DO NOT EDIT; regenerate with\n\
         //! `mcp2dekopon gen --url {} --id {} --out <dir>`.\n\
         \n",
        spec.id,
        spec.endpoint.origin,
        spec.generator_version,
        spec.server.name,
        spec.server.version,
        spec.endpoint.vendor_base,
        crate::SDK_VERSION,
        spec.endpoint.vendor_base,
        spec.id,
    ));
    out.push_str(
        "use dekopon_provider_sdk::clap::{Args, Parser, Subcommand};\n\
         use dekopon_provider_sdk::provider::{\n\
         \x20   endpoint::Base, method, Capability, Code, Failure, Header, Http, HttpBuildError, HttpError,\n\
         \x20   Proposal, Provider, Request, Response, Settings, Stdout, Usage,\n\
         };\n\
         use dekopon_provider_sdk::{EffectKind, RiskLevel};\n\
         use schemars::JsonSchema;\n\
         use serde::{Deserialize, Serialize};\n\
         use serde_json::{json, Value};\n\
         use std::io::Write;\n\
         \n\
         /// MCP protocol version offered in `initialize`.\n",
    );
    out.push_str(&format!(
        "const MCP_PROTOCOL_VERSION: &str = {};\n\
         /// The MCP endpoint this provider was generated from; the operator may\n\
         /// override it per deployment with `providerSettings.{}.baseUrl`.\n\
         const VENDOR_BASE: Base = Base::from_static({});\n\
         \n\
         /// Operator settings: `providerSettings.{}` in the broker configuration.\n\
         /// Unknown keys are ignored so older providers keep working as the\n\
         /// broker configuration grows.\n\
         #[derive(Debug, Deserialize)]\n\
         #[serde(rename_all = \"camelCase\")]\n\
         pub struct McpSettings {{\n\
         \x20   base_url: Option<Base>,\n\
         }}\n\
         \n",
        lit(&spec.server.protocol_version),
        spec.id,
        lit(&spec.endpoint.vendor_base),
        spec.id,
    ));
    out.push_str(&format!(
        "/// Client identity sent in `initialize`.\n\
         const CLIENT_NAME: &str = {};\n\
         const CLIENT_VERSION: &str = {};\n\
         \n",
        lit(&spec.id),
        lit(&spec.generator_version),
    ));
    out.push_str(
        "/// Every way an MCP call can fail.\n\
         #[derive(Debug)]\n\
         pub enum McpError {\n\
         \x20   InvalidRequest(HttpBuildError),\n\
         \x20   Http(HttpError),\n\
         \x20   HttpStatus(u16),\n\
         \x20   Protocol(String),\n\
         \x20   Rpc {\n\
         \x20       code: i64,\n\
         \x20       message: String,\n\
         \x20   },\n\
         \x20   ToolFailed,\n\
         \x20   Encoding,\n\
         \x20   Output,\n\
         \x20   #[allow(dead_code)]\n\
         \x20   InvalidArguments(&'static str),\n\
         }\n\
         \n\
         impl std::fmt::Display for McpError {\n\
         \x20   fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n\
         \x20       match self {\n\
         \x20           Self::InvalidRequest(error) => write!(formatter, \"{error}\"),\n\
         \x20           Self::Http(error) => write!(formatter, \"{error}\"),\n\
         \x20           Self::HttpStatus(status) => write!(formatter, \"mcp server returned status {status}\"),\n\
         \x20           Self::Protocol(message) => formatter.write_str(message),\n\
         \x20           Self::Rpc { code, message } => write!(formatter, \"mcp error {code}: {message}\"),\n\
         \x20           Self::ToolFailed => formatter.write_str(\"the tool reported an error\"),\n\
         \x20           Self::Encoding => formatter.write_str(\"the provider could not encode its input\"),\n\
         \x20           Self::Output => formatter.write_str(\"provider stdout closed\"),\n\
         \x20           Self::InvalidArguments(field) => write!(formatter, \"argument {field:?} must be JSON\"),\n\
         \x20       }\n\
         \x20   }\n\
         }\n\
         \n\
         impl Failure for McpError {\n\
         \x20   fn code(&self) -> Code {\n\
         \x20       Code::new(match self {\n\
         \x20           Self::InvalidRequest(_) => \"invalid-request\",\n\
         \x20           Self::Http(_) => \"http-failed\",\n\
         \x20           Self::HttpStatus(_) => \"bad-status\",\n\
         \x20           Self::Protocol(_) => \"protocol-error\",\n\
         \x20           Self::Rpc { .. } => \"rpc-error\",\n\
         \x20           Self::ToolFailed => \"tool-failed\",\n\
         \x20           Self::Encoding => \"encoding-failed\",\n\
         \x20           Self::Output => \"output-error\",\n\
         \x20           Self::InvalidArguments(_) => \"invalid-arguments\",\n\
         \x20       })\n\
         \x20   }\n\
         }\n\
         \n",
    );
    render_helpers(&mut out, spec);
    for (tool, names) in spec.tools.iter().zip(naming.tools.iter()) {
        render_tool_types(&mut out, spec, tool, names);
    }
    render_command(&mut out, spec, naming);
    for (tool, names) in spec.tools.iter().zip(naming.tools.iter()) {
        render_capability(&mut out, tool, names);
    }
    render_provider_impl(&mut out, spec, naming);
    out
}

fn render_helpers(out: &mut String, spec: &ProviderSpec<'_>) {
    let needs_json_paths = spec
        .tools
        .iter()
        .any(|tool| !tool.top_level_map && tool.fields.iter().any(has_free_map));
    out.push_str(
        "fn ht(name: &'static str, value: &str) -> Result<Header, McpError> {\n\
         \x20   Header::text(name, value).map_err(McpError::InvalidRequest)\n\
         }\n\
         \n\
         fn hb(name: &'static str, value: Vec<u8>) -> Result<Header, McpError> {\n\
         \x20   Header::new(name, value).map_err(McpError::InvalidRequest)\n\
         }\n\
         \n\
         fn header_bytes<'a>(response: &'a Response, name: &str) -> Option<&'a [u8]> {\n\
         \x20   response\n\
         \x20       .headers\n\
         \x20       .iter()\n\
         \x20       .find(|header| header.name.eq_ignore_ascii_case(name))\n\
         \x20       .map(|header| header.value.as_slice())\n\
         }\n\
         \n\
         fn content_type_of(response: &Response) -> Option<&str> {\n\
         \x20   header_bytes(response, \"content-type\").and_then(|value| std::str::from_utf8(value).ok())\n\
         }\n\
         \n\
         /// The first 120 characters of a body, for error messages.\n\
         fn snippet(text: &str) -> String {\n\
         \x20   if text.chars().count() <= 120 {\n\
         \x20       return text.to_string();\n\
         \x20   }\n\
         \x20   let mut short: String = text.chars().take(120).collect();\n\
         \x20   short.push_str(\"...\");\n\
         \x20   short\n\
         }\n\
         \n\
         /// Parses one response body: plain JSON, or the buffered SSE event carrying\n\
         /// the JSON-RPC message.\n\
         fn parse_rpc_body(body: &[u8], content_type: Option<&str>) -> Result<Value, McpError> {\n\
         \x20   let text = std::str::from_utf8(body)\n\
         \x20       .map_err(|_| McpError::Protocol(\"response body was not UTF-8\".to_string()))?;\n\
         \x20   let looks_sse = content_type\n\
         \x20       .is_some_and(|kind| kind.to_ascii_lowercase().contains(\"text/event-stream\"))\n\
         \x20       || text.trim_start().starts_with(\"data:\")\n\
         \x20       || text.trim_start().starts_with(\"event:\");\n\
         \x20   let not_json = || McpError::Protocol(format!(\"response body was not JSON: {}\", snippet(text)));\n\
         \x20   if looks_sse {\n\
         \x20       sse_json(text).or_else(|_| serde_json::from_str(text).map_err(|_| not_json()))\n\
         \x20   } else {\n\
         \x20       serde_json::from_str(text).map_err(|_| not_json())\n\
         \x20   }\n\
         }\n\
         \n\
         /// Extracts the last data payload that looks like a JSON-RPC response.\n\
         fn sse_json(text: &str) -> Result<Value, McpError> {\n\
         \x20   let mut payloads: Vec<String> = Vec::new();\n\
         \x20   let mut data: Vec<&str> = Vec::new();\n\
         \x20   for raw_line in text.split('\\n') {\n\
         \x20       let line = raw_line.strip_suffix('\\r').unwrap_or(raw_line);\n\
         \x20       if line.is_empty() {\n\
         \x20           if !data.is_empty() {\n\
         \x20               payloads.push(data.join(\"\\n\"));\n\
         \x20               data.clear();\n\
         \x20           }\n\
         \x20           continue;\n\
         \x20       }\n\
         \x20       if line.starts_with(':') {\n\
         \x20           continue;\n\
         \x20       }\n\
         \x20       if let Some(rest) = line.strip_prefix(\"data:\") {\n\
         \x20           data.push(rest.strip_prefix(' ').unwrap_or(rest));\n\
         \x20       }\n\
         \x20   }\n\
         \x20   if !data.is_empty() {\n\
         \x20       payloads.push(data.join(\"\\n\"));\n\
         \x20   }\n\
         \x20   for payload in payloads.iter().rev() {\n\
         \x20       if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(payload) {\n\
         \x20           if map.contains_key(\"result\") || map.contains_key(\"error\") {\n\
         \x20               return Ok(Value::Object(map));\n\
         \x20           }\n\
         \x20       }\n\
         \x20   }\n\
         \x20   Err(McpError::Protocol(format!(\n\
         \x20       \"no JSON-RPC message in SSE response: {}\",\n\
         \x20       snippet(text)\n\
         \x20   )))\n\
         }\n\
         \n\
         /// Splits a JSON-RPC message into its result or its error.\n\
         fn rpc_result(payload: &Value) -> Result<&Value, McpError> {\n\
         \x20   if let Some(error) = payload.get(\"error\") {\n\
         \x20       let code = error.get(\"code\").and_then(Value::as_i64).unwrap_or(0);\n\
         \x20       let message = error\n\
         \x20           .get(\"message\")\n\
         \x20           .and_then(Value::as_str)\n\
         \x20           .unwrap_or(\"mcp error\")\n\
         \x20           .to_owned();\n\
         \x20       return Err(McpError::Rpc { code, message });\n\
         \x20   }\n\
         \x20   payload\n\
         \x20       .get(\"result\")\n\
         \x20       .filter(|result| result.is_object())\n\
         \x20       .ok_or_else(|| McpError::Protocol(\"JSON-RPC response had no result\".to_string()))\n\
         }\n\
         \n\
         /// Reads the negotiated protocol version from an `initialize` answer, if any.\n\
         fn init_protocol(response: &Response) -> Option<String> {\n\
         \x20   let payload = parse_rpc_body(&response.body, content_type_of(response)).ok()?;\n\
         \x20   let result = rpc_result(&payload).ok()?;\n\
         \x20   result\n\
         \x20       .get(\"protocolVersion\")\n\
         \x20       .and_then(Value::as_str)\n\
         \x20       .map(str::to_owned)\n\
         }\n\
         \n\
         /// Runs one MCP round trip against `base`: `initialize`, the\n\
         /// `notifications/initialized` notice (best effort), then `tools/call`\n\
         /// for `tool`, printing the MCP `content` as one JSON line.\n\
         fn call_mcp(\n\
         \x20   http: &Http,\n\
         \x20   base: &Base,\n\
         \x20   tool: &'static str,\n\
         \x20   arguments: Value,\n\
         \x20   out: &mut Stdout,\n\
         ) -> Result<(), McpError> {\n\
         \x20   let url = base.as_str().to_owned();\n\
         \x20   let initialize = json!({\n\
         \x20       \"jsonrpc\": \"2.0\",\n\
         \x20       \"id\": 1,\n\
         \x20       \"method\": \"initialize\",\n\
         \x20       \"params\": {\n\
         \x20           \"protocolVersion\": MCP_PROTOCOL_VERSION,\n\
         \x20           \"capabilities\": {},\n\
         \x20           \"clientInfo\": {\"name\": CLIENT_NAME, \"version\": CLIENT_VERSION},\n\
         \x20       },\n\
         \x20   });\n\
         \x20   let init_request = Request::new(method::POST, &url)\n\
         \x20       .map_err(McpError::InvalidRequest)?\n\
         \x20       .with_header(ht(\"content-type\", \"application/json\")?)\n\
         \x20       .with_header(ht(\"accept\", \"application/json, text/event-stream\")?)\n\
         \x20       .with_body(initialize.to_string().into_bytes());\n\
         \x20   let init_response = http.send(init_request).map_err(McpError::Http)?;\n\
         \x20   if !(200..300).contains(&init_response.status) {\n\
         \x20       return Err(McpError::HttpStatus(init_response.status));\n\
         \x20   }\n\
         \x20   let session = header_bytes(&init_response, \"mcp-session-id\").map(<[u8]>::to_vec);\n\
         \x20   let protocol = init_protocol(&init_response).unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_owned());\n\
         \x20   // Best effort: some servers require the notice before `tools/call`,\n\
         \x20   // others reject it; either way the call proceeds.\n\
         \x20   let notice = json!({\"jsonrpc\": \"2.0\", \"method\": \"notifications/initialized\"});\n\
         \x20   let notice_request = (|| -> Result<Request, McpError> {\n\
         \x20       let mut request = Request::new(method::POST, &url)\n\
         \x20           .map_err(McpError::InvalidRequest)?\n\
         \x20           .with_header(ht(\"content-type\", \"application/json\")?)\n\
         \x20           .with_header(ht(\"accept\", \"application/json, text/event-stream\")?)\n\
         \x20           .with_header(ht(\"mcp-protocol-version\", &protocol)?);\n\
         \x20       if let Some(session) = &session {\n\
         \x20           request = request.with_header(hb(\"mcp-session-id\", session.clone())?);\n\
         \x20       }\n\
         \x20       Ok(request.with_body(notice.to_string().into_bytes()))\n\
         \x20   })();\n\
         \x20   if let Ok(notice_request) = notice_request {\n\
         \x20       let _ignored = http.send(notice_request);\n\
         \x20   }\n\
         \x20   let call = json!({\n\
         \x20       \"jsonrpc\": \"2.0\",\n\
         \x20       \"id\": 2,\n\
         \x20       \"method\": \"tools/call\",\n\
         \x20       \"params\": {\"name\": tool, \"arguments\": arguments},\n\
         \x20   });\n\
         \x20   let mut call_request = Request::new(method::POST, &url)\n\
         \x20       .map_err(McpError::InvalidRequest)?\n\
         \x20       .with_header(ht(\"content-type\", \"application/json\")?)\n\
         \x20       .with_header(ht(\"accept\", \"application/json, text/event-stream\")?)\n\
         \x20       .with_header(ht(\"mcp-protocol-version\", &protocol)?);\n\
         \x20   if let Some(session) = session {\n\
         \x20       call_request = call_request.with_header(hb(\"mcp-session-id\", session)?);\n\
         \x20   }\n\
         \x20   let call_request = call_request.with_body(call.to_string().into_bytes());\n\
         \x20   let call_response = http.send(call_request).map_err(McpError::Http)?;\n\
         \x20   if !(200..300).contains(&call_response.status) {\n\
         \x20       return Err(McpError::HttpStatus(call_response.status));\n\
         \x20   }\n\
         \x20   let payload = parse_rpc_body(&call_response.body, content_type_of(&call_response))?;\n\
         \x20   if payload.get(\"id\") != Some(&json!(2)) {\n\
         \x20       let got = payload\n\
         \x20           .get(\"id\")\n\
         \x20           .map_or_else(|| \"missing\".to_string(), Value::to_string);\n\
         \x20       return Err(McpError::Protocol(format!(\n\
         \x20           \"response id {got} did not match request 2\"\n\
         \x20       )));\n\
         \x20   }\n\
         \x20   let result = rpc_result(&payload)?;\n\
         \x20   let content = result.get(\"content\").unwrap_or(result);\n\
         \x20   let line = serde_json::to_string(content).map_err(|_| McpError::Encoding)?;\n\
         \x20   writeln!(out, \"{line}\").map_err(|_| McpError::Output)?;\n\
         \x20   if result\n\
         \x20       .get(\"isError\")\n\
         \x20       .and_then(Value::as_bool)\n\
         \x20       .unwrap_or(false)\n\
         \x20   {\n\
         \x20       return Err(McpError::ToolFailed);\n\
         \x20   }\n\
         \x20   Ok(())\n\
         }\n\
         \n",
    );
    if needs_json_paths {
        out.push_str(
            "/// Parses free-map fields from JSON text in place; arrays are descended so\n\
             /// maps nested under lists are parsed too.\n\
             fn parse_json_field_path(value: &mut Value, path: &[&'static str]) -> Result<(), McpError> {\n\
             \x20   let Some((head, tail)) = path.split_first() else {\n\
             \x20       return Ok(());\n\
             \x20   };\n\
             \x20   match value {\n\
             \x20       Value::Object(map) => match map.get_mut(*head) {\n\
             \x20           None => Ok(()),\n\
             \x20           Some(next) if tail.is_empty() => {\n\
             \x20               if let Value::String(text) = next {\n\
             \x20                   let parsed: Value =\n\
             \x20                       serde_json::from_str(text).map_err(|_| McpError::InvalidArguments(head))?;\n\
             \x20                   *next = parsed;\n\
             \x20               }\n\
             \x20               Ok(())\n\
             \x20           }\n\
             \x20           Some(next) => parse_json_field_path(next, tail),\n\
             \x20       },\n\
             \x20       Value::Array(items) => {\n\
             \x20           for item in items {\n\
             \x20               parse_json_field_path(item, path)?;\n\
             \x20           }\n\
             \x20           Ok(())\n\
             \x20       }\n\
             \x20       _ => Ok(()),\n\
             \x20   }\n\
             }\n\
             \n",
        );
    }
}

fn has_free_map(field: &Field) -> bool {
    match &field.field_type {
        FieldType::JsonString => true,
        FieldType::Object(shape) => shape.fields.iter().any(has_free_map),
        FieldType::Enum(shape) => shape
            .variants
            .iter()
            .any(|v| matches!(&v.kind, VariantKind::Struct(shape) if shape.fields.iter().any(has_free_map))),
        FieldType::Array { item } | FieldType::Nullable { inner: item } => has_free_map_inner(item),
        FieldType::String
        | FieldType::Integer
        | FieldType::Number
        | FieldType::Boolean
        | FieldType::Any => false,
    }
}

fn has_free_map_inner(field_type: &FieldType) -> bool {
    match field_type {
        FieldType::JsonString => true,
        FieldType::Object(shape) => shape.fields.iter().any(has_free_map),
        FieldType::Enum(shape) => shape
            .variants
            .iter()
            .any(|v| matches!(&v.kind, VariantKind::Struct(shape) if shape.fields.iter().any(has_free_map))),
        FieldType::Array { item } | FieldType::Nullable { inner: item } => has_free_map_inner(item),
        FieldType::String
        | FieldType::Integer
        | FieldType::Number
        | FieldType::Boolean
        | FieldType::Any => false,
    }
}

fn render_tool_types(
    out: &mut String,
    spec: &ProviderSpec<'_>,
    tool: &TightenedTool,
    names: &ToolNames,
) {
    out.push_str(&format!(
        "// ---- {} ----\n\npub struct {};\n\n",
        tool.mcp_name, names.cap_struct
    ));
    docs(&tool.description, "", out);
    out.push_str(
        "#[derive(Debug, Deserialize, Serialize, JsonSchema)]\n#[serde(deny_unknown_fields)]\n",
    );
    if tool.fields.is_empty() {
        out.push_str(&format!("pub struct {} {{}}\n\n", names.input_struct));
    } else {
        out.push_str(&format!("pub struct {} {{\n", names.input_struct));
        for field in &tool.fields {
            render_input_field(out, field);
        }
        out.push_str("}\n\n");
    }
    // Nested shapes, deepest first so each use follows its definition.
    let mut rendered = HashSet::new();
    for field in &tool.fields {
        render_nested(out, &field.field_type, &mut rendered);
    }
    out.push_str(&format!(
        "/// CLI flags for `{} {}`.\n#[derive(Debug, Args)]\npub struct {} {{\n",
        spec.id, tool.cap_name, names.args_struct
    ));
    for field in &tool.fields {
        render_arg_field(out, field);
    }
    out.push_str("}\n\n");
}

fn render_input_field(out: &mut String, field: &Field) {
    if !field.description.is_empty() {
        docs(&field.description, "    ", out);
    }
    let kind = rust_type(&field.field_type);
    if field.required {
        out.push_str(&format!(
            "    #[serde(rename = {})]\n    {}: {},\n",
            lit(&field.mcp_name),
            field.rust_name,
            kind
        ));
    } else {
        out.push_str(&format!(
            "    #[serde(rename = {}, skip_serializing_if = \"Option::is_none\")]\n    {}: Option<{}>,\n",
            lit(&field.mcp_name),
            field.rust_name,
            kind
        ));
    }
}

fn render_nested(out: &mut String, field_type: &FieldType, rendered: &mut HashSet<String>) {
    match field_type {
        FieldType::Object(shape) => render_object_shape(out, shape, rendered),
        FieldType::Enum(shape) => render_enum_shape(out, shape, rendered),
        FieldType::Array { item } | FieldType::Nullable { inner: item } => {
            render_nested(out, item, rendered);
        }
        FieldType::String
        | FieldType::Integer
        | FieldType::Number
        | FieldType::Boolean
        | FieldType::JsonString
        | FieldType::Any => {}
    }
}

fn render_object_shape(out: &mut String, shape: &ObjectShape, rendered: &mut HashSet<String>) {
    if !rendered.insert(shape.type_name.clone()) {
        return;
    }
    for field in &shape.fields {
        render_nested(out, &field.field_type, rendered);
    }
    out.push_str(
        "#[derive(Debug, Deserialize, Serialize, JsonSchema)]\n#[serde(deny_unknown_fields)]\n",
    );
    if shape.fields.is_empty() {
        out.push_str(&format!("pub struct {} {{}}\n\n", shape.type_name));
    } else {
        out.push_str(&format!("pub struct {} {{\n", shape.type_name));
        for field in &shape.fields {
            render_input_field(out, field);
        }
        out.push_str("}\n\n");
    }
}

fn render_enum_shape(out: &mut String, shape: &EnumShape, rendered: &mut HashSet<String>) {
    if !rendered.insert(shape.type_name.clone()) {
        return;
    }
    for variant in &shape.variants {
        if let VariantKind::Struct(shape) = &variant.kind {
            render_object_shape(out, shape, rendered);
        }
    }
    out.push_str("#[derive(Debug, Deserialize, Serialize, JsonSchema)]\n#[serde(untagged)]\n");
    out.push_str(&format!("pub enum {} {{\n", shape.type_name));
    for variant in &shape.variants {
        let held = match &variant.kind {
            VariantKind::Struct(shape) => shape.type_name.clone(),
            VariantKind::Scalar(scalar) => rust_type(scalar),
        };
        out.push_str(&format!("    {}({held}),\n", variant.name));
    }
    out.push_str("}\n\n");
}

fn render_arg_field(out: &mut String, field: &Field) {
    if !field.description.is_empty() {
        docs(&field.description, "    ", out);
    } else if field.field_type.is_json_flag() {
        out.push_str("    /// The value as JSON.\n");
    }
    let mut attrs = vec!["long".to_string()];
    if field.required {
        attrs.push("required = true".to_string());
    }
    if matches!(field.field_type, FieldType::Boolean) {
        attrs.push("num_args = 0..=1".to_string());
        attrs.push("default_missing_value = \"true\"".to_string());
    }
    let attrs = attrs.join(", ");
    if field.field_type.is_json_flag() {
        if field.required {
            out.push_str(&format!(
                "    #[arg({attrs})]\n    {}: String,\n",
                field.rust_name
            ));
        } else {
            out.push_str(&format!(
                "    #[arg({attrs})]\n    {}: Option<String>,\n",
                field.rust_name
            ));
        }
    } else {
        let kind = rust_type(&field.field_type);
        if field.required {
            out.push_str(&format!(
                "    #[arg({attrs})]\n    {}: {},\n",
                field.rust_name, kind
            ));
        } else {
            out.push_str(&format!(
                "    #[arg({attrs})]\n    {}: Option<{}>,\n",
                field.rust_name, kind
            ));
        }
    }
}

fn render_command(out: &mut String, spec: &ProviderSpec<'_>, naming: &Naming) {
    out.push_str(&format!(
        "/// {} — MCP tools from {}.\n\
         #[derive(Debug, Parser)]\n\
         #[command(name = {}, version = \"0.1.0\", subcommand_required = true)]\n\
         pub struct Command {{\n\
         \x20   #[command(subcommand)]\n\
         \x20   action: Action,\n\
         }}\n\
         \n\
         #[derive(Debug, Subcommand)]\n\
         pub enum Action {{\n",
        spec.id,
        spec.endpoint.origin,
        lit(&spec.id),
    ));
    for (tool, names) in spec.tools.iter().zip(naming.tools.iter()) {
        docs(&tool.description, "    ", out);
        out.push_str(&format!(
            "    #[command(name = {})]\n    {}({}),\n",
            lit(&tool.cap_name),
            names.variant,
            names.args_struct
        ));
    }
    out.push_str("}\n\n");
    let needs_optional = spec.tools.iter().any(|tool| {
        tool.fields
            .iter()
            .any(|f| f.field_type.is_json_flag() && !f.required)
    });
    let needs_required = spec.tools.iter().any(|tool| {
        tool.fields
            .iter()
            .any(|f| f.field_type.is_json_flag() && f.required)
    });
    if needs_optional {
        out.push_str(
            "fn parse_arg<T: serde::de::DeserializeOwned>(\n\
             \x20   tool: &'static str,\n\
             \x20   flag: &'static str,\n\
             \x20   raw: Option<String>,\n\
             ) -> Result<Option<T>, Usage> {\n\
             \x20   raw.map(|text| {\n\
             \x20       serde_json::from_str(&text)\n\
             \x20           .map_err(|_| Usage::new(format!(\"{tool}: --{flag} must be JSON\")))\n\
             \x20   })\n\
             \x20   .transpose()\n\
             }\n\
             \n",
        );
    }
    if needs_required {
        out.push_str(
            "fn parse_required_arg<T: serde::de::DeserializeOwned>(\n\
             \x20   tool: &'static str,\n\
             \x20   flag: &'static str,\n\
             \x20   raw: String,\n\
             ) -> Result<T, Usage> {\n\
             \x20   serde_json::from_str(&raw).map_err(|_| Usage::new(format!(\"{tool}: --{flag} must be JSON\")))\n\
             }\n\
             \n",
        );
    }
}

/// The kebab-case flag clap derives from a Rust field name.
fn flag_name(rust_name: &str) -> String {
    rust_name.trim_start_matches("r#").replace('_', "-")
}

/// Render a `const DESCRIPTION` line the way rustfmt would: rustfmt only breaks
/// after `=` when the single line overflows 100 columns but the continuation
/// still fits; a literal too long for any line is left alone.
fn description_const(literal: &str) -> String {
    let single = format!("    const DESCRIPTION: &'static str = {literal};");
    if single.len() > 100 && literal.len() + 9 <= 100 {
        format!("    const DESCRIPTION: &'static str =\n        {literal};")
    } else {
        single
    }
}

fn render_capability(out: &mut String, tool: &TightenedTool, names: &ToolNames) {
    out.push_str(&format!(
        "impl Capability for {} {{\n\
         \x20   type Provider = McpProvider;\n\
         \x20   const NAME: &'static str = {};\n\
{}\n\
         \x20   const EFFECT: EffectKind = {};\n\
         \x20   const RISK: RiskLevel = {};\n\
         \x20   type Input = {};\n\
         \x20   type Needs = (Settings<McpSettings>, Http);\n\
         \x20   type Error = McpError;\n\
         \n\
         \x20   fn run(\n\
         \x20       input: {},\n\
         \x20       (settings, http): (Settings<McpSettings>, Http),\n\
         \x20       out: &mut Stdout,\n\
         \x20   ) -> Result<(), McpError> {{\n\
         \x20       let base = settings\n\
         \x20           .into_inner()\n\
         \x20           .base_url\n\
         \x20           .unwrap_or_else(|| VENDOR_BASE.clone());\n",
        names.cap_struct,
        lit(&tool.cap_name),
        description_const(&lit(&tool.description)),
        tool.effect.token(),
        tool.risk.token(),
        names.input_struct,
        names.input_struct,
    ));
    if tool.top_level_map {
        out.push_str(
            "        let arguments: Value = serde_json::from_str(&input.payload)\n\
             \x20           .map_err(|_| McpError::InvalidArguments(\"payload\"))?;\n\
             \x20       if !arguments.is_object() {\n\
             \x20           return Err(McpError::InvalidArguments(\"payload\"));\n\
             \x20       }\n",
        );
    } else {
        let mut paths = Vec::new();
        for field in &tool.fields {
            paths.extend(free_map_paths(field));
        }
        if paths.is_empty() {
            out.push_str(
                "        let arguments = serde_json::to_value(&input).map_err(|_| McpError::Encoding)?;\n",
            );
        } else {
            out.push_str(
                "        let mut arguments = serde_json::to_value(&input).map_err(|_| McpError::Encoding)?;\n",
            );
            for path in paths {
                let segments: Vec<String> = path.iter().map(|s| lit(s)).collect();
                out.push_str(&format!(
                    "        parse_json_field_path(&mut arguments, &[{}])?;\n",
                    segments.join(", ")
                ));
            }
        }
    }
    out.push_str(&format!(
        "        call_mcp(&http, &base, {}, arguments, out)\n",
        lit(&tool.mcp_name),
    ));
    out.push_str("    }\n}\n\n");
}

fn render_provider_impl(out: &mut String, spec: &ProviderSpec<'_>, naming: &Naming) {
    out.push_str(
        "/// The generated provider: MCP tools from one endpoint.\n\
         pub struct McpProvider;\n\
         \n\
         impl Provider for McpProvider {\n",
    );
    out.push_str(&format!(
        "    const ID: &'static str = {};\n    const COMMAND_WORDS: &'static [&'static str] = &[{}];\n",
        lit(&spec.id),
        lit(&spec.id),
    ));
    out.push_str(&format!(
        "{}\n    type Args = Command;\n",
        description_const(&lit(&format!(
            "MCP tools from {} ({} {})",
            spec.endpoint.origin, spec.server.name, spec.server.version
        ))),
    ));
    // The tuple shape is ours (single line when it fits, vertical otherwise) and
    // pinned with rustfmt::skip: rustfmt's one-line-vs-vertical rule for type
    // aliases depends on undocumented width/item-count heuristics, so mirroring
    // it exactly is fragile. Both shapes are covered by end-to-end fmt checks.
    let single = if naming.tools.len() == 1 {
        format!(
            "    type Capabilities = ({},);\n",
            naming.tools[0].cap_struct
        )
    } else {
        format!(
            "    type Capabilities = ({});\n",
            naming
                .tools
                .iter()
                .map(|names| names.cap_struct.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    out.push_str("    #[rustfmt::skip]\n");
    if single.len() <= 100 {
        out.push_str(&single);
    } else {
        out.push_str("    type Capabilities = (\n");
        for names in naming.tools.iter() {
            out.push_str(&format!("        {},\n", names.cap_struct));
        }
        out.push_str("    );\n");
    }
    // Pinned: match-arm overflow shapes depend on tool-name lengths and rustfmt's
    // version-sensitive arm heuristics, so this mechanical dispatch is exempt.
    out.push_str(
        "    #[rustfmt::skip]\n    fn propose(args: Command, _stdin_piped: bool) -> Result<Proposal<Self>, Usage> {\n\
         \x20       match args.action {\n",
    );
    for (tool, names) in spec.tools.iter().zip(naming.tools.iter()) {
        out.push_str(&format!(
            "            Action::{}(args) => Ok(Proposal::to::<{}>({} {{\n",
            names.variant, names.cap_struct, names.input_struct
        ));
        for field in &tool.fields {
            if field.field_type.is_json_flag() {
                let kind = rust_type(&field.field_type);
                if field.required {
                    out.push_str(&format!(
                        "                {}: parse_required_arg::<{}>({}, {}, args.{})?,\n",
                        field.rust_name,
                        kind,
                        lit(&tool.cap_name),
                        lit(&flag_name(&field.rust_name)),
                        field.rust_name
                    ));
                } else {
                    out.push_str(&format!(
                        "                {}: parse_arg::<{}>({}, {}, args.{})?,\n",
                        field.rust_name,
                        kind,
                        lit(&tool.cap_name),
                        lit(&flag_name(&field.rust_name)),
                        field.rust_name
                    ));
                }
            } else {
                out.push_str(&format!(
                    "                {}: args.{},\n",
                    field.rust_name, field.rust_name
                ));
            }
        }
        out.push_str("            })),\n");
    }
    out.push_str("        }\n    }\n}\n\ndekopon_provider_sdk::export!(McpProvider);\n");
}

fn render_cargo(spec: &ProviderSpec<'_>, crate_name: &str) -> String {
    format!(
        "[workspace]\n\
         \n\
         [package]\n\
         name = \"{crate_name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         rust-version = \"1.98.1\"\n\
         publish = false\n\
         description = \"Dekopon provider generated from {} by mcp2dekopon {}\"\n\
         \n\
         # Small components; backtraces come from the broker, not the wasm.\n\
         [profile.release]\n\
         strip = true\n\
         \n\
         # cdylib for the component build, rlib so `cargo test` can drive the\n\
         # provider natively through the testkit.\n\
         [lib]\n\
         crate-type = [\"cdylib\", \"rlib\"]\n\
         \n\
         [dependencies]\n\
         dekopon-provider-sdk = \"={sdk}\"\n\
         clap = {{ version = \"4.6.0\", features = [\"derive\"] }}\n\
         schemars = {{ version = \"1.2.2\", features = [\"derive\"] }}\n\
         serde = {{ version = \"1.0.229\", features = [\"derive\"] }}\n\
         serde_json = \"1.0.151\"\n\
         \n\
         [dev-dependencies]\n\
         dekopon-provider-sdk-testkit = \"={sdk}\"\n\
         serde_json = \"1.0.151\"\n\
         \n\
         [lints.rustdoc]\n\
         broken_intra_doc_links = \"deny\"\n\
         \n\
         [lints.clippy]\n\
         dbg_macro = \"deny\"\n\
         todo = \"deny\"\n\
         unimplemented = \"deny\"\n",
        spec.endpoint.origin,
        spec.generator_version,
        sdk = crate::SDK_VERSION,
    )
}

fn render_build_sh(spec: &ProviderSpec<'_>, crate_name: &str) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         # Build the {id} provider component.\n\
         # Requires: cargo with the wasm32-unknown-unknown target, and wasm-tools.\n\
         set -euo pipefail\n\
         cd \"$(dirname \"$0\")\"\n\
         \n\
         command -v cargo >/dev/null 2>&1 || {{\n\
         \x20 echo \"error: cargo is required\" >&2\n\
         \x20 exit 3\n\
         }}\n\
         if ! cargo build --target wasm32-unknown-unknown --release 2>build-target.log; then\n\
         \x20 if grep -qiE \"target.*(not installed|may not be installed)|can't find crate\" build-target.log; then\n\
         \x20   echo \"error: the wasm32-unknown-unknown target is missing (rustup target add wasm32-unknown-unknown)\" >&2\n\
         \x20   exit 3\n\
         \x20 fi\n\
         \x20 cat build-target.log >&2\n\
         \x20 exit 1\n\
         fi\n\
         command -v wasm-tools >/dev/null 2>&1 || {{\n\
         \x20 echo \"error: wasm-tools is required (cargo install wasm-tools --locked)\" >&2\n\
         \x20 exit 3\n\
         }}\n\
         if wasm_version=$(wasm-tools --version 2>/dev/null); then\n\
         \x20 case \"$wasm_version\" in\n\
         \x20   *\" 1.259.\"*) ;;\n\
         \x20   *) echo \"warning: $wasm_version (generated code is tested with wasm-tools 1.259.x)\" >&2 ;;\n\
         \x20 esac\n\
         fi\n\
         wasm-tools component new \"target/wasm32-unknown-unknown/release/{crate_name}.wasm\" -o \"{id}.wasm\"\n\
         rm -f build-target.log\n\
         echo \"wrote {id}.wasm\"\n",
        id = spec.id,
    )
}

/// Builds a valid sample input for a tool: required fields get dummy values,
/// optional fields are omitted.
fn sample_input(tool: &TightenedTool) -> Value {
    if tool.top_level_map {
        return Value::Object(
            [("payload".to_string(), Value::String("{}".to_string()))]
                .into_iter()
                .collect(),
        );
    }
    let mut map = serde_json::Map::new();
    for field in &tool.fields {
        if field.required {
            map.insert(field.mcp_name.clone(), sample_value(&field.field_type));
        }
    }
    Value::Object(map)
}

fn sample_value(field_type: &FieldType) -> Value {
    match field_type {
        FieldType::String => Value::String(String::new()),
        FieldType::Integer => Value::from(0),
        FieldType::Number => Value::from(0.0),
        FieldType::Boolean => Value::Bool(false),
        FieldType::JsonString => Value::String("{}".to_string()),
        FieldType::Any => Value::Null,
        FieldType::Array { item } => Value::Array(vec![sample_value(item)]),
        FieldType::Nullable { inner } => sample_value(inner),
        FieldType::Object(shape) => sample_object(shape),
        FieldType::Enum(shape) => shape
            .variants
            .first()
            .map(|variant| match &variant.kind {
                VariantKind::Struct(shape) => sample_object(shape),
                VariantKind::Scalar(scalar) => sample_value(scalar),
            })
            .unwrap_or(Value::Null),
    }
}

fn sample_object(shape: &ObjectShape) -> Value {
    let mut map = serde_json::Map::new();
    for field in &shape.fields {
        if field.required {
            map.insert(field.mcp_name.clone(), sample_value(&field.field_type));
        }
    }
    Value::Object(map)
}

/// Renders `command` argv (as Rust string literals) that proposes a tool call
/// with sample values for every required flag.
fn propose_argv(tool: &TightenedTool) -> Vec<String> {
    let mut argv = vec![lit(&tool.cap_name)];
    for field in &tool.fields {
        if !field.required {
            continue;
        }
        argv.push(lit(&format!("--{}", flag_name(&field.rust_name))));
        match &field.field_type {
            FieldType::Boolean => {}
            FieldType::String => argv.push(lit("")),
            FieldType::JsonString => argv.push(lit("{}")),
            FieldType::Integer | FieldType::Number => argv.push(lit("0")),
            other => {
                let text = serde_json::to_string(&sample_value(other))
                    .unwrap_or_else(|_| "null".to_string());
                argv.push(lit(&text));
            }
        }
    }
    argv
}

/// Renders a value as `json!` macro tokens.
fn json_tokens(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => lit(text),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(json_tokens).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let mut pairs: Vec<(&String, &Value)> = map.iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(b.0));
            let inner: Vec<String> = pairs
                .iter()
                .map(|(key, value)| format!("{}: {}", lit(key), json_tokens(value)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

fn render_native_e2e(spec: &ProviderSpec<'_>, naming: &Naming, crate_name: &str) -> String {
    let mut out = format!(
        "//! End-to-end calls through the testkit fake broker against a scripted MCP origin.\n\
         use dekopon_provider_sdk::provider::{{Header, Response}};\n\
         use dekopon_provider_sdk_testkit::{{HttpScript, Native}};\n\
         use serde_json::{{json, Value}};\n\
         \n\
         use {crate_name}::McpProvider;\n\
         \n\
         const ORIGIN_HOST: &str = \"mcp.example.test\";\n\
         const ORIGIN_BASE: &str = \"https://mcp.example.test/mcp\";\n\
         \n\
         /// One scripted MCP origin answering `tools/call` with `cap` content. The same\n\
         /// answer feeds the `initialize` leg, which tolerates it and falls back to the\n\
         /// offered protocol version; each test asserts all three request legs.\n\
         fn scripted(cap: &str) -> HttpScript {{\n\
         \x20   let body = json!({{\n\
         \x20       \"jsonrpc\": \"2.0\",\n\
         \x20       \"id\": 2,\n\
         \x20       \"result\": {{\"content\": [{{\"type\": \"text\", \"text\": format!(\"ok-{{cap}}\")}}]}},\n\
         \x20   }});\n\
         \x20   HttpScript::new(\n\
         \x20       ORIGIN_HOST,\n\
         \x20       \"POST\",\n\
         \x20       Response {{\n\
         \x20           status: 200,\n\
         \x20           headers: Vec::new(),\n\
         \x20           body: body.to_string().into_bytes(),\n\
         \x20       }},\n\
         \x20   )\n\
         }}\n\
         \n\
         /// A scripted answer with full control over status, headers and body,\n\
         /// for the error-path tests below.\n\
         fn scripted_raw(status: u16, headers: Vec<Header>, body: &str) -> HttpScript {{\n\
         \x20   HttpScript::new(\n\
         \x20       ORIGIN_HOST,\n\
         \x20       \"POST\",\n\
         \x20       Response {{\n\
         \x20           status,\n\
         \x20           headers,\n\
         \x20           body: body.as_bytes().to_vec(),\n\
         \x20       }},\n\
         \x20   )\n\
         }}\n\
         \n\
         fn sse_content_type() -> Header {{\n\
         \x20   Header {{\n\
         \x20       name: \"content-type\".to_string(),\n\
         \x20       value: b\"text/event-stream\".to_vec(),\n\
         \x20   }}\n\
         }}\n\
         \n"
    );
    for (tool, names) in spec.tools.iter().zip(naming.tools.iter()) {
        let sample = json_tokens(&sample_input(tool));
        out.push_str(&format!(
            "#[test]\n\
             #[rustfmt::skip]\n\
             fn e2e_{}() {{\n\
             \x20   let native = Native::<McpProvider>::new()\n\
             \x20       .settings(json!({{\"baseUrl\": ORIGIN_BASE}}))\n\
             \x20       .http(scripted({cap_lit}));\n\
             \x20   let output = native.call({id_cap_lit}, &json!({sample}).to_string());\n\
             \x20   assert_eq!(output.status, 0, \"stderr: {{}}\", output.stderr);\n\
             \x20   let expected = json!([{{\n\
             \x20       \"type\": \"text\",\n\
             \x20       \"text\": format!(\"ok-{{}}\", {cap_lit}),\n\
             \x20   }}]);\n\
             \x20   let actual: Value = serde_json::from_slice(&output.stdout).expect(\"stdout is JSON\");\n\
             \x20   assert_eq!(actual, expected);\n\
             \x20   let requests = native.requests();\n\
             \x20   assert_eq!(requests.len(), 3, \"initialize, notice, plus tools/call\");\n\
             \x20   let init: Value = serde_json::from_slice(&requests[0].body).expect(\"init body\");\n\
             \x20   assert_eq!(init[\"method\"], \"initialize\");\n\
             \x20   let notice: Value = serde_json::from_slice(&requests[1].body).expect(\"notice body\");\n\
             \x20   assert_eq!(notice[\"method\"], \"notifications/initialized\");\n\
             \x20   let call: Value = serde_json::from_slice(&requests[2].body).expect(\"call body\");\n\
             \x20   assert_eq!(call[\"method\"], \"tools/call\");\n\
             \x20   assert_eq!(call[\"params\"][\"name\"], {mcp_lit});\n\
             }}\n\
             \n",
            names.test_name,
            cap_lit = lit(&tool.cap_name),
            id_cap_lit = lit(&format!("{}.{}", spec.id, tool.cap_name)),
            mcp_lit = lit(&tool.mcp_name),
        ));
        let argv = propose_argv(tool).join(", ");
        out.push_str(&format!(
            "#[test]\n\
             #[rustfmt::skip]\n\
             fn propose_{}() {{\n\
             \x20   let outcome = dekopon_provider_sdk::provider::command::<McpProvider>(\n\
             \x20       &[{argv}].map(str::to_string),\n\
             \x20       false,\n\
             \x20   );\n\
             \x20   match outcome {{\n\
             \x20       dekopon_provider_sdk::CommandRunOutcome::Proposed {{ capability, input, .. }} => {{\n\
             \x20           assert_eq!(capability.to_string(), {id_cap});\n\
             \x20           assert_eq!(input, json!({sample}));\n\
             \x20       }}\n\
             \x20       other => panic!(\"propose {}: expected Proposed, got {{other:?}}\"),\n\
             \x20   }}\n\
             }}\n\
             \n",
            names.test_name,
            tool.cap_name,
            id_cap = lit(&format!("{}.{}", spec.id, tool.cap_name)),
            sample = json_tokens(&sample_input(tool)),
        ));
    }
    let subcommands: Vec<String> = spec.tools.iter().map(|t| lit(&t.cap_name)).collect();
    let ids: Vec<String> = spec
        .tools
        .iter()
        .map(|t| lit(&format!("{}.{}", spec.id, t.cap_name)))
        .collect();
    let labels: Vec<String> = spec
        .tools
        .iter()
        .map(|t| {
            format!(
                "({}.to_string(), {}.to_string(), {}.to_string())",
                lit(&format!("{}.{}", spec.id, t.cap_name)),
                lit(t.effect.wire()),
                lit(t.risk.wire()),
            )
        })
        .collect();
    out.push_str(&format!(
        "#[test]\n\
         #[rustfmt::skip]\n\
         fn cli_help_renders_for_every_tool() {{\n\
         \x20   for subcommand in [{}] {{\n\
         \x20       let outcome = dekopon_provider_sdk::provider::command::<McpProvider>(\n\
         \x20           &[subcommand.to_string(), \"--help\".to_string()],\n\
         \x20           false,\n\
         \x20       );\n\
         \x20       assert!(\n\
         \x20           matches!(\n\
         \x20               outcome,\n\
         \x20               dekopon_provider_sdk::CommandRunOutcome::Rendered {{ status: 0, .. }}\n\
         \x20           ),\n\
         \x20           \"{{subcommand}} --help\"\n\
         \x20       );\n\
         \x20   }}\n\
         }}\n\
         \n\
         #[test]\n\
         #[rustfmt::skip]\n\
         fn unknown_capability_fails() {{\n\
         \x20   let native = Native::<McpProvider>::new();\n\
         \x20   let output = native.call({}, \"{{}}\");\n\
         \x20   assert_ne!(output.status, 0);\n\
         \x20   assert!(\n\
         \x20       output.stderr.contains(\"no such capability\"),\n\
         \x20       \"{{}}\",\n\
         \x20       output.stderr\n\
         \x20   );\n\
         }}\n\
         \n\
         #[test]\n\
         #[rustfmt::skip]\n\
         fn manifest_lists_every_capability() {{\n\
         \x20   let manifest =\n\
         \x20       dekopon_provider_sdk::provider::manifest::<McpProvider>().expect(\"closed schemas\");\n\
         \x20   let ids: Vec<String> = manifest\n\
         \x20       .capabilities\n\
         \x20       .iter()\n\
         \x20       .map(|capability| capability.id.to_string())\n\
         \x20       .collect();\n\
         \x20   assert_eq!(ids, vec![{}]);\n\
         \x20   let labels: Vec<(String, String, String)> = manifest\n\
         \x20       .capabilities\n\
         \x20       .iter()\n\
         \x20       .map(|capability| {{\n\
         \x20           (\n\
         \x20               capability.id.to_string(),\n\
         \x20               capability.effect.to_string(),\n\
         \x20               format!(\"{{:?}}\", capability.risk),\n\
         \x20           )\n\
         \x20       }})\n\
         \x20       .collect();\n\
         \x20   assert_eq!(labels, vec![{}]);\n\
         }}\n",
        subcommands.join(", "),
        lit(&format!("{}.no-such-tool", spec.id)),
        ids.join(", "),
        labels.join(", "),
    ));
    if let Some((tool, names)) = spec.tools.iter().zip(naming.tools.iter()).next() {
        render_error_path_tests(&mut out, spec, tool, names);
    }
    out
}

/// Error-path tests against the first capability: SSE answers, tool-reported
/// errors, answers without `content`, and HTTP failures.
fn render_error_path_tests(
    out: &mut String,
    spec: &ProviderSpec<'_>,
    tool: &TightenedTool,
    names: &ToolNames,
) {
    let sample = json_tokens(&sample_input(tool));
    let id_cap = lit(&format!("{}.{}", spec.id, tool.cap_name));
    let test = names.test_name.as_str();
    out.push_str(&format!(
        "\n\
         #[test]\n\
         #[rustfmt::skip]\n\
         fn e2e_error_{test}_sse() {{\n\
         \x20   let answer = json!({{\n\
         \x20       \"jsonrpc\": \"2.0\",\n\
         \x20       \"id\": 2,\n\
         \x20       \"result\": {{\"content\": [{{\"type\": \"text\", \"text\": \"sse-ok\"}}]}},\n\
         \x20   }});\n\
         \x20   let body = format!(\"event: message\\ndata: {{answer}}\\n\\n\");\n\
         \x20   let native = Native::<McpProvider>::new()\n\
         \x20       .settings(json!({{\"baseUrl\": ORIGIN_BASE}}))\n\
         \x20       .http(scripted_raw(200, vec![sse_content_type()], &body));\n\
         \x20   let output = native.call({id_cap}, &json!({sample}).to_string());\n\
         \x20   assert_eq!(output.status, 0, \"stderr: {{}}\", output.stderr);\n\
         \x20   let actual: Value = serde_json::from_slice(&output.stdout).expect(\"stdout is JSON\");\n\
         \x20   assert_eq!(actual, json!([{{\"type\": \"text\", \"text\": \"sse-ok\"}}]));\n\
         }}\n\
         \n\
         #[test]\n\
         #[rustfmt::skip]\n\
         fn e2e_error_{test}_tool_error() {{\n\
         \x20   let body = json!({{\n\
         \x20       \"jsonrpc\": \"2.0\",\n\
         \x20       \"id\": 2,\n\
         \x20       \"result\": {{\n\
         \x20           \"content\": [{{\"type\": \"text\", \"text\": \"bad\"}}],\n\
         \x20           \"isError\": true,\n\
         \x20       }},\n\
         \x20   }})\n\
         \x20   .to_string();\n\
         \x20   let native = Native::<McpProvider>::new()\n\
         \x20       .settings(json!({{\"baseUrl\": ORIGIN_BASE}}))\n\
         \x20       .http(scripted_raw(200, Vec::new(), &body));\n\
         \x20   let output = native.call({id_cap}, &json!({sample}).to_string());\n\
         \x20   assert_ne!(output.status, 0);\n\
         \x20   let actual: Value = serde_json::from_slice(&output.stdout).expect(\"stdout is JSON\");\n\
         \x20   assert_eq!(actual, json!([{{\"type\": \"text\", \"text\": \"bad\"}}]));\n\
         }}\n\
         \n\
         #[test]\n\
         #[rustfmt::skip]\n\
         fn e2e_error_{test}_bare_result() {{\n\
         \x20   let body = json!({{\n\
         \x20       \"jsonrpc\": \"2.0\",\n\
         \x20       \"id\": 2,\n\
         \x20       \"result\": {{\"structuredContent\": {{\"answer\": 42}}}},\n\
         \x20   }})\n\
         \x20   .to_string();\n\
         \x20   let native = Native::<McpProvider>::new()\n\
         \x20       .settings(json!({{\"baseUrl\": ORIGIN_BASE}}))\n\
         \x20       .http(scripted_raw(200, Vec::new(), &body));\n\
         \x20   let output = native.call({id_cap}, &json!({sample}).to_string());\n\
         \x20   assert_eq!(output.status, 0, \"stderr: {{}}\", output.stderr);\n\
         \x20   let actual: Value = serde_json::from_slice(&output.stdout).expect(\"stdout is JSON\");\n\
         \x20   assert_eq!(actual, json!({{\"structuredContent\": {{\"answer\": 42}}}}));\n\
         }}\n\
         \n\
         #[test]\n\
         #[rustfmt::skip]\n\
         fn e2e_error_{test}_server_error() {{\n\
         \x20   let native = Native::<McpProvider>::new()\n\
         \x20       .settings(json!({{\"baseUrl\": ORIGIN_BASE}}))\n\
         \x20       .http(scripted_raw(500, Vec::new(), \"boom\"));\n\
         \x20   let output = native.call({id_cap}, &json!({sample}).to_string());\n\
         \x20   assert_ne!(output.status, 0);\n\
         \x20   assert!(output.stderr.contains(\"status 500\"), \"{{}}\", output.stderr);\n\
         \x20   assert_eq!(native.requests().len(), 1);\n\
         }}\n",
    ));
}

fn render_conformance(spec: &ProviderSpec<'_>, crate_name: &str) -> String {
    format!(
        "//! Real-component conformance: run `./build.sh` first, then `cargo test`.\n\
         use {crate_name}::McpProvider;\n\
         \n\
         #[test]\n\
         #[rustfmt::skip]\n\
         fn component_conformance() {{\n\
         \x20   let wasm = std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"{}.wasm\");\n\
         \x20   if !wasm.exists() {{\n\
         \x20       eprintln!(\n\
         \x20           \"skipping conformance: {{}} is missing; run ./build.sh first\",\n\
         \x20           wasm.display()\n\
         \x20       );\n\
         \x20       return;\n\
         \x20   }}\n\
         \x20   dekopon_provider_sdk_testkit::conformance::<McpProvider>(&wasm).expect(\"component conformance\");\n\
         }}\n",
        spec.id
    )
}

fn render_world(spec: &ProviderSpec<'_>) -> String {
    let package = spec.id.replace('.', "-");
    format!(
        "package dekopon:{package}@0.1.0;\n\
         \n\
         /// Generated provider: {} MCP tools from {}.\n\
         world provider {{\n\
         \x20   include dekopon:provider/provider-cli@0.4.0;\n\
         \x20   import dekopon:http/client@1.1.0;\n\
         \x20   import dekopon:settings/config@0.1.0;\n\
         }}\n",
        spec.tools.len(),
        spec.endpoint.origin,
    )
}

fn render_broker(spec: &ProviderSpec<'_>) -> String {
    let mut out = format!(
        "# Starter broker fragment for provider `{id}`, generated by mcp2dekopon {genver}\n\
         # from {vendor} ({server} {version}).\n\
         #\n\
         # Install: copy this file into your broker.d/ directory and add a policy to a\n\
         # `*.cedar` file (see ../{id}.cedar). Every capability below may POST only to\n\
         # {origin}, at most three times per call (initialize, the initialized\n\
         # notice, plus tools/call).\n\
         #\n\
         # Review the effect labels in ../report.json before granting anything: a wrong\n\
         # `read-only` would place a capability in the provider's `read-only` group.\n",
        id = spec.id,
        genver = spec.generator_version,
        vendor = spec.endpoint.vendor_base,
        server = spec.server.name,
        version = spec.server.version,
        origin = spec.endpoint.origin,
    );
    if let Some(note) = &spec.endpoint.plaintext_note {
        out.push_str(&format!("#\n# NOTE: {note}\n"));
    }
    out.push_str(&format!(
        "#\n\
         # The provider posts to its vendor default ({vendor}); to point a\n\
         # deployment elsewhere, add a commented-style override to another fragment:\n\
         #\n\
         # providerSettings:\n\
         #   {id}:\n\
         #     baseUrl: {vendor}\n",
        id = spec.id,
        vendor = spec.endpoint.vendor_base,
    ));
    out.push_str(&format!(
        "capabilities:\n  {id}:\n    capabilities:\n",
        id = spec.id
    ));
    for tool in spec.tools {
        out.push_str(&format!(
            "      {id}.{cap}:\n\
             \x20       # credential: <name-in-broker-credentials.yaml>  # uncomment when the MCP server needs auth\n\
             \x20       constraints:\n\
             \x20         timeoutMs: {timeout_ms}\n\
             \x20         http:\n\
             \x20           allowedHosts: [{host}]\n\
             \x20           allowedMethods: [POST]\n\
             \x20           maxRequests: 3\n\
             \x20           maxRequestBytes: 65536\n\
             \x20           maxResponseBytes: 1048576\n",
            id = spec.id,
            cap = tool.cap_name,
            host = spec.endpoint.host_scope,
            timeout_ms = spec.timeout_secs.saturating_mul(1000),
        ));
        if spec.endpoint.plaintext_loopback {
            out.push_str("            allowPlaintextLoopback: true\n");
        }
    }
    out
}

fn render_cedar(spec: &ProviderSpec<'_>) -> String {
    let policy_id = spec.id.replace('.', "-");
    let mut out = format!(
        "// Starter policy for provider `{id}`, generated by mcp2dekopon {genver}.\n\
         // Grants nothing: uncomment and edit the example once the operator has\n\
         // reviewed the effect labels in report.json.\n\
         //\n",
        id = spec.id,
        genver = spec.generator_version,
    );
    let readonly: Vec<&TightenedTool> = spec
        .tools
        .iter()
        .filter(|tool| tool.effect == crate::tighten::Effect::ReadOnly)
        .collect();
    if readonly.is_empty() {
        out.push_str(
            "// This provider exports no read-only capabilities; every capability below\n// is a write of some kind, so grant each one deliberately.\n//\n",
        );
    } else {
        out.push_str("// Example: let principal \"local-user\" invoke the read-only\n// capabilities through agent \"reviewer\":\n//\n");
        out.push_str(&format!("// @id(\"{policy_id}-readonly-example\")\n"));
        out.push_str("// permit(\n//     principal == Dekopon::Principal::\"local-user\",\n");
        let actions: Vec<String> = readonly
            .iter()
            .map(|tool| format!("Dekopon::Action::\"{}.{}\"", spec.id, tool.cap_name))
            .collect();
        out.push_str(&format!("//     action in [{}],\n", actions.join(", ")));
        out.push_str(&format!(
            "//     resource == Dekopon::Provider::\"{}\"\n// ) when {{ context.agent == \"reviewer\" }};\n",
            spec.id
        ));
    }
    out
}

fn render_readme(spec: &ProviderSpec<'_>) -> String {
    let mut out = format!(
        "# `{id}` provider\n\
         \n\
         Generated by mcp2dekopon {genver} from [{server} {version}]({vendor}) for\n\
         `dekopon-provider-sdk ={sdk}`. One MCP tool, one capability; the command word\n\
         is `{id}` with one subcommand per tool.\n\
         \n\
         ## Capabilities\n\
         \n\
         | Capability | MCP tool | Effect | Risk | Notes |\n\
         | --- | --- | --- | --- | --- |\n",
        id = spec.id,
        genver = spec.generator_version,
        server = spec.server.name,
        version = spec.server.version,
        vendor = spec.endpoint.vendor_base,
        sdk = crate::SDK_VERSION,
    );
    for tool in spec.tools {
        let mut notes: Vec<String> = tool.flags.iter().cloned().collect();
        notes.extend(tool.warnings.iter().cloned());
        let notes = if notes.is_empty() {
            "—".to_string()
        } else {
            notes.join(", ")
        };
        out.push_str(&format!(
            "| `{id}.{cap}` | `{tool}` | {effect} | {risk} | {notes} |\n",
            id = md_cell(&spec.id),
            cap = md_cell(&tool.cap_name),
            tool = md_cell(&tool.mcp_name),
            effect = tool.effect.wire(),
            risk = tool.risk.wire(),
            notes = md_cell(&notes),
        ));
    }
    if spec.endpoint.plaintext_note.is_some() {
        out.push_str(
            "\n\
             ## Security note\n\
             \n\
             This provider talks plaintext HTTP outside loopback. The broker\n\
             fragment alone cannot authorize that: the broker also needs an\n\
             `http.plaintextHosts` entry in host.yaml covering the origin.\n",
        );
    }
    out.push_str(&format!(
        "\n\
         ## Review before granting\n\
         \n\
         Every tool is labeled `external-write / Medium` unless its MCP `annotations`\n\
         said `readOnlyHint` (then `read-only / Low`) or `destructiveHint` (then\n\
         `external-write / High`). MCP annotations are hints, not guarantees: check each\n\
         capability above against what the tool really does before writing a Cedar\n\
         policy, because a wrong `read-only` places the capability in the provider's\n\
         `read-only` group. The full per-tool record, including skipped tools and\n\
         their reasons, is in `../report.json`.\n\
         \n\
         Free maps (open JSON objects) travel as JSON-text arguments the provider\n\
         parses at call time; see the `free-map-as-string` notes above.\n\
         \n\
         ## Build and test\n\
         \n\
         ```sh\n\
         ./build.sh        # component -> {id}.wasm (needs the wasm32 target + wasm-tools)\n\
         cargo test        # native end-to-end calls; conformance runs when {id}.wasm exists\n\
         ```\n\
         \n\
         ## Install\n\
         \n\
         1. Copy `{id}.wasm` to your providers directory.\n\
         2. Copy `broker.d/{id}.yaml` into `broker.d/`.\n\
         3. Grant capabilities in a `*.cedar` file; `{id}.cedar` holds a commented\n\
         \x20   starting point that grants nothing.\n\
         \n\
         Point a deployment at another MCP endpoint without rebuilding:\n\
         \n\
         ```yaml\n\
         providerSettings:\n\
         \x20 {id}:\n\
         \x20   baseUrl: https://mcp.example.test/mcp\n\
         ```\n\
         \n\
         Regenerate with:\n\
         \n\
         ```sh\n\
         mcp2dekopon gen --url {vendor} --id {id} --out <dir>\n\
         ```\n",
        id = spec.id,
        vendor = spec.endpoint.vendor_base,
    ));
    out
}

/// Splits tools into provider-sized chunks, naming split providers `<id>-N`.
/// Tools are kept in server order.
pub fn split_tools(id: &str, tools: &[TightenedTool]) -> Result<Vec<(String, PathBuf)>, String> {
    if tools.len() <= crate::MAX_CAPABILITIES {
        return Ok(vec![(id.to_string(), PathBuf::from(id))]);
    }
    let chunks = tools.len().div_ceil(crate::MAX_CAPABILITIES);
    let mut providers = Vec::with_capacity(chunks);
    for (index, _) in tools.chunks(crate::MAX_CAPABILITIES).enumerate() {
        let part = format!("{id}-{}", index + 1);
        crate::names::validate_provider_id(&part)?;
        providers.push((part.clone(), PathBuf::from(part)));
    }
    Ok(providers)
}

#[cfg(test)]
mod tests {
    use super::{description_const, lit, md_cell, split_tools};
    use crate::fetch::{Endpoint, ServerInfo};
    use crate::tighten::{Effect, Risk, TightenedTool};

    fn tool(name: &str) -> TightenedTool {
        TightenedTool {
            mcp_name: name.to_string(),
            cap_name: name.to_string(),
            description: String::new(),
            fields: Vec::new(),
            top_level_map: false,
            effect: Effect::ExternalWrite,
            risk: Risk::Medium,
            flags: Default::default(),
            warnings: Default::default(),
        }
    }

    fn unit_dir(suite: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("mcp2dekopon-unit-{suite}-{}", std::process::id()))
    }

    fn spec<'a>(
        tools: &'a [TightenedTool],
        endpoint: &'a Endpoint,
        server: &'a ServerInfo,
    ) -> super::ProviderSpec<'a> {
        super::ProviderSpec {
            id: "mockmcp".to_string(),
            tools,
            endpoint,
            server,
            generator_version: "0.0.0-test".to_string(),
            timeout_secs: 7,
        }
    }

    fn server() -> ServerInfo {
        ServerInfo {
            name: "mock-mcp".to_string(),
            version: "0.0.0".to_string(),
            protocol_version: crate::MCP_PROTOCOL_VERSION.to_string(),
        }
    }

    #[test]
    fn small_servers_keep_their_id() {
        let tools: Vec<TightenedTool> = (0..32).map(|i| tool(&format!("tool-{i}"))).collect();
        let parts = split_tools("mockmcp", &tools).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].0, "mockmcp");
    }

    #[test]
    fn large_servers_split_into_numbered_providers() {
        let tools: Vec<TightenedTool> = (0..33).map(|i| tool(&format!("tool-{i}"))).collect();
        let parts = split_tools("mockmcp", &tools).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "mockmcp-1");
        assert_eq!(parts[1].0, "mockmcp-2");
    }

    #[test]
    fn markdown_cells_escape_pipes_and_newlines() {
        assert_eq!(md_cell("plain"), "plain");
        assert_eq!(md_cell("a|b"), "a\\|b");
        assert_eq!(md_cell("a\nb"), "a b");
    }

    #[test]
    fn atomic_file_write_leaves_no_tmp_behind() {
        let dir = unit_dir("atomic");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("report.json");
        super::write_file_atomic(&path, "{\"ok\":true}\n".to_string()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"ok\":true}\n");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provider_write_replaces_the_directory_on_force() {
        let dir = unit_dir("provider");
        let _ = std::fs::remove_dir_all(&dir);
        let endpoint = crate::fetch::validate_endpoint("http://127.0.0.1:1/mcp").unwrap();
        let server = server();
        let tools = vec![tool("search")];
        let files = super::render_provider(&spec(&tools, &endpoint, &server));
        let out = dir.join("mockmcp");
        super::write_provider(&out, &files, false).unwrap();
        assert!(out.join("src/lib.rs").is_file());
        // Without --force the second write refuses,...
        assert!(super::write_provider(&out, &files, false).is_err());
        // ... while --force swaps the directory wholesale: stray files
        // (notes, a stale component) do not survive.
        std::fs::write(out.join("notes.txt"), "operator notes").unwrap();
        std::fs::write(out.join("mockmcp.wasm"), "stale").unwrap();
        super::write_provider(&out, &files, true).unwrap();
        assert!(out.join("src/lib.rs").is_file());
        assert!(!out.join("notes.txt").exists());
        assert!(!out.join("mockmcp.wasm").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broker_fragment_carries_timeout_and_three_requests() {
        let endpoint = crate::fetch::validate_endpoint("http://127.0.0.1:1/mcp").unwrap();
        let server = server();
        let tools = vec![tool("search")];
        let files = super::render_provider(&spec(&tools, &endpoint, &server));
        let broker = files
            .iter()
            .find(|file| file.path == "broker.d/mockmcp.yaml")
            .unwrap();
        assert!(
            broker.content.contains("timeoutMs: 7000"),
            "{}",
            broker.content
        );
        assert!(
            broker.content.contains("maxRequests: 3"),
            "{}",
            broker.content
        );
        assert!(
            broker.content.contains("at most three times per call"),
            "{}",
            broker.content
        );
    }

    #[test]
    fn readme_escapes_pipes_and_warns_on_plaintext() {
        let loopback = crate::fetch::validate_endpoint("http://127.0.0.1:1/mcp").unwrap();
        let lan = crate::fetch::validate_endpoint("http://rpi.lan:8080/mcp").unwrap();
        let server = server();
        let mut piped = tool("search");
        piped.mcp_name = "a|b".to_string();
        let tools = vec![piped];
        let lan_files = super::render_provider(&spec(&tools, &lan, &server));
        let readme = lan_files
            .iter()
            .find(|file| file.path == "README.md")
            .unwrap();
        assert!(readme.content.contains("`a\\|b`"), "{}", readme.content);
        assert!(
            readme.content.contains("## Security note"),
            "{}",
            readme.content
        );
        let local_files = super::render_provider(&spec(&tools, &loopback, &server));
        let readme = local_files
            .iter()
            .find(|file| file.path == "README.md")
            .unwrap();
        assert!(
            !readme.content.contains("## Security note"),
            "{}",
            readme.content
        );
    }

    #[test]
    fn generated_lib_sends_the_notice_and_checks_ids() {
        let endpoint = crate::fetch::validate_endpoint("http://127.0.0.1:1/mcp").unwrap();
        let server = server();
        let tools = vec![tool("search")];
        let files = super::render_provider(&spec(&tools, &endpoint, &server));
        let lib = files.iter().find(|file| file.path == "src/lib.rs").unwrap();
        for needle in [
            "notifications/initialized",
            "did not match request 2",
            "Protocol(String)",
            "fn snippet",
        ] {
            assert!(lib.content.contains(needle), "missing {needle}");
        }
        let e2e = files
            .iter()
            .find(|file| file.path == "tests/native_e2e.rs")
            .unwrap();
        assert!(e2e.content.contains("requests.len(), 3"), "{}", e2e.content);
        assert!(
            e2e.content.contains("notifications/initialized"),
            "{}",
            e2e.content
        );
    }

    #[test]
    fn build_script_accepts_patch_releases() {
        let endpoint = crate::fetch::validate_endpoint("http://127.0.0.1:1/mcp").unwrap();
        let server = server();
        let tools = vec![tool("search")];
        let files = super::render_provider(&spec(&tools, &endpoint, &server));
        let build = files.iter().find(|file| file.path == "build.sh").unwrap();
        assert!(build.content.contains("*\" 1.259.\"*"), "{}", build.content);
        assert!(
            build.content.contains("wasm-tools 1.259.x"),
            "{}",
            build.content
        );
    }

    #[test]
    fn description_const_matches_rustfmt_break_rule() {
        let short = description_const(&lit("tiny"));
        assert!(!short.contains('\n'), "{short}");
        // Overflows alone but fits on the continuation: rustfmt breaks after `=`.
        let wrappable = description_const(&lit(&"x".repeat(60)));
        assert!(wrappable.starts_with("    const DESCRIPTION: &'static str =\n        \""));
        assert!(wrappable.lines().all(|line| line.len() <= 100));
        // Too long for any line: rustfmt leaves the single line alone.
        let huge = description_const(&lit(&"y".repeat(200)));
        assert!(!huge.contains('\n'), "{huge}");
    }
}
