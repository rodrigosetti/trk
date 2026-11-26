//! End-to-end tests that run the `trk` binary.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};

use assert_cmd::Command;
use serde_json::{Value, json};

#[path = "cli/restart.rs"]
mod restart;

#[path = "cli/resume.rs"]
mod resume;

#[path = "cli/gc.rs"]
mod gc;

#[path = "cli/launcher.rs"]
mod launcher;

#[path = "cli/type_install.rs"]
mod type_install;

#[path = "cli/default_type.rs"]
mod default_type;

#[path = "cli/base.rs"]
mod base;

#[path = "cli/verify.rs"]
mod verify;

#[path = "cli/user_config.rs"]
mod user_config;

#[path = "cli/install_detection.rs"]
mod install_detection;
#[path = "cli/skill.rs"]
mod skill;

#[path = "cli/monorepo.rs"]
mod monorepo;

#[path = "cli/type_guidance.rs"]
mod type_guidance;

#[path = "cli/check_colors.rs"]
mod check_colors;

#[path = "cli/check_output.rs"]
mod check_output;

#[path = "cli/epics.rs"]
mod epics;

#[path = "cli/run_foreground.rs"]
mod run_foreground;

#[path = "cli/listing.rs"]
mod listing;

#[path = "cli/issue_commands.rs"]
mod issue_commands;

#[path = "cli/check_execution.rs"]
mod check_execution;

#[path = "cli/installation.rs"]
mod installation;

#[path = "cli/pretty.rs"]
mod pretty;

#[path = "cli/run_launch.rs"]
mod run_launch;

#[path = "cli/mcp.rs"]
mod mcp;

#[path = "cli/session_delivery.rs"]
mod session_delivery;

#[path = "cli/waiting_checks.rs"]
mod waiting_checks;

fn local_host() -> String {
    trk_core::system::hostname().unwrap().to_owned()
}

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let e = Self::named();
        let path = e.trk_dir().join("config.toml");
        let config = std::fs::read_to_string(&path).unwrap();
        let start = config.find("[launcher]").unwrap();
        let end = config[start..].find("[harness]").unwrap() + start;
        // Keep terminal-boundary fixtures independent of the install seed.
        let profiles = r#"[launcher]
use = ["herdr"]
send_delay = 5
send_retries = 10
send_retry_delay = 3

[launcher.herdr]
driver = "herdr"
start      = ["hc", "run", "--bg", "--no-parent", "--name", "{name}", "--cwd", "{cwd}", "--", "sh", "-c", "{shell_command}"]
attach     = ["hc", "attach", "{name}"]
send       = ["hc", "send", "{name}", "{text}"]

"#;
        std::fs::write(
            path,
            format!("{}{profiles}{}", &config[..start], &config[end..]),
        )
        .unwrap();
        e
    }

    /// Seed profiles with deterministic preferences, independent of host tools.
    fn named() -> Env {
        let e = Self::with_install(false);
        let config: toml::Value =
            toml::from_str(&std::fs::read_to_string(e.trk_dir().join("config.toml")).unwrap())
                .unwrap();
        for (section, key, value) in [
            ("launcher", "use", "[\"herdr\", \"hc\"]"),
            ("harness", "prefer", "[\"omp\", \"claude-code\"]"),
        ] {
            e.set_config(
                &format!("{key} = {}", config[section][key]),
                &format!("{key} = {value}"),
            );
        }
        e
    }

    fn with_install(minimal: bool) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["remote", "add", "origin", "git@github.com:acme/orch.git"],
            vec![
                "-c",
                "user.email=t@e.st",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        ] {
            assert!(
                StdCommand::new("git")
                    .current_dir(&repo)
                    .args(&args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let e = Env { dir };
        if minimal {
            e.ok(&["install", "--minimal", "--prefix", "orch"]);
        } else {
            e.ok(&["install", "--prefix", "orch"]);
        }
        e
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    /// The project's `.trk` directory.
    fn trk_dir(&self) -> PathBuf {
        self.repo().join(".trk")
    }

    fn user_dir(&self) -> PathBuf {
        self.dir.path().join("user-config")
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("trk").unwrap();
        c.current_dir(self.repo())
            // No ~/.claude here: the skill's Claude Code link is opt-in per test.
            .env("TRK_USER_DIR", self.user_dir())
            .env("HOME", self.dir.path().join("home"))
            .env("TRK_ACTOR", "human:alice@example.com")
            .env("TRK_NOW", "2026-11-20T10:00:00Z")
            .env_remove("TRK_FORMAT")
            .env_remove("TRK_DIR");
        c
    }

    /// The same command with the clock moved to `now`.
    fn at(&self, now: &str, args: &[&str]) -> Value {
        let out = self
            .cmd()
            .env("TRK_NOW", now)
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["ok"], true, "{args:?} failed: {v:#}");
        v["data"].clone()
    }

    fn create(&self, args: &[&str]) -> String {
        let mut a = vec!["create"];
        a.extend_from_slice(args);
        self.ok(&a)["id"].as_str().unwrap().to_string()
    }

    /// Claim an issue and leave it waiting on `check`.
    fn waiting(&self, title: &str, check: &str) -> String {
        let id = self.create(&[title]);
        self.ok(&["claim", &id]);
        self.ok(&["wait", &id, "--check", check, "--reason", "testing"]);
        id
    }

    fn set_config(&self, from: &str, to: &str) {
        let p = self.trk_dir().join("config.toml");
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains(from), "config.toml has no '{from}'");
        std::fs::write(&p, text.replace(from, to)).unwrap();
    }

    /// Run with --json; returns (exit code, envelope).
    fn run(&self, args: &[&str]) -> (i32, Value) {
        let out = self.cmd().arg("--json").args(args).output().unwrap();
        let code = out.status.code().unwrap_or(-1);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: not JSON ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
        (code, v)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, v) = self.run(args);
        assert_eq!(code, 0, "{args:?} failed: {v:#}");
        assert_eq!(v["ok"], true);
        assert_eq!(v["schema_version"], 4);
        v["data"].clone()
    }

    fn err(&self, args: &[&str], exit: i32, code: &str) -> Value {
        let (c, v) = self.run(args);
        assert_eq!(v["ok"], false, "{args:?}: expected an error, got {v:#}");
        assert_eq!(v["error"]["code"], code, "{args:?}: {v:#}");
        assert_eq!(c, exit, "{args:?}: {v:#}");
        v["error"].clone()
    }
}

fn redact(v: &mut Value, ids: &[(String, &str)]) {
    match v {
        Value::String(s) => {
            for (id, name) in ids {
                if s.contains(id.as_str()) {
                    *s = s.replace(id.as_str(), name);
                }
            }
            if s.len() == 20 && s.ends_with('Z') && s.as_bytes()[10] == b'T' {
                *s = "<ts>".into();
            }
            if s.starts_with('/') {
                *s = "<path>".into();
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| redact(x, ids)),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if k == "host" && !x.is_null() {
                    *x = json!("<host>");
                } else {
                    redact(x, ids);
                }
            }
        }
        _ => {}
    }
}

#[test]
fn issue_lifecycle_and_json_format() {
    let e = Env::new();
    let a = e.create(&["Add claim leases", "-d", "Claims need leases.", "-p", "p1"]);
    let b = e.create(&["-t", "Use leases", "--blocked-by", &a, "--type", "bug"]);
    let v = e.ok(&["show", &a]);
    assert_eq!(v["state"], "open");
    assert_eq!(v["priority"], "P1");
    assert_eq!(v["type"], "change");
    assert!(v.get("agent").is_none());
    assert_eq!(v["deps"]["blocks"][0]["id"], json!(b));
    assert_eq!(v["derived"]["ready"], true);
    assert_eq!(e.ok(&["show", &b])["type"], "bug");
    assert_eq!(e.ok(&["list", "type=bug"])[0]["id"], json!(b));

    // Ready is open with every blocker closed.
    let ready = e.ok(&["ready"]);
    assert_eq!(ready.as_array().unwrap().len(), 1);
    assert_eq!(ready[0]["id"], json!(a));
    e.err(&["claim", &b], 5, "blocked");

    // Edit, note, claim.
    e.ok(&[
        "edit",
        &a,
        "-t",
        "Add leases to claims",
        "--type",
        "research",
    ]);
    e.ok(&["edit", &a, "--type", "change"]);
    assert_eq!(e.ok(&["show", &a])["type"], "change");
    e.ok(&["note", &a, "start with the sweep"]);
    let v = e.ok(&["claim", &a]);
    assert_eq!(v["state"], "claimed");
    assert_eq!(v["claim"]["actor"], "human:alice@example.com");
    assert!(v["claim"]["worktree"].as_str().unwrap().ends_with("/repo"));

    // Wait with a check; the claim is released and the worktree kept.
    let v = e.ok(&[
        "wait",
        &a,
        "--check",
        "true",
        "--every",
        "2m",
        "--reason",
        "merge into main",
    ]);
    assert_eq!(v["state"], "waiting");
    assert_eq!(v["claim"], Value::Null);
    assert_eq!(v["waiting"]["check"], "true");
    assert_eq!(v["waiting"]["interval"], 120);
    assert!(v["waiting"]["cwd"].as_str().unwrap().ends_with("/repo"));
    assert_eq!(v["derived"]["due"], true);
    // A waiting blocker still blocks.
    assert!(e.ok(&["ready"]).as_array().unwrap().is_empty());
    assert_eq!(e.ok(&["list", "due"])[0]["id"], json!(a));

    let mut snap = e.ok(&["show", &a, "--with", "notes,events,claims"]);
    assert_eq!(snap["routing"]["agent"], "implementer");
    // Routing has its own tests; keep this snapshot focused on lifecycle state.
    snap.as_object_mut().unwrap().remove("routing");
    redact(&mut snap, &[(a.clone(), "<A>"), (b.clone(), "<B>")]);
    insta::assert_json_snapshot!("show_waiting", snap);

    // The check passes: closed as done, and the dependent is ready.
    let r = e.ok(&["check"]);
    assert_eq!(r["busy"], false);
    assert_eq!(r["results"][0]["result"], "passed");
    let v = e.ok(&["show", &a]);
    assert_eq!(v["state"], "closed");
    assert_eq!(v["close_reason"], "done");
    assert_eq!(v["waiting"], Value::Null);
    assert_eq!(e.ok(&["ready"])[0]["id"], json!(b));

    // Close by hand with a note; reopen; cancel.
    e.ok(&["claim", &b]);
    e.ok(&["close", &b, "-m", "shipped in 1.2"]);
    let v = e.ok(&["show", &b]);
    assert_eq!(v["notes"][0]["body"], "shipped in 1.2");
    assert_eq!(e.ok(&["reopen", &b, "-m", "regressed"])["state"], "open");
    assert_eq!(
        e.ok(&["close", &b, "--reason", "abandoned"])["close_reason"],
        "abandoned"
    );
    assert_eq!(e.ok(&["list", "reason=abandoned"])[0]["id"], json!(b));
    let log = e.ok(&["log", &b]);
    let ops: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["op"].as_str().unwrap())
        .collect();
    assert_eq!(
        ops,
        [
            "create", "dep_add", "claim", "note", "close", "release", "note", "reopen", "close"
        ]
    );

    // Views, fields, jsonl.
    let rows = e.ok(&["list", "state=closed", "--fields", "id,state,reason"]);
    assert_eq!(rows[0].as_object().unwrap().len(), 3);
    assert!(e.ok(&["list", "@open"]).as_array().unwrap().is_empty());
    let out = e
        .cmd()
        .args(["--format", "jsonl", "list"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 2);
    let who = e.ok(&["whoami"]);
    assert_eq!(who["issues"]["closed"], 2);
    assert_eq!(who["default_type"], "change");
    assert_eq!(who["prefix"], "orch");
    assert_eq!(who["skill"], "current");
    assert!(who["db"].as_str().unwrap().ends_with("/repo/.trk/trk.db"));
    assert!(who.get("project").is_none() && who.get("repo").is_none());
    assert!(who.get("default_agent").is_none());
}

/// A runner and every check group it starts are cleaned up even on assertion
/// failure. The shell publishes its group before starting any descendants.
struct CheckRunner {
    child: std::process::Child,
    group_file: PathBuf,
}

impl CheckRunner {
    fn spawn(e: &Env) -> Self {
        let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
        command
            .args(["--json", "check", "--all"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self::spawn_command(e, command)
    }

    fn spawn_command(e: &Env, mut command: StdCommand) -> Self {
        let child = command
            .current_dir(e.repo())
            .env("HOME", e.dir.path().join("home"))
            .env("TRK_USER_DIR", e.user_dir())
            .env("TRK_ACTOR", "human:alice@example.com")
            .env("TRK_NOW", "2026-11-20T10:00:00Z")
            .env_remove("TRK_FORMAT")
            .env_remove("TRK_DIR")
            .process_group(0)
            .spawn()
            .unwrap();
        Self {
            child,
            group_file: e.repo().join("check-shell"),
        }
    }

    fn ready(&self) -> i64 {
        let mut pgid = None;
        check_wait_until("the check shell to publish its group", || {
            pgid = std::fs::read_to_string(&self.group_file)
                .ok()
                .and_then(|s| s.trim().parse().ok());
            pgid.is_some()
        });
        pgid.unwrap()
    }

    fn signal(&self, signal: &str) {
        assert!(check_signal(i64::from(self.child.id()), signal));
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        let mut status = None;
        check_wait_until("the check runner to exit", || {
            status = self.child.try_wait().unwrap();
            status.is_some()
        });
        status.unwrap()
    }
}

impl Drop for CheckRunner {
    fn drop(&mut self) {
        // Stop the runner first so cleanup cannot race a new group being
        // spawned before the shell has published its readiness file.
        check_signal(i64::from(self.child.id()), "-STOP");
        if let Ok(group) = std::fs::read_to_string(&self.group_file)
            && let Ok(pgid) = group.trim().parse::<i64>()
        {
            check_signal(-pgid, "-KILL");
        }
        // A failed startup can leave a child blocked before its group is
        // persisted or its readiness file exists. The stopped runner cannot
        // spawn any more children while we discover those groups.
        if let Ok(output) = StdCommand::new("ps")
            .args(["-ax", "-o", "ppid=,pgid="])
            .output()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let mut columns = line.split_whitespace();
                let parent = columns.next().and_then(|s| s.parse::<u32>().ok());
                let group = columns.next().and_then(|s| s.parse::<i64>().ok());
                if parent == Some(self.child.id())
                    && let Some(group) = group
                {
                    check_signal(-group, "-KILL");
                }
            }
        }
        // Also cover a failure before the shell's readiness file is written.
        check_signal(-i64::from(self.child.id()), "-KILL");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn check_signal(target: i64, signal: &str) -> bool {
    StdCommand::new("kill")
        .args([signal, "--", &target.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn check_wait_until(description: &str, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn check_group_alive(pgid: i64) -> bool {
    let output = StdCommand::new("ps")
        .args(["-ax", "-o", "pgid=,stat="])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .any(|line| {
            let mut columns = line.split_whitespace();
            columns.next().and_then(|s| s.parse::<i64>().ok()) == Some(pgid)
            // An orphan zombie cannot execute a check. Do not wait on init's
            // platform-dependent reaping schedule.
            && columns.next().is_some_and(|state| !state.starts_with('Z'))
        })
}

fn assert_check_busy(e: &Env, pid: u32, pgid: i64) {
    let second = e.ok(&["check", "--all"]);
    assert_eq!(second["busy"], true, "{second:#}");
    assert!(second["results"].as_array().unwrap().is_empty());
    assert_eq!(second["holder"]["pid"], pid);
    assert_eq!(second["holder"]["pgid"], pgid);
    let holder = &e.ok(&["whoami"])["check_running"];
    assert_eq!(holder["pid"], pid);
    assert_eq!(holder["pgid"], pgid);
}

fn seed_check_holder(e: &Env, pid: i64, host: Option<&str>, started: &str, pgid: Option<i64>) {
    let store =
        trk_core::Store::open(&e.trk_dir().join("trk.db"), trk_core::StoreOpts::default()).unwrap();
    store.conn().execute(
        "INSERT OR REPLACE INTO check_lock (id, pid, host, started_at, pgid) VALUES (1, ?1, ?2, ?3, ?4)",
        (pid, host, started, pgid),
    ).unwrap();
}

#[test]
fn check_recovers_a_recycled_pid_without_touching_the_unrelated_process() {
    let e = Env::new();
    let id = e.waiting(
        "recycled pid",
        "echo ran > \"$TRK_REPO/recovered\"; exit 75",
    );
    // This live test process cannot be the runner from the year 2000.
    seed_check_holder(
        &e,
        i64::from(std::process::id()),
        Some(&local_host()),
        "2000-01-01T00:00:00Z",
        None,
    );
    // Previews use the same identity rules without mutating the lock.
    e.ok(&["gc", "--dry-run"]);
    assert_ne!(e.ok(&["whoami"])["check_running"], Value::Null);
    let recovered = e.ok(&["check", &id]);
    assert_eq!(recovered["busy"], false);
    assert_eq!(recovered["results"][0]["result"], "retry");
    assert_eq!(
        std::fs::read_to_string(e.repo().join("recovered")).unwrap(),
        "ran\n"
    );
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
}

#[test]
fn check_keeps_ambiguous_process_identity_and_live_groups_locked() {
    let e = Env::new();
    let id = e.waiting("protected", "touch \"$TRK_REPO/unsafe\"; exit 75");
    let pid = i64::from(std::process::id());
    let output = StdCommand::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pgid="])
        .output()
        .unwrap();
    assert!(output.status.success());
    let pgid = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse::<i64>()
        .unwrap();
    for (started, group) in [
        ("invalid timestamp", None),
        ("9999-01-01T00:00:00Z", None),
        ("2000-01-01T00:00:00Z", Some(pgid)),
    ] {
        seed_check_holder(&e, pid, Some(&local_host()), started, group);
        assert_eq!(e.ok(&["check", &id])["busy"], true);
        e.err(&["check", "--unlock"], 4, "invalid_state");
        assert!(!e.repo().join("unsafe").exists());
    }
}

#[test]
fn check_unlock_is_human_only_audited_and_recovers_unknown_hosts() {
    let e = Env::new();
    let id = e.waiting(
        "manual recovery",
        "echo ran > \"$TRK_REPO/unlocked\"; exit 75",
    );
    let here = local_host();
    for host in [Some("previous-host.invalid"), None, Some(here.as_str())] {
        // A local absent PID is safe; remote/unknown PIDs cannot be probed.
        let pid = if host == Some(here.as_str()) {
            i64::from(i32::MAX)
        } else {
            i64::from(std::process::id())
        };
        seed_check_holder(&e, pid, host, "2000-01-01T00:00:00Z", None);
        let before = e.ok(&["log"]);
        let holder = e.ok(&["whoami"])["check_running"].clone();
        e.err(
            &["check", "--unlock", "--as", "agent:omp:unlock"],
            5,
            "not_permitted",
        );
        assert_eq!(e.ok(&["whoami"])["check_running"], holder);
        assert_eq!(e.ok(&["log"]), before);
        if host != Some(here.as_str()) {
            assert_eq!(e.ok(&["check", &id])["busy"], true);
            assert!(!e.repo().join("unlocked").exists());
        }
        let unlocked = e.ok(&["check", "--unlock"]);
        assert_eq!(unlocked["unlocked"], true);
        assert_eq!(unlocked["holder"], holder);
        assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
        let log = e.ok(&["log"]);
        let event = log.as_array().unwrap().last().unwrap();
        assert_eq!(event["actor"], "human:alice@example.com");
        assert_eq!(event["issue_id"], Value::Null);
        assert_eq!(event["payload"]["check_unlock"], holder);
    }
    let before = e.ok(&["log"]);
    assert_eq!(e.ok(&["check", "--unlock"])["unlocked"], false);
    assert_eq!(e.ok(&["log"]), before);
    assert_eq!(e.ok(&["check", &id])["results"][0]["result"], "retry");
    assert!(e.repo().join("unlocked").exists());
}

/// The descendant handles TERM without terminating, forcing group-wide
/// escalation. Its readiness file is published only after installing the trap.
const STUBBORN_CHECK: &str = "\
    echo $$ > \"$TRK_REPO/check-shell\"; \
    sh -c 'trap \":\" TERM; echo $$ > \"$TRK_REPO/check-child\"; \
        while [ ! -f \"$TRK_REPO/check-child-release\" ]; do sleep 0.02; done' & \
    child=$!; wait \"$child\"; exit 75";

/// Stateful Herdr boundary: names survive commands just as user renames do.
struct WaitingUi {
    e: Env,
    bin: PathBuf,
}

impl WaitingUi {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let e = Env::new();
        let bin = e.dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        for (name, script) in [
            (
                "herdr-app",
                r#"#!/bin/sh
set -eu
cd "$TRK_TEST_UI"
case "$1" in
ls)
    [ "$TRK_TEST_UI_FAILURE" != unavailable ] || exit 1
    if [ "$TRK_TEST_UI_FAILURE" = missing ]; then
        printf '{"spaces":[]}'
    else
        printf '{"spaces":[
          {"id":"unrelated","name":"Reviewed title","tabs":[{"terminals":[{"terminal":"other"}]}]},
          {"id":"origin","name":"%s","tabs":[{"terminals":[{"terminal":"exact-origin"}]}]},
          {"id":"replacement","name":"Replacement","tabs":[{"terminals":[{"terminal":"new-session"}]}]}
        ]}' "$(cat space-name)"
    fi
    ;;
do)
    [ "$2" = space.rename ] && [ "$3" = --id ] && [ "$4" = origin ]
    [ "$5" = --name ]
    [ "$TRK_TEST_UI_FAILURE" != rename ] || exit 1
    printf '%s' "$6" > space-name
    printf 'renamed\n' >> renames
    ;;
*) exit 1 ;;
esac
"#,
            ),
            (
                "hc",
                r#"#!/bin/sh
set -eu
case "$1" in
send) printf '%s\n%s\n' "$2" "$3" >> "$TRK_TEST_UI/messages" ;;
attach) printf '%s\n' "$2" >> "$TRK_TEST_UI/attachments" ;;
*) exit 1 ;;
esac
"#,
            ),
        ] {
            let path = bin.join(name);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(e.dir.path().join("space-name"), "Reviewed title").unwrap();
        Self { e, bin }
    }

    fn claim(&self, id: &str, terminal: &str, alive: bool) {
        let mut store = trk_core::Store::open(
            &self.e.trk_dir().join("trk.db"),
            trk_core::StoreOpts {
                now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
                ..Default::default()
            },
        )
        .unwrap();
        let actor = format!("agent:omp:{id}-{terminal}");
        store
            .write(&actor, false, |c| {
                c.claim(
                    id,
                    trk_core::ops::ClaimOpts {
                        host: Some(local_host()),
                        terminal: Some(terminal.into()),
                        launcher: Some("herdr".into()),
                        ..Default::default()
                    },
                )?;
                let meta = if alive {
                    json!({"mcp_pid": std::process::id()})
                } else {
                    json!({"mcp_exited_at": "2026-11-20T10:00:00Z"})
                };
                c.patch_actor_meta(&actor, meta)
            })
            .unwrap();
    }

    fn waiting(&self, check: &str, alive: bool) -> String {
        let id = self.e.create(&["Issue title"]);
        self.claim(&id, "exact-origin", alive);
        self.e.ok(&["wait", &id, "--force", "-c", check]);
        id
    }

    fn cmd(&self) -> Command {
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut cmd = self.e.cmd();
        cmd.env("PATH", path)
            .env("TRK_TEST_UI", self.e.dir.path())
            .env("TRK_TEST_UI_FAILURE", "");
        cmd
    }

    fn check(&self, id: &str, failure: &str) -> Value {
        let out = self
            .cmd()
            .env("TRK_TEST_UI_FAILURE", failure)
            .args(["--json", "check", id])
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(out.status.success(), "{v:#}");
        v["data"]["results"][0].clone()
    }

    fn text(&self, name: &str) -> String {
        let path = self.e.dir.path().join(name);
        if !path.exists() {
            return String::new();
        }
        std::fs::read_to_string(path).unwrap()
    }
}
