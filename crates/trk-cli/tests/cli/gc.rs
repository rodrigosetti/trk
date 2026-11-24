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
    assert!(
        issue["worktree"]["reason"]
            .as_str()
            .unwrap()
            .contains("outside the configured worktrees directory")
    );

    // A new check process must still warn about genuinely protected resources.
    let output = fixture
        .cmd()
        .env("TRK_NOW", "2026-11-20T12:00:00Z")
        .arg("check")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(&id), "{stdout}");
    assert!(stdout.contains("outside the configured"), "{stdout}");
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
fn gc_keeps_locked_worktrees_and_their_branches() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "missing-terminal");
    fixture.git_at(
        &fixture.e.repo(),
        &["worktree", "lock", path.to_str().unwrap()],
    );

    for args in [&["gc", &id, "--dry-run"][..], &["gc", &id][..]] {
        let data = fixture.run(args);
        let issue = gc_issue(&data, &id);
        gc_status(issue, "worktree", "skipped");
        gc_status(issue, "branch", "skipped");
        assert!(
            issue["worktree"]["reason"]
                .as_str()
                .unwrap()
                .contains("locked")
        );
        assert!(path.is_dir());
        assert!(fixture.branch_exists(&id));
    }
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
    fixture.claim(
        &id,
        Some(&fixture.e.repo()),
        "term-history",
        &fixture.host,
    );
    fixture.e.ok(&["unclaim", &id]);
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

    let preview = fixture.run(&["gc", &id, "--dry-run"]);
    let issue = gc_issue(&preview, &id);
    gc_status(issue, "worktree", "removed");
    gc_status(issue, "branch", "removed");
    assert!(first.is_dir());
    assert!(second.is_dir());

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
fn gc_primary_checkout_claims_do_not_own_git_resources() {
    let fixture = GcEnv::new();
    let id = fixture.e.create(&["Legacy run in the primary checkout"]);
    let alias = fixture.e.dir.path().join("primary-alias");
    std::os::unix::fs::symlink(fixture.e.repo(), &alias).unwrap();
    fixture.claim(&id, Some(&alias), "missing-terminal", &fixture.host);
    fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
    fixture.git_at(&fixture.e.repo(), &["branch", &id]);
    let notes = fixture.notes(&id);

    for args in [&["gc", &id, "--dry-run"][..], &["gc", &id][..]] {
        let data = fixture.run(args);
        let issue = gc_issue(&data, &id);
        assert_eq!(issue["worktree"]["status"], "absent", "{issue:#}");
        gc_status(issue, "branch", "absent");
        assert!(fixture.e.repo().is_dir());
        assert!(fixture.branch_exists(&id));
        assert_eq!(fixture.notes(&id), notes);
    }

    // Classification survives executable restarts; no in-process skip cache needed.
    for _ in 0..2 {
        let output = fixture
            .cmd()
            .env("TRK_NOW", "2026-11-20T12:00:00Z")
            .arg("check")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("gc:"), "{stdout}");
        assert!(fixture.e.repo().is_dir());
        assert!(fixture.branch_exists(&id));
        assert_eq!(fixture.notes(&id), notes);
    }
}

#[test]
fn gc_absent_legacy_worktrees_are_not_outside_directory_skips() {
    for registered in [false, true] {
        let fixture = GcEnv::new();
        let id = fixture.e.create(&["Legacy worktree already removed"]);
        fixture.claim(
            &id,
            Some(&fixture.e.repo()),
            "missing-terminal",
            &fixture.host,
        );
        fixture.e.ok(&["unclaim", &id]);
        let path = fixture.worktree_at(
            &id,
            &fixture
                .e
                .dir
                .path()
                .join("Library/Application Support/trk/worktrees")
                .join(&id),
        );
        fixture.claim(&id, Some(&path), "missing-terminal", &fixture.host);
        fixture.close_at(&id, "done", "2026-11-20T10:00:00Z");
        if registered {
            std::fs::remove_dir_all(&path).unwrap();
        } else {
            fixture.git_at(
                &fixture.e.repo(),
                &["worktree", "remove", path.to_str().unwrap()],
            );
        }

        let preview = fixture.run(&["gc", &id, "--dry-run"]);
        let issue = gc_issue(&preview, &id);
        gc_status(issue, "worktree", "absent");
        gc_status(
            issue,
            "branch",
            if registered { "skipped" } else { "removed" },
        );
        assert!(fixture.branch_exists(&id));

        let data = fixture.run(&["gc", &id]);
        let issue = gc_issue(&data, &id);
        gc_status(issue, "worktree", "absent");
        gc_status(
            issue,
            "branch",
            if registered { "skipped" } else { "removed" },
        );
        assert_eq!(fixture.branch_exists(&id), registered);
        assert!(fixture.e.repo().is_dir());

        for _ in 0..2 {
            let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
            let issue = gc_issue(&data["gc"], &id);
            gc_status(issue, "worktree", "absent");
            gc_status(
                issue,
                "branch",
                if registered { "skipped" } else { "absent" },
            );
            assert_eq!(fixture.branch_exists(&id), registered);
        }
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

    let sweep = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    let issue = gc_issue(&sweep["gc"], &id);
    assert_eq!(issue["worktree"]["status"], "absent", "{issue:#}");
    gc_status(issue, "branch", "absent");
    assert!(fixture.branch_exists(&id));
    assert!(fixture.e.repo().is_dir());
    assert_eq!(fixture.notes(&id), notes);
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

fn check_gc_at(fixture: &GcEnv, now: &str, args: &[&str]) -> Value {
    let output = fixture
        .cmd()
        .env("TRK_NOW", now)
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{args:?}: {error}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(output.status.success(), "{value:#}");
    assert_eq!(value["ok"], true, "{value:#}");
    value["data"].clone()
}

fn check_gc_config(fixture: &GcEnv, settings: &str) {
    let path = fixture.e.trk_dir().join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut in_check = false;
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_check = trimmed == "[check]";
        }
        if in_check
            && trimmed
                .split_once('=')
                .is_some_and(|(key, _)| matches!(key.trim(), "gc" | "gc_grace"))
        {
            continue;
        }
        lines.push(line);
        if trimmed == "[check]" {
            lines.push(settings);
        }
    }
    assert!(text.lines().any(|line| line.trim() == "[check]"));
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

#[test]
fn check_gc_missing_keys_default_to_enabled_with_one_hour_grace() {
    let fixture = GcEnv::new();
    check_gc_config(&fixture, "");
    let (id, path) = fixture.closed("done", "term-target");
    let notes = fixture.notes(&id);

    let before = check_gc_at(&fixture, "2026-11-20T10:59:59Z", &["check"]);
    assert!(before.get("gc").is_none(), "{before:#}");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);

    let after = check_gc_at(&fixture, "2026-11-20T11:00:00Z", &["check"]);
    assert_eq!(after["gc"]["dry_run"], false);
    for resource in ["session", "worktree", "branch"] {
        gc_status(gc_issue(&after["gc"], &id), resource, "removed");
    }
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert_eq!(fixture.closed_spaces(), ["target"]);
}

#[test]
fn check_gc_zero_grace_collects_issues_closed_in_the_same_pass() {
    let fixture = GcEnv::new();
    check_gc_config(&fixture, "gc_grace = \"0\"");
    let id = fixture.e.create(&["Close and collect"]);
    let path = fixture.worktree(&id);
    fixture.claim(&id, Some(&path), "term-target", &fixture.host);
    fixture
        .e
        .ok(&["wait", &id, "--check", "exit 0", "--reason", "testing"]);

    let data = check_gc_at(&fixture, "2026-11-20T10:00:00Z", &["check"]);
    assert_eq!(data["results"][0]["issue"], id);
    assert_eq!(data["results"][0]["result"], "passed");
    gc_status(gc_issue(&data["gc"], &id), "worktree", "removed");
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert_eq!(fixture.e.ok(&["show", &id])["state"], "closed");
}

#[test]
fn check_gc_can_be_disabled_without_disabling_manual_gc() {
    for settings in ["gc = false", "gc = true"] {
        let fixture = GcEnv::new();
        check_gc_config(&fixture, settings);
        let (id, path) = fixture.closed("done", "term-target");
        let notes = fixture.notes(&id);
        let args: &[&str] = if settings == "gc = false" {
            &["check"]
        } else {
            &["check", "--no-gc"]
        };
        let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", args);
        assert!(data.get("gc").is_none(), "{data:#}");
        assert!(path.exists());
        assert!(fixture.branch_exists(&id));
        assert!(fixture.closed_spaces().is_empty());
        assert_eq!(fixture.notes(&id), notes);

        gc_status(
            gc_issue(&fixture.run(&["gc", &id]), &id),
            "worktree",
            "removed",
        );
        assert!(!path.exists());
    }
}

#[test]
fn check_gc_explicit_id_does_not_sweep_other_closed_issues() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    let waiting = fixture.e.waiting("Selected check", "exit 0");
    let notes = fixture.notes(&id);
    let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check", &waiting]);
    assert_eq!(data["results"][0]["result"], "passed");
    assert!(data.get("gc").is_none(), "{data:#}");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn check_gc_ignores_abandoned_issues() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("abandoned", "term-target");
    let notes = fixture.notes(&id);
    let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    assert!(data.get("gc").is_none(), "{data:#}");
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
}

#[test]
fn check_gc_dry_run_previews_without_changing_resources_notes_or_database() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    let waiting = fixture.e.waiting("Preview check", "exit 0");
    let before = fixture.e.ok(&["show", &waiting, "--with", "notes"]);
    let notes = fixture.notes(&id);
    let database = fixture.e.trk_dir().join("trk.db");
    let bytes = std::fs::read(&database).unwrap();

    let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check", "--dry-run"]);
    assert_eq!(data["gc"]["dry_run"], true);
    let issue = gc_issue(&data["gc"], &id);
    for resource in ["session", "worktree", "branch"] {
        gc_status(issue, resource, "removed");
        assert!(
            issue[resource]["reason"]
                .as_str()
                .unwrap()
                .starts_with("would remove ")
        );
    }
    assert_eq!(std::fs::read(database).unwrap(), bytes);
    assert!(path.exists());
    assert!(fixture.branch_exists(&id));
    assert!(fixture.closed_spaces().is_empty());
    assert_eq!(fixture.notes(&id), notes);
    assert_eq!(fixture.e.ok(&["show", &waiting, "--with", "notes"]), before);
}

#[test]
fn check_gc_session_failure_does_not_change_check_results_or_exit_status() {
    let fixture = GcEnv::new();
    let (id, path) = fixture.closed("done", "term-target");
    std::fs::write(
        fixture.bin.join("herdr-app"),
        r#"#!/bin/sh
if [ "$1" = ls ]; then
    printf '%s\n' '{"spaces":[{"id":"target","tabs":[{"terminals":[{"terminal":"term-target"}]}]}]}'
else
    exit 1
fi
"#,
    )
    .unwrap();
    let waiting = fixture.e.waiting("Still closes", "exit 0");

    let data = check_gc_at(&fixture, "2026-11-20T12:00:00Z", &["check"]);
    assert_eq!(data["results"][0]["issue"], waiting);
    assert_eq!(data["results"][0]["result"], "passed");
    assert_eq!(fixture.e.ok(&["show", &waiting])["state"], "closed");
    assert!(
        data["gc"]["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty()),
        "{data:#}"
    );
    gc_status(gc_issue(&data["gc"], &id), "worktree", "removed");
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&id));
    assert!(!fixture.notes(&id).as_array().unwrap().is_empty());
}

#[test]
fn check_gc_watch_sweeps_each_pass_and_keeps_skipped_resources_quiet() {
    let fixture = GcEnv::new();
    check_gc_config(&fixture, "gc_grace = \"0\"");
    let (first, first_path) = fixture.closed("done", "term-target");
    let (skipped, skipped_path) = fixture.closed("done", "missing-terminal");
    std::fs::write(skipped_path.join("dirty"), "keep this work").unwrap();
    let skipped_notes = fixture.notes(&skipped);
    fixture.e.waiting(
        "Watch heartbeat",
        "echo pass >> \"$TRK_REPO/watch-passes\"; exit 75",
    );
    let output_path = fixture.e.dir.path().join("watch-output");
    let stderr_path = fixture.e.dir.path().join("watch-stderr");
    let path = std::env::join_paths(
        std::iter::once(fixture.bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
    command
        .args(["check", "--all", "--watch", "1s"])
        .env("PATH", path)
        .env("TRK_TEST_UI", fixture.e.dir.path())
        .env_remove("HC_TERMINAL_ID")
        .stdout(std::fs::File::create(&output_path).unwrap())
        .stderr(std::fs::File::create(&stderr_path).unwrap());
    let mut runner = CheckRunner::spawn_command(&fixture.e, command);
    check_wait_until("the first watch pass to collect a closed issue", || {
        !first_path.exists()
    });

    let second = fixture.e.create(&["Closed between watch passes"]);
    let second_path = fixture.worktree(&second);
    fixture.claim(&second, Some(&second_path), "term-history", &fixture.host);
    fixture.close_at(&second, "done", "2026-11-20T10:00:00Z");
    check_wait_until(
        "a later watch pass to collect a manually closed issue",
        || !second_path.exists(),
    );
    let passes = || {
        std::fs::read_to_string(fixture.e.repo().join("watch-passes"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let before = passes();
    check_wait_until("two more watch passes with only skipped resources", || {
        passes() >= before + 2
    });
    runner.signal("-INT");
    runner.wait();

    let output = std::fs::read_to_string(output_path).unwrap();
    assert_eq!(output.matches(&first).count(), 1, "{output}");
    assert_eq!(output.matches(&second).count(), 1, "{output}");
    assert_eq!(output.matches(&skipped).count(), 1, "{output}");
    assert!(skipped_path.exists());
    assert!(fixture.branch_exists(&skipped));
    assert_eq!(fixture.notes(&skipped), skipped_notes);
}
