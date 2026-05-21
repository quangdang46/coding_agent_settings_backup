//! Binary entry point for `casb`.
//!
//! Parses CLI arguments, configures tracing, then dispatches to
//! [`casb::commands::dispatch`].

use casb::cli::Cli;
use clap::Parser;
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let cli = Cli::parse();

    init_tracing(cli.verbose);

    match casb::commands::dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(casb::CasbError::Cancelled) => {
            eprintln!("cancelled");
            ExitCode::from(1)
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

/// Initialise the `tracing` subscriber.
fn init_tracing(verbose: bool) {
    let default = if verbose { "debug" } else { "warn" };
    let filter = EnvFilter::try_from_env("CASB_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .try_init();
}
