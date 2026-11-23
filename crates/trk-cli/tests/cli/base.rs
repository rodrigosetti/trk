use super::*;

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
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(repo: &Path, message: &str) {
    git(
        repo,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ],
    );
}

fn installed_base(e: &Env) -> String {
    let config: toml::Value =
        toml::from_str(&std::fs::read_to_string(e.trk_dir().join("config.toml")).unwrap()).unwrap();
    config["run"]["base"].as_str().unwrap().to_string()
}

#[test]
fn install_detects_origin_then_primary_branch_then_main() {
    let e = Env::new();
    let repo = e.repo();
    git(&repo, &["branch", "-m", "develop"]);
    git(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    e.ok(&["install", "--force"]);
    assert_eq!(installed_base(&e), "trunk");

    git(
        &repo,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    // Installing from a linked worktree still detects the primary branch.
    let linked = e.dir.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            linked.to_str().unwrap(),
        ],
    );
    let out = e
        .cmd()
        .current_dir(&linked)
        .args(["--json", "install", "--force"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(installed_base(&e), "develop");

    git(&repo, &["checkout", "-q", "--detach"]);
    e.ok(&["install", "--force"]);
    assert_eq!(installed_base(&e), "main");
}

#[test]
fn run_starts_at_base_and_seed_recipe_lands_with_trk_base() {
    // Both an explicit setting and legacy config detection must ignore the
    // unrelated branch currently checked out in the primary checkout.
    for explicit in [true, false] {
        let e = Env::new();
        let repo = e.repo();
        git(&repo, &["branch", "-m", "trunk"]);
        git(&repo, &["branch", "develop"]);
        git(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        let base = if explicit { "develop" } else { "trunk" };
        e.set_config(
            "base = \"main\"",
            if explicit { "base = \"develop\"" } else { "" },
        );
        let base_commit = git(&repo, &["rev-parse", base]);
        git(&repo, &["checkout", "-q", "-b", "unrelated"]);
        commit(&repo, "unrelated work");
        assert_ne!(git(&repo, &["rev-parse", "HEAD"]), base_commit);

        let id = e.create(&["Land on the configured base"]);
        let spec = e.ok(&["run", &id, "--harness", "omp", "--print"]);
        let wt = PathBuf::from(spec["worktree"].as_str().unwrap());
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), base_commit);
        commit(&wt, "issue change");
        let issue_commit = git(&wt, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-q", base]);

        // Execute the actual rendered seed recipe, not a separately maintained
        // copy. Requiring TRK_BASE in the check also proves it is exported.
        let preview = e.ok(&["agent", "preview", "implementer"]);
        let instructions = preview["instructions"].as_str().unwrap();
        let recipe = instructions
            .split_once("```sh\n")
            .unwrap()
            .1
            .split_once("```")
            .unwrap()
            .0;
        let check = format!("[ \"$TRK_BASE\" = {base} ] || exit 2; {recipe}");
        let out = e
            .cmd()
            .current_dir(&wt)
            .env("TRK_ACTOR", spec["actor"].as_str().unwrap())
            .args(["--json", "wait", &id, "--check", &check])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let result = e.ok(&["check", &id]);
        assert_eq!(result["results"][0]["result"], "passed", "{result:#}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), issue_commit);
        assert_eq!(e.ok(&["show", &id])["state"], "closed");
    }
}

#[test]
fn legacy_config_uses_primary_branch_from_linked_worktree() {
    let e = Env::new();
    let repo = e.repo();
    git(&repo, &["branch", "-m", "develop"]);
    e.set_config("base = \"main\"", "");
    let linked = e.dir.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            linked.to_str().unwrap(),
        ],
    );
    let id = e.create(&["Check the runtime primary branch fallback"]);
    e.ok(&["claim", &id, "--worktree", linked.to_str().unwrap()]);
    e.ok(&["wait", &id, "--check", "[ \"$TRK_BASE\" = develop ]"]);
    let result = e.ok(&["check", &id]);
    assert_eq!(result["results"][0]["result"], "passed", "{result:#}");
}
