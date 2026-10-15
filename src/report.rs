//! The generation report: every capability label and every skip with a reason.

use serde::Serialize;

use crate::fetch::ServerInfo;
use crate::tighten::TightenedTool;

/// One skipped tool. Skips are reported, never silent.
#[derive(Clone, Debug, Serialize)]
pub struct SkippedTool {
    pub tool: String,
    pub reason: String,
    pub detail: String,
    pub description: String,
}

/// One generated capability as recorded in the report.
#[derive(Clone, Debug, Serialize)]
pub struct ReportCapability {
    pub name: String,
    pub tool: String,
    pub effect: String,
    pub risk: String,
    pub flags: Vec<String>,
    pub warnings: Vec<String>,
}

impl ReportCapability {
    fn from_tool(tool: &TightenedTool) -> Self {
        Self {
            name: tool.cap_name.clone(),
            tool: tool.mcp_name.clone(),
            effect: tool.effect.wire().to_string(),
            risk: tool.risk.wire().to_string(),
            flags: tool.flags.iter().cloned().collect(),
            warnings: tool.warnings.iter().cloned().collect(),
        }
    }
}

/// One generated provider as recorded in the report.
#[derive(Clone, Debug, Serialize)]
pub struct ReportProvider {
    pub id: String,
    pub dir: String,
    pub command_word: String,
    pub capabilities: Vec<ReportCapability>,
}

/// Totals across the whole run, so operators can audit a report at a glance.
#[derive(Clone, Debug, Serialize)]
pub struct ReportSummary {
    pub listed: usize,
    pub generated: usize,
    pub skipped: usize,
    pub providers: usize,
}

/// One distinct tighten warning, aggregated over every capability that raised it.
#[derive(Clone, Debug, Serialize)]
pub struct ReportWarning {
    pub code: String,
    pub count: usize,
    pub capabilities: Vec<String>,
}

/// The full generation report, written to `<out>/report.json`.
#[derive(Clone, Debug, Serialize)]
pub struct GenerationReport {
    pub format_version: u32,
    pub generator: String,
    pub generator_version: String,
    pub sdk_version: String,
    pub generated_at: String,
    pub elapsed_ms: u64,
    pub options: ReportOptions,
    pub summary: ReportSummary,
    pub server: ReportServer,
    pub providers: Vec<ReportProvider>,
    pub warnings: Vec<ReportWarning>,
    pub skipped: Vec<SkippedTool>,
}

/// The run options behind a report, so a report is reproducible: the same
/// options against the same tool list render the same report.
#[derive(Clone, Debug, Serialize)]
pub struct ReportOptions {
    pub offered_protocol_version: String,
    pub timeout_secs: u64,
    pub max_attempts: u32,
    pub max_tools: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReportServer {
    pub url: String,
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

/// Unix seconds rendered as RFC 3339 UTC (`1970-01-01T00:00:00Z`), without a
/// date dependency. Civil-date conversion is Howard Hinnant's algorithm.
pub fn unix_to_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    unix_to_rfc3339(secs)
}

/// Report schema version; bumped when fields are added or removed.
pub const REPORT_FORMAT_VERSION: u32 = 1;

pub fn build_report(
    endpoint: &str,
    server: &ServerInfo,
    listed: usize,
    providers: Vec<(String, String, Vec<TightenedTool>)>,
    skipped: Vec<SkippedTool>,
    options: ReportOptions,
    elapsed_ms: u64,
) -> GenerationReport {
    let mut by_code: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut generated = 0;
    for (id, _, tools) in &providers {
        for tool in tools {
            generated += 1;
            for warning in &tool.warnings {
                by_code
                    .entry(warning.clone())
                    .or_default()
                    .push(format!("{id}.{}", tool.cap_name));
            }
        }
    }
    GenerationReport {
        format_version: REPORT_FORMAT_VERSION,
        generator: "mcp2dekopon".to_string(),
        generator_version: env!("CARGO_PKG_VERSION").to_string(),
        sdk_version: crate::SDK_VERSION.to_string(),
        generated_at: now_rfc3339(),
        elapsed_ms,
        options,
        summary: ReportSummary {
            listed,
            generated,
            skipped: skipped.len(),
            providers: providers.len(),
        },
        server: ReportServer {
            url: endpoint.to_string(),
            name: server.name.clone(),
            version: server.version.clone(),
            protocol_version: server.protocol_version.clone(),
        },
        providers: providers
            .into_iter()
            .map(|(id, dir, tools)| ReportProvider {
                command_word: id.clone(),
                id,
                dir,
                capabilities: tools.iter().map(ReportCapability::from_tool).collect(),
            })
            .collect(),
        warnings: by_code
            .into_iter()
            .map(|(code, mut capabilities)| {
                capabilities.sort();
                capabilities.dedup();
                let count = capabilities.len();
                ReportWarning {
                    code,
                    count,
                    capabilities,
                }
            })
            .collect(),
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::{unix_to_rfc3339, ReportOptions, REPORT_FORMAT_VERSION};
    use crate::fetch::ServerInfo;

    #[test]
    fn epoch_and_known_dates_render_as_rfc3339() {
        assert_eq!(unix_to_rfc3339(0), "1970-01-01T00:00:00Z");
        // `date -u -d @1700000000` ground truth.
        assert_eq!(unix_to_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        // Leap day, `date -u -d @951827696` ground truth.
        assert_eq!(unix_to_rfc3339(951_827_696), "2000-02-29T12:34:56Z");
    }

    #[test]
    fn reports_carry_version_options_and_elapsed() {
        let server = ServerInfo {
            name: "mock".to_string(),
            version: "0".to_string(),
            protocol_version: "2025-06-18".to_string(),
        };
        let report = super::build_report(
            "http://127.0.0.1:1/mcp",
            &server,
            0,
            Vec::new(),
            Vec::new(),
            ReportOptions {
                offered_protocol_version: "2025-06-18".to_string(),
                timeout_secs: 7,
                max_attempts: 2,
                max_tools: 50,
            },
            123,
        );
        assert_eq!(report.format_version, REPORT_FORMAT_VERSION);
        assert_eq!(report.format_version, 1);
        assert_eq!(report.elapsed_ms, 123);
        assert_eq!(report.options.offered_protocol_version, "2025-06-18");
        assert_eq!(report.options.timeout_secs, 7);
        assert_eq!(report.options.max_attempts, 2);
        assert_eq!(report.options.max_tools, 50);
        let text = serde_json::to_string(&report).unwrap();
        for key in [
            "format_version",
            "elapsed_ms",
            "options",
            "offered_protocol_version",
            "max_attempts",
            "max_tools",
        ] {
            assert!(text.contains(key), "{text}");
        }
    }
}
