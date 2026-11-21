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
        assert_eq!(v["schema_version"], 1);
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
    let d = e.ok(&[
        "create",
        "decision",
        "-t",
        "Use SQLite as the tracker store",
        "-f",
        "context=We need a store",
        "-f",
        "question=Which embedded DB?",
        "-f",
        "tags+=storage",
    ]);
    let id = d["id"].as_str().unwrap().to_string();
    let r = e.ok(&[
        "create",
        "research",
        "-t",
        "Compare DBs",
        "-f",
        "context=c",
        "-f",
        "question=q",
        "--dep",
        &format!("informs:{id}"),
    ]);
    let rid = r["id"].as_str().unwrap().to_string();
    e.ok(&["claim", &id, "-f", "options=SQLite,Dolt,Cozo"]);
    let mut v = e.ok(&["show", &id]);
    redact(
        &mut v,
        &[(id.clone(), "<decision>"), (rid.clone(), "<research>")],
    );
    insta::assert_json_snapshot!("show_decision_proposed", v);

    // requirements_unmet: exit 4 with missing fields and a hint.
    let err = e.err(
        &["close", &id, "--resolution", "decided"],
        4,
        "requirements_unmet",
    );
    assert_eq!(
        err["missing_fields"],
        json!(["record_url", "outcome", "chosen", "rationale", "deciders"])
    );
    assert!(err["hint"].as_str().unwrap().starts_with(&format!(
        "trk close {id} --resolution decided -f record_url=..."
    )));
    assert!(err["hint"].as_str().unwrap().contains("-f outcome=@-"));
    // --dry-run checks everything and writes nothing.
    let close = [
        "close",
        &id,
        "-r",
        "decided",
        "-f",
        "record_url=https://slack.com/t/1",
        "-f",
        "outcome=SQLite",
        "-f",
        "chosen=SQLite",
        "-f",
        "rationale=@-",
        "-f",
        "deciders=me",
    ];
    let out = e
        .cmd()
        .arg("--json")
        .args(close)
        .arg("--dry-run")
        .write_stdin("Embedded, mature, FTS5.")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(e.ok(&["show", &id])["status"], "proposed");
    let out = e
        .cmd()
        .arg("--json")
        .args(close)
        .write_stdin("Embedded, mature, FTS5.")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = e.ok(&["show", &id, "--with", "notes,events,claims"]);
    assert_eq!(v["status"], "closed");
    assert_eq!(v["fields"]["rationale"], "Embedded, mature, FTS5.");
    assert_eq!(v["fields"]["deciders"], json!(["human:alice@example.com"]));
    assert_eq!(v["claims"][0]["release_reason"], "closed");
    let ops: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["op"].as_str().unwrap())
        .collect();
    assert_eq!(
        ops,
        [
            "create", "dep_add", "set", "claim", "set", "close", "release"
        ]
    );

    // jsonl: one object per line, no envelope.
    let out = e
        .cmd()
        .args(["list", "--format", "jsonl", "--fields", "id,type,status"])
        .output()
        .unwrap();
    let lines: Vec<Value> = out
        .stdout
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["id", "type", "status"]
    );
    // Query language.
    let hits = e.ok(&[
        "list",
        "type in (decision,approval) closed>-7d resolution=decided",
    ]);
    assert_eq!(hits.as_array().unwrap().len(), 1);
    let hits = e.ok(&["list", &format!("dep:informs={id}")]);
    assert_eq!(hits[0]["id"], json!(rid));
    let hits = e.ok(&["ready", "project=."]);
    assert_eq!(hits.as_array().unwrap().len(), 1);
    e.err(&["list", "bogus"], 2, "bad_query");
    e.err(&["list", "nosuchfield=1"], 2, "bad_query");
}

#[test]
fn errors_and_exit_codes() {
    let e = Env::new();
    e.err(&["show", "zzz"], 3, "not_found");
    let a = e.ok(&[
        "create",
        "research",
        "-t",
        "a",
        "-f",
        "context=c",
        "-f",
        "question=q",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string();
    e.ok(&[
        "create",
        "research",
        "-t",
        "b",
        "-f",
        "context=c",
        "-f",
        "question=q",
    ]);
    e.err(&["show", "orch"], 3, "ambiguous_id");
    e.err(&["create", "research", "-t", "x"], 4, "requirements_unmet");
    e.err(&["edit", &a, "-f", "confidence=huge"], 4, "invalid_field");
    e.err(&["edit", &a, "-f", "status=closed"], 4, "invalid_field");
    e.ok(&["claim", &a]);
    e.err(
        &["claim", &a, "--as", "human:bob@example.com"],
        5,
        "claimed_by_other",
    );
    e.err(
        &[
            "close",
            &a,
            "-r",
            "abandoned",
            "--as",
            "human:bob@example.com",
        ],
        5,
        "not_claimant",
    );
    e.err(&["dep", "add", &a, "blocks", &a], 4, "cycle");
    e.err(&["list", "--sort", "nope"], 2, "bad_args");
    // Outside a registered repo, create needs a project.
    let out = e
        .cmd()
        .current_dir(e.dir.path())
        .args(["--json", "create", "research", "-t", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "unknown_repo");
}

#[test]
fn pretty_output() {
    let e = Env::new();
    let id = e.ok(&[
        "create",
        "code-change",
        "-t",
        "Add claim leases",
        "-f",
        "context=Leases.",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = e
        .cmd()
        .args(["--format", "pretty", "list"])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(
        s.contains("ID")
            && s.contains(&id)
            && s.contains("○ open")
            && s.contains("Add claim leases"),
        "{s}"
    );
    let out = e
        .cmd()
        .args(["--format", "pretty", "show", &id])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("github.com/acme/orch"), "{s}");
    assert!(
        s.contains("next → closed as merged: needs merge_commit"),
        "{s}"
    );
    let out = e
        .cmd()
        .args(["--format", "pretty", "close", &id, "-r", "merged"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("error: code-change cannot close as 'merged'")
            && err.contains("hint: trk close"),
        "{err}"
    );
}

#[test]
fn schema_commands() {
    let e = Env::new();
    assert_eq!(e.ok(&["schema", "check"])["ok"], true);
    assert!(
        e.ok(&["schema", "plan"])["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let g = e.ok(&["schema", "graph", "code-change"]);
    assert!(
        g["graph"]
            .as_str()
            .unwrap()
            .contains("in_progress --> in_review")
    );
    let dot = e.ok(&["schema", "graph", "operation", "--dot"]);
    assert!(dot["graph"].as_str().unwrap().starts_with("digraph"));
    assert_eq!(e.ok(&["schema", "research"])["name"], "research");
    assert_eq!(
        e.ok(&["schema", "index", "environment"])["index"],
        "issue_f_environment"
    );
    // Break the config: check reports it, apply refuses.
    let types = e.dir.path().join("config/types/research.toml");
    let text = std::fs::read_to_string(&types).unwrap();
    std::fs::write(&types, text.replace("[\"doc_url\"]", "[\"nonexistent\"]")).unwrap();
    let err = e.err(&["schema", "check"], 4, "schema_invalid");
    assert!(err["problems"][0].as_str().unwrap().contains("nonexistent"));
    e.err(&["schema", "apply"], 4, "schema_invalid");
    let backup = e.dir.path().join("backup.db");
    e.ok(&["backup", backup.to_str().unwrap()]);
    assert!(backup.exists());
}

#[test]
fn agent_preview_and_run_dry_run() {
    let e = Env::new();
    let list = e.ok(&["agent", "list"]);
    assert_eq!(list.as_array().unwrap().len(), 5);
    let p = e.ok(&["agent", "preview", "implementer"]);
    assert!(
        p["instructions"]
            .as_str()
            .unwrap()
            .contains("implementer agent")
    );
    assert!(
        p["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "submit_for_review")
    );
    let id = e.ok(&["create", "code-change", "-t", "Fix", "-f", "context=c"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let spec = e.ok(&["run", &id, "--harness", "omp", "--dry-run"]);
    assert_eq!(spec["agent"], "implementer");
    assert_eq!(spec["argv"][0], "omp");
    assert!(
        spec["worktree"]
            .as_str()
            .unwrap()
            .ends_with(&format!("worktrees/{id}"))
    );
    assert!(
        spec["briefing"]
            .as_str()
            .unwrap()
            .contains("move to **in_review**: set pr_url")
    );
    assert!(spec["actor"].as_str().unwrap().starts_with("agent:omp:"));
    // Nothing was claimed or created.
    assert_eq!(e.ok(&["show", &id])["claim"], Value::Null);
    assert!(!Path::new(spec["worktree"].as_str().unwrap()).exists());
    // --print claims, creates the worktree and writes the files.
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    let wt = PathBuf::from(spec["worktree"].as_str().unwrap());
    assert!(wt.join(".omp/mcp.json").exists());
    let exclude = std::fs::read_to_string(e.repo().join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("/.omp/mcp.json"));
    let v = e.ok(&["show", &id]);
    assert_eq!(v["claim"]["agent"], "implementer");
    assert_eq!(v["fields"]["branch"], json!(id));
    assert_eq!(e.ok(&["runs"])[0]["issue"], json!(id));
    e.err(
        &["run", &id, "--harness", "omp", "--dry-run"],
        5,
        "claimed_by_other",
    );
}

/// Talk to `trk mcp` over stdio like a harness would.
#[test]
fn mcp_server_over_stdio() {
    let e = Env::new();
    let id = e.ok(&[
        "create",
        "research",
        "-t",
        "Q",
        "-f",
        "context=c",
        "-f",
        "question=What is 6*7?",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let actor = "agent:omp:test-1";
    e.ok(&["claim", &id, "--as", actor]);
    e.ok(&["note", &id, "cite a source", "--to-agent"]);
    let bin = assert_cmd::cargo::cargo_bin("trk");
    let mut child = StdCommand::new(bin)
        .args([
            "mcp",
            "--issue",
            &id,
            "--actor",
            actor,
            "--agent",
            "researcher",
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
        if method == "initialize" {
            // fallthrough to read the response
        }
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
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains(&id)
    );
    let tools = req("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"close_as_answered") && !names.iter().any(|n| n.contains("issue")));
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
    assert_eq!(v["messages_for_you"][0]["body"], "cite a source");
    let (err, v) = call(
        &mut req,
        "close_as_answered",
        json!({"confidence": "high", "outcome": "42"}),
    );
    assert!(err);
    assert_eq!(v["error"]["code"], "requirements_unmet");
    assert_eq!(v["error"]["missing_fields"], json!(["doc_url"]));
    let (err, v) = call(
        &mut req,
        "report_followup",
        json!({"type": "research", "title": "Why 42?", "context": "c", "fields": {"question": "why"}}),
    );
    assert!(!err, "{v}");
    let res = req("resources/read", json!({"uri": "trk://task"}));
    assert!(
        res["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&id)
    );
    let (err, v) = call(
        &mut req,
        "close_as_answered",
        json!({"confidence": "high", "outcome": "42", "doc_url": "https://example.com/a"}),
    );
    assert!(!err, "{v}");
    let (err, v) = call(&mut req, "add_note", json!({"body": "after close"}));
    assert!(err);
    assert_eq!(v["error"]["code"], "not_permitted");
    drop(stdin);
    child.wait().unwrap();
    let v = e.ok(&["show", &id]);
    assert_eq!(v["resolution"], "answered");
    let follow = e.ok(&["list", &format!("dep:discovered-from={id}")]);
    assert_eq!(follow[0]["created_by"], actor);
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
    let id = e.ok(&["create", "code-change", "-t", "x", "-f", "context=c"])["id"]
        .as_str()
        .unwrap()
        .to_string();
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
    let id = e.ok(&["create", "code-change", "-t", "x", "-f", "context=c"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = e
        .cmd()
        .args(["--json", "run", &id, "--harness", "omp", "--print"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("tracked in this repo"));
}
