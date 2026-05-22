//! Central CLI command dispatcher.
//!
//! Translates a parsed [`crate::cli::Cli`] into calls on the various
//! feature modules and produces output via [`crate::output::Printer`].

use crate::agent::Registry;
use crate::backup;
use crate::cli::{Cli, Command, ConfigAction, HooksAction, ScheduleAction, TagAction};
use crate::config::Config;
use crate::diff;
use crate::discover;
use crate::doctor;
use crate::error::{CasbError, Result};
use crate::export;
use crate::history;
use crate::hooks;
use crate::output::Printer;
use crate::restore;
use crate::schedule;
use crate::stats;
use crate::tag;
use crate::util::format_bytes;
use crate::verify;
use serde::Serialize;

/// Entry point used by `main`. Loads config, builds registry, then routes to
/// the appropriate command.
pub fn dispatch(cli: Cli) -> Result<()> {
    if matches!(cli.command, Command::Version) {
        println!("{} {}", crate::TOOL_NAME, crate::VERSION);
        return Ok(());
    }

    let cfg = match &cli.config {
        Some(p) => Config::load_from(Some(p))?,
        None => Config::load()?,
    };
    let registry = Registry::from_config(&cfg)?;
    let format = cli.effective_format();
    let mut printer = Printer::new(format, cli.quiet, cli.verbose);

    match cli.command {
        Command::Init { root } => cmd_init(&cfg, &mut printer, root.as_deref()),
        Command::Backup {
            agents,
            message,
            parallel,
        } => cmd_backup(
            &cfg,
            &registry,
            &mut printer,
            &agents,
            message.as_deref(),
            parallel,
            cli.dry_run,
        ),
        Command::Restore { agent, reference } => cmd_restore(
            &cfg,
            &registry,
            &mut printer,
            &agent,
            reference.as_deref(),
            cli.force,
            cli.dry_run,
        ),
        Command::Export { agent, file } => {
            cmd_export(&cfg, &registry, &mut printer, &agent, file.as_deref())
        }
        Command::Import { file } => cmd_import(&cfg, &mut printer, file.as_deref()),
        Command::List => cmd_list(&cfg, &registry, &mut printer),
        Command::History { agent, limit } => {
            cmd_history(&cfg, &registry, &mut printer, &agent, limit)
        }
        Command::Diff { agent } => cmd_diff(&cfg, &registry, &mut printer, &agent),
        Command::Verify { agents } => cmd_verify(&cfg, &registry, &mut printer, &agents),
        Command::Tag { action } => cmd_tag(
            &cfg,
            &registry,
            &mut printer,
            action,
            cli.force,
            cli.dry_run,
        ),
        Command::Stats { agent } => cmd_stats(&cfg, &registry, &mut printer, agent.as_deref()),
        Command::Discover { list_only } => cmd_discover(&registry, &mut printer, list_only),
        Command::Schedule { action } => cmd_schedule(&mut printer, action),
        Command::Hooks { action } => cmd_hooks(&mut printer, action),
        Command::Doctor => cmd_doctor(&cfg, &registry, &mut printer),
        Command::Config { action } => cmd_config(cfg, cli.config.clone(), &mut printer, action),
        Command::Completion { shell } => cmd_completion(&mut printer, shell),
        Command::Version => {
            // Already handled above, but keep exhaustive match.
            Ok(())
        }
    }
}

#[derive(Serialize)]
struct Envelope<T: Serialize> {
    ok: bool,
    command: &'static str,
    data: T,
}

fn emit_data<T: Serialize>(p: &mut Printer, command: &'static str, data: T) -> Result<()> {
    let env = Envelope {
        ok: true,
        command,
        data,
    };
    p.emit(&env)
}

// ----- init -----

fn cmd_init(cfg: &Config, p: &mut Printer, root: Option<&std::path::Path>) -> Result<()> {
    let root = backup::init_backup_root(cfg, root)?;
    p.text_line(&format!("initialised backup root at {}", root.display()))?;
    emit_data(
        p,
        "init",
        serde_json::json!({"backup_root": root.display().to_string()}),
    )
}

// ----- backup -----

fn cmd_backup(
    cfg: &Config,
    registry: &Registry,
    p: &mut Printer,
    keys: &[String],
    message: Option<&str>,
    parallel: bool,
    dry_run: bool,
) -> Result<()> {
    let outcomes = backup::backup_agents(cfg, registry, keys, message, parallel, dry_run)?;
    if outcomes.is_empty() {
        p.text_line("no installed agents to back up")?;
    } else {
        for o in &outcomes {
            p.text_line(&backup::format_outcome_text(o))?;
        }
    }
    // Surface per-agent errors as a non-zero exit so CI / systemd timers /
    // hook chains can detect them. The per-agent error messages have already
    // been printed above; the envelope still carries the full outcome list.
    let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
    emit_data(p, "backup", &outcomes)?;
    if failed > 0 {
        return Err(CasbError::BackupFailed { count: failed });
    }
    Ok(())
}

// ----- restore -----

fn cmd_restore(
    cfg: &Config,
    registry: &Registry,
    p: &mut Printer,
    key: &str,
    reference: Option<&str>,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let agent = registry.get(key)?;
    let (preview, outcome) = restore::restore_agent(cfg, agent, reference, force, dry_run)?;
    print_preview(p, &preview)?;
    if outcome.applied {
        p.text_line(&format!(
            "restored {} from {} ({} files)",
            outcome.agent, outcome.reference, outcome.sync.files_copied
        ))?;
    } else if dry_run {
        p.text_line("dry-run: no changes applied")?;
    }
    emit_data(
        p,
        "restore",
        serde_json::json!({"preview": preview, "outcome": outcome}),
    )
}

fn print_preview(p: &mut Printer, preview: &restore::RestorePreview) -> Result<()> {
    if preview.total() == 0 {
        p.text_line("no differences vs backup")?;
        return Ok(());
    }
    p.text_line(&format!(
        "{} added, {} removed, {} modified",
        preview.added.len(),
        preview.removed.len(),
        preview.modified.len()
    ))?;
    for path in &preview.added {
        p.verbose_line(&format!("  + {path}"))?;
    }
    for path in &preview.removed {
        p.verbose_line(&format!("  - {path}"))?;
    }
    for path in &preview.modified {
        p.verbose_line(&format!("  ~ {path}"))?;
    }
    Ok(())
}

// ----- export / import -----

fn cmd_export(
    cfg: &Config,
    registry: &Registry,
    p: &mut Printer,
    key: &str,
    dest: Option<&str>,
) -> Result<()> {
    let path = export::export_agent(cfg, registry, key, dest)?;
    if let Some(path) = &path {
        p.text_line(&format!("exported {key} to {}", path.display()))?;
        emit_data(
            p,
            "export",
            serde_json::json!({"path": path.display().to_string()}),
        )?;
    } else {
        // Stdout export — no log line, no JSON envelope (raw bytes already
        // streamed to stdout in `export_agent`).
    }
    Ok(())
}

fn cmd_import(cfg: &Config, p: &mut Printer, src: Option<&str>) -> Result<()> {
    let dest = export::import_archive(cfg, src)?;
    p.text_line(&format!("imported into {}", dest.display()))?;
    emit_data(
        p,
        "import",
        serde_json::json!({"backup_root": dest.display().to_string()}),
    )
}

// ----- list -----

#[derive(Serialize)]
struct ListItem {
    key: String,
    display_name: String,
    installed: bool,
    has_backup: bool,
    last_commit: Option<String>,
    locations: Vec<String>,
}

fn cmd_list(cfg: &Config, registry: &Registry, p: &mut Printer) -> Result<()> {
    let mut items = Vec::new();
    for agent in registry.all() {
        let repo_path = backup::agent_repo_path(&cfg.backup_root(), agent);
        let repo = crate::git::Repo::new(&repo_path);
        let has_backup = repo.exists();
        let last = if has_backup { repo.head_short()? } else { None };
        items.push(ListItem {
            key: agent.key.clone(),
            display_name: agent.display_name.clone(),
            installed: agent.is_installed(),
            has_backup,
            last_commit: last,
            locations: agent
                .locations
                .iter()
                .map(|l| l.path.display().to_string())
                .collect(),
        });
    }
    p.heading(&format!("{} agents", items.len()))?;
    for item in &items {
        let mark = if item.installed { "✓" } else { "•" };
        let backup = if item.has_backup {
            item.last_commit.clone().unwrap_or_else(|| "?".into())
        } else {
            "no backup".into()
        };
        p.text_line(&format!(
            "{} {} ({}) — {}",
            mark, item.key, item.display_name, backup
        ))?;
    }
    emit_data(p, "list", &items)
}

// ----- history -----

fn cmd_history(
    cfg: &Config,
    registry: &Registry,
    p: &mut Printer,
    key: &str,
    limit: usize,
) -> Result<()> {
    let agent = registry.get(key)?;
    let h = history::agent_history(cfg, agent, limit)?;
    if h.entries.is_empty() {
        p.text_line(&format!("no commits in {} repo", h.agent))?;
    } else {
        for e in &h.entries {
            let tags = if e.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", e.tags.join(","))
            };
            p.text_line(&format!(
                "{} {} {}{tags} — {}",
                e.short_hash, e.date, e.subject, e.author
            ))?;
        }
    }
    emit_data(p, "history", &h)
}

// ----- diff -----

fn cmd_diff(cfg: &Config, registry: &Registry, p: &mut Printer, key: &str) -> Result<()> {
    let agent = registry.get(key)?;
    let preview = diff::diff_since_last_backup(cfg, agent)?;
    if preview.total() == 0 {
        p.text_line(&format!("{}: no changes since last backup", agent.key))?;
    } else {
        p.text_line(&format!(
            "{}: {} added, {} removed, {} modified",
            agent.key,
            preview.added.len(),
            preview.removed.len(),
            preview.modified.len()
        ))?;
        for path in &preview.added {
            p.text_line(&format!("  + {path}"))?;
        }
        for path in &preview.removed {
            p.text_line(&format!("  - {path}"))?;
        }
        for path in &preview.modified {
            p.text_line(&format!("  ~ {path}"))?;
        }
    }
    emit_data(p, "diff", &preview)
}

// ----- verify -----

fn cmd_verify(cfg: &Config, registry: &Registry, p: &mut Printer, keys: &[String]) -> Result<()> {
    let report = verify::verify_agents(cfg, registry, keys)?;
    for entry in &report.agents {
        let mark = if entry.ok { "✓" } else { "✗" };
        let suffix = if !entry.repo_exists {
            "no backup"
        } else if entry.errors.is_empty() && entry.warnings.is_empty() {
            "clean"
        } else if entry.errors.is_empty() {
            "warnings"
        } else {
            "errors"
        };
        p.text_line(&format!("{mark} {} — {suffix}", entry.agent))?;
    }
    emit_data(p, "verify", &report)?;
    if !report.all_ok {
        return Err(CasbError::other("one or more verify checks failed"));
    }
    Ok(())
}

// ----- tag -----

fn cmd_tag(
    cfg: &Config,
    registry: &Registry,
    p: &mut Printer,
    action: TagAction,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    match action {
        TagAction::Create {
            agent,
            name,
            message,
        } => {
            let a = registry.get(&agent)?;
            if !dry_run {
                tag::create_tag(cfg, a, &name, message.as_deref())?;
            }
            p.text_line(&format!("tagged {agent} as {name}"))?;
            emit_data(
                p,
                "tag-create",
                serde_json::json!({"agent": agent, "name": name}),
            )
        }
        TagAction::List { agent } => {
            let a = registry.get(&agent)?;
            let tags = tag::list_tags(cfg, a)?;
            if tags.is_empty() {
                p.text_line(&format!("no tags for {agent}"))?;
            } else {
                for t in &tags {
                    p.text_line(&format!(
                        "{} -> {}",
                        t.name,
                        &t.commit[..t.commit.len().min(7)]
                    ))?;
                }
            }
            emit_data(p, "tag-list", &tags)
        }
        TagAction::Delete { agent, name } => {
            let a = registry.get(&agent)?;
            if !dry_run {
                tag::delete_tag(cfg, a, &name)?;
            }
            p.text_line(&format!("deleted tag {name} from {agent}"))?;
            emit_data(
                p,
                "tag-delete",
                serde_json::json!({"agent": agent, "name": name}),
            )
        }
        TagAction::Restore { agent, name } => {
            let a = registry.get(&agent)?;
            let (preview, outcome) = restore::restore_agent(cfg, a, Some(&name), force, dry_run)?;
            print_preview(p, &preview)?;
            if outcome.applied {
                p.text_line(&format!(
                    "restored {} from tag {name} ({} files)",
                    outcome.agent, outcome.sync.files_copied
                ))?;
            }
            emit_data(
                p,
                "tag-restore",
                serde_json::json!({"preview": preview, "outcome": outcome}),
            )
        }
    }
}

// ----- stats -----

fn cmd_stats(cfg: &Config, registry: &Registry, p: &mut Printer, key: Option<&str>) -> Result<()> {
    let s = stats::compute_stats(cfg, registry, key)?;
    for entry in &s.agents {
        if !entry.repo_exists && !entry.installed {
            continue;
        }
        p.text_line(&format!(
            "{}: commits={} repo={} source={}",
            entry.agent,
            entry.commits,
            format_bytes(entry.repo_bytes),
            format_bytes(entry.source_bytes),
        ))?;
    }
    p.text_line(&format!(
        "total: commits={} repo={} source={}",
        s.total_commits,
        format_bytes(s.total_repo_bytes),
        format_bytes(s.total_source_bytes),
    ))?;
    emit_data(p, "stats", &s)
}

// ----- discover -----

fn cmd_discover(registry: &Registry, p: &mut Printer, _list_only: bool) -> Result<()> {
    let found = discover::discover(registry)?;
    if found.is_empty() {
        p.text_line("no candidate agents found")?;
    } else {
        for f in &found {
            p.text_line(&format!("{} → {} ({})", f.key, f.path.display(), f.reason))?;
        }
    }
    emit_data(p, "discover", &found)
}

// ----- schedule -----

fn cmd_schedule(p: &mut Printer, action: ScheduleAction) -> Result<()> {
    match action {
        ScheduleAction::Status => {
            let s = schedule::status()?;
            if s.installed {
                p.text_line(&format!(
                    "schedule installed: {:?} {}",
                    s.method.unwrap_or(schedule::Method::Cron),
                    s.trigger.clone().unwrap_or_default()
                ))?;
            } else {
                p.text_line("no schedule installed")?;
            }
            emit_data(p, "schedule-status", &s)
        }
        ScheduleAction::Install { interval, method } => {
            let m = match method {
                Some(ref s) => schedule::Method::parse(s)?,
                None => schedule::Method::platform_default(),
            };
            let i = schedule::Interval::parse(&interval)?;
            schedule::install(m, i)?;
            let method_label = format!("{m:?}").to_lowercase();
            p.text_line(&format!("installed {method_label} schedule ({interval})"))?;
            emit_data(
                p,
                "schedule-install",
                serde_json::json!({"method": method_label, "interval": interval}),
            )
        }
        ScheduleAction::Remove => {
            schedule::remove()?;
            p.text_line("schedule removed")?;
            emit_data(p, "schedule-remove", serde_json::json!({}))
        }
    }
}

// ----- hooks -----

fn cmd_hooks(p: &mut Printer, action: HooksAction) -> Result<()> {
    match action {
        HooksAction::List => {
            let entries = hooks::list_all()?;
            if entries.is_empty() {
                p.text_line("no hooks configured")?;
            } else {
                for h in &entries {
                    let exec = if h.executable { "exec" } else { "noexec" };
                    p.text_line(&format!("{:?}: {} ({exec})", h.kind, h.path.display()))?;
                }
            }
            emit_data(p, "hooks-list", &entries)
        }
        HooksAction::Path => {
            let root = hooks::hooks_root()?;
            p.text_line(&root.display().to_string())?;
            emit_data(
                p,
                "hooks-path",
                serde_json::json!({"path": root.display().to_string()}),
            )
        }
    }
}

// ----- doctor -----

fn cmd_doctor(cfg: &Config, registry: &Registry, p: &mut Printer) -> Result<()> {
    let report = doctor::run(cfg, registry)?;
    for c in &report.checks {
        let mark = if c.ok { "✓" } else { "✗" };
        match &c.detail {
            Some(d) => p.text_line(&format!("{mark} {} — {d}", c.name))?,
            None => p.text_line(&format!("{mark} {}", c.name))?,
        }
    }
    p.text_line(if report.all_ok {
        "doctor: all checks passed"
    } else {
        "doctor: failures present"
    })?;
    emit_data(p, "doctor", &report)?;
    if !report.all_ok {
        return Err(CasbError::other("doctor reported failures"));
    }
    Ok(())
}

// ----- config -----

fn cmd_config(
    mut cfg: Config,
    explicit_path: Option<std::path::PathBuf>,
    p: &mut Printer,
    action: ConfigAction,
) -> Result<()> {
    let path = match explicit_path {
        Some(x) => x,
        None => Config::config_path()?,
    };
    match action {
        ConfigAction::Show => {
            let text = toml::to_string_pretty(&cfg)?;
            p.text_line(&text)?;
            emit_data(p, "config-show", &cfg)
        }
        ConfigAction::Path => {
            p.text_line(&path.display().to_string())?;
            emit_data(
                p,
                "config-path",
                serde_json::json!({"path": path.display().to_string()}),
            )
        }
        ConfigAction::Get { key } => match cfg.get_dotted(&key) {
            Some(v) => {
                p.text_line(&v)?;
                emit_data(p, "config-get", serde_json::json!({"key": key, "value": v}))
            }
            None => Err(CasbError::InvalidArgument(format!(
                "unknown config key: {key}"
            ))),
        },
        ConfigAction::Set { key, value } => {
            cfg.set_dotted(&key, &value)?;
            cfg.save_to(&path)?;
            p.text_line(&format!("set {key} = {value}"))?;
            emit_data(
                p,
                "config-set",
                serde_json::json!({"key": key, "value": value}),
            )
        }
        ConfigAction::Init => {
            if path.exists() {
                p.text_line(&format!("config exists at {}", path.display()))?;
            } else {
                cfg.save_to(&path)?;
                p.text_line(&format!("wrote default config to {}", path.display()))?;
            }
            emit_data(
                p,
                "config-init",
                serde_json::json!({"path": path.display().to_string()}),
            )
        }
    }
}

// ----- completion -----

fn cmd_completion(p: &mut Printer, shell: clap_complete::Shell) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    crate::completion::generate_completion(shell, &mut buf)?;
    p.raw_bytes(&buf)?;
    Ok(())
}
