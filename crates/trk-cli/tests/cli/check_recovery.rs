use super::*;

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
