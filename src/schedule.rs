//! `casb schedule` implementation.
//!
//! Manages automated backups via either `systemd` user timers or `cron`.
//! Both backends install a unit/job that runs `casb backup` at the chosen
//! interval; both can be removed cleanly without affecting other entries.

use crate::error::{CasbError, Result};
use crate::util::{home_dir, run_capture, which};
use std::path::PathBuf;
use std::process::Command;

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

/// Install a schedule with the chosen `method` and `interval`.
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
    let dir = systemd_user_dir()?;
    std::fs::create_dir_all(&dir)?;
    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("casb"));
    let service = format!(
        "[Unit]\nDescription=casb backup\n\n[Service]\nType=oneshot\nExecStart={bin} backup\n",
        bin = bin.display(),
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
    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("casb"));
    let new_line = format!(
        "{cron} {bin} backup {tag}",
        cron = interval.cron_line(),
        bin = bin.display(),
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
    let existing = read_crontab().unwrap_or_default();
    if existing.is_empty() {
        return Ok(());
    }
    let filtered: Vec<&str> = existing.lines().filter(|l| !l.contains(CRON_TAG)).collect();
    write_crontab(&filtered.join("\n"))
}

fn cron_trigger() -> Result<Option<String>> {
    let existing = read_crontab().unwrap_or_default();
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
    let out = run_capture("crontab", &["-l"])?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        // No crontab is not an error.
        Ok(String::new())
    }
}

fn write_crontab(content: &str) -> Result<()> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let mut body = content.to_string();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        stdin.write_all(body.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(CasbError::CommandFailed {
            command: "crontab -".into(),
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
    // Remove existing task first (best-effort).
    let _ = remove_task_scheduler();
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

#[cfg(test)]
mod tests {
    use super::*;

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
