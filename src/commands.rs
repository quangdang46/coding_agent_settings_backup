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
        Command::Init { root } => cmd_init(&cfg, &mut printer, root.as_deref(), cli.dry_run),
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
        Command::Import { file } => cmd_import(&cfg, &mut printer, file.as_deref(), cli.dry_run),
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
        Command::Schedule { action } => cmd_schedule(&mut printer, action, cli.dry_run),
        Command::Hooks { action } => cmd_hooks(&mut printer, action),
        Command::Doctor => cmd_doctor(&cfg, &registry, &mut printer),
        Command::Config { action } => {
            cmd_config(cfg, cli.config.clone(), &mut printer, action, cli.dry_run)
        }
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
    /// Populated only when the command partially or wholly failed, so robot
    /// consumers get the failure reasons without digging through `data`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: Vec<String>,
    /// Non-fatal warnings, when the command produces any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    data: T,
}

/// Emit a successful envelope (`ok: true`, no errors/warnings).
fn emit_data<T: Serialize>(p: &mut Printer, command: &'static str, data: T) -> Result<()> {
    emit_envelope(p, command, true, &[], &[], data)
}

/// Emit an envelope with an explicit `ok` flag plus error/warning lists.
fn emit_envelope<T: Serialize>(
    p: &mut Printer,
    command: &'static str,
    ok: bool,
    errors: &[String],
    warnings: &[String],
    data: T,
) -> Result<()> {
    let env = Envelope {
        ok,
        command,
        errors: errors.to_vec(),
        warnings: warnings.to_vec(),
        data,
    };
    p.emit(&env)
}

// ----- init -----

fn cmd_init(
    cfg: &Config,
    p: &mut Printer,
    root: Option<&std::path::Path>,
    dry_run: bool,
) -> Result<()> {
    // Resolve the target root the same way `init_backup_root` does so the
    // dry-run preview reports the real destination.
    let resolved = match root {
        Some(r) => crate::util::expand_tilde(r),
        None => cfg.backup_root(),
    };
    if dry_run {
        p.text_line(&format!(
            "dry-run: would initialise backup root at {}",
            resolved.display()
        ))?;
        emit_data(
            p,
            "init",
            serde_json::json!({"backup_root": resolved.display().to_string(), "dry_run": true}),
        )?;
        return Ok(());
    }
    let root = backup::init_backup_root(cfg, root)?;
    // `init` should leave a usable repository behind, not just a directory
    // with a README — create the shared .git if it does not exist yet.
    let repo = crate::git::Repo::new(&root);
    if !repo.exists() {
        repo.init()?;
    }
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
    let errors: Vec<String> = outcomes
        .iter()
        .filter_map(|o| o.error.as_ref().map(|e| format!("{}: {e}", o.agent)))
        .collect();
    emit_envelope(p, "backup", failed == 0, &errors, &[], &outcomes)?;
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
    require_confirmation_mode(p, force, dry_run, "restore")?;
    let (preview, outcome) = match restore::restore_agent(cfg, agent, reference, force, dry_run) {
        Ok(v) => v,
        // Declining the prompt is a normal user choice, not an error:
        // exit 0 so scripts wrapping `casb restore` don't treat "no" as
        // a failure.
        Err(CasbError::Cancelled) => {
            p.text_line("restore cancelled")?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
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

/// Restore operations prompt for confirmation. In machine-readable modes
/// (`--json`, `--format toon`) there is no interactive user, so a missing
/// `--force` must fail fast with a structured error instead of blocking on
/// stdin — the prompt itself now goes to stderr via [`crate::util::confirm`],
/// but robot consumers should never have to answer it.
fn require_confirmation_mode(p: &Printer, force: bool, dry_run: bool, what: &str) -> Result<()> {
    if !force && !dry_run && !p.is_text() {
        return Err(CasbError::other(format!(
            "{what} requires confirmation; re-run with --force for machine-readable output"
        )));
    }
    Ok(())
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

fn cmd_import(cfg: &Config, p: &mut Printer, src: Option<&str>, dry_run: bool) -> Result<()> {
    if dry_run {
        // Validate the archive is a readable gzip+tar and report what would
        // be unpacked — without writing anything to the backup root.
        let entries = inspect_archive_entries(src)?;
        let dest = cfg.backup_root();
        p.text_line(&format!(
            "dry-run: would import {} entries from archive into {}",
            entries,
            dest.display()
        ))?;
        emit_data(
            p,
            "import",
            serde_json::json!({
                "backup_root": dest.display().to_string(),
                "entries": entries,
                "dry_run": true,
            }),
        )?;
        return Ok(());
    }
    let dest = export::import_archive(cfg, src)?;
    p.text_line(&format!("imported into {}", dest.display()))?;
    emit_data(
        p,
        "import",
        serde_json::json!({"backup_root": dest.display().to_string()}),
    )
}

/// Count the entries in a tar.gz archive without extracting it.
/// `src` follows the same convention as [`export::import_archive`]:
/// `None`/`Some("-")` reads stdin, anything else is a file path.
fn inspect_archive_entries(src: Option<&str>) -> Result<usize> {
    use flate2::read::GzDecoder;

    let stdin_mode = matches!(src, None | Some("-"));
    let mut count = 0usize;
    if stdin_mode {
        let stdin = std::io::stdin();
        let lock = stdin.lock();
        let dec = GzDecoder::new(lock);
        let mut ar = tar::Archive::new(dec);
        for entry in ar.entries()? {
            let _ = entry?;
            count += 1;
        }
    } else {
        let path = crate::util::expand_tilde(src.unwrap_or_default());
        let file = std::fs::File::open(&path)?;
        let dec = GzDecoder::new(file);
        let mut ar = tar::Archive::new(dec);
        for entry in ar.entries()? {
            let _ = entry?;
            count += 1;
        }
    }
    Ok(count)
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
    let repo_path = cfg.backup_root();
    let repo = crate::git::Repo::new(&repo_path);
    let repo_exists = repo.exists();
    for agent in registry.all() {
        // The backup repo is shared, so "has backup" must be answered per
        // agent: it means this agent's own directories appear in the repo's
        // history, not merely that the shared repository exists.
        let subdirs = vec![agent.backup_root_subdir()];
        let (has_backup, last) = if repo_exists {
            let commits = repo.commit_count_for_paths(&subdirs)?;
            (commits > 0, repo.last_commit_for_paths(&subdirs)?)
        } else {
            (false, None)
        };
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
    let errors: Vec<String> = report
        .agents
        .iter()
        .flat_map(|a| a.errors.iter().map(|e| format!("{}: {e}", a.agent)))
        .collect();
    let warnings: Vec<String> = report
        .agents
        .iter()
        .flat_map(|a| a.warnings.iter().map(|w| format!("{}: {w}", a.agent)))
        .collect();
    emit_envelope(p, "verify", report.all_ok, &errors, &warnings, &report)?;
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
            require_confirmation_mode(p, force, dry_run, "tag restore")?;
            let (preview, outcome) =
                match restore::restore_agent(cfg, a, Some(&name), force, dry_run) {
                    Ok(v) => v,
                    Err(CasbError::Cancelled) => {
                        p.text_line("restore cancelled")?;
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
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
    let mut s = stats::compute_stats(cfg, registry, key)?;
    if key.is_some() {
        // A filtered view must not print repo-global totals: scope them to
        // the selected agent(s) so `stats <agent>` doesn't claim the whole
        // shared repository's commit count and byte size.
        s.total_repo_bytes = s.agents.iter().map(|a| a.repo_bytes).sum();
        s.total_commits = s.agents.iter().map(|a| a.commits).sum();
    }
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

fn cmd_schedule(p: &mut Printer, action: ScheduleAction, dry_run: bool) -> Result<()> {
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
            if dry_run {
                let method_label = format!("{m:?}").to_lowercase();
                p.text_line(&format!(
                    "dry-run: would install {method_label} schedule ({interval})"
                ))?;
                emit_data(
                    p,
                    "schedule-install",
                    serde_json::json!({
                        "method": method_label,
                        "interval": interval,
                        "dry_run": true,
                    }),
                )?;
                return Ok(());
            }
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
            if dry_run {
                let s = schedule::status()?;
                p.text_line(&format!(
                    "dry-run: would remove schedule (currently {})",
                    if s.installed {
                        "installed"
                    } else {
                        "not installed"
                    }
                ))?;
                emit_data(
                    p,
                    "schedule-remove",
                    serde_json::json!({"dry_run": true, "was_installed": s.installed}),
                )?;
                return Ok(());
            }
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
    let errors: Vec<String> = report
        .checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| match &c.detail {
            Some(d) => format!("{}: {d}", c.name),
            None => c.name.clone(),
        })
        .collect();
    emit_envelope(p, "doctor", report.all_ok, &errors, &[], &report)?;
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
    dry_run: bool,
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
            // Validate the key/value pair against the typed setter first so
            // a bad value fails before anything touches the file.
            cfg.set_dotted(&key, &value)?;
            if dry_run {
                p.text_line(&format!("dry-run: would set {key} = {value}"))?;
                emit_data(
                    p,
                    "config-set",
                    serde_json::json!({"key": key, "value": value, "dry_run": true}),
                )?;
                return Ok(());
            }
            if !path.exists() {
                // Seed a complete default document first so `set` on a fresh
                // file produces a usable config, not a lone key. Defaults —
                // not the env-merged `cfg` — so env vars never leak in.
                Config::default().save_to(&path)?;
            }
            // Patch only the requested key. Serialising the whole effective
            // config here would bake env-resolved values (e.g.
            // CASB_BACKUP_ROOT) into the user's real config file.
            Config::set_key_in_file(&path, &key, &value)?;
            p.text_line(&format!("set {key} = {value}"))?;
            emit_data(
                p,
                "config-set",
                serde_json::json!({"key": key, "value": value}),
            )
        }
        ConfigAction::Init => {
            if dry_run {
                p.text_line(&format!(
                    "dry-run: would {} config at {}",
                    if path.exists() {
                        "keep existing"
                    } else {
                        "write default"
                    },
                    path.display()
                ))?;
            } else if path.exists() {
                p.text_line(&format!("config exists at {}", path.display()))?;
            } else {
                // Write built-in defaults, not the env-merged `cfg`, so
                // environment overrides (e.g. CASB_BACKUP_ROOT) are not
                // persisted into the file.
                Config::default().save_to(&path)?;
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
