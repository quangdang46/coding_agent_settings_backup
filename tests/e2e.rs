//! End-to-end integration test for `casb`.
//!
//! Drives the compiled binary via `assert_cmd` against an isolated temp
//! directory tree populated by `scripts/generate_mock_agents.sh`. Each
//! scenario exercises a full lifecycle: init → backup → list → history →
//! diff → tag → restore → export → import → verify → doctor.

#![allow(dead_code, unused_imports)]

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Test fixture: an isolated HOME with a casb config that defines a single
/// custom "mockagent" pointing at the supplied source directory.
struct Env {
    home: TempDir,
    backup_root: PathBuf,
    config_path: PathBuf,
    src_dir: PathBuf,
}

impl Env {
    fn new(agent_key: &str, agent_src: &Path) -> Self {
        let home = TempDir::new().expect("home tempdir");
        let backup_root = home.path().join(".agent_settings_backups");
        fs::create_dir_all(&backup_root).unwrap();
        let cfg_dir = home.path().join(".config").join("casb");
        fs::create_dir_all(&cfg_dir).unwrap();
        let config_path = cfg_dir.join("config.toml");
        // Use forward-slash paths in the TOML so backslashes on Windows
        // don't get interpreted as Unicode escape sequences by the TOML parser.
        let backup_root_str = backup_root.display().to_string().replace('\\', "/");
        let src_str = agent_src.display().to_string().replace('\\', "/");
        let cfg = format!(
            r#"[general]
backup_root = "{backup_root_str}"
auto_commit = true
verbose = false
quiet = false
output_format = "text"

[backup]
exclusions = ["*.log", "*.tmp", "*.sqlite3-wal", "*.sqlite3-shm"]
use_rsync = false
checksum_verify = false

[schedule]
method = "systemd"
interval = "daily"

[agents.{key}]
enabled = true
display_name = "Mock {key}"
locations = ["{src_str}"]
exclusions = []
"#,
            key = agent_key,
        );
        fs::write(&config_path, cfg).unwrap();
        Self {
            home,
            backup_root,
            config_path,
            src_dir: agent_src.to_path_buf(),
        }
    }

    fn casb(&self) -> Command {
        let mut cmd = Command::cargo_bin("casb").unwrap();
        cmd.env("HOME", self.home.path());
        // On Windows the `dirs` crate reads USERPROFILE.
        #[cfg(windows)]
        cmd.env("USERPROFILE", self.home.path());
        cmd.env("CASB_CONFIG", &self.config_path);
        cmd.env_remove("XDG_CONFIG_HOME");
        cmd.env_remove("XDG_DATA_HOME");
        cmd
    }
}

/// Run the bash mock generator into a tempdir and return the populated
/// directory holding a single agent's mock data.
#[cfg(unix)]
fn generate_mock(agent: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("generate_mock_agents.sh");
    let status = std::process::Command::new("bash")
        .arg(script)
        .arg(dir.path())
        .arg("--agent")
        .arg(agent)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run mock generator");
    assert!(status.success(), "mock generator failed");
    dir
}

/// Generate minimal mock agent data without requiring bash.
fn generate_mock_portable(agent: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let agent_dir = dir.path().join(format!(".{agent}"));
    fs::create_dir_all(&agent_dir).unwrap();
    let config_name = match agent {
        "claude" | "codex" | "gemini" | "cursor" => "config.toml",
        _ => "settings.json",
    };
    let config_content = match agent {
        "codex" => "# Codex CLI Configuration\nmodel = \"codex-mini\"\n".to_string(),
        "claude" => "# Claude Code Configuration\nmodel = \"claude-4\"\n".to_string(),
        _ => format!("# {agent} Configuration\nenabled = true\n"),
    };
    fs::write(agent_dir.join(config_name), &config_content).unwrap();
    fs::write(
        agent_dir.join("state.json"),
        format!("{{\"agent\": \"{agent}\", \"version\": 1}}"),
    )
    .unwrap();
    // Add a nested directory.
    let sub = agent_dir.join("cache");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("data.bin"), "cached-data").unwrap();
    dir
}

#[test]
#[cfg(unix)] // requires bash for generate_mock
fn lifecycle_codex_single_location() {
    let mock = generate_mock("codex");
    let agent_src = mock.path().join(".codex");
    assert!(agent_src.exists(), "mock generator must create .codex");
    let env = Env::new("codex_e2e", &agent_src);

    // `init` is implicit, but call it for coverage.
    env.casb().arg("init").assert().success();

    // List shows our custom agent and not-installed built-ins.
    env.casb()
        .arg("list")
        .assert()
        .success()
        .stdout(predicates::str::contains("codex_e2e"));

    // Backup the custom agent only.
    env.casb()
        .arg("backup")
        .arg("codex_e2e")
        .assert()
        .success()
        .stdout(predicates::str::contains("codex_e2e"));

    assert!(
        env.backup_root.join(".git").exists(),
        "shared repo must be initialised"
    );
    assert!(env.backup_root.join(".codex_e2e/config.toml").exists());

    // History returns at least one entry.
    env.casb()
        .arg("history")
        .arg("codex_e2e")
        .assert()
        .success()
        .stdout(
            predicates::str::contains("casb backup codex_e2e")
                .or(predicates::str::contains("codex_e2e")),
        );

    // Diff against current source: excluded files (memories, sqlite, etc.) are
    // "added" because they are in the source but not in the backup commit.
    // After modifying config.toml we expect it to show as modified.
    env.casb()
        .arg("diff")
        .arg("codex_e2e")
        .assert()
        .success()
        .stdout(predicates::str::contains("memories").or(predicates::str::contains("sqlite")));

    // Modify a file in the source and re-run diff.
    fs::write(env.src_dir.join("config.toml"), "modified=true\n").unwrap();
    env.casb()
        .arg("diff")
        .arg("codex_e2e")
        .assert()
        .success()
        .stdout(predicates::str::contains("modified"));

    // Tag-create.
    env.casb()
        .args(["tag", "create", "codex_e2e", "v1", "-m", "first tag"])
        .assert()
        .success();
    env.casb()
        .args(["tag", "list", "codex_e2e"])
        .assert()
        .success()
        .stdout(predicates::str::contains("v1"));

    // Restore (force) brings config.toml back to its original content.
    env.casb()
        .args(["restore", "codex_e2e", "--force"])
        .assert()
        .success();
    let restored = fs::read_to_string(env.src_dir.join("config.toml")).unwrap();
    assert!(
        restored.contains("Codex CLI Configuration"),
        "expected original mock content, got: {restored}"
    );

    // Verify reports clean.
    env.casb()
        .args(["verify", "codex_e2e"])
        .assert()
        .success()
        .stdout(predicates::str::contains("clean"));

    // Stats includes our agent.
    env.casb()
        .args(["stats", "codex_e2e"])
        .assert()
        .success()
        .stdout(predicates::str::contains("codex_e2e"));

    // JSON output is valid for list.
    let assert = env.casb().args(["--json", "list"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(json["ok"], serde_json::Value::Bool(true));
    assert_eq!(json["command"], "list");
}

#[test]
#[cfg(unix)] // requires bash for generate_mock
fn export_import_round_trip() {
    let mock = generate_mock("kiro");
    let agent_src = mock.path().join(".kiro");
    let env = Env::new("kiro_e2e", &agent_src);

    env.casb().arg("init").assert().success();
    env.casb().arg("backup").arg("kiro_e2e").assert().success();

    let archive_dir = TempDir::new().unwrap();
    let archive = archive_dir.path().join("kiro.tar.gz");
    env.casb()
        .args(["export", "kiro_e2e"])
        .arg(&archive)
        .assert()
        .success();
    assert!(archive.exists(), "archive must be created");

    // Import into a fresh casb home.
    let mock2 = generate_mock("kiro");
    let agent_src2 = mock2.path().join(".kiro");
    let env2 = Env::new("kiro_e2e", &agent_src2);
    env2.casb().arg("import").arg(&archive).assert().success();
    assert!(env2.backup_root.join(".git").exists());
}

#[test]
#[cfg(unix)] // requires bash for generate_mock
fn doctor_passes_with_minimal_setup() {
    let mock = generate_mock("cursor");
    let agent_src = mock.path().join(".cursor");
    let env = Env::new("cursor_e2e", &agent_src);

    env.casb().arg("init").assert().success();
    env.casb()
        .arg("backup")
        .arg("cursor_e2e")
        .assert()
        .success();
    // Doctor should pass — repo exists, fsck clean, source readable.
    env.casb()
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("doctor: all checks passed"));
}

#[test]
#[cfg(unix)] // requires bash for generate_mock
fn verify_detects_clean_repo() {
    let mock = generate_mock("gemini");
    let env = Env::new("gemini_e2e", &mock.path().join(".gemini"));
    env.casb().arg("init").assert().success();
    env.casb()
        .arg("backup")
        .arg("gemini_e2e")
        .assert()
        .success();
    env.casb()
        .arg("verify")
        .arg("gemini_e2e")
        .assert()
        .success()
        .stdout(predicates::str::contains("clean"));
}

#[test]
#[cfg(unix)] // requires bash for generate_mock
fn dry_run_makes_no_repo() {
    let mock = generate_mock("codex");
    let env = Env::new("codex_dry", &mock.path().join(".codex"));
    env.casb().arg("init").assert().success();
    env.casb()
        .args(["--dry-run", "backup", "codex_dry"])
        .assert()
        .success();
    let repo = env.backup_root.join(".codex_dry");
    assert!(
        !repo.join(".git").exists(),
        "dry-run must not create a git repo"
    );
}

#[test]
fn version_and_help_succeed() {
    let dummy = TempDir::new().unwrap();
    let env = Env::new("noop", dummy.path());
    env.casb()
        .arg("version")
        .assert()
        .success()
        .stdout(predicates::str::contains("casb"));
    env.casb()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("Backup and restore"));
}

#[test]
fn completion_generates_bash() {
    let dummy = TempDir::new().unwrap();
    let env = Env::new("noop", dummy.path());
    env.casb()
        .args(["completion", "bash"])
        .assert()
        .success()
        .stdout(predicates::str::contains("casb"));
}

#[test]
fn config_commands_round_trip() {
    let dummy = TempDir::new().unwrap();
    let env = Env::new("noop", dummy.path());
    env.casb().args(["config", "init"]).assert().success();
    env.casb()
        .args(["config", "set", "general.verbose", "true"])
        .assert()
        .success();
    env.casb()
        .args(["config", "get", "general.verbose"])
        .assert()
        .success()
        .stdout(predicates::str::contains("true"));
}

/// Regression: `casb backup` must exit non-zero when a per-agent backup
/// fails (e.g. a pre-backup hook returns a non-zero status).
#[test]
#[cfg(unix)] // requires bash hook script
fn backup_exits_non_zero_when_agent_fails() {
    let src = TempDir::new().unwrap();
    fs::write(src.path().join("a.txt"), "x").unwrap();
    let env = Env::new("failagent", src.path());

    // Install a failing pre-backup hook.
    let hook_dir = env
        .home
        .path()
        .join(".config")
        .join("casb")
        .join("pre-backup.d");
    fs::create_dir_all(&hook_dir).unwrap();
    let hook_path = hook_dir.join("00-fail.sh");
    fs::write(&hook_path, "#!/usr/bin/env bash\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).unwrap();
    }

    env.casb().arg("init").assert().success();
    env.casb()
        .args(["backup", "failagent"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("✗ failagent"));
}

/// Regression: round-trip backup → modify source → restore → backup must
/// not leave a stray `.git` gitlink file in the source location. See
/// scripts/check_features.sh bug-1 / bug-2.
#[test]
#[cfg(unix)] // uses git which may not be configured identically on Windows
fn restore_does_not_leak_git_gitlink_into_source() {
    let src = TempDir::new().unwrap();
    fs::write(src.path().join("a.txt"), "one").unwrap();
    let env = Env::new("roundtrip", src.path());
    env.casb().arg("init").assert().success();
    env.casb()
        .args(["backup", "roundtrip", "-m", "first"])
        .assert()
        .success();
    fs::write(src.path().join("a.txt"), "two").unwrap();
    env.casb()
        .args(["--force", "restore", "roundtrip"])
        .assert()
        .success();
    assert!(
        !src.path().join(".git").exists(),
        "restore must not leave a .git gitlink in the source location"
    );
    // And a subsequent backup must still succeed cleanly.
    env.casb()
        .args(["backup", "roundtrip", "-m", "second"])
        .assert()
        .success();
}

// ---- Cross-platform tests using portable mock generator ----

#[test]
fn portable_lifecycle_backup_and_restore() {
    let mock = generate_mock_portable("codex");
    let agent_src = mock.path().join(".codex");
    let env = Env::new("codex_p", &agent_src);

    env.casb().arg("init").assert().success();
    env.casb()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("codex_p"));

    env.casb()
        .args(["backup", "codex_p"])
        .assert()
        .success()
        .stdout(predicates::str::contains("codex_p"));

    assert!(
        env.backup_root.join(".git").exists(),
        "shared repo must be initialised"
    );

    env.casb()
        .args(["history", "codex_p"])
        .assert()
        .success()
        .stdout(predicates::str::contains("codex_p"));

    // A second backup after no source changes ensures the repo is in
    // sync, avoiding false diffs caused by git line-ending normalisation
    // on Windows during the initial commit.
    env.casb().args(["backup", "codex_p"]).assert().success();

    // Modify source and verify diff detects it.
    fs::write(agent_src.join("config.toml"), "modified=true\n").unwrap();
    env.casb()
        .args(["diff", "codex_p"])
        .assert()
        .success()
        .stdout(predicates::str::contains("modified"));

    // Tag, verify, stats.
    env.casb()
        .args(["tag", "create", "codex_p", "v1", "-m", "first tag"])
        .assert()
        .success();
    env.casb()
        .args(["tag", "list", "codex_p"])
        .assert()
        .success()
        .stdout(predicates::str::contains("v1"));

    env.casb()
        .args(["verify", "codex_p"])
        .assert()
        .success()
        .stdout(predicates::str::contains("clean"));

    env.casb()
        .args(["stats", "codex_p"])
        .assert()
        .success()
        .stdout(predicates::str::contains("codex_p"));

    // Restore with --force.
    env.casb()
        .args(["restore", "codex_p", "--force"])
        .assert()
        .success();
    let restored = fs::read_to_string(agent_src.join("config.toml")).unwrap();
    assert!(
        restored.contains("Codex CLI Configuration"),
        "expected original mock content, got: {restored}"
    );

    // JSON output round-trip.
    let assert = env.casb().args(["--json", "list"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(json["ok"], serde_json::Value::Bool(true));
}

#[test]
fn portable_export_import_round_trip() {
    let mock = generate_mock_portable("kiro");
    let agent_src = mock.path().join(".kiro");
    let env = Env::new("kiro_p", &agent_src);

    env.casb().arg("init").assert().success();
    env.casb().arg("backup").arg("kiro_p").assert().success();

    let archive_dir = TempDir::new().unwrap();
    let archive = archive_dir.path().join("kiro.tar.gz");
    env.casb()
        .args(["export", "kiro_p"])
        .arg(&archive)
        .assert()
        .success();
    assert!(archive.exists(), "archive must be created");

    // Import into a fresh env.
    let mock2 = generate_mock_portable("kiro");
    let agent_src2 = mock2.path().join(".kiro");
    let env2 = Env::new("kiro_p", &agent_src2);
    env2.casb().arg("import").arg(&archive).assert().success();
    assert!(env2.backup_root.join(".git").exists());
}
