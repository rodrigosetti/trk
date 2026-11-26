use super::*;

fn spawn(e: &Env) -> CheckRunner {
    let mut command = StdCommand::new(assert_cmd::cargo::cargo_bin("trk"));
    command
        .args(["--json", "check", "--all", "--watch", "1h"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    CheckRunner::spawn_command(e, command)
}

pub(super) fn snapshot(e: &Env, pid: u32, busy: bool) -> Value {
    let mut watcher = Value::Null;
    check_wait_until("the expected watcher dashboard state", || {
        watcher = e.ok(&["dashboard"])["check_runner"].clone();
        watcher["pid"] == pid && watcher["busy"] == busy
    });
    assert_eq!(watcher["host"], local_host());
    for field in ["started_at", "heartbeat_at"] {
        watcher[field]
            .as_str()
            .unwrap()
            .parse::<jiff::Timestamp>()
            .unwrap();
    }
    watcher
}

fn rows(e: &Env) -> Vec<(i64, String, String, String)> {
    let store =
        trk_core::Store::open(&e.trk_dir().join("trk.db"), trk_core::StoreOpts::default()).unwrap();
    let mut statement = store
        .conn()
        .prepare(
            "SELECT pid, host, started_at, heartbeat_at FROM check_watch ORDER BY started_at, pid",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn idle_watcher_is_visible_refreshes_heartbeat_and_cleans_up_on_signal() {
    let e = Env::new();
    let mut runner = spawn(&e);
    let first = snapshot(&e, runner.child.id(), false);
    // Env fixes TRK_NOW; watch liveness must instead use physical wall time.
    let started = first["started_at"]
        .as_str()
        .unwrap()
        .parse::<jiff::Timestamp>()
        .unwrap();
    assert!((jiff::Timestamp::now().as_second() - started.as_second()).abs() < 30);
    let mut refreshed = Value::Null;
    check_wait_until("an idle heartbeat refresh within two intervals", || {
        refreshed = snapshot(&e, runner.child.id(), false);
        refreshed["heartbeat_at"] != first["heartbeat_at"]
    });
    assert_eq!(refreshed["started_at"], first["started_at"]);
    assert!(
        refreshed["heartbeat_at"]
            .as_str()
            .unwrap()
            .parse::<jiff::Timestamp>()
            .unwrap()
            > first["heartbeat_at"]
                .as_str()
                .unwrap()
                .parse::<jiff::Timestamp>()
                .unwrap()
    );
    runner.signal("-TERM");
    assert!(!runner.wait().success());
    assert!(
        rows(&e).is_empty(),
        "clean shutdown must delete the registration"
    );
    assert_eq!(e.ok(&["dashboard"])["check_runner"], Value::Null);
}

#[test]
fn killed_watcher_is_filtered_despite_fresh_heartbeat() {
    let e = Env::new();
    let mut runner = spawn(&e);
    snapshot(&e, runner.child.id(), false);
    assert_eq!(rows(&e).len(), 1);
    runner.signal("-KILL");
    assert!(!runner.wait().success());
    // No expiry sleep: the heartbeat is still fresh, but the local pid is dead.
    assert_eq!(e.ok(&["dashboard"])["check_runner"], Value::Null);
}

#[test]
fn two_watchers_coexist_and_releasing_one_preserves_the_other() {
    let e = Env::new();
    let mut first = spawn(&e);
    snapshot(&e, first.child.id(), false);
    let mut second = spawn(&e);
    check_wait_until("both independent watcher registrations", || {
        rows(&e).len() == 2
    });
    let registered = rows(&e);
    for runner in [&first, &second] {
        assert!(
            registered
                .iter()
                .any(|row| row.0 == i64::from(runner.child.id()))
        );
    }
    first.signal("-INT");
    assert!(!first.wait().success());
    snapshot(&e, second.child.id(), false);
    assert_eq!(rows(&e).len(), 1);
    second.signal("-TERM");
    assert!(!second.wait().success());
    assert!(rows(&e).is_empty());
}

#[test]
fn one_shot_lock_holder_is_not_a_watcher() {
    let e = Env::new();
    e.waiting(
        "one shot",
        "echo $$ > \"$TRK_REPO/check-shell\"; \
        while [ ! -f \"$TRK_REPO/release\" ]; do sleep 0.02; done; exit 75",
    );
    let mut runner = CheckRunner::spawn(&e);
    let pgid = runner.ready();
    assert_check_busy(&e, runner.child.id(), pgid);
    assert_eq!(e.ok(&["dashboard"])["check_runner"], Value::Null);
    assert!(rows(&e).is_empty());
    std::fs::write(e.repo().join("release"), "").unwrap();
    assert!(runner.wait().success());
}

#[test]
fn watcher_reports_busy_and_refreshes_heartbeat_during_a_long_check() {
    let e = Env::new();
    e.waiting(
        "long watch check",
        "echo $$ > \"$TRK_REPO/check-shell\"; \
        while [ ! -f \"$TRK_REPO/release\" ]; do sleep 0.02; done; exit 75",
    );
    let mut runner = spawn(&e);
    runner.ready();
    let first = snapshot(&e, runner.child.id(), true);
    check_wait_until("a heartbeat while the check owns the queue lock", || {
        snapshot(&e, runner.child.id(), true)["heartbeat_at"] != first["heartbeat_at"]
    });
    std::fs::write(e.repo().join("release"), "").unwrap();
    let idle = snapshot(&e, runner.child.id(), false);
    assert_eq!(idle["started_at"], first["started_at"]);
    runner.signal("-INT");
    assert!(!runner.wait().success());
}
