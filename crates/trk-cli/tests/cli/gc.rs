use super::*;

struct GcEnv {
    e: Env,
    bin: PathBuf,
    host: String,
}

impl GcEnv {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let e = Env::new();
        let bin = e.dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(e.dir.path().join("closed-spaces")).unwrap();
        let executable = bin.join("herdr-app");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
set -eu
cd "$TRK_TEST_UI"
case "$1" in
ls)
    printf '{"spaces":['
    separator=''
    for space in target history active current shared decoy; do
        [ ! -f "closed-spaces/$space" ] || continue
        case "$space" in
            current) terminals='{"terminal":"term-current"},{"terminal":"term-current-target"}' ;;
            shared) terminals='{"terminal":"term-shared-closed"},{"terminal":"term-shared-active"}' ;;
            *) terminals="{\"terminal\":\"term-$space\"}" ;;
        esac
        printf '%s{"id":"%s","name":"Closed issue","tabs":[{"terminals":[%s]}]}' "$separator" "$space" "$terminals"
        separator=','
    done
    printf ']}'
    ;;
do)
    [ "$#" = 6 ] && [ "$2" = space.close ] && [ "$3" = --id ] && [ "$5" = --confirmed ] && [ "$6" = true ]
    case "$4" in target|history|active|current|shared|decoy) ;; *) exit 1 ;; esac
    [ ! -f "closed-spaces/$4" ]
    : > "closed-spaces/$4"
    printf '%s\n' "$4" >> closes
    ;;
*) exit 1 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = StdCommand::new("hostname").output().unwrap();
        assert!(output.status.success());
        let host = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        Self { e, bin, host }
    }

    fn cmd(&self) -> Command {
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut command = self.e.cmd();
        command
            .env("PATH", path)
            .env("TRK_TEST_UI", self.e.dir.path())
            .env_remove("HC_TERMINAL_ID");
        command
    }

    fn run(&self, args: &[&str]) -> Value {
        gc_success(self.cmd(), args)
    }

    fn git_at(&self, directory: &Path, args: &[&str]) -> String {
        let output = StdCommand::new("git")
            .current_dir(directory)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn worktree_at(&self, branch: &str, path: &Path) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        self.git_at(
            &self.e.repo(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
            ],
        );
        path.canonicalize().unwrap()
    }

    fn worktree(&self, id: &str) -> PathBuf {
        self.worktree_at(id, &self.e.trk_dir().join("worktrees").join(id))
    }

    fn store_at(&self, now: &str) -> trk_core::Store {
        trk_core::Store::open(
            &self.e.trk_dir().join("trk.db"),
            trk_core::StoreOpts {
                now: Some(now.parse().unwrap()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn type_worktree(&self, enabled: bool) {
        std::fs::write(
            self.e.trk_dir().join("types/code-change.toml"),
            format!(
                "name = \"code-change\"\ndescription = \"code change\"\nagent = \"implementer\"\nworktree = {enabled}\n"
            ),
        )
        .unwrap();
    }

    fn claim(&self, id: &str, path: Option<&Path>, terminal: &str, host: &str) {
        self.store_at("2026-11-20T10:00:00Z")
            .write("human:alice@example.com", false, |context| {
                context.claim(
                    id,
                    trk_core::ops::ClaimOpts {
                        worktree: path.map(|path| path.to_string_lossy().into_owned()),
                        terminal: Some(terminal.into()),
                        host: Some(host.into()),
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn close_at(&self, id: &str, reason: &str, now: &str) {
        self.store_at(now)
            .write("human:alice@example.com", false, |context| {
                context.close(id, reason, None, false)
            })
            .unwrap();
    }

    fn closed(&self, reason: &str, terminal: &str) -> (String, PathBuf) {
        let id = self.e.create(&["Closed issue"]);
        let path = self.worktree(&id);
        self.claim(&id, Some(&path), terminal, &self.host);
        self.close_at(&id, reason, "2026-11-20T10:00:00Z");
        (id, path)
    }

    fn branch_exists(&self, id: &str) -> bool {
        StdCommand::new("git")
            .current_dir(self.e.repo())
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{id}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    fn notes(&self, id: &str) -> Value {
        self.e.ok(&["show", id, "--with", "notes"])["notes"].clone()
    }

    fn closed_spaces(&self) -> Vec<String> {
        std::fs::read_to_string(self.e.dir.path().join("closes"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn gc_success(mut command: Command, args: &[&str]) -> Value {
    let output = command.arg("--json").args(args).output().unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{args:?}: invalid JSON ({error}): stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(output.status.success(), "{args:?}: {value:#}");
    assert_eq!(value["ok"], true, "{value:#}");
    assert_eq!(value["schema_version"], 4);
    assert!(value["data"]["dry_run"].is_boolean(), "{value:#}");
    assert!(value["data"]["issues"].is_array(), "{value:#}");
    value["data"].clone()
}

fn gc_issue<'a>(data: &'a Value, id: &str) -> &'a Value {
    data["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["id"] == id)
        .unwrap_or_else(|| panic!("missing {id}: {data:#}"))
}

fn gc_status(issue: &Value, resource: &str, status: &str) {
    assert_eq!(issue[resource]["status"], status, "{issue:#}");
    assert!(issue[resource]["reason"].is_string(), "{issue:#}");
    if resource != "branch" {
        assert!(issue[resource]["items"].is_array(), "{issue:#}");
    }
}

#[test]
fn gc_removes_merged_done_resources_and_records_the_calling_actor() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    fixture.git_at(
        &path,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "completed work",
        ],
    );
    fixture.git_at(&fixture.e.repo(), &["merge", "--ff-only", &id]);
    let before = fixture.notes(&id).as_array().unwrap().len();

    let data = fixture.run(&["gc", &id]);
    assert_eq!(data["dry_run"], false);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
    }
    assert_eq!(issue["session"]["items"][0]["terminal"], "term-target");
    assert_eq!(issue["session"]["items"][0]["space"], "target");
    assert_eq!(
        issue["worktree"]["items"][0]["path"],
        path.to_str().unwrap()
    );
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert_eq!(fixture.closed_spaces(), ["target"]);
    let notes = fixture.notes(&id);
    let added = &notes.as_array().unwrap()[before..];
    assert!(!added.is_empty(), "{notes:#}");
    assert!(
        added
            .iter()
            .all(|note| note["actor"] == "human:alice@example.com")
    );
}

#[test]
fn gc_dirty_worktree_gates_the_branch_but_not_the_session() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    std::fs::write(path.join("untracked.txt"), "keep this work").unwrap();

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "removed");
    gc_status(issue, "worktree", "skipped");
    gc_status(issue, "branch", "skipped");
    assert_eq!(
        std::fs::read_to_string(path.join("untracked.txt")).unwrap(),
        "keep this work"
    );
    assert!(fixture.branch_exists(&id));
    assert_eq!(fixture.closed_spaces(), ["target"]);
}

#[test]
fn gc_keeps_an_unmerged_branch_after_removing_its_worktree() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    fixture.git_at(
        &path,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "unmerged work",
        ],
    );

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "worktree", "removed");
    gc_status(issue, "branch", "skipped");
    assert!(!path.exists());
    assert!(fixture.branch_exists(&id));
}

#[test]
fn gc_only_collects_abandoned_issues_when_requested() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("abandoned", "term-target");
    let notes = fixture.notes(&id);

    fixture.run(&["gc", &id]);
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);

    let data = fixture.run(&["gc", &id, "--abandoned"]);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
    }
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
}

#[test]
fn gc_never_collects_claimed_or_waiting_issues_even_with_explicit_ids() {
    let fixture = GcEnv::new();
    let claimed = fixture.e.create(&["Claimed issue"]);
    let claimed_path = fixture.worktree(&claimed);
    fixture.claim(&claimed, Some(&claimed_path), "term-target", &fixture.host);
    let waiting = fixture.e.create(&["Waiting issue"]);
    let waiting_path = fixture.worktree(&waiting);
    fixture.claim(&waiting, Some(&waiting_path), "term-history", &fixture.host);
    fixture
        .e
        .ok(&["wait", &waiting, "--reason", "human decision"]);
    let claimed_notes = fixture.notes(&claimed);
    let waiting_notes = fixture.notes(&waiting);

    fixture.run(&["gc", &claimed, &waiting, "--abandoned"]);
    for (id, path) in [(&claimed, &claimed_path), (&waiting, &waiting_path)] {
        assert!(path.exists());
        assert!(fixture.branch_exists(id));
    }
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&claimed), claimed_notes);
    assert_eq!(fixture.notes(&waiting), waiting_notes);
    assert_eq!(fixture.e.ok(&["show", &claimed])["state"], "claimed");
    assert_eq!(fixture.e.ok(&["show", &waiting])["state"], "waiting");
}

#[test]
fn gc_keeps_worktrees_outside_the_configured_directory() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Closed issue"]);
    let outside = fixture.e.dir.path().join("outside-worktree");
    let path = fixture.worktree_at(&id, &outside);
    fixture.claim(&id, Some(&path), "term-target", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "worktree", "skipped");
    gc_status(issue, "branch", "skipped");
    gc_status(issue, "session", "removed");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
}

#[test]
fn gc_does_not_touch_resources_recorded_on_a_foreign_host() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Closed issue"]);
    let path = fixture.worktree(&id);
    let foreign_host = format!("{}.not-this-host.invalid", fixture.host);
    fixture.claim(&id, Some(&path), "term-target", &foreign_host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let notes = fixture.notes(&id);

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "skipped");
    }
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn gc_never_removes_its_current_worktree() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    let mut command = fixture.cmd();
    command.current_dir(&path);

    let data = gc_success(command, &["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "removed");
    gc_status(issue, "worktree", "skipped");
    gc_status(issue, "branch", "skipped");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
}

#[test]
fn gc_protects_the_callers_entire_space_not_just_its_terminal() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-current-target");
    let mut command = fixture.cmd();
    command.env("HC_TERMINAL_ID", "term-current");

    let data = gc_success(command, &["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "skipped");
    gc_status(issue, "worktree", "removed");
    assert!(fixture.closed_spaces().is_empty());
    assert!(!path.exists());
}

#[test]
fn gc_protects_spaces_shared_with_another_claimed_or_waiting_issue() {
    for state in ["claimed", "waiting"] {
        let fixture = GcEnv::new();
        let (closed, path) = fixture.closed("done", "term-shared-closed");
        let active = fixture.e.create(&["Protected issue"]);
        fixture.claim(&active, None, "term-shared-active", &fixture.host);
        if state == "waiting" {
            fixture
                .e
                .ok(&["wait", &active, "--reason", "human decision"]);
        }

        let data = fixture.run(&["gc", &closed]);
        let issue = gc_issue(&data, &closed);
        gc_status(issue, "session", "skipped");
        gc_status(issue, "worktree", "removed");
        assert!(!path.exists());
        assert!(fixture.closed_spaces().is_empty());
        assert_eq!(fixture.e.ok(&["show", &active])["state"], state);
    }
}

#[test]
fn gc_second_run_is_absent_and_does_not_duplicate_notes() {
    let fixture = GcEnv::new();
    let (id, _) = fixture.closed("done", "term-target");
    fixture.run(&["gc", &id]);
    let notes = fixture.notes(&id);
    let closes = fixture.closed_spaces();

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "absent");
    }
    assert_eq!(fixture.notes(&id), notes);
    assert_eq!(fixture.closed_spaces(), closes);
}

#[test]
fn gc_older_than_filters_by_close_time() {
    let fixture = GcEnv::new();
    let (old, old_path) = fixture.closed("done", "term-target");
    let recent = fixture.e.create(&["Recently closed"]);
    let recent_path = fixture.worktree(&recent);
    fixture.claim(&recent, Some(&recent_path), "term-history", &fixture.host);
    fixture.close_at(&recent, "done", "2026-11-22T09:00:00Z");
    let notes = fixture.notes(&recent);
    let mut command = fixture.cmd();
    command.env("TRK_NOW", "2026-11-22T10:00:00Z");

    let data = gc_success(command, &["gc", "--older-than", "24h"]);
    gc_status(gc_issue(&data, &old), "worktree", "removed");
    assert!(!old_path.exists());
    assert!(!fixture.branch_exists(&old));
    assert!(recent_path.exists());
    assert!(fixture.branch_exists(&recent));
    assert_eq!(fixture.closed_spaces(), ["target"]);
    assert_eq!(fixture.notes(&recent), notes);
}

#[test]
fn gc_dry_run_reports_removals_without_changing_resources_or_notes() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    let notes = fixture.notes(&id);

    let data = fixture.run(&["gc", &id, "--dry-run"]);
    assert_eq!(data["dry_run"], true);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
        assert!(
            issue[resource]["reason"]
                .as_str()
                .unwrap()
                .starts_with("would remove "),
            "{issue:#}"
        );
    }
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn gc_collects_every_historical_local_claim_not_only_the_latest() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Closed issue"]);
    let first = fixture.worktree(&id);
    fixture.claim(&id, Some(&first), "term-history", &fixture.host);
    fixture.e.ok(&["unclaim", &id]);
    let second_branch = format!("{id}-second");
    let second = fixture.worktree_at(
        &second_branch,
        &fixture.e.trk_dir().join("worktrees").join(&second_branch),
    );
    fixture.claim(&id, Some(&second), "term-target", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "removed");
    gc_status(issue, "worktree", "removed");
    gc_status(issue, "branch", "removed");
    assert_eq!(issue["session"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(issue["worktree"]["items"].as_array().unwrap().len(), 2);
    assert!(!first.exists());
    assert!(!second.exists());
    assert!(!fixture.branch_exists(&id));
    assert!(fixture.branch_exists(&second_branch));
    let mut closed = fixture.closed_spaces();
    closed.sort();
    assert_eq!(closed, ["history", "target"]);
}

#[test]
fn gc_respects_the_check_queue_lock_including_previews() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    fixture
        .store_at("2026-11-20T10:00:00Z")
        .write("human:alice@example.com", false, |c| {
            c.check_lock_take(i64::from(std::process::id()), Some(&fixture.host))
        })
        .unwrap();
    for args in [
        vec!["gc", id.as_str()],
        vec!["gc", id.as_str(), "--dry-run"],
    ] {
        let output = fixture.cmd().arg("--json").args(args).output().unwrap();
        assert!(!output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "invalid_state");
        assert!(path.exists());
        assert!(fixture.branch_exists(&id));
        assert!(fixture.closed_spaces().is_empty());
        assert_eq!(fixture.notes(&id), json!([]));
    }
}

#[test]
fn gc_removes_ignored_build_artifacts_through_git() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    fixture.git_at(
        &fixture.e.repo(),
        &[
            "config",
            "core.excludesFile",
            fixture.e.dir.path().join("ignore").to_str().unwrap(),
        ],
    );
    std::fs::write(fixture.e.dir.path().join("ignore"), "target/\n").unwrap();
    std::fs::create_dir(path.join("target")).unwrap();
    std::fs::write(path.join("target/artifact"), "ignored build output").unwrap();
    let preview = fixture.run(&["gc", &id, "--dry-run"]);
    gc_status(gc_issue(&preview, &id), "worktree", "removed");
    assert!(path.join("target/artifact").exists());
    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "worktree", "removed");
    assert!(!path.exists());
}

#[test]
fn gc_rejects_a_worktree_reached_through_an_escaping_symlink() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Outside worktree"]);
    let path = fixture.worktree_at(&id, &fixture.e.dir.path().join("outside"));
    let root = fixture.e.trk_dir().join("worktrees");
    std::fs::create_dir_all(&root).unwrap();
    let link = root.join(&id);
    std::os::unix::fs::symlink(&path, &link).unwrap();
    fixture.claim(&id, Some(&link), "term-target", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    let data = fixture.run(&["gc", &id]);
    gc_status(gc_issue(&data, &id), "worktree", "skipped");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
}

#[test]
fn gc_still_collects_git_resources_when_the_terminal_is_gone() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "missing-terminal");
    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    gc_status(issue, "session", "absent");
    gc_status(issue, "worktree", "removed");
    gc_status(issue, "branch", "removed");
    assert!(!path.exists());
    assert!(fixture.closed_spaces().is_empty());
}

#[test]
fn gc_without_recorded_worktrees_keeps_an_unrelated_issue_named_branch() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Never launched"]);
    fixture.git_at(&fixture.e.repo(), &["branch", &id]);
    fixture.e.ok(&["close", &id]);
    let notes = fixture.notes(&id);

    for args in [&["gc", &id, "--dry-run"][..], &["gc", &id][..]] {
        let data = fixture.run(args);
        let issue = gc_issue(&data, &id);
        assert_eq!(issue["worktree"]["status"], "absent", "{issue:#}");
        gc_status(issue, "branch", "absent");
        assert!(fixture.branch_exists(&id));
        assert_eq!(fixture.notes(&id), notes);
    }
}

#[test]
fn gc_worktreeless_run_never_collects_a_coincidentally_named_branch() {
    let fixture = GcEnv::new();
    fixture.type_worktree(false);
    let id = fixture.e.create(&["Run in the project checkout"]);
    fixture
        .cmd()
        .args(["run", &id, "--harness", "omp", "--print"])
        .assert()
        .success();
    let claim = fixture.e.ok(&["show", &id])["claim"].clone();
    assert_eq!(claim["worktree"], Value::Null);
    assert!(!fixture.e.trk_dir().join("worktrees").join(&id).exists());
    assert!(!fixture.branch_exists(&id));

    fixture.git_at(&fixture.e.repo(), &["branch", &id]);
    fixture
        .e
        .ok(&["--as", claim["actor"].as_str().unwrap(), "close", &id]);
    let notes = fixture.notes(&id);
    // Configuration changes cannot turn this launch into ownership of a branch.
    fixture.type_worktree(true);

    for args in [&["gc", &id, "--dry-run"][..], &["gc", &id][..]] {
        let data = fixture.run(args);
        let issue = gc_issue(&data, &id);
        assert_eq!(issue["worktree"]["status"], "absent", "{issue:#}");
        gc_status(issue, "branch", "absent");
        assert!(fixture.branch_exists(&id));
        assert!(fixture.e.repo().is_dir());
        assert_eq!(fixture.notes(&id), notes);
    }
}

#[test]
fn gc_collects_historical_worktrees_after_the_type_opts_out() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Previously launched in a worktree"]);
    let path = fixture.worktree(&id);
    fixture.claim(&id, Some(&path), "term-history", &fixture.host);
    fixture.e.ok(&["unclaim", &id]);

    fixture.type_worktree(false);
    fixture
        .cmd()
        .args(["run", &id, "--harness", "omp", "--print"])
        .assert()
        .success();
    let claim = fixture.e.ok(&["show", &id])["claim"].clone();
    assert_eq!(claim["worktree"], Value::Null);
    fixture
        .e
        .ok(&["--as", claim["actor"].as_str().unwrap(), "close", &id]);

    let data = fixture.run(&["gc", &id]);
    let issue = gc_issue(&data, &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
    }
    assert_eq!(issue["worktree"]["items"].as_array().unwrap().len(), 1);
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert!(fixture.e.repo().is_dir());
    assert_eq!(fixture.closed_spaces(), ["history"]);
}
