//! CLI argument definitions using `clap` derive macros.
//!
//! The CLI mirrors the `asb` bash script command surface with several
//! additions (multi-location agents, `doctor`, structured output formats).

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

/// Output format selector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable text (default).
    #[default]
    Text,
    /// Machine-readable JSON.
    Json,
    /// Tabular Optimized Object Notation.
    Toon,
}

/// Top-level CLI parser.
#[derive(Debug, Parser)]
#[command(
    name = "casb",
    version,
    about = "Backup and restore AI coding agent configuration folders",
    long_about = None,
    propagate_version = true,
)]
pub struct Cli {
    /// Show what would happen without making changes.
    #[arg(short = 'n', long, global = true)]
    pub dry_run: bool,

    /// Skip confirmation prompts.
    #[arg(short = 'f', long, global = true)]
    pub force: bool,

    /// Show detailed output.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Suppress non-error output.
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Output format.
    #[arg(long, value_enum, global = true)]
    pub format: Option<OutputFormat>,

    /// Override config file path.
    #[arg(long, global = true)]
    pub config: Option<std::path::PathBuf>,

    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize the backup root directory.
    Init {
        /// Override the backup root path.
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },

    /// Backup one or more agents (all installed if none specified).
    Backup {
        /// Agent keys to back up.
        agents: Vec<String>,

        /// Custom commit message.
        #[arg(short = 'm', long)]
        message: Option<String>,

        /// Run multiple agent backups in parallel.
        #[arg(long)]
        parallel: bool,
    },

    /// Restore an agent's settings from a backup.
    Restore {
        /// Agent key to restore.
        agent: String,

        /// Optional commit hash or tag to restore from (defaults to HEAD).
        reference: Option<String>,
    },

    /// Export an agent backup to a tar.gz archive (use `-` for stdout).
    Export {
        /// Agent key to export.
        agent: String,

        /// Destination archive path or `-` for stdout.
        file: Option<String>,
    },

    /// Import an agent backup from a tar.gz archive (use `-` for stdin).
    Import {
        /// Source archive path or `-` for stdin.
        file: Option<String>,
    },

    /// List all agents and their backup status.
    List,

    /// Show backup history for an agent.
    History {
        /// Agent key.
        agent: String,

        /// Maximum number of commits to show.
        #[arg(short = 'l', long, default_value_t = 20)]
        limit: usize,
    },

    /// Show changes since the last backup for an agent.
    Diff {
        /// Agent key.
        agent: String,
    },

    /// Verify backup integrity for one or more agents.
    Verify {
        /// Agent keys to verify (all if empty).
        agents: Vec<String>,
    },

    /// Manage backup tags.
    Tag {
        /// Subcommand.
        #[command(subcommand)]
        action: TagAction,
    },

    /// Show backup statistics.
    Stats {
        /// Optional agent key (aggregate if omitted).
        agent: Option<String>,
    },

    /// Discover newly installed AI agents on this system.
    Discover {
        /// Print results without prompting to add anything.
        #[arg(long)]
        list_only: bool,
    },

    /// Manage automated backup schedules.
    Schedule {
        /// Subcommand.
        #[command(subcommand)]
        action: ScheduleAction,
    },

    /// Manage hooks (pre/post backup/restore scripts).
    Hooks {
        /// Subcommand.
        #[command(subcommand)]
        action: HooksAction,
    },

    /// Run health checks against the casb installation.
    Doctor,

    /// Manage configuration values.
    Config {
        /// Subcommand.
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Generate shell completion scripts.
    Completion {
        /// Target shell.
        shell: Shell,
    },

    /// Print version information.
    Version,
}

/// `casb tag` actions.
#[derive(Debug, Subcommand)]
pub enum TagAction {
    /// Create a tag pointing to the most recent backup commit.
    Create {
        /// Agent key.
        agent: String,
        /// Tag name.
        name: String,
        /// Optional tag message.
        #[arg(short = 'm', long)]
        message: Option<String>,
    },
    /// List tags for an agent.
    List {
        /// Agent key.
        agent: String,
    },
    /// Delete a tag.
    Delete {
        /// Agent key.
        agent: String,
        /// Tag name.
        name: String,
    },
    /// Restore an agent from a tag (preview + confirm).
    Restore {
        /// Agent key.
        agent: String,
        /// Tag name.
        name: String,
    },
}

/// `casb schedule` actions.
#[derive(Debug, Subcommand)]
pub enum ScheduleAction {
    /// Show current schedule status.
    Status,
    /// Install a schedule.
    Install {
        /// Schedule interval (`hourly` | `daily` | `weekly`).
        #[arg(default_value = "daily")]
        interval: String,
        /// Backend (`systemd` | `cron` | `taskscheduler`).
        /// Auto-detected if not specified.
        #[arg(long)]
        method: Option<String>,
    },
    /// Remove the active schedule.
    Remove,
}

/// `casb hooks` actions.
#[derive(Debug, Subcommand)]
pub enum HooksAction {
    /// List configured hook scripts.
    List,
    /// Print the hooks directory path.
    Path,
}

/// `casb config` actions.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the entire effective configuration.
    Show,
    /// Print the resolved config file path.
    Path,
    /// Get a single config key (dotted path).
    Get {
        /// Dotted key (e.g. `general.backup_root`).
        key: String,
    },
    /// Set a config key (dotted path) to a string value.
    Set {
        /// Dotted key.
        key: String,
        /// New value (string).
        value: String,
    },
    /// Write the default config file if missing.
    Init,
}

impl Cli {
    /// Resolve the effective output format from `--json` and `--format` flags.
    pub fn effective_format(&self) -> OutputFormat {
        if let Some(fmt) = self.format {
            return fmt;
        }
        if self.json {
            return OutputFormat::Json;
        }
        OutputFormat::Text
    }
}
