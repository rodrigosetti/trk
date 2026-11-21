//! End-to-end tests that run the `trk` binary.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};

use assert_cmd::Command;
use serde_json::{Value, json};

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
        e.ok(&["init"]);
        e.ok(&["project", "add", "orch", "--name", "Orchestra"]);
        e.ok(&["repo", "add", "--project", "orch"]);
        e
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("trk").unwrap();
        c.current_dir(self.repo())
            .env("TRK_DB", self.dir.path().join("trk.db"))
            .env("TRK_CONFIG", self.dir.path().join("config"))
            .env("TRK_ACTOR", "human:alice@example.com")
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("TRK_NOW", "2026-11-20T10:00:00Z")
            .env_remove("TRK_FORMAT")
            .env_remove("TRK_PROJECT");
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
        let p = self.dir.path().join("config/config.toml");
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
        assert_eq!(v["schema_version"], 2);
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
    let b = e.create(&[
        "-t",
        "Use leases",
        "--blocked-by",
        &a,
        "--agent",
        "reviewer",
    ]);
    let v = e.ok(&["show", &a]);
    assert_eq!(v["state"], "open");
    assert_eq!(v["priority"], "P1");
    assert_eq!(v["repo"]["remote_url"], "github.com/acme/orch");
    assert_eq!(v["deps"]["blocks"][0]["id"], json!(b));
    assert_eq!(v["derived"]["ready"], true);
    assert_eq!(e.ok(&["show", &b])["agent"], "reviewer");

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
        "--agent",
        "implementer",
    ]);
    e.ok(&["edit", &a, "--agent", ""]);
    assert_eq!(e.ok(&["show", &a])["agent"], Value::Null);
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
    assert_eq!(who["default_agent"], "implementer");
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
    let id = e.waiting("slow merge", "sleep 2; exit 75");
    let mut first = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"))
        .current_dir(e.repo())
        .args(["--json", "check"])
        .env("TRK_DB", e.dir.path().join("trk.db"))
        .env("TRK_CONFIG", e.dir.path().join("config"))
        .env("TRK_ACTOR", "human:alice@example.com")
        .env("XDG_STATE_HOME", e.dir.path().join("state"))
        .env("TRK_NOW", "2026-11-20T10:00:00Z")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    let second = e.ok(&["check", "--all"]);
    assert_eq!(second["busy"], true, "{second:#}");
    assert!(second["results"].as_array().unwrap().is_empty());
    assert!(e.ok(&["whoami"])["check_running"]["pid"].is_number());
    assert!(first.wait().unwrap().success());
    // The lock is released, and a run that died holding it does not keep it.
    assert_eq!(e.ok(&["whoami"])["check_running"], Value::Null);
    let mut dying = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"))
        .current_dir(e.repo())
        .args(["--json", "check", "--all"])
        .env("TRK_DB", e.dir.path().join("trk.db"))
        .env("TRK_CONFIG", e.dir.path().join("config"))
        .env("TRK_ACTOR", "human:alice@example.com")
        .env("XDG_STATE_HOME", e.dir.path().join("state"))
        .env("TRK_NOW", "2026-11-20T10:00:00Z")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    dying.kill().unwrap();
    dying.wait().unwrap();
    assert!(e.ok(&["whoami"])["check_running"]["pid"].is_number());
    e.ok(&["wait", &id, "-c", "exit 75"]);
    let after = e.ok(&["check", "--all"]);
    assert_eq!(after["busy"], false, "{after:#}");
    assert_eq!(after["results"][0]["result"], "retry");
}

#[test]
fn errors_and_exit_codes() {
    let e = Env::new();
    let id = e.create(&["Thing"]);
    e.err(&["show", "nope-zzz"], 3, "not_found");
    e.err(&["list", "bogus"], 2, "bad_query");
    e.err(&["list", "type=decision"], 2, "bad_query");
    e.err(&["create", "X", "-p", "P7"], 4, "invalid_field");
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
fn an_old_database_is_refused_with_a_hint() {
    let e = Env::new();
    let old = e.dir.path().join("old.db");
    assert!(
        StdCommand::new("sqlite3")
            .arg(&old)
            .arg("CREATE TABLE issue_type (name TEXT PRIMARY KEY);")
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        "sqlite3 is needed for this test"
    );
    let out = e
        .cmd()
        .env("TRK_DB", &old)
        .args(["--json", "list"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "old_database");
    assert!(
        v["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("import-v1.sh")
    );
    assert_eq!(out.status.code(), Some(1));
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
fn init_reports_stale_files_and_agents_check() {
    let e = Env::new();
    let cfg = e.dir.path().join("config");
    assert!(cfg.join("agents/implementer.toml").exists());
    assert!(!cfg.join("types").exists());
    std::fs::create_dir_all(cfg.join("types")).unwrap();
    std::fs::write(cfg.join("fields.toml"), "").unwrap();
    let v = e.ok(&["init"]);
    assert_eq!(v["written"].as_array().unwrap().len(), 0);
    assert_eq!(v["stale"], json!(["types", "fields.toml"]));
    assert_eq!(e.ok(&["agent", "check"])["ok"], true);
    std::fs::write(
        cfg.join("agents/broken.toml"),
        "name = \"broken\"\ninstructions = \"prompts/none.j2\"\n[trk_tools]\ninclude = [\"submit_for_review\"]\n",
    )
    .unwrap();
    let err = e.err(&["agent", "check"], 4, "config_invalid");
    let problems = err["problems"].to_string();
    assert!(problems.contains("prompts/none.j2"), "{problems}");
    assert!(
        problems.contains("no tool 'submit_for_review'"),
        "{problems}"
    );
}

#[test]
fn agent_preview_and_run_dry_run() {
    let e = Env::new();
    let list = e.ok(&["agent", "list"]);
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(list[0]["name"], "implementer");
    assert_eq!(list[0]["default"], true);
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
    // The issue's agent wins over the default; --agent wins over both.
    e.ok(&["edit", &id, "--agent", "reviewer"]);
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
    e.ok(&["edit", &id, "--agent", ""]);
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
        .env("TRK_DB", e.dir.path().join("trk.db"))
        .env("TRK_CONFIG", e.dir.path().join("config"))
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
    let (err, v) = call(&mut req, "close", json!({"reason": "merged", "note": "x"}));
    assert!(err);
    assert_eq!(v["error"]["code"], "invalid_field");
    let (err, v) = call(
        &mut req,
        "report_followup",
        json!({"title": "Also add bye.txt", "description": "Symmetry."}),
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
    assert!(err);
    assert_eq!(v["error"]["code"], "not_permitted");
    drop(stdin);
    child.wait().unwrap();

    let v = e.ok(&["show", &id]);
    assert_eq!(v["state"], "waiting");
    assert_eq!(v["waiting"]["interval"], 60);
    assert_eq!(v["waiting"]["cwd"], json!(wt.to_string_lossy()));
    assert!(e.ok(&["runs"]).as_array().unwrap().is_empty());
    assert!(!e.repo().join("hello.txt").exists());
    let r = e.ok(&["check"]);
    assert_eq!(r["results"][0]["result"], "passed", "{r:#}");
    assert!(e.repo().join("hello.txt").exists(), "the branch was merged");
    assert_eq!(e.ok(&["show", &id])["close_reason"], "done");
    let f = e.ok(&["show", &followup, "--with", "events"]);
    assert_eq!(f["created_by"], json!(actor));
    assert_eq!(f["events"][0]["payload"]["followup_of"], json!(id));
    assert_eq!(f["repo"]["remote_url"], "github.com/acme/orch");
}

#[test]
fn skill_install_project_and_global() {
    let e = Env::new();
    let home = e.dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let skill = |args: &[&str]| -> Value {
        let out = e
            .cmd()
            .env("HOME", &home)
            .arg("--json")
            .arg("skill")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"].clone()
    };
    let repo = e.repo();

    // Project-local by default: skill files, a Claude Code link, kept out of git.
    let v = skill(&["install"]);
    assert_eq!(v["scope"], "project");
    assert_eq!(v["state"], "current");
    let installed = std::fs::read_to_string(repo.join(".agents/skills/trk/SKILL.md")).unwrap();
    assert!(installed.starts_with("---\nname: trk\n"));
    assert!(
        repo.join(".agents/skills/trk/references/reference.md")
            .exists()
    );
    let link = repo.join(".claude/skills/trk");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        PathBuf::from("../../.agents/skills/trk")
    );
    assert!(link.join("SKILL.md").exists(), "the relative link resolves");
    let status = StdCommand::new("git")
        .current_dir(&repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        status.stdout.is_empty(),
        "skill is excluded from git: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    // Worktrees made by `trk run` do not contain it.
    let id = e.create(&["x"]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let wt = PathBuf::from(spec["worktree"].as_str().unwrap());
    assert!(wt.exists() && !wt.join(".agents").exists() && !wt.join(".claude").exists());

    // Status notices edits; install refreshes.
    std::fs::write(repo.join(".agents/skills/trk/SKILL.md"), "stale").unwrap();
    let st = skill(&["status"]);
    assert_eq!(st[0]["scope"], "project");
    assert_eq!(st[0]["state"], "outdated");
    assert_eq!(st[1]["scope"], "global");
    assert_eq!(st[1]["state"], "missing");
    assert_eq!(skill(&["install"])["state"], "current");

    // Uninstall removes the files, the link and the excludes.
    skill(&["uninstall"]);
    assert!(!repo.join(".agents/skills/trk").exists() && !link.is_symlink());
    let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert!(!exclude.contains(".agents/skills/trk"), "{exclude}");

    // --global goes to ~/.agents/skills/trk with an absolute Claude Code link.
    let v = skill(&["install", "--global"]);
    assert_eq!(v["scope"], "global");
    assert!(home.join(".agents/skills/trk/SKILL.md").exists());
    assert_eq!(
        std::fs::read_link(home.join(".claude/skills/trk")).unwrap(),
        home.join(".agents/skills/trk")
    );
    assert!(v["warnings"][0].as_str().unwrap().contains("trk run"));

    // --dir targets any skills directory, without links.
    let other = e.dir.path().join("other-skills");
    skill(&["install", "--dir", other.to_str().unwrap()]);
    assert!(other.join("trk/SKILL.md").exists());
}

#[test]
fn run_warns_when_the_repo_tracks_the_skill() {
    let e = Env::new();
    let repo = e.repo();
    let out = e
        .cmd()
        .env("HOME", e.dir.path())
        .args(["--json", "skill", "install"])
        .output()
        .unwrap();
    assert!(out.status.success());
    for args in [
        vec!["add", "-f", ".agents/skills/trk"],
        vec![
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "track skill",
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
    let out = e
        .cmd()
        .env("HOME", e.dir.path())
        .args(["--json", "skill", "install"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["data"]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("tracked in git"),
        "{v:#}"
    );
    let id = e.create(&["x"]);
    let out = e
        .cmd()
        .args(["--json", "run", &id, "--harness", "omp", "--print"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("tracked in this repo"));
}
