//! Manual continuation and lifecycle ownership races, isolated from live sessions.

use trk_core::ops::{CheckResult, CheckRun, ClaimOpts, NewIssue, WaitOpts};
use trk_core::{Code, State, Store, StoreOpts};

const HUMAN: &str = "human:alice@example.com";
const AGENT: &str = "agent:omp:original";
const OTHER: &str = "agent:omp:replacement";

fn waiting() -> (tempfile::TempDir, Store, String, i64, i64) {
    waiting_on(Some("exit 0"))
}

fn waiting_on(check: Option<&str>) -> (tempfile::TempDir, Store, String, i64, i64) {
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
                    check: check.map(str::to_owned),
                    reason: Some("Choose a continuation".into()),
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

#[test]
fn chat_reply_restores_original_claim_and_records_agent_report_verbatim() {
    let (_dir, mut store, id, original, revision) = waiting();
    // A prior automatic wake must survive a subsequent no-check chat resume.
    store
        .write(HUMAN, false, |c| {
            c.check_record(
                &id,
                "exit 0",
                revision,
                &CheckRun {
                    exit: Some(1),
                    ..Default::default()
                },
                Some(original),
            )
        })
        .unwrap();
    let previous = store.read(None, |c| c.active_claim(&id)).unwrap().unwrap();
    store
        .write(AGENT, false, |c| {
            c.wait(
                &id,
                WaitOpts {
                    reason: Some("Choose a continuation".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    store.opts.now = Some("2026-11-20T10:05:00Z".parse().unwrap());
    let message = "  Approved, use option B.\nKeep the audit trail.  ";
    let restored = store
        .write(AGENT, false, |c| c.resume_chat(&id, message))
        .unwrap();
    assert_ne!(restored.id, previous.id);
    assert_eq!(restored.actor, previous.actor);
    assert_eq!(restored.worktree, previous.worktree);
    assert_eq!(restored.host, previous.host);
    assert_eq!(restored.agent, previous.agent);
    assert_eq!(restored.terminal, previous.terminal);
    assert_eq!(restored.launcher, previous.launcher);
    assert!(restored.lease_until > previous.lease_until);
    let shown = store
        .read(None, |c| c.show(&id, trk_core::view::With::parse("all")))
        .unwrap();
    assert_eq!(shown["state"], "claimed");
    assert_eq!(shown["wake_count"], 1);
    assert!(shown["waiting"].is_null());
    let notes = shown["notes"].as_array().unwrap();
    let report = notes.last().unwrap();
    assert_eq!(report["actor"], AGENT);
    assert_eq!(
        report["body"],
        format!("Session reported a human reply in chat:\n\n{message}")
    );
    let events = store.read(None, |c| c.events(&id)).unwrap();
    let event = events.iter().find(|e| e.op == "resume_chat").unwrap();
    assert_eq!(event.actor, AGENT);
    assert_eq!(event.payload["message"], message);
    assert_eq!(event.payload["claim"], restored.id);
    assert_eq!(event.payload["resumed_from"], previous.id);
    // Ownership is restored, not merely a successful response.
    store
        .write(AGENT, false, |c| c.close(&id, "done", None, false))
        .unwrap();
}

#[test]
fn chat_resume_refuses_checked_disabled_foreign_and_empty_replies_without_mutation() {
    for scenario in ["check", "disabled", "foreign", "human", "empty"] {
        let (_dir, mut store, id, _, _) = waiting_on((scenario == "check").then_some("exit 0"));
        if scenario == "disabled" {
            store.opts.chat_resume = false;
        }
        let actor = match scenario {
            "foreign" => OTHER,
            "human" => HUMAN,
            _ => AGENT,
        };
        let message = if scenario == "empty" {
            " \n\t "
        } else {
            "approved"
        };
        let before = store.read(None, |c| c.issue(&id)).unwrap();
        let events = store.read(None, |c| c.events(&id)).unwrap();
        let error = store
            .write(actor, false, |c| c.resume_chat(&id, message))
            .unwrap_err();
        assert_eq!(
            error.code(),
            if scenario == "empty" {
                Code::BadArgs
            } else {
                Code::NotPermitted
            },
            "{scenario}: {error}"
        );
        assert_eq!(store.read(None, |c| c.issue(&id)).unwrap(), before);
        assert_eq!(store.read(None, |c| c.events(&id)).unwrap(), events);
        assert!(store.read(None, |c| c.active_claim(&id)).unwrap().is_none());
    }
}

#[test]
fn chat_resume_never_resurrects_nonwaiting_or_replaced_sessions() {
    for state in ["claimed", "open", "closed", "replacement_waiting"] {
        let (_dir, mut store, id, _, _) = waiting_on(None);
        store.write(HUMAN, false, |c| c.reopen(&id, None)).unwrap();
        if state != "open" {
            store
                .write(OTHER, false, |c| c.claim(&id, ClaimOpts::default()))
                .unwrap();
            match state {
                "closed" => store
                    .write(OTHER, false, |c| c.close(&id, "done", None, false))
                    .unwrap(),
                "replacement_waiting" => store
                    .write(OTHER, false, |c| {
                        c.wait(
                            &id,
                            WaitOpts {
                                reason: Some("Replacement decision".into()),
                                ..Default::default()
                            },
                        )
                    })
                    .unwrap(),
                _ => (),
            }
        }
        let before = store.read(None, |c| c.issue(&id)).unwrap();
        let active = store.read(None, |c| c.active_claim(&id)).unwrap();
        assert_eq!(
            store
                .write(AGENT, false, |c| c.resume_chat(&id, "approved"))
                .unwrap_err()
                .code(),
            Code::NotPermitted,
            "{state}"
        );
        assert_eq!(store.read(None, |c| c.issue(&id)).unwrap(), before);
        assert_eq!(store.read(None, |c| c.active_claim(&id)).unwrap(), active);
    }
}
