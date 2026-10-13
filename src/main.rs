//! `mcp2dekopon gen`: fetch an MCP tool list, tighten it, render providers.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::HashSet;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use mcp2dekopon::codegen::{self, ProviderSpec};
use mcp2dekopon::fetch::{self, Endpoint};
use mcp2dekopon::names;
use mcp2dekopon::report::{self, SkippedTool};
use mcp2dekopon::tighten::{self, TightenedTool};

#[derive(Parser)]
#[command(
    name = "mcp2dekopon",
    version,
    about = "Generate Dekopon providers from MCP servers"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch tools/list from an MCP server and generate provider crate(s)
    Gen {
        /// The MCP Streamable HTTP endpoint, e.g. https://mcp.example.test/mcp
        #[arg(long)]
        url: String,
        /// The provider id (lowercase, digits, '.', '-', '_')
        #[arg(long)]
        id: String,
        /// The output directory; provider crates land in subdirectories
        #[arg(long)]
        out: PathBuf,
        /// An extra request header for fetching, NAME:VALUE; repeatable. Used
        /// only to fetch the tool list, never written to generated files.
        #[arg(long)]
        header: Vec<String>,
        /// Per-request timeout in seconds
        #[arg(long, default_value_t = 30)]
        timeout_secs: u64,
        /// Overwrite existing provider directories
        #[arg(long)]
        force: bool,
        /// MCP protocol version to offer
        #[arg(long, default_value = mcp2dekopon::MCP_PROTOCOL_VERSION)]
        protocol_version: String,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Gen {
            url,
            id,
            out,
            header,
            timeout_secs,
            force,
            protocol_version,
        } => {
            if let Err(message) = run_gen(
                &url,
                &id,
                &out,
                &header,
                timeout_secs,
                force,
                &protocol_version,
            ) {
                eprintln!("mcp2dekopon: {message}");
                std::process::exit(1);
            }
        }
    }
}

fn run_gen(
    url: &str,
    id: &str,
    out: &PathBuf,
    headers: &[String],
    timeout_secs: u64,
    force: bool,
    protocol_version: &str,
) -> Result<(), String> {
    let endpoint: Endpoint = fetch::validate_endpoint(url)?;
    let provider_id = names::validate_provider_id(id)?;
    let extra_headers = parse_headers(headers)?;

    eprintln!("fetching tools/list from {} ...", endpoint.vendor_base);
    let (server, listed) = fetch::fetch_tools(
        &endpoint.vendor_base,
        &extra_headers,
        timeout_secs,
        protocol_version,
        "mcp2dekopon",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| e.to_string())?;
    eprintln!(
        "server {} {} speaks {}; {} tool(s) listed",
        server.name,
        server.version,
        server.protocol_version,
        listed.len()
    );

    // Tighten every tool; skips carry reasons into the report.
    let max_name_len = mcp2dekopon::names::MAX_IDENTIFIER_LEN - provider_id.len() - 1;
    let mut used_names = HashSet::new();
    let mut used_types = HashSet::new();
    let mut tightened: Vec<TightenedTool> = Vec::new();
    let mut skipped: Vec<SkippedTool> = Vec::new();
    for tool in &listed {
        match tighten::tighten_tool(tool, &mut used_names, max_name_len, &mut used_types) {
            Ok(ready) => tightened.push(ready),
            Err(skip) => {
                eprintln!(
                    "skipping {:?}: {} ({})",
                    tool.name, skip.reason, skip.detail
                );
                skipped.push(SkippedTool {
                    tool: tool.name.clone(),
                    reason: skip.reason.to_string(),
                    detail: skip.detail,
                });
            }
        }
    }

    let parts = codegen::split_tools(&provider_id, &tightened)?;
    if parts.len() > 1 {
        eprintln!(
            "splitting {} tools into {} providers of at most {}",
            tightened.len(),
            parts.len(),
            mcp2dekopon::MAX_CAPABILITIES
        );
    }

    std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let generator_version = env!("CARGO_PKG_VERSION").to_string();
    let mut providers = Vec::new();
    for ((part_id, part_dir), chunk) in parts
        .iter()
        .zip(tightened.chunks(mcp2dekopon::MAX_CAPABILITIES))
    {
        let spec = ProviderSpec {
            id: part_id.clone(),
            tools: chunk,
            endpoint: &endpoint,
            server: &server,
            generator_version: generator_version.clone(),
        };
        let files = codegen::render_provider(&spec);
        let dir = out.join(part_dir);
        codegen::write_provider(&dir, &files, force)?;
        eprintln!("wrote {} ({} capabilities)", dir.display(), chunk.len());
        providers.push((
            part_id.clone(),
            part_dir.display().to_string(),
            chunk.to_vec(),
        ));
    }

    let report_text = serde_json::to_string_pretty(&report::build_report(
        &endpoint.vendor_base,
        &server,
        providers,
        skipped.clone(),
    ))
    .map_err(|e| format!("cannot encode report: {e}"))?;
    let report_path = out.join("report.json");
    std::fs::write(&report_path, format!("{report_text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", report_path.display()))?;

    if tightened.is_empty() {
        return Err(format!(
            "no usable tools ({} skipped); report written to {}",
            skipped.len(),
            report_path.display()
        ));
    }
    eprintln!(
        "done: {} provider(s), {} capabilities, {} skipped; report at {}",
        parts.len(),
        tightened.len(),
        skipped.len(),
        report_path.display()
    );
    eprintln!("next: cd into a provider, run ./build.sh, then cargo test");
    Ok(())
}

fn parse_headers(headers: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut parsed = Vec::with_capacity(headers.len());
    for header in headers {
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| format!("invalid --header {header:?}: want NAME:VALUE"))?;
        let name = name.trim();
        if name.is_empty() {
            return Err(format!("invalid --header {header:?}: want NAME:VALUE"));
        }
        parsed.push((name.to_string(), value.trim_start().to_string()));
    }
    Ok(parsed)
}
