//! `mcp2dekopon`: generate Dekopon providers from MCP servers.

#![cfg_attr(test, allow(clippy::unwrap_used))]

//!
//! Three stages: [`fetch`] the tool list over MCP Streamable HTTP, [`tighten`]
//! each input schema until the provider SDK accepts it, then [`codegen`] a
//! buildable provider crate plus a starter broker fragment.

pub mod codegen;
pub mod fetch;
pub mod names;
pub mod report;
pub mod rpc;
pub mod tighten;

/// Dekopon provider SDK version pinned in every generated crate.
pub const SDK_VERSION: &str = "0.46.1";

/// MCP protocol version offered to servers (spec 2025-06-18).
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Maximum capabilities per provider; larger servers are split.
pub const MAX_CAPABILITIES: usize = 32;
