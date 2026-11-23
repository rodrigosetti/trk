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

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
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
        e.ok(&["install", "--prefix", "orch"]);
        e
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    /// The project's `.trk` directory.
    fn trk_dir(&self) -> PathBuf {
        self.repo().join(".trk")
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("trk").unwrap();
        c.current_dir(self.repo())
            // No ~/.claude here: the skill's Claude Code link is opt-in per test.
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

#[test]
fn list_defaults_to_priority_then_age_and_honors_sort_overrides() {
    let e = Env::new();
    let issue = |now, title, priority| e.at(now, &["create", title, "-p", priority]);
    let older = issue("2026-11-20T10:00:00Z", "Older normal issue", "P2");
    let newer = issue("2026-11-20T11:00:00Z", "Newer normal issue", "P2");
    let urgent = issue("2026-11-20T12:00:00Z", "Urgent issue", "P0");
    let id = |issue: &Value| issue["id"].as_str().unwrap().to_string();
    let mut age_order = vec![older.clone(), newer.clone(), urgent.clone()];
    age_order.sort_by(|a, b| {
        a["created_at"]
            .as_str()
            .cmp(&b["created_at"].as_str())
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let ids = |rows: &Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let mut default_order = vec![id(&urgent)];
    default_order.extend(age_order[..2].iter().map(id));
    assert_eq!(ids(&e.ok(&["list"])), default_order);
    assert_eq!(
        ids(&e.ok(&["list", "--sort", "created"])),
        age_order.iter().map(id).collect::<Vec<_>>()
    );

    e.set_config(
        r#"[views.mine]
query = "claimed-by=me""#,
        r#"[views.mine]
query = "state=open"
sort = "-created""#,
    );
    let mut reverse_age_order = age_order.clone();
    reverse_age_order.sort_by(|a, b| {
        b["created_at"]
            .as_str()
            .cmp(&a["created_at"].as_str())
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let reverse_age_ids = reverse_age_order.iter().map(id).collect::<Vec<_>>();
    assert_eq!(ids(&e.ok(&["list", "@mine"])), reverse_age_ids);
    assert_eq!(
        ids(&e.ok(&["list", "@mine", "--sort", "created"])),
        age_order.iter().map(id).collect::<Vec<_>>()
    );
}

#[test]
fn list_and_ready_filter_and_sort_by_type() {
    let e = Env::new();
    let code = e.create(&["--type", "code-change", "Code change"]);
    let research = e.create(&["--type", "research", "Research"]);
    let review = e.create(&["--type", "review", "Review"]);
    let ids = |rows: &Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        ids(&e.ok(&["list", "type=research"])),
        vec![research.clone()]
    );
    assert_eq!(ids(&e.ok(&["ready", "type=review"])), vec![review.clone()]);
    assert_eq!(
        ids(&e.ok(&["list", "--sort", "type"])),
        vec![code.clone(), research.clone(), review.clone()]
    );
    assert_eq!(
        ids(&e.ok(&["ready", "--sort", "type"])),
        vec![code, research, review]
    );
}

#[test]
fn default_list_and_ready_tables_show_issue_type() {
    let e = Env::new();
    for issue_type in ["code-change", "research", "review"] {
        e.create(&["--type", issue_type, issue_type]);
    }

    for command in ["list", "ready"] {
        let out = e
            .cmd()
            .args(["--format", "pretty", command])
            .output()
            .unwrap();
        assert!(out.status.success());
        let table = String::from_utf8(out.stdout).unwrap();
        let header = table.lines().next().unwrap();
        assert!(
            header.find("PRIORITY").unwrap() < header.find("TYPE").unwrap()
                && header.find("TYPE").unwrap() < header.find("STATE").unwrap(),
            "{table}"
        );
        for issue_type in ["code-change", "research", "review"] {
            assert!(table.contains(issue_type), "{table}");
        }
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
    let b = e.create(&["-t", "Use leases", "--blocked-by", &a, "--type", "review"]);
    let v = e.ok(&["show", &a]);
    assert_eq!(v["state"], "open");
    assert_eq!(v["priority"], "P1");
    assert_eq!(v["type"], "code-change");
    assert!(v.get("agent").is_none());
    assert_eq!(v["deps"]["blocks"][0]["id"], json!(b));
    assert_eq!(v["derived"]["ready"], true);
    assert_eq!(e.ok(&["show", &b])["type"], "review");
    assert_eq!(e.ok(&["list", "type=review"])[0]["id"], json!(b));

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
    e.ok(&["edit", &a, "--type", "code-change"]);
    assert_eq!(e.ok(&["show", &a])["type"], "code-change");
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
    assert_eq!(who["default_type"], "code-change");
    assert_eq!(who["prefix"], "orch");
    assert_eq!(who["skill"], "current");
    assert!(who["db"].as_str().unwrap().ends_with("/repo/.trk/trk.db"));
    assert!(who.get("project").is_none() && who.get("repo").is_none());
    assert!(who.get("default_agent").is_none());
}

#[test]
fn issue_agent_flags_are_rejected() {
    let e = Env::new();
    let id = e.create(&["Type-only issue"]);
    for args in [
        vec!["create", "Old flag", "--agent", "implementer"],
        vec!["edit", &id, "--agent", "reviewer"],
    ] {
        let out = e.cmd().args(&args).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--agent"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn deps_and_imported_ids() {
    let e = Env::new();
    let a = e.create(&["A", "--id", "orch-aaa"]);
    assert_eq!(a, "orch-aaa");
    let b = e.create(&["B"]);
    let c = e.create(&["C", "--blocks", &a]);
    assert_eq!(e.ok(&["dep", "add", "aaa", &b])["added"], true);
    assert_eq!(e.ok(&["dep", "add", &a, &b])["added"], false);
    e.err(&["dep", "add", &b, &c], 4, "cycle");
    assert_eq!(
        e.ok(&["list", &format!("blocked-by={a}")])[0]["id"],
        json!(b)
    );
    assert_eq!(e.ok(&["list", "blocked"]).as_array().unwrap().len(), 2);
    e.ok(&["dep", "rm", &a, &b]);
    e.err(&["dep", "rm", &a, &b], 3, "not_found");
    e.err(&["create", "X", "--id", "other-1"], 2, "bad_args");
    e.err(&["create", "X", "--id", "orch-aaa"], 2, "bad_args");
}

#[test]
fn checks_pass_retry_and_fail() {
    let e = Env::new();
    // Not yet: exit 75 keeps it waiting, with no event, and it is not due again
    // until the interval has passed.
    let slow = e.waiting("slow", "echo still open; exit 75");
    let before = e.ok(&["log", &slow]).as_array().unwrap().len();
    let r = e.ok(&["check"]);
    assert_eq!(r["results"][0]["result"], "retry");
    assert_eq!(r["results"][0]["exit"], 75);
    let v = e.ok(&["show", &slow]);
    assert_eq!(v["state"], "waiting");
    assert_eq!(v["waiting"]["last_check_exit"], 75);
    assert_eq!(v["waiting"]["next_check_at"], "2026-11-20T10:05:00Z");
    assert_eq!(e.ok(&["log", &slow]).as_array().unwrap().len(), before);
    assert!(e.ok(&["check"])["results"].as_array().unwrap().is_empty());
    assert_eq!(e.ok(&["check", "--all"])["results"][0]["result"], "retry");
    assert_eq!(
        e.at("2026-11-20T10:06:00Z", &["check"])["results"][0]["issue"],
        json!(slow)
    );

    // The dry run shows the command and where it would run.
    let d = e.ok(&["check", "--all", "--dry-run"]);
    assert_eq!(d["results"][0]["result"], "would_run");
    assert_eq!(d["results"][0]["check"], "echo still open; exit 75");
    assert!(d["results"][0]["cwd"].as_str().unwrap().ends_with("/repo"));

    // A failure returns the issue to open with the output and the
    // environment the check saw.
    let bad = e.waiting(
        "bad",
        "echo boom; echo \"$TRK_ISSUE $TRK_BRANCH $(basename \"$TRK_REPO\") $(pwd | xargs basename)\"; exit 3",
    );
    let r = e.ok(&["check", &bad]);
    assert_eq!(r["results"][0]["result"], "failed");
    assert_eq!(r["results"][0]["exit"], 3);
    assert!(r["results"][0]["output"].as_str().unwrap().contains("boom"));
    let v = e.ok(&["show", &bad]);
    assert_eq!(v["state"], "open");
    assert_eq!(v["derived"]["ready"], true);
    let note = v["notes"].as_array().unwrap().last().unwrap()["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(note.contains("exited with 3"), "{note}");
    assert!(note.contains(&format!("{bad} main repo repo")), "{note}");
    // There is nothing left to check on it.
    e.err(&["check", &bad], 4, "invalid_state");

    // A command that cannot run is a failure, not a wait.
    let missing = e.waiting("missing", "definitely-not-a-command-xyz");
    assert_eq!(e.ok(&["check", &missing])["results"][0]["exit"], 127);
    assert_eq!(e.ok(&["show", &missing])["state"], "open");

    // A worktree that has gone cannot be checked in.
    let gone = e.create(&["gone"]);
    let wt = e.dir.path().join("vanished");
    std::fs::create_dir_all(&wt).unwrap();
    e.ok(&["claim", &gone, "--worktree", wt.to_str().unwrap()]);
    e.ok(&["wait", &gone, "-c", "true"]);
    std::fs::remove_dir_all(&wt).unwrap();
    let r = e.ok(&["check", &gone]);
    assert_eq!(r["results"][0]["result"], "failed");
    assert!(
        r["results"][0]["output"]
            .as_str()
            .unwrap()
            .contains("no longer exists")
    );
}

#[test]
fn waiting_on_a_human_has_no_check() {
    let e = Env::new();
    let id = e.create(&["Pick a database"]);
    e.err(&["wait", &id, "-r", "which one?"], 4, "invalid_state");
    e.ok(&["claim", &id]);
    e.err(&["wait", &id], 4, "invalid_field");
    let v = e.ok(&["wait", &id, "-r", "SQLite or Dolt?"]);
    assert_eq!(v["waiting"]["check"], Value::Null);
    assert_eq!(v["derived"]["due"], false);
    assert!(
        e.ok(&["check", "--all"])["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    e.err(&["check", &id], 4, "invalid_state");
    e.err(&["claim", &id], 4, "invalid_state");
    // A human replaces the wait with a check, or answers and reopens.
    let v = e.ok(&["wait", &id, "-c", "exit 75", "-r", "now automated"]);
    assert_eq!(v["waiting"]["check"], "exit 75");
    assert_eq!(v["waiting"]["interval"], 300);
    assert_eq!(e.ok(&["reopen", &id, "-m", "SQLite"])["state"], "open");
}

#[test]
fn a_check_that_runs_too_long_is_killed() {
    let e = Env::new();
    e.set_config("timeout  = \"10m\"", "timeout  = \"1s\"");
    let id = e.waiting("hangs", "echo started; sleep 30; exit 0");
    let t = std::time::Instant::now();
    let r = e.ok(&["check"]);
    assert!(t.elapsed().as_secs() < 15, "the check was not killed");
    assert_eq!(r["results"][0]["result"], "failed");
    assert_eq!(r["results"][0]["timed_out"], true);
    let v = e.ok(&["show", &id]);
    assert_eq!(v["state"], "open");
    let note = v["notes"].as_array().unwrap().last().unwrap()["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(note.contains("timed out"), "{note}");
    assert!(note.contains("started"), "{note}");
}

#[test]
fn only_one_check_runs_at_a_time() {
    let e = Env::new();
    let id = e.waiting(
        "slow merge",
        "echo $$ > \"$TRK_REPO/check-shell\"; \
         while [ ! -f \"$TRK_REPO/check-release\" ]; do sleep 0.02; done; exit 75",
    );
    let mut first = CheckRunner::spawn(&e);
    let pgid = first.ready();

    // A broken lock must fail through a side effect, not hang a second runner
    // on the first command's readiness gate.
    e.ok(&[
        "wait",
        &id,
        "-c",
        "echo second > \"$TRK_REPO/check-second\"; exit 75",
    ]);
    assert_check_busy(&e, first.child.id(), pgid);
    assert!(!e.repo().join("check-second").exists());
    std::fs::write(e.repo().join("check-release"), "").unwrap();
    assert!(first.wait().success());
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
    assert_eq!(e.ok(&["check", "--all"])["results"][0]["result"], "retry");
    assert!(e.repo().join("check-second").exists());
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

/// The descendant handles TERM without terminating, forcing group-wide
/// escalation. Its readiness file is published only after installing the trap.
const STUBBORN_CHECK: &str = "\
    echo $$ > \"$TRK_REPO/check-shell\"; \
    sh -c 'trap \":\" TERM; echo $$ > \"$TRK_REPO/check-child\"; \
        while [ ! -f \"$TRK_REPO/check-child-release\" ]; do sleep 0.02; done' & \
    child=$!; wait \"$child\"; exit 75";

fn check_interruption(signal: &str) {
    let e = Env::new();
    let id = e.waiting("interrupted check", STUBBORN_CHECK);
    let before = e.ok(&["show", &id]);
    let log_before = e.ok(&["log", &id]);
    let mut runner = CheckRunner::spawn(&e);
    let pgid = runner.ready();
    check_wait_until("the TERM-resistant descendant to start", || {
        std::fs::read_to_string(e.repo().join("check-child"))
            .is_ok_and(|s| s.trim().parse::<i64>().is_ok())
    });
    runner.signal(signal);
    runner.wait();
    assert!(
        !check_group_alive(pgid),
        "a check descendant survived {signal}"
    );
    let after = e.ok(&["show", &id]);
    assert_eq!(after["state"], "waiting");
    assert_eq!(after["waiting"], before["waiting"]);
    assert_eq!(after["notes"], before["notes"]);
    assert_eq!(e.ok(&["log", &id]), log_before);
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);

    e.ok(&[
        "wait",
        &id,
        "-c",
        "echo second > \"$TRK_REPO/check-second\"; exit 75",
    ]);
    assert_eq!(e.ok(&["check", "--all"])["results"][0]["result"], "retry");
    assert!(e.repo().join("check-second").exists());
}

#[test]
fn check_sigint_cleans_up_without_recording_a_result() {
    check_interruption("-INT");
}

#[test]
fn check_sigterm_cleans_up_without_recording_a_result() {
    check_interruption("-TERM");
}

#[test]
fn check_sighup_cleans_up_without_recording_a_result() {
    check_interruption("-HUP");
}

#[test]
fn check_sigkill_keeps_ownership_until_the_old_group_is_gone() {
    let e = Env::new();
    let id = e.waiting("crashed check", STUBBORN_CHECK);
    let before = e.ok(&["show", &id]);
    let mut runner = CheckRunner::spawn(&e);
    let pgid = runner.ready();
    check_wait_until("the crash survivor to start", || {
        std::fs::read_to_string(e.repo().join("check-child"))
            .is_ok_and(|s| s.trim().parse::<i64>().is_ok())
    });
    runner.signal("-KILL");
    assert!(!runner.wait().success());
    assert!(check_group_alive(pgid));
    let after = e.ok(&["show", &id]);
    assert_eq!(after["state"], "waiting");
    assert_eq!(after["waiting"], before["waiting"]);

    e.ok(&[
        "wait",
        &id,
        "-c",
        "echo second > \"$TRK_REPO/check-second\"; exit 75",
    ]);
    assert_check_busy(&e, runner.child.id(), pgid);
    assert!(!e.repo().join("check-second").exists());

    // Let the original shell reap its descendant itself. Recovery must not
    // depend on how quickly init happens to reap orphaned background children.
    std::fs::write(e.repo().join("check-child-release"), "").unwrap();
    check_wait_until("the crashed runner's check group to finish", || {
        !check_group_alive(pgid)
    });
    let recovered = e.ok(&["check", "--all"]);
    assert_eq!(recovered["busy"], false, "{recovered:#}");
    assert_eq!(recovered["results"][0]["result"], "retry");
    assert!(e.repo().join("check-second").exists());
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
}

#[test]
fn check_shell_exit_cleans_up_surviving_descendants_before_release() {
    let e = Env::new();
    let check = STUBBORN_CHECK.replace(
        "child=$!; wait \"$child\"; exit 75",
        "while [ ! -f \"$TRK_REPO/check-release\" ]; do sleep 0.02; done; exit 75",
    );
    let id = e.waiting("background check", &check);
    let mut runner = CheckRunner::spawn(&e);
    let pgid = runner.ready();
    check_wait_until("the background descendant to start", || {
        std::fs::read_to_string(e.repo().join("check-child"))
            .is_ok_and(|s| s.trim().parse::<i64>().is_ok())
    });
    e.ok(&[
        "wait",
        &id,
        "-c",
        "echo second > \"$TRK_REPO/check-second\"; exit 75",
    ]);
    assert_check_busy(&e, runner.child.id(), pgid);
    assert!(!e.repo().join("check-second").exists());
    std::fs::write(e.repo().join("check-release"), "").unwrap();
    assert!(runner.wait().success());
    assert!(
        !check_group_alive(pgid),
        "the background check survived release"
    );
    assert_eq!(e.ok(&["show", &id])["state"], "waiting");
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
    assert_eq!(e.ok(&["check", "--all"])["results"][0]["result"], "retry");
    assert!(e.repo().join("check-second").exists());
}

#[test]
fn errors_and_exit_codes() {
    let e = Env::new();
    let id = e.create(&["Thing"]);
    e.err(&["show", "nope-zzz"], 3, "not_found");
    e.err(&["list", "bogus"], 2, "bad_query");
    e.err(&["list", "agent=implementer"], 2, "bad_query");
    e.err(&["create", "X", "-p", "P7"], 4, "invalid_field");
    e.err(&["create", "X", "--type", ""], 4, "invalid_field");
    e.err(&["edit", &id, "--type", ""], 4, "invalid_field");
    e.err(&["close", &id, "--reason", "merged"], 4, "invalid_field");
    e.err(&["unclaim", &id], 4, "invalid_state");
    e.err(&["reopen", &id], 4, "invalid_state");
    e.err(
        &["wait", &id, "-c", "true", "--every", "soon"],
        2,
        "bad_args",
    );
    e.ok(&["claim", &id, "--as", "agent:omp:s1"]);
    let err = e.err(&["claim", &id], 5, "claimed_by_other");
    assert_eq!(err["claimant"], "agent:omp:s1");
    e.err(&["close", &id], 5, "not_claimant");
    e.err(
        &["note", &id, "x", "--as", "agent:omp:s2"],
        5,
        "not_permitted",
    );
    e.err(
        &["create", "Stray", "--as", "agent:omp:s1"],
        5,
        "not_permitted",
    );
    // --force is recorded.
    e.ok(&["close", &id, "--force"]);
    let log = e.ok(&["log", &id]);
    let close = log
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["op"] == "close")
        .unwrap();
    assert_eq!(close["payload"]["forced"], true);
    // --dry-run changes nothing.
    let other = e.create(&["Other"]);
    assert_eq!(e.ok(&["close", &other, "--dry-run"])["state"], "closed");
    assert_eq!(e.ok(&["show", &other])["state"], "open");
    // Ambiguous id prefixes are refused.
    e.create(&["P", "--id", "orch-ab1"]);
    e.create(&["Q", "--id", "orch-ab2"]);
    let err = e.err(&["show", "ab"], 3, "ambiguous_id");
    assert_eq!(err["candidates"].as_array().unwrap().len(), 2);
    // Without --json on a non-TTY the output is still JSON.
    let out = e.cmd().args(["show", "nope-zzz"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
}

#[test]
fn commands_require_an_installed_project() {
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let trk = |args: &[&str]| {
        let out = Command::cargo_bin("trk")
            .unwrap()
            .current_dir(&bare)
            .env("HOME", dir.path().join("home"))
            .env("TRK_ACTOR", "human:alice@example.com")
            .env_remove("TRK_DIR")
            .env_remove("TRK_FORMAT")
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        (out.status.code().unwrap(), v)
    };
    for args in [
        &["list"][..],
        &["create", "x"],
        &["whoami"],
        &["agent", "list"],
        &["check"],
    ] {
        let (code, v) = trk(args);
        assert_eq!(code, 3, "{args:?}: {v:#}");
        assert_eq!(v["error"]["code"], "not_installed", "{args:?}");
        assert!(
            v["error"]["hint"].as_str().unwrap().contains("trk install"),
            "{v:#}"
        );
    }
    // Nothing was created by the refused commands.
    assert!(!bare.join(".trk").exists());

    // Outside git, install uses the working directory; the prefix comes
    // from its name.
    let (code, v) = trk(&["install"]);
    assert_eq!(code, 0, "{v:#}");
    assert_eq!(v["data"]["prefix"], "bare");
    assert_eq!(v["data"]["db_created"], true);
    assert!(bare.join(".trk/trk.db").is_file());
    let (_, v) = trk(&["create", "First"]);
    assert!(v["data"]["id"].as_str().unwrap().starts_with("bare-"));
    // The removed commands and scopes are gone.
    for args in [
        &["init"][..],
        &["project", "list"],
        &["repo", "list"],
        &["skill", "install"],
        &["list", "--all-projects"],
        &["--project", "x", "list"],
    ] {
        let out = Command::cargo_bin("trk")
            .unwrap()
            .current_dir(&bare)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    let (code, v) = trk(&["list", "project=bare"]);
    assert_eq!((code, v["error"]["code"].as_str()), (2, Some("bad_query")));
}

#[test]
fn install_writes_the_project_directory_and_the_skill() {
    let e = Env::new();
    let repo = e.repo();
    let dir = e.trk_dir();
    // Everything is in .trk/, written explicitly, and ignored by git.
    for f in [
        ".gitignore",
        "trk.db",
        "config.toml",
        "types/code-change.toml",
        "agents/implementer.toml",
        "prompts/briefing.md.j2",
    ] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    for setting in [
        "prefix = \"orch\"",
        "email = \"alice@example.com\"",
        "agent_lease = \"30m\"",
        "interval = \"5m\"",
        "timeout  = \"10m\"",
        "worktrees = \".trk/worktrees\"",
    ] {
        assert!(config.contains(setting), "config.toml lacks {setting}");
    }
    // The skill comes from the binary and is kept out of git.
    let skill = repo.join(".agents/skills/trk");
    let installed = std::fs::read_to_string(skill.join("SKILL.md")).unwrap();
    assert!(installed.starts_with("---\nname: trk\n"), "{installed}");
    assert!(skill.join("references/reference.md").is_file());
    assert!(!repo.join(".claude").exists(), "no Claude Code here");
    let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("/.agents/skills/trk/"), "{exclude}");
    let status = StdCommand::new("git")
        .current_dir(&repo)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");

    // Again: config is kept, edits are reported, the skill is refreshed.
    let id = e.create(&["Survives a second install"]);
    std::fs::write(skill.join("SKILL.md"), "stale").unwrap();
    std::fs::write(dir.join("prompts/briefing.md.j2"), "mine").unwrap();
    assert_eq!(e.ok(&["whoami"])["skill"], "outdated");
    let v = e.ok(&["install"]);
    assert_eq!(v["db_created"], false);
    assert_eq!(v["written"], json!([]));
    assert_eq!(v["differ"], json!(["prompts/briefing.md.j2"]));
    assert_eq!(v["skill"]["state"], "current");
    assert_eq!(e.ok(&["show", &id])["id"], json!(id));
    assert_eq!(
        std::fs::read_to_string(dir.join("prompts/briefing.md.j2")).unwrap(),
        "mine"
    );
    // A prefix is only taken when the config is written.
    e.err(&["install", "--prefix", "other"], 2, "bad_args");
    e.err(
        &["install", "--prefix", "Bad-1", "--force"],
        4,
        "config_invalid",
    );
    let v = e.ok(&["install", "--force", "--prefix", "other"]);
    assert!(
        v["written"]
            .as_array()
            .unwrap()
            .contains(&json!("prompts/briefing.md.j2"))
    );
    assert!(e.create(&["New prefix"]).starts_with("other-"));
    assert_eq!(e.ok(&["show", &id])["id"], json!(id), "old ids remain");

    // With Claude Code present, its skills directory links to the skill.
    std::fs::create_dir_all(e.dir.path().join("home/.claude")).unwrap();
    let v = e.ok(&["install"]);
    assert_eq!(v["skill"]["claude_linked"], true);
    assert_eq!(
        std::fs::read_link(repo.join(".claude/skills/trk")).unwrap(),
        PathBuf::from("../../.agents/skills/trk")
    );
}

#[test]
fn the_project_is_found_from_subdirectories_worktrees_and_trk_dir() {
    let e = Env::new();
    let id = e.create(&["Visible everywhere"]);
    let sub = e.repo().join("a/b");
    std::fs::create_dir_all(&sub).unwrap();
    // A worktree outside the project does not contain the ignored .trk/.
    let outside = e.dir.path().join("outside");
    assert!(
        StdCommand::new("git")
            .current_dir(e.repo())
            .args(["worktree", "add", "-q", "-b", "side"])
            .arg(&outside)
            .status()
            .unwrap()
            .success()
    );
    assert!(!outside.join(".trk").exists());
    for cwd in [&sub, &outside] {
        let out = e
            .cmd()
            .current_dir(cwd)
            .args(["--json", "list"])
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["data"][0]["id"], json!(id), "{}: {v:#}", cwd.display());
    }
    // Installing from a worktree or a subdirectory targets the same project.
    let out = e
        .cmd()
        .current_dir(&outside)
        .args(["--json", "install"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["db_created"], false, "{v:#}");
    assert!(!outside.join(".trk").exists());
    // TRK_DIR reaches it from an unrelated directory.
    let out = e
        .cmd()
        .current_dir(e.dir.path())
        .env("TRK_DIR", e.trk_dir())
        .args(["--json", "show", &id])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = e
        .cmd()
        .current_dir(e.dir.path())
        .args(["--json", "show", &id])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}
#[test]
fn pretty_output() {
    let e = Env::new();
    let a = e.waiting("Add claim leases", "git merge --no-edit no-such-branch");
    let b = e.create(&["Enable leases", "--blocked-by", &a]);
    e.ok(&[
        "claim",
        &e.create(&["In flight"]),
        "--as",
        "agent:omp:12938192381",
    ]);
    let human = e.create(&["Pick a store"]);
    e.ok(&["claim", &human]);
    e.ok(&["wait", &human, "-r", "SQLite or Dolt?"]);
    let out = e
        .cmd()
        .args(["--format", "pretty", "list", "--sort", "created"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("ID"), "{text}");
    assert!(text.contains("◷ waiting"), "{text}");
    assert!(text.contains("testing · check now"), "{text}");
    assert!(text.contains("⛔ blocked (0/1 done)"), "{text}");
    assert!(text.contains("● claimed"), "{text}");
    assert!(text.contains("omp:1293…"), "{text}");
    assert!(text.contains("SQLite or Dolt? · needs a human"), "{text}");
    let out = e
        .cmd()
        .args(["--format", "pretty", "show", &a])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    // The check is shown in full: it runs unattended.
    assert!(
        text.contains("check  git merge --no-edit no-such-branch"),
        "{text}"
    );
    assert!(text.contains("every  5m"), "{text}");
    assert!(text.contains(&format!("blocks     {b}")), "{text}");
    let out = e
        .cmd()
        .args(["--format", "pretty", "show", &human])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("no check: a human resolves it"), "{text}");
    let out = e
        .cmd()
        .args(["--format", "pretty", "check"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains(&format!("✗ {a}  failed")), "{text}");
    let out = e
        .cmd()
        .args(["--format", "pretty", "log", &a])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("waiting for testing; check every 5m: git merge"),
        "{text}"
    );
    assert!(text.contains("check failed (exit"), "{text}");
    assert!(
        text.contains("reopened from waiting by its check"),
        "{text}"
    );
    let out = e
        .cmd()
        .args(["--format", "pretty", "close", &b, "--reason", "nope"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("error: invalid close reason 'nope'"), "{err}");
    assert!(err.contains("allowed: done, abandoned"), "{err}");
}

#[test]
fn create_run_launches_the_new_issue_and_preserves_it_on_launch_failure() {
    let e = Env::new();
    std::fs::write(
        e.trk_dir().join("config.toml"),
        r#"
[issues]
prefix = "orch"
[claims]
agent_lease = "30m"
[check]
interval = "5m"
timeout = "10m"
[run]
worktrees = ".trk/worktrees"
[launcher]
start = ["sh", "-c", "exec sh -c \"$1\"", "trk-launch", "{shell_command}"]
[harness.omp]
command = "sh"
args = ["-c", "printf '%s\\n' \"$1\"", "trk-harness", "{briefing}"]
builtin_tools = { read-only = [] }
"#,
    )
    .unwrap();
    let older = e.create(&["Older urgent issue", "-p", "P0"]);
    let launched = e.ok(&[
        "create",
        "Review this change",
        "-d",
        "Check the edge cases.",
        "--type",
        "review",
        "-p",
        "P3",
        "--run",
    ]);
    let id = launched["issue"].as_str().unwrap();
    let issue = e.ok(&["show", id]);
    assert_eq!(issue["title"], "Review this change");
    assert_eq!(issue["state"], "claimed");
    assert_eq!(issue["claim"]["actor"], launched["actor"]);
    assert_eq!(issue["claim"]["worktree"], launched["worktree"]);
    assert_eq!(issue["claim"]["agent"], "reviewer");
    assert!(
        launched["launcher_output"]
            .as_str()
            .unwrap()
            .contains("Check the edge cases.")
    );
    assert_eq!(e.ok(&["show", &older])["state"], "open");

    e.set_config(
        r#"start = ["sh", "-c", "exec sh -c \"$1\"", "trk-launch", "{shell_command}"]"#,
        r#"start = ["sh", "-c", "echo unavailable >&2; exit 23"]"#,
    );
    let err = e.err(&["create", "Retry launch", "--run"], 1, "internal");
    let id = err["issue"].as_str().unwrap();
    let issue = e.ok(&["show", id]);
    assert_eq!(issue["title"], "Retry launch");
    assert_eq!(issue["state"], "open");
    assert!(issue["claim"].is_null());
    assert!(err["message"].as_str().unwrap().contains("unavailable"));
}

#[test]
fn create_run_respects_blockers_and_rejects_dry_run_without_creating() {
    let e = Env::new();
    let blocker = e.create(&["Blocker"]);
    let err = e.err(
        &["create", "Blocked", "--blocked-by", &blocker, "--run"],
        5,
        "blocked",
    );
    let id = err["issue"].as_str().unwrap();
    let issue = e.ok(&["show", id]);
    assert_eq!(issue["title"], "Blocked");
    assert_eq!(issue["state"], "open");
    assert_eq!(issue["deps"]["blocked_by"][0]["id"], blocker);
    assert!(!e.trk_dir().join("worktrees").exists());

    e.cmd()
        .args(["create", "Must not persist", "--run", "--dry-run"])
        .assert()
        .code(2);
    assert!(
        e.ok(&["list"])
            .as_array()
            .unwrap()
            .iter()
            .all(|issue| issue["title"] != "Must not persist")
    );
}

#[test]
fn run_without_id_selects_ready_work_by_priority_and_age() {
    let e = Env::new();
    e.create(&["Older normal issue", "-p", "P2"]);
    let blocker = e.create(&["Blocker", "-p", "P3"]);
    e.create(&["Blocked urgent issue", "-p", "P0", "--blocked-by", &blocker]);
    let claimed = e.create(&["Claimed urgent issue", "-p", "P0"]);
    e.ok(&["claim", &claimed]);
    let closed = e.create(&["Closed urgent issue", "-p", "P0"]);
    e.ok(&["close", &closed]);
    let waiting = e.create(&["Waiting urgent issue", "-p", "P0"]);
    e.ok(&["claim", &waiting]);
    e.ok(&["wait", &waiting, "--reason", "Needs a decision"]);
    let older = e.at(
        "2026-11-20T11:00:00Z",
        &["create", "Older high-priority issue", "-p", "P1"],
    );
    let newer = e.at(
        "2026-11-20T12:00:00Z",
        &["create", "Newer high-priority issue", "-p", "P1"],
    );
    let id = older["id"].as_str().unwrap();

    let preview = e.ok(&["run", "--harness", "omp", "--dry-run"]);
    assert_eq!(preview["issue"], older["id"]);
    assert_eq!(e.ok(&["show", id])["state"], "open");
    assert!(!Path::new(preview["worktree"].as_str().unwrap()).exists());
    let launched = e.ok(&["run", "--harness", "omp", "--print"]);
    assert_eq!(launched["issue"], older["id"]);
    assert_eq!(e.ok(&["show", id])["state"], "claimed");
    assert!(Path::new(launched["worktree"].as_str().unwrap()).exists());
    assert_eq!(
        e.ok(&["run", "--harness", "omp", "--dry-run"])["issue"],
        newer["id"]
    );
}

#[test]
fn agent_preview_and_run_dry_run() {
    let e = Env::new();
    let list = e.ok(&["agent", "list"]);
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(list[0]["name"], "implementer");
    assert!(list[0].get("default").is_none());
    let p = e.ok(&["agent", "preview", "implementer"]);
    assert!(
        p["instructions"]
            .as_str()
            .unwrap()
            .contains("implementer agent")
    );
    let names: Vec<&str> = p["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "get_task",
            "add_note",
            "report_followup",
            "hand_back",
            "close",
            "wait"
        ]
    );
    let id = e.create(&["Fix", "-d", "The thing is broken."]);
    e.ok(&["note", &id, "The waiting check exited with 1"]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--dry-run"]);
    assert_eq!(spec["agent"], "implementer");
    assert_eq!(spec["argv"][0], "omp");
    assert!(
        spec["worktree"]
            .as_str()
            .unwrap()
            .ends_with(&format!("worktrees/{id}"))
    );
    let briefing = spec["briefing"].as_str().unwrap();
    assert!(briefing.contains("The thing is broken."), "{briefing}");
    // A returning issue explains itself through its notes.
    assert!(
        briefing.contains("The waiting check exited with 1"),
        "{briefing}"
    );
    assert!(spec["actor"].as_str().unwrap().starts_with("agent:omp:"));
    // Nothing was claimed or created.
    assert_eq!(e.ok(&["show", &id])["claim"], Value::Null);
    assert!(!Path::new(spec["worktree"].as_str().unwrap()).exists());
    // The issue type selects its agent; --agent overrides that routing.
    e.ok(&["edit", &id, "--type", "review"]);
    assert_eq!(
        e.ok(&["run", &id, "--harness", "omp", "--dry-run"])["agent"],
        "reviewer"
    );
    assert_eq!(
        e.ok(&[
            "run",
            &id,
            "--harness",
            "omp",
            "--agent",
            "researcher",
            "--dry-run"
        ])["agent"],
        "researcher"
    );
    e.ok(&["edit", &id, "--type", "code-change"]);
    // --print claims, creates the worktree on a branch named after the issue,
    // and writes the files.
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let wt = PathBuf::from(spec["worktree"].as_str().unwrap());
    assert!(wt.join(".omp/mcp.json").exists());
    let exclude = std::fs::read_to_string(e.repo().join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("/.omp/mcp.json"));
    let branch = StdCommand::new("git")
        .current_dir(&wt)
        .args(["branch", "--show-current"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), id);
    let v = e.ok(&["show", &id]);
    assert_eq!(v["claim"]["agent"], "implementer");
    assert_eq!(v["state"], "claimed");
    assert_eq!(e.ok(&["runs"])[0]["issue"], json!(id));
    e.err(
        &["run", &id, "--harness", "omp", "--dry-run"],
        5,
        "claimed_by_other",
    );
    // An issue that is not open cannot be run.
    let done = e.create(&["Done"]);
    e.ok(&["close", &done]);
    e.err(&["run", &done, "--dry-run"], 4, "invalid_state");
}

#[test]
fn live_mcp_refuses_a_newer_database_without_mutating_the_issue() {
    let e = Env::new();
    let id = e.create(&["Preserve active session"]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let path = e.trk_dir().join("trk.db");
    let opts = trk_core::StoreOpts {
        now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
        prefix: "orch".into(),
        ..Default::default()
    };
    let server = trk_mcp::TrkServer::new(trk_mcp::McpArgs {
        db: path.clone(),
        config: trk_core::config::ConfigDir { path: e.trk_dir() },
        opts: opts.clone(),
        issue: id.clone(),
        actor: spec["actor"].as_str().unwrap().into(),
        agent: "implementer".into(),
    })
    .unwrap();
    let (ok, before) = server.call_sync("get_task", json!({}));
    assert!(ok, "{before}");
    let store = trk_core::Store::open(&path, opts).unwrap();
    let current: u32 = store
        .conn()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    let future = current + 1;
    // A deterministic additive future migration while the MCP stays alive.
    store
        .conn()
        .execute_batch(&format!(
            "BEGIN IMMEDIATE;
             CREATE TABLE future_data (body TEXT NOT NULL);
             INSERT INTO future_data VALUES ('preserve future data');
             PRAGMA user_version = {future};
             COMMIT;"
        ))
        .unwrap();
    for (tool, args) in [
        ("get_task", json!({})),
        ("hand_back", json!({"note": "must not be written"})),
    ] {
        let (ok, error) = server.call_sync(tool, args);
        assert!(!ok, "{error}");
        assert_eq!(error["code"], "newer_database");
        assert_eq!(error["database"], path.to_str().unwrap());
        assert_eq!(error["database_version"], future);
        assert_eq!(error["supported_version"], current);
        let hint = error["hint"].as_str().unwrap();
        assert!(hint.contains("restart") && hint.contains("MCP") && hint.contains("get_task"));
    }
    e.err(&["show", &id], 1, "newer_database");
    let startup = e
        .cmd()
        .args([
            "mcp",
            "--issue",
            &id,
            "--actor",
            spec["actor"].as_str().unwrap(),
            "--agent",
            "implementer",
        ])
        .output()
        .unwrap();
    assert_eq!(startup.status.code(), Some(1));
    assert!(startup.stdout.is_empty(), "MCP stdout is protocol-only");
    let stderr = String::from_utf8(startup.stderr).unwrap();
    assert!(stderr.contains("SQLite migration version"), "{stderr}");
    assert!(
        stderr.contains("restart") && stderr.contains("MCP"),
        "{stderr}"
    );
    let after = store
        .read(None, |c| {
            c.show(
                &id,
                trk_core::view::With {
                    deps: true,
                    notes: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(after["claim"], before["claim"]);
    assert_eq!(after["state"], "claimed");
    assert_eq!(after["notes"], before["notes"]);
    assert_eq!(
        store
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        future
    );
    assert_eq!(
        store
            .conn()
            .query_row("SELECT body FROM future_data", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "preserve future data"
    );
}

/// Talk to `trk mcp` over stdio like a harness would: the session ends by
/// waiting on a check that merges its branch, and `trk check` lands it.
#[test]
fn mcp_server_over_stdio() {
    let e = Env::new();
    let id = e.create(&["Add a file", "-d", "Add hello.txt."]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let actor = spec["actor"].as_str().unwrap().to_string();
    let wt = PathBuf::from(spec["worktree"].as_str().unwrap());
    e.ok(&["note", &id, "keep it small", "--to-agent"]);
    let bin = assert_cmd::cargo::cargo_bin("trk");
    let mut child = StdCommand::new(bin)
        .args([
            "mcp",
            "--issue",
            &id,
            "--actor",
            &actor,
            "--agent",
            "implementer",
        ])
        // As the harness starts it: from anywhere, with TRK_DIR.
        .env("TRK_DIR", e.trk_dir())
        .env("HOME", e.dir.path().join("home"))
        .env("TRK_NOW", "2026-11-20T10:00:00Z")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut n = 0;
    let mut req = |method: &str, params: Value| -> Value {
        n += 1;
        let msg = json!({"jsonrpc": "2.0", "id": n, "method": method, "params": params});
        writeln!(stdin, "{msg}").unwrap();
        loop {
            let mut line = String::new();
            assert!(stdout.read_line(&mut line).unwrap() > 0, "server closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(n) {
                if method == "initialize" {
                    writeln!(
                        stdin,
                        "{}",
                        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
                    )
                    .unwrap();
                }
                return v;
            }
        }
    };
    let init = req(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "trk");
    let rules = init["result"]["instructions"].as_str().unwrap();
    assert!(
        rules.contains(&id) && rules.contains("Exit 75: not yet"),
        "{rules}"
    );
    let tools = req("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "get_task",
            "add_note",
            "report_followup",
            "hand_back",
            "close",
            "wait"
        ]
    );
    let call =
        |req: &mut dyn FnMut(&str, Value) -> Value, name: &str, args: Value| -> (bool, Value) {
            let r = req("tools/call", json!({"name": name, "arguments": args}));
            let res = &r["result"];
            let text = res["content"][0]["text"].as_str().unwrap();
            (
                res["isError"] == json!(true),
                serde_json::from_str(text).unwrap(),
            )
        };
    let (err, v) = call(&mut req, "add_note", json!({"body": "working"}));
    assert!(!err);
    assert_eq!(v["messages_for_you"][0]["body"], "keep it small");
    assert_eq!(v["task"]["state"], "claimed");
    let (err, task) = call(&mut req, "get_task", json!({}));
    assert!(!err, "{task}");
    assert_eq!(task["type"], "code-change");
    assert_eq!(task["routing"]["agent"], "implementer");
    let (err, v) = call(&mut req, "close", json!({"reason": "merged", "note": "x"}));
    assert!(err);
    assert_eq!(v["error"]["code"], "invalid_field");
    let (err, v) = call(
        &mut req,
        "report_followup",
        json!({"title": "Also add bye.txt", "description": "Symmetry.", "type": "research"}),
    );
    assert!(!err, "{v}");
    let followup = v["result"]["created"].as_str().unwrap().to_string();
    let res = req("resources/read", json!({"uri": "trk://task"}));
    assert!(
        res["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&id)
    );
    // The "agent" does its work on the branch, then waits on a merge.
    std::fs::write(wt.join("hello.txt"), "hello\n").unwrap();
    for args in [
        vec!["add", "hello.txt"],
        vec![
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "hello",
        ],
    ] {
        assert!(
            StdCommand::new("git")
                .current_dir(&wt)
                .args(&args)
                .status()
                .unwrap()
                .success()
        );
    }
    let (err, v) = call(&mut req, "wait", json!({"check": "true"}));
    assert!(err, "a reason is required: {v}");
    let (err, v) = call(
        &mut req,
        "wait",
        json!({
            "reason": "merge into main",
            "interval": "1m",
            "check": "cd \"$TRK_REPO\" && [ \"$(git branch --show-current)\" = main ] && git -c user.email=t@e.st -c user.name=t merge --no-edit \"$TRK_BRANCH\"",
        }),
    );
    assert!(!err, "{v}");
    assert_eq!(v["waiting"], json!(id));
    let (err, v) = call(&mut req, "add_note", json!({"body": "after wait"}));
    assert!(!err, "{v}");
    assert_eq!(v["task"]["state"], "waiting");
    drop(stdin);
    child.wait().unwrap();

    let v = e.ok(&["show", &id]);
    assert_eq!(v["state"], "waiting");
    assert_eq!(v["waiting"]["interval"], 60);
    assert_eq!(v["waiting"]["cwd"], json!(wt.to_string_lossy()));
    let runs = e.ok(&["runs"]);
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["issue"], id);
    assert_eq!(runs[0]["state"], "waiting");
    assert!(!e.repo().join("hello.txt").exists());
    let r = e.ok(&["check"]);
    assert_eq!(r["results"][0]["result"], "passed", "{r:#}");
    assert!(e.repo().join("hello.txt").exists(), "the branch was merged");
    assert_eq!(e.ok(&["show", &id])["close_reason"], "done");
    let f = e.ok(&["show", &followup, "--with", "events"]);
    assert_eq!(f["created_by"], json!(actor));
    assert_eq!(f["type"], "research");
    assert_eq!(f["events"][0]["payload"]["followup_of"], json!(id));
}

#[test]
fn pretty_priority_and_state_labels_are_readable_without_color() {
    let e = Env::new();
    e.create(&["-p", "P0", "open"]);
    let claimed = e.create(&["-p", "P1", "claimed"]);
    e.ok(&["claim", &claimed]);
    let waiting = e.create(&["-p", "P2", "waiting"]);
    e.ok(&["claim", &waiting]);
    e.ok(&["wait", &waiting, "--reason", "later"]);
    let closed = e.create(&["-p", "P3", "closed"]);
    e.ok(&["close", &closed]);

    let pretty_output = || {
        e.cmd()
            .args(["--format", "pretty", "list"])
            .output()
            .unwrap()
    };
    let out = pretty_output();
    assert!(out.status.success());
    let pretty = String::from_utf8(out.stdout).unwrap();
    assert!(
        pretty.contains("P0") && pretty.contains("○ open"),
        "{pretty}"
    );
    assert!(
        pretty.contains("P1") && pretty.contains("● claimed"),
        "{pretty}"
    );
    assert!(
        pretty.contains("P2") && pretty.contains("◷ waiting"),
        "{pretty}"
    );
    assert!(
        pretty.contains("P3") && pretty.contains("✓ closed"),
        "{pretty}"
    );
    assert!(!pretty.contains('\x1b'), "{pretty:?}");

    let no_color = e
        .cmd()
        .env("NO_COLOR", "1")
        .args(["--format", "pretty", "list"])
        .output()
        .unwrap();
    assert!(no_color.status.success());
    assert!(!no_color.stdout.contains(&0x1b));

    let json = e.cmd().args(["--json", "ready"]).output().unwrap();
    assert!(json.status.success());
    assert!(!json.stdout.contains(&0x1b));
    serde_json::from_slice::<Value>(&json.stdout).unwrap();
}

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
                        terminal: Some(terminal.into()),
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

#[test]
fn waiting_sessions_accept_send_and_attach_and_runs_labels_both_states() {
    let ui = WaitingUi::new();
    let waiting = ui.waiting("exit 75", false);
    let active = ui.e.create(&["Still working"]);
    ui.claim(&active, "new-session", true);

    for args in [
        vec!["send", waiting.as_str(), "keep going"],
        vec!["attach", waiting.as_str()],
    ] {
        let out = ui.cmd().arg("--json").args(&args).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(out.status.success(), "{args:?}: {v:#}");
        assert_eq!(v["data"]["terminal"], "exact-origin");
    }
    assert_eq!(ui.text("messages"), "exact-origin\nkeep going\n");
    assert_eq!(ui.text("attachments"), "exact-origin\n");

    let runs = ui.e.ok(&["runs"]);
    let rows = runs.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (id, state) in [(&waiting, "waiting"), (&active, "claimed")] {
        let row = rows.iter().find(|r| r["issue"] == *id).unwrap();
        assert_eq!(row["state"], state);
    }
    let out =
        ui.e.cmd()
            .args(["--format", "pretty", "runs"])
            .output()
            .unwrap();
    assert!(out.status.success());
    let table = String::from_utf8(out.stdout).unwrap();
    assert!(table.lines().next().unwrap().contains("STATE"), "{table}");
    for (id, state) in [(&waiting, "waiting"), (&active, "claimed")] {
        let row = table.lines().find(|line| line.contains(id)).unwrap();
        assert!(row.contains(state), "{table}");
    }
}

#[test]
fn send_and_attach_refuse_waiting_terminals_after_close_or_reopen() {
    for transition in ["close", "reopen"] {
        let ui = WaitingUi::new();
        let id = ui.waiting("exit 75", false);
        ui.e.ok(&[transition, &id]);

        for args in [
            vec!["send", id.as_str(), "do not deliver"],
            vec!["attach", id.as_str()],
        ] {
            let out = ui.cmd().arg("--json").args(&args).output().unwrap();
            let v: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(out.status.code(), Some(3), "{args:?}: {v:#}");
            assert_eq!(v["error"]["code"], "not_found");
        }
        assert_eq!(ui.text("messages"), "");
        assert_eq!(ui.text("attachments"), "");
        assert!(ui.e.ok(&["runs"]).as_array().unwrap().is_empty());
    }
}

#[test]
fn send_and_attach_prefer_active_claim_even_without_a_terminal() {
    for terminal in [Some("new-session"), None] {
        let ui = WaitingUi::new();
        let id = ui.waiting("exit 75", false);
        ui.e.ok(&["reopen", &id]);
        if let Some(terminal) = terminal {
            ui.claim(&id, terminal, true);
        } else {
            ui.e.ok(&["claim", &id, "--as", "agent:omp:replacement"]);
        }

        for args in [
            vec!["send", id.as_str(), "replacement only"],
            vec!["attach", id.as_str()],
        ] {
            let out = ui.cmd().arg("--json").args(&args).output().unwrap();
            let v: Value = serde_json::from_slice(&out.stdout).unwrap();
            if let Some(terminal) = terminal {
                assert!(out.status.success(), "{args:?}: {v:#}");
                assert_eq!(v["data"]["terminal"], terminal);
            } else {
                assert_eq!(out.status.code(), Some(3), "{args:?}: {v:#}");
                assert_eq!(v["error"]["code"], "not_found");
            }
        }
        if terminal.is_some() {
            assert_eq!(ui.text("messages"), "new-session\nreplacement only\n");
            assert_eq!(ui.text("attachments"), "new-session\n");
        } else {
            assert_eq!(ui.text("messages"), "");
            assert_eq!(ui.text("attachments"), "");
        }
    }
}

#[test]
fn reap_skips_waiting_sessions_even_when_their_mcp_server_has_exited() {
    let ui = WaitingUi::new();
    let waiting = ui.waiting("exit 75", false);
    let dead = ui.e.create(&["Ended active session"]);
    ui.claim(&dead, "dead-session", false);
    let alive = ui.e.create(&["Live active session"]);
    ui.claim(&alive, "new-session", true);
    let before = ui.e.ok(&["show", &waiting, "--with", "all"]);

    let reaped = ui.e.ok(&["reap"]);
    assert_eq!(reaped.as_array().unwrap().len(), 1);
    assert_eq!(reaped[0]["issue"], dead);
    assert_eq!(ui.e.ok(&["show", &dead])["state"], "open");
    assert_eq!(ui.e.ok(&["show", &alive])["state"], "claimed");
    assert_eq!(ui.e.ok(&["show", &waiting, "--with", "all"]), before);
    assert!(ui.e.ok(&["reap"]).as_array().unwrap().is_empty());
    let runs = ui.e.ok(&["runs"]);
    assert_eq!(runs.as_array().unwrap().len(), 2);
    assert!(
        runs.as_array()
            .unwrap()
            .iter()
            .any(|row| row["issue"] == waiting && row["state"] == "waiting")
    );
}

#[test]
fn waiting_check_marks_only_the_origin_space_once_and_preserves_notifications() {
    let ui = WaitingUi::new();
    let id = ui.waiting("exit 0", true);
    let replacement = ui.e.create(&["Replacement session"]);
    ui.claim(&replacement, "new-session", true);

    let result = ui.check(&id, "");
    assert_eq!(result["result"], "passed");
    assert_eq!(result["state"], "closed");
    assert_eq!(ui.e.ok(&["show", &id])["close_reason"], "done");
    assert_eq!(ui.text("space-name"), "[closed] Reviewed title");
    let messages = ui.text("messages");
    assert!(messages.starts_with("exact-origin\n"), "{messages}");
    assert!(messages.contains("waiting check passed"), "{messages}");
    assert_eq!(ui.e.ok(&["show", &replacement])["state"], "claimed");

    ui.e.ok(&["reopen", &id]);
    ui.claim(&id, "exact-origin", true);
    ui.e.ok(&["wait", &id, "--force", "-c", "exit 0"]);
    assert_eq!(ui.check(&id, "")["state"], "closed");
    assert_eq!(ui.text("space-name"), "[closed] Reviewed title");
    assert_eq!(ui.text("renames"), "renamed\n");
}

#[test]
fn waiting_check_normalizes_edge_markers_and_preserves_embedded_literal_text() {
    for (title, expected) in [
        (
            "Append [closed] to task titles",
            "[closed] Append [closed] to task titles",
        ),
        ("Reviewed title [closed]", "[closed] Reviewed title"),
        ("Reviewed title [closed] [closed]", "[closed] Reviewed title"),
        ("[closed] Reviewed title", "[closed] Reviewed title"),
        ("[closed] Reviewed title [closed]", "[closed] Reviewed title"),
        (
            "Append [closed] to titles [closed]",
            "[closed] Append [closed] to titles",
        ),
    ] {
        let ui = WaitingUi::new();
        std::fs::write(ui.e.dir.path().join("space-name"), title).unwrap();
        let id = ui.waiting("exit 0", false);
        assert_eq!(ui.check(&id, "")["state"], "closed");
        assert_eq!(ui.text("space-name"), expected);
        assert_eq!(
            ui.text("renames"),
            if title == expected { "" } else { "renamed\n" },
            "{title}"
        );
    }
}

#[test]
fn failed_and_retrying_waiting_checks_do_not_mark_spaces_closed() {
    for (exit, result, state) in [(1, "woken", "claimed"), (75, "retry", "waiting")] {
        let ui = WaitingUi::new();
        ui.e.set_config("max_wakes = 5", "");
        let id = ui.waiting(&format!("echo failure evidence; exit {exit}"), true);
        let out = ui.check(&id, "");
        assert_eq!(out["result"], result);
        assert_eq!(out["state"], state);
        assert_eq!(ui.text("space-name"), "Reviewed title");
        assert_eq!(ui.text("renames"), "");
        let messages = ui.text("messages");
        if exit == 1 {
            assert!(messages.starts_with("exact-origin\n"), "{messages}");
            assert!(messages.contains("waiting check failed"), "{messages}");
            assert!(messages.contains("wake 1 of 5"), "{messages}");
            assert!(messages.contains("failure evidence"), "{messages}");
            assert!(messages.contains("Address the failure"), "{messages}");
            assert!(messages.contains("calling wait"), "{messages}");
            let issue = ui.e.ok(&["show", &id, "--with", "events,claims,notes"]);
            assert_eq!(issue["claim"]["terminal"], "exact-origin");
            let claims = issue["claims"].as_array().unwrap();
            assert_eq!(claims[0]["actor"], claims[1]["actor"]);
            let events = issue["events"].as_array().unwrap();
            assert!(
                events
                    .iter()
                    .any(|e| e["op"] == "check" && e["payload"]["result"] == "failed")
            );
            assert_eq!(events.last().unwrap()["op"], "claim");
        } else {
            assert_eq!(messages, "");
        }
    }
}

#[test]
fn wake_budget_is_shown_in_both_active_states_and_caps_delivered_wakes() {
    let ui = WaitingUi::new();
    ui.e.set_config("max_wakes = 5", "max_wakes = 2");
    let check = "echo capped failure evidence; exit 1";
    let id = ui.waiting(check, true);
    assert!(ui.e.ok(&["show", &id]).get("wake_count").is_none());
    for count in 1..=2 {
        assert_eq!(ui.check(&id, "")["result"], "woken");
        let issue = ui.e.ok(&["show", &id]);
        assert_eq!(issue["state"], "claimed");
        assert_eq!(issue["wake_count"], count);
        let messages = ui.text("messages");
        assert!(
            messages.contains(&format!("wake {count} of 2")),
            "{messages}"
        );
        for state in ["claimed", "waiting"] {
            if state == "waiting" {
                ui.e.ok(&["wait", &id, "--force", "-c", check]);
            }
            let issue = ui.e.ok(&["show", &id]);
            assert_eq!(issue["state"], state);
            assert_eq!(issue["wake_count"], count);
            let out = ui
                .e
                .cmd()
                .args(["--format", "pretty", "show", &id])
                .output()
                .unwrap();
            assert!(out.status.success());
            let pretty = String::from_utf8(out.stdout).unwrap();
            assert!(
                pretty.lines().any(|line| {
                    line.contains("wakes") && line.contains(&count.to_string())
                }),
                "{pretty}"
            );
        }
    }
    let messages = ui.text("messages");
    let result = ui.check(&id, "");
    assert_eq!(result["result"], "failed");
    assert_eq!(result["state"], "open");
    assert_eq!(ui.text("messages"), messages);
    let issue = ui.e.ok(&["show", &id, "--with", "notes"]);
    assert!(issue.get("wake_count").is_none());
    assert!(
        issue["notes"].as_array().unwrap().iter().any(|note| {
            let body = note["body"].as_str().unwrap();
            body.contains("wake cap of 2 reached") && body.contains("capped failure evidence")
        }),
        "{issue}"
    );
}

#[test]
fn zero_wake_budget_never_delivers_to_a_live_waiting_session() {
    let ui = WaitingUi::new();
    ui.e.set_config("max_wakes = 5", "max_wakes = 0");
    let id = ui.waiting("echo disabled wake evidence; exit 1", true);
    let result = ui.check(&id, "");
    assert_eq!(result["result"], "failed");
    assert_eq!(result["state"], "open");
    assert_eq!(ui.text("messages"), "");
    let issue = ui.e.ok(&["show", &id, "--with", "claims"]);
    assert!(issue.get("wake_count").is_none());
    assert_eq!(issue["claims"].as_array().unwrap().len(), 1);
}

#[test]
fn failed_waiting_checks_reopen_when_session_is_gone_or_wake_cannot_be_sent() {
    for mode in [
        "dead",
        "no-send",
        "missing",
        "unavailable",
        "send-error",
        "send-rejected",
    ] {
        let ui = WaitingUi::new();
        match mode {
            "no-send" => ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "",
            ),
            "send-error" => ui.e.set_config(
                "send       = [\"hc\",",
                "send       = [\"missing-send-program\",",
            ),
            "send-rejected" => ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "send       = [\"sh\", \"-c\", \"exit 1\"]",
            ),
            _ => {}
        }
        let id = ui.waiting("echo failure evidence; exit 2", mode != "dead");
        let result = ui.check(&id, mode);
        assert_eq!(result["result"], "failed", "{mode}: {result}");
        assert_eq!(result["state"], "open", "{mode}: {result}");
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("failure evidence")
        );
        let issue = ui.e.ok(&["show", &id, "--with", "events,claims,notes"]);
        assert_eq!(issue["claim"], Value::Null, "{mode}: {issue}");
        assert_eq!(issue["derived"]["ready"], true, "{mode}: {issue}");
        assert!(
            issue["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| { n["body"].as_str().unwrap().contains("failure evidence") })
        );
        let claims = issue["claims"].as_array().unwrap();
        if mode.starts_with("send-") {
            assert_eq!(claims.len(), 2, "{mode}: {claims:?}");
            assert!(claims[1]["released_at"].is_string(), "{mode}: {claims:?}");
        } else {
            assert_eq!(claims.len(), 1, "{mode}: {claims:?}");
        }
        assert_eq!(ui.text("messages"), "", "{mode}");
        assert_eq!(ui.text("renames"), "", "{mode}");
    }
}

#[test]
fn timed_out_waiting_check_wakes_its_live_session() {
    let ui = WaitingUi::new();
    ui.e.set_config("timeout  = \"10m\"", "timeout  = \"1s\"");
    let id = ui.waiting("echo timeout evidence; sleep 30", true);
    let result = ui.check(&id, "");
    assert_eq!(result["result"], "woken");
    assert_eq!(result["state"], "claimed");
    assert_eq!(result["timed_out"], true);
    let message = ui.text("messages");
    assert!(message.contains("timed out"), "{message}");
    assert!(message.contains("timeout evidence"), "{message}");
}

#[test]
fn waiting_check_ui_and_message_failures_are_independent_and_nonfatal() {
    for mode in [
        "dead",
        "no-send",
        "send",
        "rename",
        "unavailable",
        "missing",
        "headless",
    ] {
        let ui = WaitingUi::new();
        match mode {
            "no-send" => ui.e.set_config(
                "send       = [\"hc\", \"send\", \"{name}\", \"{text}\"]",
                "",
            ),
            "send" => ui.e.set_config(
                "send       = [\"hc\",",
                "send       = [\"missing-send-program\",",
            ),
            "headless" => ui.e.set_config("herdr      = true", "herdr      = false"),
            _ => {}
        }
        let id = ui.waiting("exit 0", mode != "dead");
        assert_eq!(ui.check(&id, mode)["state"], "closed", "{mode}");
        assert_eq!(ui.e.ok(&["show", &id])["state"], "closed", "{mode}");
        let renamed = matches!(mode, "dead" | "no-send" | "send");
        assert_eq!(
            ui.text("space-name"),
            if renamed {
                "[closed] Reviewed title"
            } else {
                "Reviewed title"
            },
            "{mode}"
        );
        let notified = matches!(mode, "rename" | "headless");
        assert_eq!(!ui.text("messages").is_empty(), notified, "{mode}");
    }
}
