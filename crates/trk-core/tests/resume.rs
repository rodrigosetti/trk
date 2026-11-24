//! Human authorization and lifecycle ownership races, isolated from live sessions.

use trk_core::ops::{CheckResult, CheckRun, ClaimOpts, NewIssue, WaitOpts};
use trk_core::{Code, State, Store, StoreOpts};

const HUMAN: &str = "human:alice@example.com";
const AGENT: &str = "agent:omp:original";
const OTHER: &str = "agent:omp:replacement";

fn waiting() -> (tempfile::TempDir, Store, String, i64, i64) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::install(
        &dir.path().join("trk.db"),
        StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    let id = store
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                title: "Resume race".into(),
                ..Default::default()
            })
        })
        .unwrap();
    let claim = store
        .write(AGENT, false, |c| {
            c.claim(
                &id,
                ClaimOpts {
                    terminal: Some("original-terminal".into()),
                    launcher: Some("original-profile".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    store
        .write(AGENT, false, |c| {
            c.wait(
                &id,
                WaitOpts {
                    check: Some("exit 0".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let revision = store.read(None, |c| c.issue_revision(&id)).unwrap();
    (dir, store, id, claim.id, revision)
}

#[test]
fn resume_denies_agents_and_rejects_changed_waits_even_with_identical_timestamps() {
    let (_dir, mut store, id, claim, revision) = waiting();
    assert_eq!(
        store
            .write(AGENT, false, |c| c.resume(&id, claim, revision, "continue"))
            .unwrap_err()
            .code(),
        Code::NotPermitted
    );
    assert_eq!(
        store
            .write(HUMAN, false, |c| c.resume(&id, claim, revision, " \n"))
            .unwrap_err()
            .code(),
        Code::BadArgs
    );
    // The same waiting command and second-precision clock must not hide a change.
    store
        .write(HUMAN, false, |c| {
            c.wait(
                &id,
                WaitOpts {
                    check: Some("exit 0".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(
        store
            .write(HUMAN, false, |c| c.resume(&id, claim, revision, "continue"))
            .unwrap_err()
            .code(),
        Code::InvalidState
    );
    assert!(store.read(None, |c| c.active_claim(&id)).unwrap().is_none());
    assert_eq!(
        store.read(None, |c| c.issue(&id)).unwrap().state,
        State::Waiting
    );
}

#[test]
fn stale_check_cannot_close_retry_or_wake_a_resumed_and_waiting_session() {
    for exit in [0, 75, 1] {
        let (_dir, mut store, id, claim, revision) = waiting();
        let restored = store
            .write(HUMAN, false, |c| c.resume(&id, claim, revision, "continue"))
            .unwrap();
        assert_eq!(restored.launcher.as_deref(), Some("original-profile"));
        store
            .write(AGENT, false, |c| {
                c.wait(
                    &id,
                    WaitOpts {
                        check: Some("exit 0".into()),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let before = store.read(None, |c| c.issue(&id)).unwrap();
        assert_eq!(
            store
                .write(HUMAN, false, |c| c.check_record(
                    &id,
                    "exit 0",
                    revision,
                    &CheckRun {
                        exit: Some(exit),
                        ..Default::default()
                    },
                    Some(restored.id)
                ))
                .unwrap(),
            CheckResult::Skipped
        );
        assert_eq!(store.read(None, |c| c.issue(&id)).unwrap(), before);
        assert_eq!(store.read(None, |c| c.claims(&id)).unwrap().len(), 2);
    }
}

#[test]
fn stale_authorization_and_failed_delivery_never_release_replacement_work() {
    let (_dir, mut store, id, claim, revision) = waiting();
    let restored = store
        .write(HUMAN, false, |c| c.resume(&id, claim, revision, "continue"))
        .unwrap();
    let replacement = store
        .write(OTHER, false, |c| {
            c.claim(
                &id,
                ClaimOpts {
                    steal: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(
        store
            .write(HUMAN, false, |c| c.resume(&id, claim, revision, "again"))
            .unwrap_err()
            .code(),
        Code::InvalidState
    );
    assert!(
        !store
            .write(HUMAN, false, |c| c.resume_failed(
                &id,
                restored.id,
                "terminal vanished"
            ))
            .unwrap()
    );
    assert_eq!(
        store.read(None, |c| c.active_claim(&id)).unwrap().unwrap(),
        replacement
    );
    store
        .write(OTHER, false, |c| {
            c.wait(
                &id,
                WaitOpts {
                    reason: Some("replacement review".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let current_revision = store.read(None, |c| c.issue_revision(&id)).unwrap();
    // A fresh revision cannot authorize an old historical session either.
    assert_eq!(
        store
            .write(HUMAN, false, |c| c.resume(
                &id,
                claim,
                current_revision,
                "old session"
            ))
            .unwrap_err()
            .code(),
        Code::InvalidState
    );
    assert_eq!(
        store.read(None, |c| c.issue(&id)).unwrap().state,
        State::Waiting
    );
    let events = store.read(None, |c| c.events(&id)).unwrap();
    assert!(events.iter().any(|event| event.op == "note"
        && event.payload["resume"]["result"] == "delivery_failed"
        && event.payload["resume"]["reopened"] == false));
}
