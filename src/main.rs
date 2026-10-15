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
    /// Fetch attempts per request before giving up
    #[arg(long, default_value_t = mcp2dekopon::fetch::DEFAULT_MAX_ATTEMPTS)]
    retries: u32,
    /// Most tools accepted from tools/list before refusing
    #[arg(long, default_value_t = mcp2dekopon::fetch::DEFAULT_MAX_TOOLS)]
    max_tools: usize,
    /// Overwrite existing provider directories, replacing them entirely
    #[arg(long)]
    force: bool,
    /// MCP protocol version to offer
    #[arg(long, default_value = mcp2dekopon::MCP_PROTOCOL_VERSION)]
    protocol_version: String,
    /// Fetch and tighten, then print the summary without writing anything
    #[arg(long, conflicts_with = "save_tools")]
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
    if args.retries == 0 {
        return Err("--retries must be at least 1".to_string());
    }
    if args.max_tools == 0 {
        return Err("--max-tools must be at least 1".to_string());
    }
    if args.protocol_version.is_empty() {
        return Err("--protocol-version must not be empty".to_string());
    }
    let started = std::time::Instant::now();
    let log = Log { quiet: args.quiet };
    let endpoint: Endpoint = fetch::validate_endpoint(&args.url)?;
    let provider_id =
        names::validate_provider_id(&args.id).map_err(
            |message| match names::suggest_provider_id(&args.id) {
                Some(suggestion) => format!("{message}; did you mean {suggestion:?}?"),
                None => message,
            },
        )?;
    if let Some(note) = &endpoint.plaintext_note {
        log.warn(&format!("warning: {note}"));
    }
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
        fetch::fetch_tools(&fetch::FetchOptions {
            endpoint: &endpoint.vendor_base,
            extra_headers: &extra_headers,
            timeout_secs: args.timeout_secs,
            protocol_version: &args.protocol_version,
            client_name: "mcp2dekopon",
            client_version: env!("CARGO_PKG_VERSION"),
            verbose: args.verbose,
            max_attempts: args.retries,
            max_tools: args.max_tools,
        })
        .map_err(|e| e.to_string())?
    };
    log.info(&format!(
        "server {} {} speaks {}; {} tool(s) listed",
        server.name,
        server.version,
        server.protocol_version,
        listed.len()
    ));
    // Nothing is negotiated from a capture, so a replay that offers a
    // different version than the capture holds is not worth warning about.
    if server.protocol_version != args.protocol_version && args.from_file.is_none() {
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
    // The name budget accounts for the longest split suffix (`-N`) the run
    // could need, so `provider.name` fits even after a split.
    let max_providers = listed.len().div_ceil(mcp2dekopon::MAX_CAPABILITIES).max(1);
    let suffix_len = if max_providers > 1 {
        1 + max_providers.to_string().len()
    } else {
        0
    };
    let max_name_len = mcp2dekopon::names::MAX_IDENTIFIER_LEN
        .saturating_sub(provider_id.len())
        .saturating_sub(suffix_len)
        .saturating_sub(1);
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
                    description: mcp2dekopon::rpc::truncate_chars(
                        tool.description.as_deref().unwrap_or_default(),
                        1000,
                    ),
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
            timeout_secs: args.timeout_secs,
        };
        let files = codegen::render_provider(&spec);
        let dir = args.out.join(part_dir);
        codegen::write_provider(&dir, &files, args.force)?;
        log.info(&format!(
            "wrote {} ({} capabilities)",
            dir.display(),
            chunk.len()
        ));
        providers.push((part_id.clone(), dir.display().to_string(), chunk.to_vec()));
    }

    let report_text = serde_json::to_string_pretty(&report::build_report(
        &endpoint.vendor_base,
        &server,
        listed.len(),
        providers,
        skipped.clone(),
        report::ReportOptions {
            offered_protocol_version: args.protocol_version.clone(),
            timeout_secs: args.timeout_secs,
            max_attempts: args.retries,
            max_tools: args.max_tools,
        },
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    ))
    .map_err(|e| format!("cannot encode report: {e}"))?;
    let report_path = args.out.join("report.json");
    codegen::write_file_atomic(&report_path, format!("{report_text}\n"))?;

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
    // Headers framing the MCP exchange itself cannot be overridden: a wrong
    // content-type or session id would break the fetch in confusing ways.
    const RESERVED: [&str; 7] = [
        "content-type",
        "content-length",
        "accept",
        "host",
        "user-agent",
        "mcp-session-id",
        "mcp-protocol-version",
    ];
    let mut parsed = Vec::with_capacity(headers.len());
    for header in headers {
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| format!("invalid --header {header:?}: want NAME:VALUE"))?;
        let name = name.trim();
        if !is_header_token(name) {
            return Err(format!(
                "invalid --header {header:?}: the name must be an HTTP token (visible ASCII, no separators)"
            ));
        }
        if RESERVED.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!(
                "invalid --header {header:?}: {name:?} is reserved by the MCP exchange"
            ));
        }
        let value = value.trim();
        if !value
            .bytes()
            .all(|b| b == b'\t' || (b >= 0x20 && b != 0x7f))
        {
            return Err(format!(
                "invalid --header {header:?}: the value must not hold control characters"
            ));
        }
        parsed.push((name.to_string(), value.to_string()));
    }
    Ok(parsed)
}

/// Whether a header name is an RFC 7230 token.
fn is_header_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
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

    #[test]
    fn headers_parse_and_trim() {
        let parsed = super::parse_headers(&["X-Test-Auth: test-token-1".to_string()]).unwrap();
        assert_eq!(
            parsed,
            vec![("X-Test-Auth".to_string(), "test-token-1".to_string())]
        );
        // Surrounding whitespace is not part of the header.
        let parsed = super::parse_headers(&["  X-Token :  abc  ".to_string()]).unwrap();
        assert_eq!(parsed, vec![("X-Token".to_string(), "abc".to_string())]);
        // Empty values are legal HTTP.
        let parsed = super::parse_headers(&["X-Empty:".to_string()]).unwrap();
        assert_eq!(parsed, vec![("X-Empty".to_string(), String::new())]);
    }

    #[test]
    fn headers_reject_malformed_reserved_and_controlled() {
        for bad in [
            "no-colon-here",
            ": missing-name",
            "has space: x",
            "paren(thesis): x",
            "content-type: application/xml",
            "MCP-Session-Id: abc",
            "User-Agent: evil",
            "X-Bad: line\nbreak",
        ] {
            assert!(
                super::parse_headers(&[bad.to_string()]).is_err(),
                "{bad:?} parsed"
            );
        }
        // ... but tabs are legal in values.
        assert!(super::parse_headers(&["X-Tab: a\tb".to_string()]).is_ok());
    }
}
