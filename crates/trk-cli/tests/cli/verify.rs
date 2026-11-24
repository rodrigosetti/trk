use super::*;

fn recipe() -> &'static str {
    include_str!("../../../../seed/prompts/_landing.merge.md.j2")
        .split_once("Merge your branch")
        .unwrap()
        .1
        .split_once("```sh\n")
        .unwrap()
        .1
        .split_once("```")
        .unwrap()
        .0
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn configure(e: &Env, verify: Option<&str>) {
    git(&e.repo(), &["config", "user.name", "Landing test"]);
    git(&e.repo(), &["config", "user.email", "landing@example.test"]);
    git(&e.repo(), &["branch", "-m", "trunk"]);
    let path = e.trk_dir().join("config.toml");
    let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    config["run"]["base"] = "trunk".into();
    let check = config["check"].as_table_mut().unwrap();
    match verify {
        Some(command) => {
            check.insert("verify".into(), command.into());
        }
        None => {
            check.remove("verify");
        }
    }
    std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
}

fn commit_file(repo: &Path, name: &str, contents: &str) {
    std::fs::write(repo.join(name), contents).unwrap();
    git(repo, &["add", "--", name]);
    git(repo, &["commit", "-q", "-m", name]);
}

fn issue(e: &Env, title: &str) -> (String, PathBuf, String) {
    let id = e.create(&[title]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
    (
        id,
        PathBuf::from(spec["worktree"].as_str().unwrap()),
        spec["actor"].as_str().unwrap().to_owned(),
    )
}

fn wait(e: &Env, id: &str, worktree: &Path, actor: &str) {
    let out = e
        .cmd()
        .current_dir(worktree)
        .env("TRK_ACTOR", actor)
        .args(["--json", "wait", id, "--check", recipe()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn assert_open_with_output(e: &Env, id: &str, evidence: &str) {
    let issue = e.ok(&["show", id]);
    assert_eq!(issue["state"], "open", "{issue:#}");
    assert!(
        issue["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note["body"].as_str().unwrap().contains(evidence)),
        "{issue:#}"
    );
}

fn assert_no_merge(repo: &Path) {
    let out = StdCommand::new("git")
        .current_dir(repo)
        .args(["rev-parse", "--verify", "MERGE_HEAD"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "unfinished merge in {repo:?}");
    assert!(git(repo, &["diff", "--name-only", "--diff-filter=U"]).is_empty());
}

#[test]
fn verifies_the_combined_tree_before_landing_each_issue() {
    let e = Env::new();
    let verify = "if [ -f first ] && [ -f second ]; then echo combined-tree-failure; exit 1; fi";
    configure(&e, Some(verify));
    let (first, first_wt, first_actor) = issue(&e, "First independent change");
    let (second, second_wt, second_actor) = issue(&e, "Second independent change");
    commit_file(&first_wt, "first", "first\n");
    commit_file(&second_wt, "second", "second\n");
    for wt in [&first_wt, &second_wt] {
        assert!(
            StdCommand::new("sh")
                .current_dir(wt)
                .args(["-c", verify])
                .status()
                .unwrap()
                .success()
        );
    }
    wait(&e, &first, &first_wt, &first_actor);
    wait(&e, &second, &second_wt, &second_actor);
    let result = e.ok(&["check", &first]);
    assert_eq!(result["results"][0]["result"], "passed", "{result:#}");
    assert_eq!(e.ok(&["show", &first])["state"], "closed");
    let landed = git(&e.repo(), &["rev-parse", "HEAD"]);
    assert_eq!(landed, git(&first_wt, &["rev-parse", "HEAD"]));
    let result = e.ok(&["check", &second]);
    assert_eq!(result["results"][0]["result"], "failed", "{result:#}");
    assert!(
        result["results"][0]["output"]
            .as_str()
            .unwrap()
            .contains("combined-tree-failure")
    );
    assert_eq!(git(&e.repo(), &["rev-parse", "HEAD"]), landed);
    assert!(!e.repo().join("second").exists());
    assert!(second_wt.join("first").exists());
    assert!(second_wt.join("second").exists());
    assert_open_with_output(&e, &second, "combined-tree-failure");
}

#[test]
fn verify_failure_wakes_a_live_waiting_session_even_for_exit_75() {
    let ui = WaitingUi::new();
    configure(&ui.e, Some("echo verification-wake-evidence; exit 75"));
    let id = ui.e.create(&["Live verification failure"]);
    let worktree = ui.e.dir.path().join("live-worktree");
    git(
        &ui.e.repo(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &id,
            worktree.to_str().unwrap(),
        ],
    );
    commit_file(&worktree, "live-change", "not landed\n");
    let actor = format!("agent:omp:{id}-live");
    let mut store = trk_core::Store::open(
        &ui.e.trk_dir().join("trk.db"),
        trk_core::StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    store
        .write(&actor, false, |c| {
            c.claim(
                &id,
                trk_core::ops::ClaimOpts {
                    host: Some(local_host()),
                    worktree: Some(worktree.to_string_lossy().into_owned()),
                    terminal: Some("exact-origin".into()),
                    launcher: Some("herdr".into()),
                    ..Default::default()
                },
            )?;
            c.patch_actor_meta(&actor, json!({"mcp_pid": std::process::id()}))
        })
        .unwrap();
    drop(store);
    wait(&ui.e, &id, &worktree, &actor);
    let before = git(&ui.e.repo(), &["rev-parse", "HEAD"]);
    let result = ui.check(&id, "");
    assert_eq!(result["result"], "woken", "{result:#}");
    assert_eq!(result["exit"], 1, "{result:#}");
    assert_eq!(result["state"], "claimed", "{result:#}");
    let messages = ui.text("messages");
    assert!(messages.starts_with("exact-origin\n"), "{messages}");
    assert!(
        messages.contains("verification-wake-evidence"),
        "{messages}"
    );
    assert!(messages.contains("waiting check failed"), "{messages}");
    assert_eq!(git(&ui.e.repo(), &["rev-parse", "HEAD"]), before);
    assert_eq!(ui.e.ok(&["show", &id])["state"], "claimed");
}

#[test]
fn conflicting_base_merge_is_aborted_without_landing() {
    let e = Env::new();
    configure(&e, Some("echo verification-must-not-run"));
    commit_file(&e.repo(), "shared", "original\n");
    let (id, worktree, actor) = issue(&e, "Conflicting change");
    commit_file(&worktree, "shared", "issue version\n");
    commit_file(&e.repo(), "shared", "base version\n");
    let before = git(&e.repo(), &["rev-parse", "HEAD"]);
    let issue_before = git(&worktree, &["rev-parse", "HEAD"]);
    wait(&e, &id, &worktree, &actor);
    let result = e.ok(&["check", &id]);
    assert_eq!(result["results"][0]["result"], "failed", "{result:#}");
    let output = result["results"][0]["output"].as_str().unwrap();
    assert!(output.contains("CONFLICT"), "{output}");
    assert!(!output.contains("verification-must-not-run"), "{output}");
    assert_eq!(git(&e.repo(), &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), issue_before);
    assert_no_merge(&e.repo());
    assert_no_merge(&worktree);
    assert_open_with_output(&e, &id, "CONFLICT");
}

#[test]
fn empty_and_legacy_verify_configuration_override_the_inherited_environment() {
    for verify in [Some(""), None] {
        let e = Env::new();
        configure(&e, verify);
        let (id, worktree, actor) = issue(&e, "Verification disabled");
        commit_file(&worktree, "change", "landed\n");
        let issue_head = git(&worktree, &["rev-parse", "HEAD"]);
        wait(&e, &id, &worktree, &actor);
        let out = e
            .cmd()
            .env("TRK_VERIFY", "echo inherited-verify-must-not-run; exit 42")
            .args(["--json", "check", &id])
            .output()
            .unwrap();
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(out.status.success(), "{result:#}");
        assert_eq!(
            result["data"]["results"][0]["result"], "passed",
            "{result:#}"
        );
        assert_eq!(git(&e.repo(), &["rev-parse", "HEAD"]), issue_head);
        assert_eq!(e.ok(&["show", &id])["state"], "closed");
    }
}

#[test]
fn base_advancing_during_verification_cannot_land_an_unverified_merge() {
    let e = Env::new();
    configure(
        &e,
        Some(
            "git -C \"$TRK_REPO\" commit -q --allow-empty -m base-advanced-during-verify || exit 1; echo base-advanced-during-verify",
        ),
    );
    let (id, worktree, actor) = issue(&e, "Base moves during verification");
    commit_file(&worktree, "issue-only", "must not land\n");
    let before = git(&e.repo(), &["rev-parse", "HEAD"]);
    let issue_head = git(&worktree, &["rev-parse", "HEAD"]);
    wait(&e, &id, &worktree, &actor);
    let result = e.ok(&["check", &id]);
    assert_eq!(result["results"][0]["result"], "failed", "{result:#}");
    let output = result["results"][0]["output"].as_str().unwrap();
    assert!(output.contains("base-advanced-during-verify"), "{output}");
    let after = git(&e.repo(), &["rev-parse", "HEAD"]);
    assert_ne!(after, before);
    assert_eq!(git(&e.repo(), &["rev-parse", "HEAD^"]), before);
    assert_eq!(
        git(&e.repo(), &["rev-parse", "HEAD^{tree}"]),
        git(&e.repo(), &["rev-parse", &format!("{before}^{{tree}}")])
    );
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), issue_head);
    assert!(!e.repo().join("issue-only").exists());
    assert_no_merge(&e.repo());
    assert_no_merge(&worktree);
    assert_open_with_output(&e, &id, "base-advanced-during-verify");
}
