//! OpenHuman terminal client, embedding the core in-process.
//!
//! A [ratatui]-based agent cockpit with Chat, Logs, Config, and Settings,
//! persistent thread resume, command/file pickers, approvals, plan review,
//! task/goal/agent/skill/MCP/artifact views, Git review, and a multiline composer.
//! Chat uses the **same `web_chat` surface** the desktop app drives (`openhuman.channel_web_chat` /
//! `openhuman.channel_web_cancel` +
//! [`web_chat::subscribe_web_channel_events`](openhuman_core::openhuman::web_chat::subscribe_web_channel_events)).
//! It boots the core in-process — no HTTP, no sockets — via
//! `CoreBuilder::new(HostKind::Cli).domains(DomainSet::full()).services(ServiceSet::none())`
//! and streams a live transcript in the terminal.
//!
//! The terminal dependencies and UI code live entirely in this crate, keeping
//! the shared core crate free of terminal-specific dependencies.

mod app;
mod cockpit;
mod composer;
mod controls;
mod crash_reporting;
mod render;
mod runner;
mod state;
mod terminal;
mod ui_state;

pub use crash_reporting::init_crash_reporting;
pub use runner::run_from_cli;

// State reducer is behaviour-only but has no terminal deps, so its tests run in
// feature-on builds. Exported for the sibling submodules + tests.
pub use state::{Entry, EntryKind, TranscriptState};
