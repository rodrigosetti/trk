use super::*;

/// Fake tmux speaks the seed's argv protocol, without a server or a terminal.
struct Tmux {
    e: Env,
    bin: PathBuf,
}

impl Tmux {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let e = Env::named();
        e.set_config("use = [\"herdr\", \"hc\"]", "use = [\"tmux\"]");
        e.set_config("command = \"omp\"", "command = \"sh\"");
        e.set_config("send_delay = 5", "send_delay = 0");
        e.set_config("send_retries = 10", "send_retries = 1");
        e.set_config("send_retry_delay = 3", "send_retry_delay = 0");
        let bin = e.dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(e.dir.path().join("sessions")).unwrap();
        for (name, script) in [
            ("hc", "#!/bin/sh\nexit 1\n"),
            ("herdr-app", "#!/bin/sh\nexit 1\n"),
            (
                "tmux",
                r#"#!/bin/sh
set -eu
cd "$TRK_TEST_LAUNCHER"
printf '%s\n' "$@" >> trace
case "$1" in
-V)
    [ ! -f unavailable-tmux ] || exit 3
    printf 'tmux fake\n'
    ;;
new-session)
    shift
    generated=''
    while [ "$#" -gt 0 ]; do
        case "$1" in
            -s) generated="$2"; shift ;;
            -c) printf '%s' "$2" > cwd; shift ;;
            -F) [ "$2" = '#{session_id}' ]; shift ;;
            -d|-P) ;;
            *) printf '%s' "$1" > shell-command ;;
        esac
        shift
    done
    printf '%s' "$generated" > generated
    address='$41'
    [ "$TRK_TEST_START" != stderr-only ] || address="$generated"
    : > "sessions/$address"
    printf '%s' "$address" > address
    printf 'diagnostic-not-an-address\n' >&2
    if [ "$TRK_TEST_START" != stderr-only ]; then
        printf '%s\nignored-second-line\n' "$address"
    fi
    ;;
has-session)
    [ "$2" = -t ]
    [ -f "sessions/$3" ] || exit 4
    ;;
attach-session|switch-client)
    [ "$2" = -t ] && [ -f "sessions/$3" ]
    printf '%s\n%s\n' "$1" "$3" >> attachments
    ;;
send-keys)
    [ "$2" = -t ] && [ -f "sessions/$3" ] && [ "$4" = -l ]
    printf '%s\n%s\n' "$3" "$5" >> messages
    ;;
rename-session)
    [ "$2" = -t ] && [ -f "sessions/$3" ]
    printf '%s\n%s\n' "$3" "$4" >> renames
    ;;
kill-session)
    [ "$2" = -t ]
    rm "sessions/$3"
    printf '%s\n' "$3" >> closes
    ;;
*) exit 9 ;;
esac
"#,
            ),
        ] {
            let path = bin.join(name);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { e, bin }
    }

    fn cmd(&self) -> Command {
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut cmd = self.e.cmd();
        cmd.env("PATH", path)
            .env("TRK_TEST_LAUNCHER", self.e.dir.path())
            .env("TRK_TEST_START", "normal")
            .env_remove("HC_TERMINAL_ID")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        cmd
    }

    fn ok(&self, args: &[&str]) -> Value {
        resume::success(self.cmd(), args)
    }

    fn text(&self, name: &str) -> String {
        let path = self.e.dir.path().join(name);
        if path.exists() {
            std::fs::read_to_string(path).unwrap()
        } else {
            String::new()
        }
    }

    fn launch(&self) -> String {
        let id = self.e.create(&["Named launcher issue", "--type", "review"]);
        self.ok(&["run", &id, "--harness", "omp"]);
        id
    }

    fn pin_to_other_selection(&self) {
        self.e.set_config("use = [\"tmux\"]", "use = [\"hc\"]");
    }
}

#[test]
fn named_tmux_run_binds_only_first_stdout_line_and_pins_send_and_attach() {
    let tmux = Tmux::new();
    let id = tmux.launch();
    let issue = tmux.e.ok(&["show", &id]);
    assert_eq!(issue["claim"]["launcher"], "tmux");
    assert_eq!(issue["claim"]["terminal"], "$41");
    assert_eq!(
        tmux.text("cwd"),
        tmux.e.repo().canonicalize().unwrap().to_str().unwrap()
    );
    assert!(tmux.text("shell-command").contains("TRK_ACTOR"));
    assert_eq!(tmux.text("generated"), format!("trk-{id}"));

    tmux.pin_to_other_selection();
    tmux.ok(&["attach", &id]);
    tmux.ok(&["send", &id, "spaces and 'quotes' stay one argument"]);
    assert_eq!(tmux.text("attachments"), "switch-client\n$41\n");
    assert_eq!(
        tmux.text("messages"),
        "$41\nspaces and 'quotes' stay one argument\n"
    );
}

#[test]
fn named_start_empty_stdout_keeps_generated_name_and_ignores_stderr() {
    let tmux = Tmux::new();
    let id = tmux.e.create(&["Stderr is diagnostic", "--type", "review"]);
    let mut cmd = tmux.cmd();
    cmd.env("TRK_TEST_START", "stderr-only");
    resume::success(cmd, &["run", &id, "--harness", "omp"]);
    let issue = tmux.e.ok(&["show", &id]);
    assert_eq!(issue["claim"]["terminal"], tmux.text("generated"));
    assert_ne!(issue["claim"]["terminal"], "diagnostic-not-an-address");
    tmux.ok(&["send", &id, "still addressable"]);
    assert!(tmux.text("messages").ends_with("still addressable\n"));
}

#[test]
fn named_custom_template_uses_project_and_resolved_routing_and_keeps_claim_address() {
    let tmux = Tmux::new();
    tmux.e.set_config(
        "[launcher.tmux]",
        "[launcher.tmux]\nname = \"{project}.{prefix}.{agent}.{type}.{id}\"\nname_max = 64\nname_chars = \"a-z0-9_.-\"",
    );
    let id = tmux.e.create(&["Custom terminal name", "--type", "review"]);
    let mut cmd = tmux.cmd();
    cmd.env("TRK_TEST_START", "stderr-only");
    let launched = resume::success(cmd, &["run", &id, "--harness", "omp"]);
    let expected = format!("repo.orch.reviewer.review.{id}");
    assert_eq!(tmux.text("generated"), expected);
    assert_eq!(launched["terminal"], expected);
    assert_eq!(tmux.e.ok(&["show", &id])["claim"]["terminal"], expected);

    tmux.e.set_config(
        "name = \"{project}.{prefix}.{agent}.{type}.{id}\"",
        "name = \"changed-{id}\"",
    );
    tmux.ok(&["send", &id, "recorded address"]);
    assert_eq!(
        tmux.text("messages"),
        format!("{expected}\nrecorded address\n")
    );
}

#[test]
fn named_custom_length_and_character_set_apply_before_start() {
    let tmux = Tmux::new();
    tmux.e.set_config(
        "[launcher.tmux]",
        "[launcher.tmux]\nname = \"ABC.DEF_0123456789\"\nname_max = 10\nname_chars = \"a-z._\"",
    );
    tmux.launch();
    assert_eq!(tmux.text("generated"), "abc.def_--");
}

#[test]
fn named_tmux_resume_wake_notify_rename_and_gc_keep_the_pinned_profile() {
    let tmux = Tmux::new();
    let id = tmux.launch();
    tmux.pin_to_other_selection();
    tmux.e.ok(&["wait", &id, "--force", "-r", "human decision"]);
    let resumed = tmux.ok(&["resume", &id, "-m", "continue with tmux"]);
    assert_eq!(resumed["delivered"], true);
    assert_eq!(tmux.e.ok(&["show", &id])["claim"]["launcher"], "tmux");

    tmux.e.ok(&[
        "wait",
        &id,
        "--force",
        "-c",
        "echo named wake evidence; exit 1",
    ]);
    let checked = tmux.ok(&["check", &id]);
    assert_eq!(checked["results"][0]["result"], "woken");
    assert_eq!(tmux.e.ok(&["show", &id])["claim"]["launcher"], "tmux");
    assert!(tmux.text("messages").contains("named wake evidence"));

    tmux.e.ok(&["wait", &id, "--force", "-c", "exit 0"]);
    assert_eq!(tmux.ok(&["check", &id])["results"][0]["result"], "passed");
    assert!(tmux.text("messages").contains("waiting check passed"));
    assert_eq!(tmux.text("renames"), "$41\n[closed] Named launcher issue\n");
    tmux.ok(&["gc", &id]);
    assert_eq!(tmux.text("closes"), "$41\n");
    assert!(!tmux.e.dir.path().join("sessions/$41").exists());
}

#[test]
fn named_selection_skips_unavailable_profiles_and_missing_probe_is_usable() {
    let tmux = Tmux::new();
    tmux.e
        .set_config("use = [\"tmux\"]", "use = [\"offline\", \"tmux\"]");
    tmux.e.set_config("[launcher.tmux]", "[launcher.offline]\navailable = [\"sh\", \"-c\", \"exit 7\"]\nstart = [\"must-not-start\"]\n\n[launcher.tmux]");
    tmux.e.set_config("available = [\"tmux\", \"-V\"]", "");
    let id = tmux.launch();
    assert_eq!(tmux.e.ok(&["show", &id])["claim"]["launcher"], "tmux");
    assert!(!tmux.text("trace").lines().any(|line| line == "-V"));
}

#[test]
fn named_selection_failure_leaves_issue_open_without_a_session() {
    let tmux = Tmux::new();
    std::fs::write(tmux.e.dir.path().join("unavailable-tmux"), "").unwrap();
    tmux.e.set_config(
        "[launcher.tmux]",
        "[launcher.offline]\navailable = [\"sh\", \"-c\", \"exit 7\"]\n\n[launcher.tmux]",
    );
    for (selection, tried) in [
        ("[\"offline\", \"tmux\"]", "profiles tried: offline, tmux"),
        ("[]", "profiles tried: (none; [launcher] use is empty)"),
    ] {
        tmux.e
            .set_config("use = [\"tmux\"]", &format!("use = {selection}"));
        let id = tmux
            .e
            .create(&["No available launcher", "--type", "review"]);
        let out = tmux
            .cmd()
            .args(["--json", "run", &id, "--harness", "omp"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["error"]["code"], "config_invalid");
        let hint = result["error"]["hint"].as_str().unwrap();
        for remedy in [tried, "install", "[launcher] use", "trk run --fg"] {
            assert!(hint.contains(remedy), "{hint}");
        }
        let issue = tmux.e.ok(&["show", &id]);
        assert_eq!(issue["state"], "open");
        assert!(issue["claim"].is_null());
        assert_eq!(tmux.text("generated"), "");
        tmux.e
            .set_config(&format!("use = {selection}"), "use = [\"tmux\"]");
    }
}

#[test]
fn named_missing_operations_are_skipped_and_do_not_borrow_other_profiles() {
    let tmux = Tmux::new();
    tmux.e.set_config("use = [\"tmux\"]", "use = [\"bare\"]");
    tmux.e.set_config("[launcher.tmux]", "[launcher.bare]\nstart = [\"tmux\", \"new-session\", \"-s\", \"{name}\", \"-c\", \"{cwd}\", \"{shell_command}\"]\n\n[launcher.tmux]");
    let id = tmux.launch();
    assert_eq!(tmux.e.ok(&["show", &id])["claim"]["launcher"], "bare");
    tmux.ok(&["attach", &id]);
    tmux.ok(&["send", &id, "not delivered"]);
    assert_eq!(tmux.text("attachments"), "");
    assert_eq!(tmux.text("messages"), "");
    tmux.e.ok(&["wait", &id, "--force", "-c", "exit 1"]);
    assert_eq!(tmux.ok(&["check", &id])["results"][0]["result"], "failed");
    assert_eq!(tmux.e.ok(&["show", &id])["state"], "open");
    tmux.e.ok(&["close", &id]);
    tmux.ok(&["gc", &id]);
    assert_eq!(tmux.text("closes"), "");
    assert!(tmux.e.dir.path().join("sessions/$41").exists());
}

#[test]
fn named_exists_nonzero_means_gone_and_prevents_resume() {
    let tmux = Tmux::new();
    let id = tmux.launch();
    tmux.e.ok(&["wait", &id, "--force", "-r", "decision"]);
    std::fs::remove_file(tmux.e.dir.path().join("sessions/$41")).unwrap();
    let out = tmux
        .cmd()
        .args(["--json", "resume", &id, "-m", "cannot deliver"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["error"]["code"], "invalid_state");
    assert_eq!(tmux.e.ok(&["show", &id])["state"], "waiting");
    assert_eq!(tmux.text("messages"), "");
}

#[test]
fn named_seed_selects_hc_when_herdr_is_unavailable() {
    let tmux = Tmux::new();
    tmux.e
        .set_config("use = [\"tmux\"]", "use = [\"herdr\", \"hc\"]");
    std::fs::write(
        tmux.bin.join("hc"),
        r#"#!/bin/sh
set -eu
case "$1" in
ls) exit 0 ;;
run)
    [ "$4" = "--name" ]
    printf '%s' "$5" > "$TRK_TEST_LAUNCHER/hc-generated"
    : > "$TRK_TEST_LAUNCHER/hc-started"
    printf 'hc-session\n'
    ;;
*) exit 9 ;;
esac
"#,
    )
    .unwrap();
    let id = tmux.launch();
    let issue = tmux.e.ok(&["show", &id]);
    assert_eq!(issue["claim"]["launcher"], "hc");
    assert_eq!(issue["claim"]["terminal"], "hc-session");
    assert!(tmux.e.dir.path().join("hc-started").exists());
    assert_eq!(tmux.text("hc-generated"), format!("trk-{id}"));
    assert_eq!(tmux.text("generated"), "");
}

#[test]
fn named_attach_falls_back_to_attach_when_focus_is_missing() {
    let tmux = Tmux::new();
    tmux.e.set_config(
        "focus = [\"tmux\", \"switch-client\", \"-t\", \"{name}\"]",
        "",
    );
    let id = tmux.launch();
    tmux.ok(&["attach", &id]);
    assert_eq!(tmux.text("attachments"), "attach-session\n$41\n");
}

#[test]
fn unnamed_claims_remain_visible_without_controlling_or_reaping_the_terminal() {
    for launcher in [None, Some("")] {
        for (check, result, state) in [("exit 0", "passed", "closed"), ("exit 1", "failed", "open")]
        {
            let tmux = Tmux::new();
            let id = tmux.launch();
            // A pre-profile claim, with an address that still exists today.
            let store = trk_core::Store::open(
                &tmux.e.trk_dir().join("trk.db"),
                trk_core::StoreOpts::default(),
            )
            .unwrap();
            store
                .conn()
                .execute(
                    "UPDATE claim SET launcher = ?1 WHERE issue_id = ?2",
                    (launcher, id.as_str()),
                )
                .unwrap();
            tmux.pin_to_other_selection();

            for waiting in [false, true] {
                if waiting {
                    tmux.e.ok(&["wait", &id, "--force", "-c", check]);
                }
                let snapshot = tmux.ok(&["dashboard"]);
                let issue = snapshot["issues"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|issue| issue["id"] == id)
                    .unwrap();
                assert!(issue["session"]["alive"].is_null(), "{issue}");
                assert_eq!(issue["session"]["terminal"], "$41");
                assert_eq!(issue["session"]["active"], !waiting);
                let runs = tmux.ok(&["runs"]);
                assert_eq!(runs[0]["issue"], id);
                assert!(runs[0]["session_alive"].is_null(), "{runs}");
                assert_eq!(tmux.ok(&["reap"]), json!([]));

                let mut operations = vec![
                    vec!["attach", id.as_str()],
                    vec!["send", id.as_str(), "do not deliver"],
                ];
                if waiting {
                    operations.push(vec!["resume", id.as_str(), "-m", "do not restore"]);
                }
                for args in operations {
                    let out = tmux.cmd().arg("--json").args(&args).output().unwrap();
                    assert!(!out.status.success(), "{args:?}");
                    let response: Value = serde_json::from_slice(&out.stdout).unwrap();
                    assert_eq!(response["error"]["code"], "invalid_state", "{response}");
                }
                assert_eq!(
                    tmux.e.ok(&["show", &id])["state"],
                    if waiting { "waiting" } else { "claimed" }
                );
            }

            assert_eq!(tmux.ok(&["check", &id])["results"][0]["result"], result);
            assert_eq!(tmux.e.ok(&["show", &id])["state"], state);
            if state == "open" {
                tmux.e.ok(&["close", &id]);
            }
            let gc = tmux.ok(&["gc", &id]);
            assert!(gc.get("errors").is_none(), "{gc}");
            assert_eq!(gc["issues"][0]["session"]["status"], "skipped", "{gc}");
            assert!(tmux.e.dir.path().join("sessions/$41").exists());
            for operation in ["attachments", "messages", "renames", "closes"] {
                assert_eq!(tmux.text(operation), "", "{operation}");
            }
            let claims = tmux.e.ok(&["show", &id, "--with", "claims"]);
            assert_eq!(claims["claims"][0]["launcher"], json!(launcher));
        }
    }
}

#[test]
fn named_missing_claim_profile_is_still_a_configuration_error() {
    let tmux = Tmux::new();
    let id = tmux.launch();
    tmux.e.set_config("[launcher.tmux]", "[launcher.removed]");
    for operation in ["dashboard", "runs"] {
        let out = tmux.cmd().args(["--json", operation]).output().unwrap();
        assert!(!out.status.success());
        let response: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(response["error"]["code"], "config_invalid", "{response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("launcher.tmux"),
            "{response}"
        );
    }
    assert_eq!(tmux.e.ok(&["show", &id])["claim"]["launcher"], "tmux");
}
