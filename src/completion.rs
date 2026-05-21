//! Shell completion script generation.
//!
//! Wraps `clap_complete` to render completion scripts for bash, zsh, and
//! fish from the [`crate::cli::Cli`] derive definition.

use crate::cli::Cli;
use crate::error::Result;
use clap::CommandFactory;
use clap_complete::{generate, Shell};
use std::io::Write;

/// Generate a completion script for the supplied shell into `writer`.
pub fn generate_completion(shell: Shell, writer: &mut dyn Write) -> Result<()> {
    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();
    generate(shell, &mut cmd, bin_name, writer);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_bash_completion() {
        let mut buf = Vec::new();
        generate_completion(Shell::Bash, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("casb"));
    }

    #[test]
    fn renders_zsh_completion() {
        let mut buf = Vec::new();
        generate_completion(Shell::Zsh, &mut buf).unwrap();
        assert!(!buf.is_empty());
    }

    #[test]
    fn renders_fish_completion() {
        let mut buf = Vec::new();
        generate_completion(Shell::Fish, &mut buf).unwrap();
        assert!(!buf.is_empty());
    }
}
