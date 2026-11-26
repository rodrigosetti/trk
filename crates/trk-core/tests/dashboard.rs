use serde_json::{Value, json};
use trk_core::ops::{Edit, NewIssue};
use trk_core::view::With;
use trk_core::{Store, StoreOpts};

const HUMAN: &str = "human:alice@example.com";

fn snapshot(store: &Store) -> Value {
    serde_json::to_value(
        store
            .read(None, |c| c.dashboard(0, &|_, _, _| None, &|_| None))
            .unwrap(),
    )
    .unwrap()
}

fn row<'a>(dashboard: &'a Value, id: &str) -> &'a Value {
    dashboard["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["id"] == id)
        .unwrap()
}

#[test]
fn dashboard_and_dependency_epics_preserve_default_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::install(
        &dir.path().join("trk.db"),
        StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    let mut ids = Vec::new();
    for (title, priority, time) in [
        ("Older", "P2", "2026-11-20T10:00:00Z"),
        ("Newer", "P2", "2026-11-20T11:00:00Z"),
        ("Urgent", "P1", "2026-11-20T12:00:00Z"),
        ("Blocked", "P0", "2026-11-20T13:00:00Z"),
    ] {
        store.opts.now = Some(time.parse().unwrap());
        ids.push(
            store
                .write(HUMAN, false, |c| {
                    c.create(NewIssue {
                        title: title.into(),
                        priority: Some(priority.into()),
                        ..Default::default()
                    })
                })
                .unwrap(),
        );
    }
    let (older, newer, urgent, blocked) = (&ids[0], &ids[1], &ids[2], &ids[3]);
    for source in [older, newer] {
        store
            .write(HUMAN, false, |c| c.dep_add(source, blocked))
            .unwrap();
    }
    let before = snapshot(&store);
    let ordered_ids = |dashboard: &Value| {
        dashboard["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ordered_ids(&before),
        [urgent, older, newer, blocked].map(String::clone)
    );
    for name in ["alpha", "zulu"] {
        store
            .write(HUMAN, false, |c| c.create_epic(name, ""))
            .unwrap();
    }
    for (id, epic) in [(older, "zulu"), (blocked, "alpha")] {
        store
            .write(HUMAN, false, |c| {
                c.edit(
                    id,
                    Edit {
                        epic: Some(Some(epic.into())),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
    }
    let after = snapshot(&store);
    assert_eq!(ordered_ids(&after), ordered_ids(&before));
    for (id, epic) in [
        (older, json!("zulu")),
        (blocked, json!("alpha")),
        (newer, Value::Null),
        (urgent, Value::Null),
    ] {
        assert_eq!(row(&after, id).get("epic"), Some(&epic));
    }
    let blockers = row(&after, blocked)["blocked_by"].as_array().unwrap();
    for (id, epic) in [(older, json!("zulu")), (newer, Value::Null)] {
        let edge = blockers.iter().find(|edge| edge["id"] == *id).unwrap();
        assert_eq!(edge.get("epic"), Some(&epic));
    }
    for id in [older, newer] {
        assert_eq!(row(&after, id)["blocks"][0]["epic"], "alpha");
    }

    // The same endpoints must serialize consistently in the full issue view.
    for id in [older, newer, blocked] {
        let shown = store
            .read(None, |c| c.show(id, With::parse("deps")))
            .unwrap();
        for direction in ["blocked_by", "blocks"] {
            assert_eq!(shown["deps"][direction], row(&after, id)[direction]);
        }
    }

    // Removing the destination epic must expose an explicit null on outgoing edges.
    store
        .write(HUMAN, false, |c| {
            c.edit(
                blocked,
                Edit {
                    epic: Some(None),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let cleared = snapshot(&store);
    for id in [older, newer] {
        assert_eq!(
            row(&cleared, id)["blocks"][0].get("epic"),
            Some(&Value::Null)
        );
        let shown = store
            .read(None, |c| c.show(id, With::parse("deps")))
            .unwrap();
        assert_eq!(shown["deps"]["blocks"][0].get("epic"), Some(&Value::Null));
    }
}

#[test]
fn check_run_json_survives_reopen_and_bounds_retained_output() {
    use trk_core::ops::{CHECK_OUTPUT_TAIL, CheckResult, CheckRun};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trk.db");
    let opts = StoreOpts {
        now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
        ..Default::default()
    };
    let mut store = Store::install(&path, opts.clone()).unwrap();
    let id = store
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                title: "check history".into(),
                ..Default::default()
            })
        })
        .unwrap();
    for (result, expected) in [
        (CheckResult::Passed, "passed"),
        (CheckResult::Retry, "retry"),
        (CheckResult::Woken { claim: 1 }, "woken"),
        (CheckResult::Failed, "failed"),
        (CheckResult::Skipped, "skipped"),
        (CheckResult::PassWoken { claim: 2 }, "woken"),
        (CheckResult::PassReopened, "passed"),
    ] {
        store
            .write(HUMAN, false, |c| {
                c.check_run_record(
                    &id,
                    result,
                    &CheckRun {
                        exit: Some(0),
                        timed_out: false,
                        output: "stdout\nstderr\n".into(),
                    },
                    123,
                )
            })
            .unwrap();
        let recent = &snapshot(&store)["check_runs"][0];
        assert_eq!(recent["issue_id"], id);
        assert_eq!(recent["result"], expected);
        assert_eq!(recent["at"], "2026-11-20T10:00:00Z");
        assert_eq!(recent["duration_ms"], 123);
        assert_eq!(recent["exit"], 0);
        assert_eq!(recent["timed_out"], false);
        assert_eq!(recent["output"], "stdout\nstderr\n");
        assert_eq!(recent["output_truncated"], false);
    }
    let output = format!("{}tail", "λ".repeat(CHECK_OUTPUT_TAIL));
    for n in 0..105 {
        store
            .write(HUMAN, false, |c| {
                c.check_run_record(
                    &id,
                    CheckResult::Retry,
                    &CheckRun {
                        exit: Some(75),
                        timed_out: false,
                        output: output.clone(),
                    },
                    n,
                )
            })
            .unwrap();
    }
    drop(store);
    let store = Store::open(&path, opts).unwrap();
    let dashboard = snapshot(&store);
    let runs = dashboard["check_runs"].as_array().unwrap();
    assert_eq!(runs.len(), 100);
    assert_eq!(runs[0]["duration_ms"], 104);
    assert_eq!(runs[99]["duration_ms"], 5);
    assert!(
        runs.windows(2)
            .all(|w| w[0]["seq"].as_i64() > w[1]["seq"].as_i64())
    );
    let text = runs[0]["output"].as_str().unwrap();
    assert!(text.len() <= CHECK_OUTPUT_TAIL);
    assert!(text.ends_with("λtail"));
    assert_eq!(runs[0]["output_truncated"], true);
    assert_eq!(
        store
            .conn()
            .query_row("SELECT count(*) FROM check_run", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        100,
    );
}

#[test]
fn check_watch_is_visible_while_idle_and_uses_physical_time() {
    let dir = tempfile::tempdir().unwrap();
    let fixed: jiff::Timestamp = "2000-01-01T00:00:00Z".parse().unwrap();
    let mut store = Store::install(
        &dir.path().join("trk.db"),
        StoreOpts {
            now: Some(fixed),
            ..Default::default()
        },
    )
    .unwrap();
    let before = jiff::Timestamp::now();
    let pid = i64::from(std::process::id());
    store
        .write(HUMAN, false, |c| c.check_watch_register(pid, None))
        .unwrap();
    let after = jiff::Timestamp::now();
    let dashboard = snapshot(&store);
    let runner = &dashboard["check_runner"];
    assert_eq!(runner["pid"], pid);
    assert_eq!(runner["host"], Value::Null);
    assert_eq!(runner["busy"], false);
    assert_eq!(runner["started_at"], runner["heartbeat_at"]);
    let started: jiff::Timestamp = runner["started_at"].as_str().unwrap().parse().unwrap();
    assert!(before <= started && started <= after);
    assert_eq!(
        dashboard["generated_at"]
            .as_str()
            .unwrap()
            .parse::<jiff::Timestamp>()
            .unwrap(),
        fixed
    );

    // A one-shot queue owner makes the watcher busy, without becoming a watcher.
    store
        .write(HUMAN, false, |c| c.check_lock_take(pid + 1, None))
        .unwrap();
    assert_eq!(snapshot(&store)["check_runner"]["busy"], true);
    store
        .write(HUMAN, false, |c| c.check_watch_release(pid, None))
        .unwrap();
    assert!(snapshot(&store)["check_runner"].is_null());
    assert!(store.read(None, |c| c.check_lock()).unwrap().is_some());
}

#[test]
fn expired_watchers_are_filtered_without_mutating_reads() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::install(&dir.path().join("trk.db"), StoreOpts::default()).unwrap();
    store
        .write(HUMAN, false, |c| c.check_watch_register(41, None))
        .unwrap();
    let expired = (jiff::Timestamp::now() - jiff::SignedDuration::from_secs(31)).to_string();
    store
        .conn()
        .execute("UPDATE check_watch SET heartbeat_at = ?1", [&expired])
        .unwrap();
    assert!(snapshot(&store)["check_runner"].is_null());
    assert_eq!(
        store
            .conn()
            .query_row("SELECT count(*) FROM check_watch", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    // A second startup must not erase an expired watcher's ability to resume.
    store
        .write(HUMAN, false, |c| {
            c.check_watch_register(42, None)?;
            c.check_watch_release(42, None)
        })
        .unwrap();
    assert!(snapshot(&store)["check_runner"].is_null());

    // Heartbeats must not use a far-future issue/test clock either.
    store.opts.now = Some("2099-01-01T00:00:00Z".parse().unwrap());
    let before = jiff::Timestamp::now();
    store
        .write(HUMAN, false, |c| c.check_watch_heartbeat(41, None))
        .unwrap();
    let dashboard = snapshot(&store);
    let heartbeat = dashboard["check_runner"]["heartbeat_at"]
        .as_str()
        .unwrap()
        .parse::<jiff::Timestamp>()
        .unwrap();
    assert!(before <= heartbeat && heartbeat <= jiff::Timestamp::now());
}

#[test]
fn fresh_remote_and_unknown_host_watchers_do_not_probe_local_pids() {
    for host in [None, Some("watch.remote.invalid")] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::install(&dir.path().join("trk.db"), StoreOpts::default()).unwrap();
        // A reaped local PID cannot rule out a process on a different/unknown host.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = i64::from(child.id());
        child.wait().unwrap();
        store
            .write(HUMAN, false, |c| c.check_watch_register(pid, host))
            .unwrap();
        assert_eq!(snapshot(&store)["check_runner"]["pid"], pid);
    }
}

#[test]
fn fresh_known_local_watchers_reject_dead_and_reused_pids() {
    let Some(host) = trk_core::system::hostname() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::install(&dir.path().join("trk.db"), StoreOpts::default()).unwrap();
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let pid = i64::from(child.id());
    child.wait().unwrap();
    store
        .write(HUMAN, false, |c| c.check_watch_register(pid, Some(host)))
        .unwrap();
    assert!(snapshot(&store)["check_runner"].is_null());
    store
        .conn()
        .execute(
            "UPDATE check_watch SET pid = ?1, started_at = '1970-01-01T00:00:00Z'",
            [i64::from(std::process::id())],
        )
        .unwrap();
    assert!(snapshot(&store)["check_runner"].is_null());
}

#[test]
fn multiple_watchers_prefer_queue_owner_then_oldest_with_stable_ties() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::install(&dir.path().join("trk.db"), StoreOpts::default()).unwrap();
    let now = jiff::Timestamp::now().to_string();
    for (pid, host, started) in [
        (42, "", "2020-01-01T00:00:00Z"),
        (41, "", "2020-01-01T00:00:00Z"),
        (43, "watch.remote.invalid", "2021-01-01T00:00:00Z"),
    ] {
        store.conn().execute(
            "INSERT INTO check_watch (pid, host, started_at, heartbeat_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![pid, host, started, now],
        ).unwrap();
    }
    assert_eq!(snapshot(&store)["check_runner"]["pid"], 41);
    store
        .write(HUMAN, false, |c| {
            c.check_lock_take(43, Some("watch.remote.invalid"))
        })
        .unwrap();
    let dashboard = snapshot(&store);
    assert_eq!(dashboard["check_runner"]["pid"], 43);
    // Holding the queue is busy even before a subprocess group exists.
    assert_eq!(dashboard["check_runner"]["busy"], true);
    store
        .write(HUMAN, false, |c| c.check_lock_take(43, None))
        .unwrap();
    // A PID match on a different host does not identify the owning watcher.
    assert_eq!(snapshot(&store)["check_runner"]["pid"], 41);
    store
        .write(HUMAN, false, |c| c.check_lock_release(43))
        .unwrap();
    assert_eq!(snapshot(&store)["check_runner"]["busy"], false);
}
