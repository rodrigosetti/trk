use serde_json::{Value, json};
use trk_core::ops::{Edit, NewIssue};
use trk_core::{Code, Store, StoreOpts};

const HUMAN: &str = "human:alice@example.com";
const AGENT: &str = "agent:omp:epics";

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::install(&dir.path().join("trk.db"), StoreOpts::default()).unwrap();
    (dir, store)
}

fn new(epic: Option<&str>) -> NewIssue {
    NewIssue {
        title: "Member".into(),
        epic: epic.map(String::from),
        ..Default::default()
    }
}

#[test]
fn curated_operations_and_global_events() {
    let (_dir, mut s) = setup();
    let e = s
        .write(AGENT, false, |c| c.create_epic("release-2", "Ship it"))
        .unwrap();
    assert_eq!(e.name, "release-2");
    assert_eq!(e.description, "Ship it");
    assert_eq!(e.created_by, AGENT);
    assert_eq!(e.issue_count, 0);
    for invalid in ["", "2release", "Release", "release_2", "release two"] {
        let err = s
            .write(HUMAN, false, |c| c.create_epic(invalid, ""))
            .unwrap_err();
        assert_eq!(err.code(), Code::InvalidField);
    }
    assert_eq!(
        s.write(HUMAN, false, |c| c.create_epic("release-2", "duplicate"))
            .unwrap_err()
            .code(),
        Code::InvalidField
    );
    s.write(AGENT, false, |c| c.rename_epic("release-2", "release-3"))
        .unwrap();
    s.write(AGENT, false, |c| c.archive_epic("release-3"))
        .unwrap();
    s.write(AGENT, false, |c| c.archive_epic("release-3"))
        .unwrap();
    assert!(s.read(None, |c| c.epics(false)).unwrap().is_empty());
    let all = s.read(None, |c| c.epics(true)).unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].archived_at.is_some());
    let events = s
        .read(None, |c| {
            let mut stmt = c.conn.prepare(
                "SELECT issue_id, op, payload FROM event WHERE op LIKE 'epic_%' ORDER BY seq",
            )?;
            Ok(stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap();
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|e| e.0.is_none()));
    assert_eq!(events[0].1, "epic_create");
    assert_eq!(events[1].1, "epic_rename");
    assert_eq!(
        serde_json::from_str::<Value>(&events[1].2).unwrap(),
        json!({"from": "release-2", "to": "release-3"})
    );
    assert_eq!(events[2].1, "epic_archive");
}

#[test]
fn assignment_validation_is_explicit_and_transactional() {
    let (_dir, mut s) = setup();
    for name in ["zulu", "alpha", "retired"] {
        s.write(HUMAN, false, |c| c.create_epic(name, "")).unwrap();
    }
    s.write(HUMAN, false, |c| c.archive_epic("retired"))
        .unwrap();
    for name in ["missing", "retired"] {
        let err = s
            .write(HUMAN, false, |c| c.create(new(Some(name))))
            .unwrap_err();
        assert_eq!(err.code(), Code::InvalidField);
        let value = err.to_json();
        assert_eq!(value["allowed"], json!(["alpha", "zulu"]));
        assert!(value["hint"].as_str().unwrap().contains("omit epic"));
    }
    let id = s
        .write(HUMAN, false, |c| c.create(new(Some("alpha"))))
        .unwrap();
    assert_eq!(
        s.read(None, |c| c.issue(&id)).unwrap().epic.as_deref(),
        Some("alpha")
    );
    let events = s.read(None, |c| c.events(&id)).unwrap();
    assert_eq!(
        events.iter().map(|e| e.op.as_str()).collect::<Vec<_>>(),
        ["create", "set"]
    );
    assert_eq!(
        events[1].payload["changes"]["epic"],
        json!({"from": null, "to": "alpha"})
    );
    let err = s
        .write(HUMAN, false, |c| {
            c.edit(
                &id,
                Edit {
                    title: Some("Must roll back".into()),
                    epic: Some(Some("missing".into())),
                    ..Default::default()
                },
            )
        })
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidField);
    assert_eq!(s.read(None, |c| c.issue(&id)).unwrap().title, "Member");
    let changes = s
        .write(HUMAN, false, |c| {
            c.edit(
                &id,
                Edit {
                    epic: Some(Some("zulu".into())),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(changes["epic"], json!({"from": "alpha", "to": "zulu"}));
    let changes = s
        .write(HUMAN, false, |c| {
            c.edit(
                &id,
                Edit {
                    epic: Some(None),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(changes["epic"], json!({"from": "zulu", "to": null}));
    assert_eq!(s.read(None, |c| c.issue(&id)).unwrap().epic, None);
    assert_eq!(s.read(None, |c| c.epics(true)).unwrap().len(), 3);
}

#[test]
fn archive_retains_members_and_rename_cascades() {
    let (_dir, mut s) = setup();
    s.write(HUMAN, false, |c| c.create_epic("original", ""))
        .unwrap();
    let id = s
        .write(HUMAN, false, |c| c.create(new(Some("original"))))
        .unwrap();
    let closed = s
        .write(HUMAN, false, |c| c.create(new(Some("original"))))
        .unwrap();
    s.write(HUMAN, false, |c| c.close(&closed, "done", None, false))
        .unwrap();
    let renamed = s
        .write(HUMAN, false, |c| c.rename_epic("original", "renamed"))
        .unwrap();
    assert_eq!(renamed.issue_count, 2);
    for member in [&id, &closed] {
        assert_eq!(
            s.read(None, |c| c.issue(member)).unwrap().epic.as_deref(),
            Some("renamed")
        );
    }
    s.write(HUMAN, false, |c| c.archive_epic("renamed"))
        .unwrap();
    s.write(HUMAN, false, |c| {
        c.edit(
            &id,
            Edit {
                description: Some("Still editable".into()),
                ..Default::default()
            },
        )
    })
    .unwrap();
    assert_eq!(
        s.read(None, |c| c.issue(&id)).unwrap().epic.as_deref(),
        Some("renamed")
    );
    assert_eq!(s.read(None, |c| c.epics(true)).unwrap()[0].issue_count, 2);
    assert_eq!(
        s.write(HUMAN, false, |c| c.edit(
            &id,
            Edit {
                epic: Some(Some("renamed".into())),
                ..Default::default()
            }
        ))
        .unwrap_err()
        .code(),
        Code::InvalidField
    );
    s.write(HUMAN, false, |c| {
        c.edit(
            &id,
            Edit {
                epic: Some(None),
                ..Default::default()
            },
        )
    })
    .unwrap();
    assert_eq!(s.read(None, |c| c.epics(true)).unwrap()[0].issue_count, 1);
}
