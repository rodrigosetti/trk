//! Store-level tests against the seed configuration.

use serde_json::json;
use trk_core::config::{ConfigDir, Level, check_schema};
use trk_core::fields::{Assign, AssignOp};
use trk_core::ops::{ClaimOpts, NewDep, NewIssue};
use trk_core::view::With;
use trk_core::{Code, Store, StoreOpts};

const HUMAN: &str = "human:alice@example.com";
const AGENT: &str = "agent:omp:s1";

fn seed() -> ConfigDir {
    ConfigDir::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../seed"))
}

fn ts(s: &str) -> jiff::Timestamp {
    s.parse().unwrap()
}

fn store_at(dir: &tempfile::TempDir, now: &str) -> Store {
    let opts = StoreOpts {
        now: Some(ts(now)),
        ..Default::default()
    };
    Store::open(&dir.path().join("trk.db"), opts).unwrap()
}

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store_at(&dir, "2026-11-20T10:00:00Z");
    let schema = seed().compile().unwrap();
    s.write(HUMAN, false, |c| {
        c.apply(&schema, &["omp".to_string()], &[], None)?;
        c.project_add("orch", "Orchestra")?;
        c.repo_add("orch", Some("git@github.com:acme/orch.git"), None)?;
        Ok(())
    })
    .unwrap();
    (dir, s)
}

fn set(f: &str, v: &str) -> Assign {
    Assign::set(f, v)
}

fn create(s: &mut Store, ty: &str, extra: &[Assign]) -> String {
    let mut assigns = vec![set("title", "T"), set("context", "C")];
    match ty {
        "code-change" => assigns.push(set("repo", "github.com/acme/orch")),
        "research" | "decision" => assigns.push(set("question", "Q?")),
        "approval" => assigns.push(set("approver", "me")),
        "operation" => assigns.extend([
            set("change_kind", "flag"),
            set("target", "x"),
            set("environment", "prod"),
            set("change", "flip"),
        ]),
        _ => {}
    }
    assigns.extend(extra.iter().cloned());
    s.write(HUMAN, false, |c| {
        c.create(NewIssue {
            project: "orch".into(),
            ty: ty.into(),
            assigns,
            deps: vec![],
        })
    })
    .unwrap()
}

#[test]
fn seed_config_is_valid() {
    let schema = seed().compile().unwrap();
    let errors: Vec<_> = check_schema(&schema)
        .into_iter()
        .filter(|d| d.level == Level::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(schema.types.len(), 6);
}

#[test]
fn ids_have_project_prefix_and_resolve_by_suffix() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "research", &[]);
    assert!(id.starts_with("orch-"));
    assert_eq!(id.len(), "orch-".len() + 3);
    let suffix = &id[5..];
    let got = s.read(None, |c| c.resolve_id(suffix)).unwrap();
    assert_eq!(got, id);
    let err = s.read(None, |c| c.resolve_id("nope")).unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
}

#[test]
fn create_requires_fields() {
    let (_d, mut s) = setup();
    let err = s
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                project: "orch".into(),
                ty: "research".into(),
                assigns: vec![set("title", "x")],
                deps: vec![],
            })
        })
        .unwrap_err();
    assert_eq!(err.code(), Code::RequirementsUnmet);
    assert_eq!(
        err.to_json()["missing_fields"],
        json!(["context", "question"])
    );
    // Bad enum values are rejected with the allowed list.
    let err = s
        .write(HUMAN, false, |c| {
            c.create(NewIssue {
                project: "orch".into(),
                ty: "research".into(),
                assigns: vec![
                    set("title", "x"),
                    set("context", "c"),
                    set("question", "q"),
                    set("confidence", "huge"),
                ],
                deps: vec![],
            })
        })
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidField);
}

#[test]
fn claim_invariant_and_transitions() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "code-change", &[]);
    // Moving an unclaimed issue is refused.
    let e = s
        .write(HUMAN, false, |c| c.move_to(&id, "in_review", &[], false))
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidTransition);
    s.write(HUMAN, false, |c| {
        c.claim(
            &id,
            ClaimOpts {
                branch: Some("feat".into()),
                ..Default::default()
            },
        )
    })
    .unwrap();
    let v = s.read(None, |c| c.show(&id, With::default())).unwrap();
    assert_eq!(v["status"], "in_progress");
    assert_eq!(v["fields"]["branch"], "feat", "claim fills an empty branch");
    assert_eq!(
        v["claim"]["lease_until"],
        json!(null),
        "human claims have no lease"
    );
    // Requirements on entering in_review.
    let e = s
        .write(HUMAN, false, |c| c.move_to(&id, "in_review", &[], false))
        .unwrap_err();
    assert_eq!(e.code(), Code::RequirementsUnmet);
    assert_eq!(e.to_json()["missing_fields"], json!(["pr_url"]));
    s.write(HUMAN, false, |c| {
        c.move_to(&id, "in_review", &[set("pr_url", "https://x/pr/1")], false)
    })
    .unwrap();
    // Only the claimant may close, unless forced.
    let e = s
        .write("human:bob@example.com", false, |c| {
            c.close(&id, "abandoned", &[], false)
        })
        .unwrap_err();
    assert_eq!(e.code(), Code::NotClaimant);
    s.write("human:bob@example.com", false, |c| {
        c.close(&id, "abandoned", &[], true)
    })
    .unwrap();
    let v = s
        .read(None, |c| {
            c.show(
                &id,
                With {
                    claims: true,
                    events: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(v["status"], "closed");
    assert_eq!(v["resolution"], "abandoned");
    assert_eq!(v["claim"], json!(null));
    assert_eq!(v["claims"][0]["release_reason"], "closed");
    let close = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"] == "close")
        .unwrap();
    assert_eq!(close["payload"]["forced"], true);
}

#[test]
fn unclaim_returns_to_open_and_steal() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "research", &[]);
    s.write(HUMAN, false, |c| c.claim(&id, ClaimOpts::default()))
        .unwrap();
    let e = s
        .write("human:bob@example.com", false, |c| {
            c.claim(&id, ClaimOpts::default())
        })
        .unwrap_err();
    assert_eq!(e.code(), Code::ClaimedByOther);
    s.write("human:bob@example.com", false, |c| {
        c.claim(
            &id,
            ClaimOpts {
                steal: true,
                ..Default::default()
            },
        )
    })
    .unwrap();
    let v = s
        .read(None, |c| {
            c.show(
                &id,
                With {
                    claims: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(v["claim"]["actor"], "human:bob@example.com");
    assert_eq!(v["claims"][0]["release_reason"], "stolen");
    s.write("human:bob@example.com", false, |c| {
        c.unclaim(&id, false, Some("not now"))
    })
    .unwrap();
    let v = s
        .read(None, |c| {
            c.show(
                &id,
                With {
                    notes: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(v["status"], "open");
    assert_eq!(v["derived"]["ready"], true);
    assert_eq!(v["notes"][0]["body"], "not now");
}

#[test]
fn agent_leases_renew_and_expire() {
    let (d, mut s) = setup();
    let id = create(&mut s, "research", &[]);
    s.write(AGENT, false, |c| c.claim(&id, ClaimOpts::default()))
        .unwrap();
    let lease = |s: &Store| {
        s.read(None, |c| c.show(&id, With::default())).unwrap()["claim"]["lease_until"].clone()
    };
    assert_eq!(lease(&s), "2026-11-20T10:30:00Z");
    // Any write by the claimant renews the lease.
    let mut s2 = store_at(&d, "2026-11-20T10:20:00Z");
    s2.write(AGENT, false, |c| c.note(&id, "progress", None, false))
        .unwrap();
    assert_eq!(lease(&s2), "2026-11-20T10:50:00Z");
    // Past the lease, the next write transaction sweeps it.
    let mut s3 = store_at(&d, "2026-11-20T11:00:00Z");
    let stale = s3
        .read(None, |c| c.list("stale-lease", None, None, None))
        .unwrap();
    assert_eq!(stale.len(), 1);
    s3.write(HUMAN, false, |_| Ok(())).unwrap();
    let v = s3
        .read(None, |c| {
            c.show(
                &id,
                With {
                    claims: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(v["status"], "open");
    assert_eq!(v["claims"][0]["release_reason"], "expired");
    // The expired agent can no longer write.
    let e = s3
        .write(AGENT, false, |c| c.note(&id, "late", None, false))
        .unwrap_err();
    assert_eq!(e.code(), Code::NotPermitted);
}

#[test]
fn agents_write_only_to_claimed_issues() {
    let (_d, mut s) = setup();
    let mine = create(&mut s, "research", &[]);
    let other = create(&mut s, "research", &[]);
    s.write(AGENT, false, |c| c.claim(&mine, ClaimOpts::default()))
        .unwrap();
    let e = s
        .write(AGENT, false, |c| {
            c.edit(&other, &[set("doc_url", "https://x")])
        })
        .unwrap_err();
    assert_eq!(e.code(), Code::NotPermitted);
    // Creating is allowed only as a discovered-from follow-up of a claimed issue.
    let new = |deps: Vec<NewDep>| NewIssue {
        project: "orch".into(),
        ty: "research".into(),
        assigns: vec![set("title", "f"), set("context", "c"), set("question", "q")],
        deps,
    };
    let e = s
        .write(AGENT, false, |c| c.create(new(vec![])))
        .unwrap_err();
    assert_eq!(e.code(), Code::NotPermitted);
    let f = s
        .write(AGENT, false, |c| {
            c.create(new(vec![NewDep {
                kind: "discovered-from".into(),
                other: mine.clone(),
                new_is_src: true,
            }]))
        })
        .unwrap();
    let v = s
        .read(None, |c| {
            c.show(
                &f,
                With {
                    deps: true,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(v["deps"]["out"][0]["kind"], "discovered-from");
    assert_eq!(v["deps"]["out"][0]["id"], json!(mine));
    // The unknown-harness check.
    let e = s.write("agent:nope:1", false, |_| Ok(())).unwrap_err();
    assert_eq!(e.code(), Code::NotFound);
}

#[test]
fn gates_and_approvals() {
    let (_d, mut s) = setup();
    let op = create(&mut s, "operation", &[set("rollback_plan", "flip back")]);
    let ap = create(&mut s, "approval", &[]);
    s.write(HUMAN, false, |c| c.dep_add(&ap, "approves", &op))
        .unwrap();
    s.write(HUMAN, false, |c| c.claim(&op, ClaimOpts::default()))
        .unwrap();
    let v = s.read(None, |c| c.show(&op, With::default())).unwrap();
    assert_eq!(v["derived"]["gates_unmet"][0]["src"], json!(ap));
    let gated = s.read(None, |c| c.list("gated", None, None, None)).unwrap();
    assert_eq!(gated.len(), 1);
    let exec = [set("executed_by", "me"), set("executed_at", "now")];
    let e = s
        .write(HUMAN, false, |c| c.move_to(&op, "executing", &exec, false))
        .unwrap_err();
    assert_eq!(e.code(), Code::GateUnmet);
    assert_eq!(e.code().exit_code(), 5);
    // A rejected approval still blocks; an approved one opens the gate.
    s.write(HUMAN, false, |c| {
        c.claim(&ap, ClaimOpts::default())?;
        c.close(
            &ap,
            "approved",
            &[set("record_url", "https://slack/x")],
            false,
        )
    })
    .unwrap();
    s.write(HUMAN, false, |c| c.move_to(&op, "executing", &exec, false))
        .unwrap();
    let v = s.read(None, |c| c.show(&op, With::default())).unwrap();
    assert_eq!(v["status"], "executing");
    assert_eq!(v["fields"]["executed_at"], "2026-11-20T10:00:00Z");
    assert_eq!(v["fields"]["executed_by"], HUMAN);
}

#[test]
fn blocks_milestones_and_cycles() {
    let (_d, mut s) = setup();
    let a = create(&mut s, "research", &[]);
    let b = create(&mut s, "research", &[]);
    let m = create(&mut s, "milestone", &[]);
    s.write(HUMAN, false, |c| {
        c.dep_add(&a, "blocks", &b)?;
        c.dep_add(&a, "blocks", &m)?;
        c.dep_add(&b, "blocks", &m)
    })
    .unwrap();
    let e = s
        .write(HUMAN, false, |c| c.dep_add(&b, "blocks", &a))
        .unwrap_err();
    assert_eq!(e.code(), Code::Cycle);
    let e = s
        .write(HUMAN, false, |c| c.dep_add(&m, "blocks", &a))
        .unwrap_err();
    assert_eq!(
        e.code(),
        Code::InvalidField,
        "a milestone never blocks a non-milestone"
    );
    let e = s
        .write(HUMAN, false, |c| c.claim(&m, ClaimOpts::default()))
        .unwrap_err();
    assert_eq!(
        e.code(),
        Code::InvalidTransition,
        "milestones cannot be claimed"
    );
    let ready: Vec<String> = s
        .read(None, |c| c.list("ready", None, None, None))
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ready.contains(&a) && !ready.contains(&b));
    let blocked = s
        .read(None, |c| {
            c.list(&format!("blocks={m} blocked"), None, None, None)
        })
        .unwrap();
    assert_eq!(blocked.len(), 1);
    let e = s
        .write(HUMAN, false, |c| c.close(&m, "reached", &[], false))
        .unwrap_err();
    assert_eq!(e.code(), Code::GateUnmet);
    for id in [&a, &b] {
        s.write(HUMAN, false, |c| {
            c.close(
                id,
                "abandoned",
                &[set("confidence", "low"), set("outcome", "x")],
                false,
            )
        })
        .unwrap();
    }
    s.write(HUMAN, false, |c| c.close(&m, "reached", &[], false))
        .unwrap();
    let v = s.read(None, |c| c.show(&m, With::default())).unwrap();
    assert_eq!(v["derived"]["progress"], json!({"total": 2, "closed": 2}));
}

#[test]
fn supersede_requires_edge_and_derived_next() {
    let (_d, mut s) = setup();
    let old = create(&mut s, "decision", &[]);
    let new = create(&mut s, "decision", &[]);
    let v = s.read(None, |c| c.show(&old, With::default())).unwrap();
    assert_eq!(v["derived"]["next"]["proposed"], json!(["options"]));
    assert_eq!(
        v["derived"]["next"]["closed"]["superseded"],
        json!(["record_url", "outcome", "edge:supersedes"])
    );
    let fields = [set("record_url", "https://x"), set("outcome", "replaced")];
    let e = s
        .write(HUMAN, false, |c| {
            c.close(&old, "superseded", &fields, false)
        })
        .unwrap_err();
    assert_eq!(e.to_json()["missing_edges"], json!(["supersedes"]));
    s.write(HUMAN, false, |c| {
        c.dep_add(&old, "supersedes", &new)?;
        c.close(&old, "superseded", &fields, false)
    })
    .unwrap();
    // min_items: two options to enter proposed.
    let e = s
        .write(HUMAN, false, |c| {
            c.claim(
                &new,
                ClaimOpts {
                    assigns: vec![set("options", "only one")],
                    ..Default::default()
                },
            )
        })
        .unwrap_err();
    assert_eq!(e.to_json()["min_items"][0]["have"], 1);
    s.write(HUMAN, false, |c| {
        c.claim(
            &new,
            ClaimOpts {
                assigns: vec![set("options", "SQLite, Dolt")],
                ..Default::default()
            },
        )
    })
    .unwrap();
}

#[test]
fn list_fields_and_edit_clear_rules() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "research", &[set("tags", "a,b")]);
    let add = Assign {
        field: "tags".into(),
        op: AssignOp::Append,
        value: json!("c"),
    };
    let rm = Assign {
        field: "tags".into(),
        op: AssignOp::Remove,
        value: json!("a"),
    };
    let link = Assign {
        field: "links".into(),
        op: AssignOp::Append,
        value: json!("pr=https://x/1 the PR"),
    };
    s.write(HUMAN, false, |c| c.edit(&id, &[add, rm, link]))
        .unwrap();
    let v = s.read(None, |c| c.show(&id, With::default())).unwrap();
    assert_eq!(v["tags"], json!(["b", "c"]));
    assert_eq!(
        v["links"][0],
        json!({"kind": "pr", "url": "https://x/1", "note": "the PR"})
    );
    // Clearing a field required in the current status is refused.
    s.write(HUMAN, false, |c| c.claim(&id, ClaimOpts::default()))
        .unwrap();
    let e = s
        .write(HUMAN, false, |c| c.edit(&id, &[set("title", "")]))
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidField);
    let e = s
        .write(HUMAN, false, |c| c.edit(&id, &[set("status", "closed")]))
        .unwrap_err();
    assert_eq!(e.code(), Code::InvalidField);
    // Queries over custom fields and tags.
    let hits = s
        .read(None, |c| {
            c.list("tag=c type=research category=active", None, None, None)
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
    let hits = s
        .read(None, |c| c.list("text~\"T\" NOT tag=zzz", None, None, None))
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn dry_run_writes_nothing_and_events_are_append_only() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "research", &[]);
    s.write(HUMAN, true, |c| c.claim(&id, ClaimOpts::default()))
        .unwrap();
    let v = s.read(None, |c| c.show(&id, With::default())).unwrap();
    assert_eq!(v["status"], "open");
    assert!(
        s.conn()
            .execute("UPDATE event SET op = 'note'", [])
            .is_err()
    );
    assert!(s.conn().execute("DELETE FROM event", []).is_err());
    assert!(s.conn().execute("DELETE FROM issue", []).is_err());
}

#[test]
fn inbox_delivers_once() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "research", &[]);
    s.write(AGENT, false, |c| c.claim(&id, ClaimOpts::default()))
        .unwrap();
    s.write(HUMAN, false, |c| c.note(&id, "please hurry", None, true))
        .unwrap();
    let first = s.write(AGENT, false, |c| c.take_inbox(&id)).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["body"], "please hurry");
    let second = s.write(AGENT, false, |c| c.take_inbox(&id)).unwrap();
    assert!(second.is_empty());
}

#[test]
fn schema_plan_maps_removed_statuses() {
    let (_d, mut s) = setup();
    let id = create(&mut s, "research", &[]);
    s.write(HUMAN, false, |c| c.claim(&id, ClaimOpts::default()))
        .unwrap();
    let mut schema = seed().compile().unwrap();
    let t = schema.types.get_mut("research").unwrap();
    for st in &mut t.statuses {
        if st.name == "in_progress" {
            st.name = "investigating".into();
        }
    }
    let tr = std::mem::take(&mut t.transitions);
    for (from, tos) in tr {
        let from = if from == "in_progress" {
            "investigating".to_string()
        } else {
            from
        };
        let tos = tos
            .into_iter()
            .map(|x| {
                if x == "in_progress" {
                    "investigating".to_string()
                } else {
                    x
                }
            })
            .collect();
        t.transitions.insert(from, tos);
    }
    let plan = s.read(None, |c| c.plan(&schema, &[])).unwrap();
    assert_eq!(plan.blockers.len(), 1);
    let e = s
        .write(HUMAN, false, |c| c.apply(&schema, &[], &[], None))
        .unwrap_err();
    assert_eq!(e.code(), Code::SchemaInvalid);
    s.write(HUMAN, false, |c| {
        c.apply(
            &schema,
            &[],
            &[("in_progress".into(), "investigating".into())],
            Some("abc".into()),
        )
    })
    .unwrap();
    let v = s.read(None, |c| c.show(&id, With::default())).unwrap();
    assert_eq!(v["status"], "investigating");
    assert_eq!(v["claim"]["actor"], HUMAN, "the claim survives the mapping");
}
