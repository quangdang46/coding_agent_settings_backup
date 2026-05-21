//! `casb` — coding_agent_settings_backup library crate.
//!
//! A Rust port of the `asb` bash tool that backs up AI coding agent
//! configuration folders to git-versioned repositories. The library exposes
//! the full command set as Rust functions; the binary is a thin clap wrapper.

#![deny(missing_docs)]

pub mod agent;
pub mod backup;
pub mod cli;
pub mod commands;
pub mod completion;
pub mod config;
pub mod diff;
pub mod discover;
pub mod doctor;
pub mod error;
pub mod export;
pub mod filter;
pub mod git;
pub mod history;
pub mod hooks;
pub mod output;
pub mod restore;
pub mod schedule;
pub mod stats;
pub mod sync;
pub mod tag;
pub mod toon;
pub mod util;
pub mod verify;

pub use error::{CasbError, Result};

/// The crate version, derived from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Tool name used in command output.
pub const TOOL_NAME: &str = "casb";
