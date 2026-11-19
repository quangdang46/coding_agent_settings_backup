//! `casb schedule` implementation.
//!
//! Manages automated backups via either `systemd` user timers or `cron`.
//! Both backends install a unit/job that runs `casb backup` at the chosen
//! interval; both can be removed cleanly without affecting other entries.

use crate::error::{CasbError, Result};
use crate::util::{home_dir, run_capture, run_capture_timeout, which};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Supported backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// systemd user timer.
    Systemd,
    /// cron line.
    Cron,
    /// Windows Task Scheduler.
    TaskScheduler,
}

impl Method {
    /// Parse a method name.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "systemd" => Ok(Self::Systemd),
            "cron" => Ok(Self::Cron),
            "taskscheduler" | "schtasks" => Ok(Self::TaskScheduler),
            other => Err(CasbError::InvalidArgument(format!(
                "unknown schedule method: {other}"
            ))),
        }
    }

    /// Return the best method for the current platform.
    pub fn platform_default() -> Self {
        if cfg!(windows) {
            Self::TaskScheduler
        } else if which("systemctl") {
            Self::Systemd
        } else {
            Self::Cron
        }
    }
}

/// Supported intervals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    /// Every hour.
    Hourly,
    /// Every day at 02:00 local time.
    Daily,
    /// Every Sunday at 02:00 local time.
    Weekly,
}

impl Interval {
    /// Parse an interval name.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "hourly" => Ok(Self::Hourly),
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            other => Err(CasbError::InvalidArgument(format!(
                "unknown schedule interval: {other}"
            ))),
        }
    }

    fn cron_line(&self) -> &'static str {
        match self {
            Self::Hourly => "0 * * * *",
            Self::Daily => "0 2 * * *",
            Self::Weekly => "0 2 * * 0",
        }
    }

    fn systemd_oncalendar(&self) -> &'static str {
        match self {
            Self::Hourly => "hourly",
            Self::Daily => "*-*-* 02:00:00",
            Self::Weekly => "Sun *-*-* 02:00:00",
        }
    }
}

/// Status of the active schedule.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduleStatus {
    /// Whether any schedule is currently installed.
    pub installed: bool,
    /// Method (`systemd` or `cron`) if installed.
    pub method: Option<Method>,
    /// Description of the trigger.
    pub trigger: Option<String>,
}

const SYSTEMD_SERVICE_NAME: &str = "casb-backup.service";
const SYSTEMD_TIMER_NAME: &str = "casb-backup.timer";
const CRON_TAG: &str = "# managed-by: casb";
/// Upper bound for any single `crontab` invocation. `crontab` is a local
/// operation and should finish in milliseconds; the timeout only exists so
/// a misbehaving crontab can never hang `casb schedule` forever.
const CRONTAB_TIMEOUT: Duration = Duration::from_secs(30);

/// Install a schedule with the chosen `method` and `interval`.
///
/// Exactly one backend may hold a schedule at a time: each `install_*`
/// removes the OTHER backends' entries after its own prerequisite check
/// passes, so a failed install never leaves the user with no schedule.
pub fn install(method: Method, interval: Interval) -> Result<()> {
    match method {
        Method::Systemd => install_systemd(interval),
        Method::Cron => install_cron(interval),
        Method::TaskScheduler => install_task_scheduler(interval),
    }
}

/// Remove any installed schedule. Best-effort across both backends.
pub fn remove() -> Result<()> {
    let _ = remove_systemd();
    let _ = remove_cron();
    let _ = remove_task_scheduler();
    Ok(())
}

/// Inspect the current schedule.
pub fn status() -> Result<ScheduleStatus> {
    if let Some(trigger) = systemd_trigger()? {
        return Ok(ScheduleStatus {
            installed: true,
            method: Some(Method::Systemd),
            trigger: Some(trigger),
        });
    }
    if let Some(line) = cron_trigger()? {
        return Ok(ScheduleStatus {
            installed: true,
            method: Some(Method::Cron),
            trigger: Some(line),
        });
    }
    if let Some(trigger) = task_scheduler_trigger()? {
        return Ok(ScheduleStatus {
            installed: true,
            method: Some(Method::TaskScheduler),
            trigger: Some(trigger),
        });
    }
    Ok(ScheduleStatus {
        installed: false,
        method: None,
        trigger: None,
    })
}

fn systemd_user_dir() -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(home.join(".config").join("systemd").join("user"))
}

fn install_systemd(interval: Interval) -> Result<()> {
    if !which("systemctl") {
        return Err(CasbError::other(
            "systemctl not found; install systemd or use --method cron",
        ));
    }
    // One backend at a time: drop any cron/Task-Scheduler schedule so the
    // backup doesn't run twice per interval. Best-effort — an absent
    // backend must not block this install.
    let _ = remove_cron();
    let _ = remove_task_scheduler();
    let dir = systemd_user_dir()?;
    std::fs::create_dir_all(&dir)?;
    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("casb"));
    // ExecStart is parsed by systemd, not a shell — quote the binary path
    // so installs under directories containing spaces still work. `%` is
    // systemd's specifier character and must be doubled or the path is
    // mangled (`%h` → home dir, unknown specifiers → literal `%`).
    let service = format!(
        "[Unit]\nDescription=casb backup\n\n[Service]\nType=oneshot\nExecStart={} backup\n",
        systemd_bin_arg(&bin),
    );
    std::fs::write(dir.join(SYSTEMD_SERVICE_NAME), service)?;
    let timer = format!(
        "[Unit]\nDescription=casb backup timer\n\n[Timer]\nOnCalendar={cal}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
        cal = interval.systemd_oncalendar(),
    );
    std::fs::write(dir.join(SYSTEMD_TIMER_NAME), timer)?;

    // Reload + enable + start (best-effort: report errors).
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let _ = Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_TIMER_NAME])
        .status();
    Ok(())
}

fn remove_systemd() -> Result<()> {
    let dir = systemd_user_dir()?;
    let timer_path = dir.join(SYSTEMD_TIMER_NAME);
    let service_path = dir.join(SYSTEMD_SERVICE_NAME);
    if timer_path.exists() {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", SYSTEMD_TIMER_NAME])
            .status();
        std::fs::remove_file(&timer_path)?;
    }
    if service_path.exists() {
        std::fs::remove_file(&service_path)?;
    }
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    Ok(())
}

fn systemd_trigger() -> Result<Option<String>> {
    let dir = systemd_user_dir()?;
    let timer = dir.join(SYSTEMD_TIMER_NAME);
    if !timer.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&timer)?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("OnCalendar=") {
            return Ok(Some(rest.to_string()));
        }
    }
    Ok(Some("installed (no trigger detected)".to_string()))
}

fn install_cron(interval: Interval) -> Result<()> {
    if !which("crontab") {
        return Err(CasbError::other(
            "crontab not found; install cron or use --method systemd",
        ));
    }
    // One backend at a time — see install_systemd.
    let _ = remove_systemd();
    let _ = remove_task_scheduler();
    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("casb"));
    // The crontab line is executed by `/bin/sh` — an unquoted path with
    // spaces would split into multiple arguments and the job would fail.
    // `%` is cron's "newline in command" character and must be backslash-
    // escaped or a binary path containing it truncates the command.
    let new_line = format!(
        "{cron} {bin} backup {tag}",
        cron = interval.cron_line(),
        bin = cron_bin_arg(&bin),
        tag = CRON_TAG,
    );
    let existing = read_crontab()?;
    let mut filtered: Vec<String> = existing
        .lines()
        .filter(|l| !l.contains(CRON_TAG))
        .map(|s| s.to_string())
        .collect();
    filtered.push(new_line);
    write_crontab(&filtered.join("\n"))
}

fn remove_cron() -> Result<()> {
    // Propagate read failures: treating an unreadable crontab as empty
    // would make `schedule remove` report success while the entry stays.
    let existing = read_crontab()?;
    if existing.is_empty() {
        return Ok(());
    }
    let filtered: Vec<&str> = existing.lines().filter(|l| !l.contains(CRON_TAG)).collect();
    write_crontab(&filtered.join("\n"))
}

fn cron_trigger() -> Result<Option<String>> {
    let existing = read_crontab()?;
    for line in existing.lines() {
        if line.contains(CRON_TAG) {
            return Ok(Some(line.to_string()));
        }
    }
    Ok(None)
}

fn read_crontab() -> Result<String> {
    if !which("crontab") {
        return Ok(String::new());
    }
    let out = run_capture_timeout("crontab", &["-l"], CRONTAB_TIMEOUT)?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    // "no crontab for <user>" is the normal empty case (exit 1 on both
    // GNU and BSD crontab). Any OTHER failure — permissions, a broken
    // crontab binary — must surface: install/remove would otherwise
    // silently overwrite or leave entries they failed to read.
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("no crontab") {
        Ok(String::new())
    } else {
        Err(CasbError::CommandFailed {
            command: "crontab -l".into(),
            stderr: stderr.into_owned(),
        })
    }
}

fn write_crontab(content: &str) -> Result<()> {
    use std::io::Write;
    // Feed crontab via a temp file (`crontab file`) rather than `crontab -`:
    // the `-` operand is not portable — BSD/macOS crontab does not document
    // it — and a crontab that falls back to reading the terminal would
    // block forever. The file is deleted when `tmp` drops after the call.
    let mut tmp = tempfile::NamedTempFile::new()?;
    let mut body = content.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    tmp.write_all(body.as_bytes())?;
    tmp.flush()?;
    let path_arg = tmp.path().to_string_lossy().into_owned();
    let out = run_capture_timeout("crontab", &[path_arg.as_str()], CRONTAB_TIMEOUT)?;
    if !out.status.success() {
        return Err(CasbError::CommandFailed {
            command: "crontab <file>".into(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

// ---- Windows Task Scheduler ----

const SCHTASKS_NAME: &str = "casb-backup";

fn install_task_scheduler(interval: Interval) -> Result<()> {
    if !which("schtasks") {
        return Err(CasbError::other(
            "schtasks not found; Task Scheduler not available",
        ));
    }
    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("casb"));
    let (schedule, modifier) = match interval {
        Interval::Hourly => ("HOURLY", "1"),
        Interval::Daily => ("DAILY", "1"),
        Interval::Weekly => ("WEEKLY", "1"),
    };
    // Remove existing task first (best-effort), plus any other backend's
    // schedule — one backend at a time.
    let _ = remove_task_scheduler();
    let _ = remove_systemd();
    let _ = remove_cron();
    let out = run_capture(
        "schtasks",
        &[
            "/Create",
            "/TN",
            SCHTASKS_NAME,
            "/TR",
            &format!("\"{}\" backup", bin.display()),
            "/SC",
            schedule,
            "/MO",
            modifier,
            "/ST",
            "02:00",
            "/F",
        ],
    )?;
    if !out.status.success() {
        return Err(CasbError::CommandFailed {
            command: "schtasks /Create".into(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

fn remove_task_scheduler() -> Result<()> {
    if !which("schtasks") {
        return Ok(());
    }
    let _ = run_capture("schtasks", &["/Delete", "/TN", SCHTASKS_NAME, "/F"]);
    Ok(())
}

fn task_scheduler_trigger() -> Result<Option<String>> {
    if !which("schtasks") {
        return Ok(None);
    }
    let out = run_capture("schtasks", &["/Query", "/TN", SCHTASKS_NAME, "/FO", "LIST"])?;
    if !out.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Schedule Type:") {
            return Ok(Some(rest.trim().to_string()));
        }
    }
    Ok(Some("installed".to_string()))
}

// ---- quoting helpers ----

/// Quote `path` for a POSIX shell command line (crontab entries). Paths
/// without whitespace or quotes pass through unchanged.
fn shell_quote(path: &Path) -> String {
    let s = path.display().to_string();
    if !s.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"') {
        return s;
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Quote `path` for a systemd `ExecStart=` line. systemd parses its own
/// quoting (not shell quoting); paths without whitespace or quotes pass
/// through unchanged.
fn systemd_quote(path: &Path) -> String {
    let s = path.display().to_string();
    if !s.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') {
        return s;
    }
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Binary path as it must appear in a crontab command line: shell-quoted,
/// then cron's `%`→newline character backslash-escaped. Without the
/// escape a binary under e.g. `/opt/100%/casb` would have its command
/// truncated at the `%`.
fn cron_bin_arg(bin: &Path) -> String {
    shell_quote(bin).replace('%', "\\%")
}

/// Binary path as it must appear in a systemd `ExecStart=` line:
/// systemd-quoted, then `%` doubled (`%%`) so it survives specifier
/// expansion literally.
fn systemd_bin_arg(bin: &Path) -> String {
    systemd_quote(bin).replace('%', "%%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_handles_spaces_and_quotes() {
        let plain = PathBuf::from("/usr/local/bin/casb");
        assert_eq!(shell_quote(&plain), "/usr/local/bin/casb");
        assert_eq!(systemd_quote(&plain), "/usr/local/bin/casb");

        let spaced = PathBuf::from("/opt/my tools/casb");
        assert_eq!(shell_quote(&spaced), "'/opt/my tools/casb'");
        assert_eq!(systemd_quote(&spaced), "\"/opt/my tools/casb\"");

        let squote = PathBuf::from("/opt/it's/casb");
        assert_eq!(shell_quote(&squote), "'/opt/it'\\''s/casb'");
    }

    /// Regression (bead hd3): `%` in the binary path must be escaped for
    /// both backends — cron treats it as a newline, systemd as a
    /// specifier.
    #[test]
    fn percent_in_binary_path_is_escaped() {
        let pct = PathBuf::from("/opt/100% legit/casb");
        assert_eq!(cron_bin_arg(&pct), "'/opt/100\\% legit/casb'");
        let sys = systemd_bin_arg(&pct);
        assert!(sys.contains("%%"), "systemd arg must double %: {sys}");
        assert!(!sys.contains("100% legit"));
    }

    #[test]
    fn parse_method_variants() {
        assert_eq!(Method::parse("systemd").unwrap(), Method::Systemd);
        assert_eq!(Method::parse("CRON").unwrap(), Method::Cron);
        assert_eq!(
            Method::parse("taskscheduler").unwrap(),
            Method::TaskScheduler
        );
        assert_eq!(Method::parse("schtasks").unwrap(), Method::TaskScheduler);
        assert!(Method::parse("nonsense").is_err());
    }

    #[test]
    fn parse_interval_variants() {
        assert_eq!(Interval::parse("hourly").unwrap(), Interval::Hourly);
        assert_eq!(Interval::parse("Daily").unwrap(), Interval::Daily);
        assert_eq!(Interval::parse("WEEKLY").unwrap(), Interval::Weekly);
        assert!(Interval::parse("forever").is_err());
    }

    #[test]
    fn cron_lines_match_intervals() {
        assert_eq!(Interval::Hourly.cron_line(), "0 * * * *");
        assert_eq!(Interval::Daily.cron_line(), "0 2 * * *");
        assert_eq!(Interval::Weekly.cron_line(), "0 2 * * 0");
    }
}
