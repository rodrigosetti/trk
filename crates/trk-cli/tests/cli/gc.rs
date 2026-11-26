use super::*;

#[path = "gc/resources.rs"]
mod resources;

#[path = "gc/automatic.rs"]
mod automatic;

#[path = "gc/legacy.rs"]
mod legacy;

#[path = "gc/concurrency.rs"]
mod concurrency;

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
printf '%s\n' "$*" >> herdr-calls
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
    if [ "${TRK_TEST_GC_CLOSE_SPACE:-}" = "$4" ]; then
        : > "$TRK_TEST_GC_CLOSE_GATE/ready"
        attempts=0
        while [ ! -f "$TRK_TEST_GC_CLOSE_GATE/release" ]; do
            attempts=$((attempts + 1))
            [ "$attempts" -lt 1000 ] || exit 1
            sleep 0.02
        done
    fi
    : > "closed-spaces/$4"
    printf '%s\n' "$4" >> closes
    ;;
*) exit 1 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = local_host();
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
            self.e.trk_dir().join("types/change.toml"),
            format!(
                "name = \"change\"\ndescription = \"code change\"\nagent = \"implementer\"\nworktree = {enabled}\n"
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
                        launcher: Some("herdr".into()),
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

    fn pause_gc(&self, args: &[&str]) -> CheckRunner {
        let gate = self.e.dir.path().join("gc-gate");
        std::fs::create_dir(&gate).unwrap();
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
        command
            .arg("--json")
            .args(args)
            .env("PATH", path)
            .env("TRK_TEST_UI", self.e.dir.path())
            .env("TRK_TEST_GC_CLOSE_SPACE", "target")
            .env("TRK_TEST_GC_CLOSE_GATE", &gate)
            .env_remove("HC_TERMINAL_ID")
            .stdout(std::fs::File::create(self.e.dir.path().join("gc-output")).unwrap())
            .stderr(std::fs::File::create(self.e.dir.path().join("gc-stderr")).unwrap());
        let runner = CheckRunner::spawn_command(&self.e, command);
        check_wait_until("GC to pause inside launcher close", || {
            gate.join("ready").exists()
        });
        runner
    }

    fn finish_gc(&self, runner: &mut CheckRunner) -> Value {
        std::fs::write(self.e.dir.path().join("gc-gate/release"), "").unwrap();
        let status = runner.wait();
        let stdout = std::fs::read(self.e.dir.path().join("gc-output")).unwrap();
        let stderr = std::fs::read_to_string(self.e.dir.path().join("gc-stderr")).unwrap();
        assert!(
            status.success(),
            "stdout={} stderr={stderr}",
            String::from_utf8_lossy(&stdout)
        );
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(value["ok"], true, "{value:#}");
        value["data"].clone()
    }

    fn log_git_calls(&self) {
        use std::os::unix::fs::PermissionsExt;

        let output = StdCommand::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let real_git = String::from_utf8(output.stdout).unwrap();
        let executable = self.bin.join("git");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRK_TEST_UI/git-calls\"\nexec '{}' \"$@\"\n",
                real_git.trim().replace('\'', "'\\''")
            ),
        )
        .unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn calls(&self, launcher: &str) -> String {
        std::fs::read_to_string(self.e.dir.path().join(format!("{launcher}-calls")))
            .unwrap_or_default()
    }

    fn clear_calls(&self) {
        for launcher in ["herdr", "git"] {
            std::fs::write(self.e.dir.path().join(format!("{launcher}-calls")), "").unwrap();
        }
    }

    fn legacy_claim(&self, id: &str, terminal: &str, host: &str, launcher: Option<&str>) {
        self.store_at("2026-11-20T10:00:00Z")
            .write("human:alice@example.com", false, |context| {
                context.claim(
                    id,
                    trk_core::ops::ClaimOpts {
                        terminal: Some(terminal.into()),
                        host: Some(host.into()),
                        launcher: launcher.map(str::to_owned),
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn legacy_closed(&self) -> String {
        let id = self.e.create(&["Historical session"]);
        self.legacy_claim(&id, "legacy-address", &self.host, None);
        self.close_at(&id, "done", "2026-11-20T10:00:00Z");
        id
    }

    fn fake_hc(&self, inventory: &str, record: &Value) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(self.e.dir.path().join("hc-inventory"), inventory).unwrap();
        std::fs::write(
            self.e.dir.path().join("hc-record"),
            serde_json::to_vec(record).unwrap(),
        )
        .unwrap();
        let executable = self.bin.join("hc");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
set -eu
cd "$TRK_TEST_UI"
printf '%s\n' "$*" >> hc-calls
case "$1" in
ls)
    [ "$#" = 3 ] && [ "$2" = --all ] && [ "$3" = --json ]
    [ ! -f hc-fail-inventory ] || exit 1
    if [ -f hc-removed ]; then
        printf '{"terminals":[]}'
    else
        cat hc-inventory
    fi
    ;;
get)
    [ "$#" = 3 ] && [ "$2" = legacy-address ] && [ "$3" = --json ]
    [ ! -f hc-fail-get ] || exit 1
    cat hc-record
    ;;
rm)
    [ "$#" = 3 ] && [ "$2" = legacy-address ] && [ "$3" = --json ]
    [ ! -f hc-fail-rm ] || exit 1
    : > hc-removed
    printf '{}'
    ;;
*) exit 1 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn hc_record(&self) -> Value {
        json!({
            "address": "legacy-address",
            "state": "finished",
            "cwd": self.e.repo(),
            "argv": ["env", "TRK_ACTOR=human:alice@example.com", "trk"]
        })
    }

    fn hc_calls(&self) -> String {
        std::fs::read_to_string(self.e.dir.path().join("hc-calls")).unwrap_or_default()
    }

    fn legacy_receipts(&self, id: &str) -> Vec<Value> {
        self.e.ok(&["show", id, "--with", "events"])["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["payload"].get("gc_session").cloned())
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

