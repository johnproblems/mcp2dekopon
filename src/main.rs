//! `mcp2dekopon gen`: fetch an MCP tool list, tighten it, render providers.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::HashSet;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

use mcp2dekopon::codegen::{self, ProviderSpec};
use mcp2dekopon::fetch::{self, Endpoint};
use mcp2dekopon::names;
use mcp2dekopon::report::{self, SkippedTool};
use mcp2dekopon::tighten::{self, TightenedTool};

#[derive(Parser)]
#[command(
    name = "mcp2dekopon",
    version,
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (sdk 0.46.1, mcp 2025-06-18)"
    ),
    about = "Generate Dekopon providers from MCP servers"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch tools/list from an MCP server and generate provider crate(s)
    #[command(after_long_help = "Examples:\n  \
        mcp2dekopon gen --url https://mcp.example.test/mcp --id example --out ./out\n  \
        mcp2dekopon gen --url https://mcp.example.test/mcp --id example --out ./out --dry-run\n  \
        mcp2dekopon gen --url https://mcp.example.test/mcp --id example --out ./out --save-tools tools.json\n  \
        mcp2dekopon gen --url https://mcp.example.test/mcp --id example --out ./out --from-file tools.json")]
    Gen(GenArgs),
    /// Print shell completions
    Completions {
        /// The shell to complete
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(clap::Args)]
struct GenArgs {
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
    /// Fetch and tighten, then print the summary without writing anything
    #[arg(long)]
    dry_run: bool,
    /// Generate from a saved tool list (see --save-tools) instead of fetching
    #[arg(long, conflicts_with = "save_tools")]
    from_file: Option<PathBuf>,
    /// Fetch the tool list, save it as JSON, and stop before generating
    #[arg(long, conflicts_with = "from_file")]
    save_tools: Option<PathBuf>,
    /// Print only warnings and errors
    #[arg(long, conflicts_with = "verbose")]
    quiet: bool,
    /// Log every fetch attempt
    #[arg(long, conflicts_with = "quiet")]
    verbose: bool,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Gen(args) => {
            if let Err(message) = run_gen(&args) {
                eprintln!("mcp2dekopon: {message}");
                std::process::exit(1);
            }
        }
        Command::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "mcp2dekopon",
                &mut std::io::stdout(),
            );
        }
    }
}

/// Progress output honoring --quiet.
struct Log {
    quiet: bool,
}

impl Log {
    fn info(&self, message: &str) {
        if !self.quiet {
            eprintln!("{message}");
        }
    }

    fn warn(&self, message: &str) {
        eprintln!("{message}");
    }
}

fn run_gen(args: &GenArgs) -> Result<(), String> {
    if args.timeout_secs == 0 {
        return Err("--timeout-secs must be at least 1".to_string());
    }
    let log = Log { quiet: args.quiet };
    let endpoint: Endpoint = fetch::validate_endpoint(&args.url)?;
    let provider_id = names::validate_provider_id(&args.id)?;
    let extra_headers = parse_headers(&args.header)?;

    let (server, listed) = if let Some(path) = &args.from_file {
        if !args.header.is_empty() {
            log.warn("warning: ignoring --header with --from-file (nothing is fetched)");
        }
        log.info(&format!("loading tool list from {} ...", path.display()));
        fetch::SavedTools::load(path)?
    } else {
        log.info(&format!(
            "fetching tools/list from {} ...",
            endpoint.vendor_base
        ));
        fetch::fetch_tools(
            &endpoint.vendor_base,
            &extra_headers,
            args.timeout_secs,
            &args.protocol_version,
            "mcp2dekopon",
            env!("CARGO_PKG_VERSION"),
            args.verbose,
        )
        .map_err(|e| e.to_string())?
    };
    log.info(&format!(
        "server {} {} speaks {}; {} tool(s) listed",
        server.name,
        server.version,
        server.protocol_version,
        listed.len()
    ));
    if server.protocol_version != args.protocol_version {
        log.warn(&format!(
            "warning: server speaks {}, not the offered {}; proceeding",
            server.protocol_version, args.protocol_version
        ));
    }

    if let Some(path) = &args.save_tools {
        fetch::SavedTools::save(path, &server, &listed)?;
        log.info(&format!(
            "saved {} tool(s) to {}; stopping before generation",
            listed.len(),
            path.display()
        ));
        return Ok(());
    }

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
                log.info(&format!(
                    "skipping {:?}: {} ({})",
                    tool.name, skip.reason, skip.detail
                ));
                skipped.push(SkippedTool {
                    tool: tool.name.clone(),
                    reason: skip.reason.to_string(),
                    detail: skip.detail,
                    description: tool.description.clone().unwrap_or_default(),
                });
            }
        }
    }

    let parts = codegen::split_tools(&provider_id, &tightened)?;
    if parts.len() > 1 {
        log.info(&format!(
            "splitting {} tools into {} providers of at most {}",
            tightened.len(),
            parts.len(),
            mcp2dekopon::MAX_CAPABILITIES
        ));
    }

    if args.dry_run {
        log.info(&format!(
            "dry run: {} provider(s), {} capabilities, {} skipped; wrote nothing",
            parts.len(),
            tightened.len(),
            skipped.len()
        ));
        if tightened.is_empty() {
            return Err(format!("no usable tools ({} skipped)", skipped.len()));
        }
        return Ok(());
    }

    std::fs::create_dir_all(&args.out)
        .map_err(|e| format!("cannot create {}: {e}", args.out.display()))?;
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
        let dir = args.out.join(part_dir);
        codegen::write_provider(&dir, &files, args.force)?;
        log.info(&format!(
            "wrote {} ({} capabilities)",
            dir.display(),
            chunk.len()
        ));
        providers.push((
            part_id.clone(),
            part_dir.display().to_string(),
            chunk.to_vec(),
        ));
    }

    let report_text = serde_json::to_string_pretty(&report::build_report(
        &endpoint.vendor_base,
        &server,
        listed.len(),
        providers,
        skipped.clone(),
    ))
    .map_err(|e| format!("cannot encode report: {e}"))?;
    let report_path = args.out.join("report.json");
    std::fs::write(&report_path, format!("{report_text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", report_path.display()))?;

    if tightened.is_empty() {
        return Err(format!(
            "no usable tools ({} skipped); report written to {}",
            skipped.len(),
            report_path.display()
        ));
    }
    log.info(&format!(
        "done: {} provider(s), {} capabilities, {} skipped; report at {}",
        parts.len(),
        tightened.len(),
        skipped.len(),
        report_path.display()
    ));
    log.info("next: cd into a provider, run ./build.sh, then cargo test");
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

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn long_version_tracks_pins() {
        let rendered = Cli::command().render_long_version();
        assert!(rendered.contains(mcp2dekopon::SDK_VERSION), "{rendered}");
        assert!(
            rendered.contains(mcp2dekopon::MCP_PROTOCOL_VERSION),
            "{rendered}"
        );
    }
}
