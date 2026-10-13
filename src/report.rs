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

/// The full generation report, written to `<out>/report.json`.
#[derive(Clone, Debug, Serialize)]
pub struct GenerationReport {
    pub generator: String,
    pub generator_version: String,
    pub sdk_version: String,
    pub server: ReportServer,
    pub providers: Vec<ReportProvider>,
    pub skipped: Vec<SkippedTool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReportServer {
    pub url: String,
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

pub fn build_report(
    endpoint: &str,
    server: &ServerInfo,
    providers: Vec<(String, String, Vec<TightenedTool>)>,
    skipped: Vec<SkippedTool>,
) -> GenerationReport {
    GenerationReport {
        generator: "mcp2dekopon".to_string(),
        generator_version: env!("CARGO_PKG_VERSION").to_string(),
        sdk_version: crate::SDK_VERSION.to_string(),
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
        skipped,
    }
}
