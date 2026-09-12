//! Core library for the OpenHuman platform.
//!
//! This crate provides the central logic for the OpenHuman core binary, including:
//! - API and RPC handlers for external interactions.
//! - Core system services (CLI, configuration, monitoring).
//! - Domain-specific logic for the OpenHuman agent runtime.

// The RPC dispatch chokepoint wraps each handler future in an ambient
// `CoreContext` scope (Phase 2). Combined with the already very deep async type
// stacks in the axum routes that fan out into the tinyagents harness, the extra
// future layer pushes the compiler's `Send` auto-trait solver past the default
// depth of 128 (E0275). Raising the limit is the standard remedy for deep async
// type recursion and costs nothing at runtime.
#![recursion_limit = "256"]
// These modules define the public API surface for agent features.
// Many types/functions are intended for future use or integration with the frontend.
#![allow(dead_code)]

pub mod agent;
pub mod api;
pub mod channels;
pub mod config;
pub mod core;
pub mod cron;
pub mod desktop;
#[cfg(feature = "flows")]
pub mod flows;
pub mod hooks;
pub mod hosted;
#[cfg(feature = "hosting")]
pub mod hosting;
#[cfg(feature = "http-server")]
pub mod http_host;
pub mod inference;
pub mod integrations;
pub mod json_schema;
pub mod mcp;
#[cfg(feature = "media")]
pub mod media;
pub mod medulla;
pub mod memory;
#[cfg(feature = "modules")]
pub mod modules;
pub mod platform;
pub use openhuman_rpc as rpc;
pub mod runtime;
pub mod sandbox;
pub mod search;
pub mod security;
pub mod skills;
#[cfg(feature = "e2e-test-support")]
pub mod test_support;
pub mod threads;
pub mod tools;
pub mod util;
pub mod voice;
pub mod web3;
pub mod web_chat;

pub use config::DaemonConfig;

/// Embeddable core composition API. Host the OpenHuman core in any process —
/// the Tauri shell, a CLI, a stdio MCP server, or a cloud/team server — via
/// [`CoreBuilder`] → [`CoreRuntime`]. See `docs/plans/pluggable-core/`.
pub use core::runtime::{CoreBuilder, CoreRuntime, DomainSet, ServiceSet, TokenSource};
pub use core::types::HostKind;

/// Runs the core logic based on the provided command-line arguments.
///
/// This is the primary entry point for the OpenHuman binary, delegating to the
/// CLI module for argument parsing and command dispatch.
///
/// # Arguments
///
/// * `args` - A slice of strings containing the command-line arguments.
///
/// # Errors
///
/// Returns an error if command execution fails.
pub fn run_core_from_args(args: &[String]) -> anyhow::Result<()> {
    core::cli::load_dotenv_for_cli()?;
    platform::service::apply_startup_restart_delay_from_env();
    security::keyring::init_master_key();
    core::cli::run_from_cli_args(args)
}
