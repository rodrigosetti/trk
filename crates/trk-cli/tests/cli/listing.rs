use super::*;

#[test]
fn list_defaults_to_priority_then_age_and_honors_sort_overrides() {
    let e = Env::new();
    let issue = |now, title, priority| e.at(now, &["create", title, "-p", priority]);
    let older = issue("2026-11-20T10:00:00Z", "Older normal issue", "P2");
    let newer = issue("2026-11-20T11:00:00Z", "Newer normal issue", "P2");
    let urgent = issue("2026-11-20T12:00:00Z", "Urgent issue", "P0");
    let id = |issue: &Value| issue["id"].as_str().unwrap().to_string();
    let mut age_order = vec![older.clone(), newer.clone(), urgent.clone()];
    age_order.sort_by(|a, b| {
        a["created_at"]
            .as_str()
            .cmp(&b["created_at"].as_str())
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let ids = |rows: &Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let mut default_order = vec![id(&urgent)];
    default_order.extend(age_order[..2].iter().map(id));
    assert_eq!(ids(&e.ok(&["list"])), default_order);
    assert_eq!(
        ids(&e.ok(&["list", "--sort", "created"])),
        age_order.iter().map(id).collect::<Vec<_>>()
    );

    e.set_config(
        r#"[views.mine]
query = "claimed-by=me""#,
        r#"[views.mine]
query = "state=open"
sort = "-created""#,
    );
    let mut reverse_age_order = age_order.clone();
    reverse_age_order.sort_by(|a, b| {
        b["created_at"]
            .as_str()
            .cmp(&a["created_at"].as_str())
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let reverse_age_ids = reverse_age_order.iter().map(id).collect::<Vec<_>>();
    assert_eq!(ids(&e.ok(&["list", "@mine"])), reverse_age_ids);
    assert_eq!(
        ids(&e.ok(&["list", "@mine", "--sort", "created"])),
        age_order.iter().map(id).collect::<Vec<_>>()
    );
}

#[test]
fn list_and_ready_filter_and_sort_by_type() {
    let e = Env::new();
    let code = e.create(&["--type", "change", "Code change"]);
    let research = e.create(&["--type", "research", "Research"]);
    let review = e.create(&["--type", "bug", "Review"]);
    let ids = |rows: &Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        ids(&e.ok(&["list", "type=research"])),
        vec![research.clone()]
    );
    assert_eq!(ids(&e.ok(&["ready", "type=bug"])), vec![review.clone()]);
    assert_eq!(
        ids(&e.ok(&["list", "--sort", "type"])),
        vec![review.clone(), code.clone(), research.clone()]
    );
    assert_eq!(
        ids(&e.ok(&["ready", "--sort", "type"])),
        vec![review, code, research]
    );
}

#[test]
fn default_list_and_ready_tables_show_issue_type() {
    let e = Env::new();
    for issue_type in ["change", "research", "bug"] {
        e.create(&["--type", issue_type, issue_type]);
    }

    for command in ["list", "ready"] {
        let out = e
            .cmd()
            .args(["--format", "pretty", command])
            .output()
            .unwrap();
        assert!(out.status.success());
        let table = String::from_utf8(out.stdout).unwrap();
        let header = table.lines().next().unwrap();
        assert!(
            header.find("PRIORITY").unwrap() < header.find("TYPE").unwrap()
                && header.find("TYPE").unwrap() < header.find("STATE").unwrap(),
            "{table}"
        );
        for issue_type in ["change", "research", "bug"] {
            assert!(table.contains(issue_type), "{table}");
        }
    }
}
