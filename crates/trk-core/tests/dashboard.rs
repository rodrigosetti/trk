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
