use super::*;

#[test]
fn install_reports_old_research_and_review_defaults_without_overwriting() {
    let e = Env::new();
    let old_types = [
        (
            "types/research.toml",
            "name = \"research\"\ndescription = \"research\"\nagent = \"researcher\"\n",
        ),
        (
            "types/review.toml",
            "name = \"review\"\ndescription = \"review\"\nagent = \"reviewer\"\n",
        ),
    ];
    let code_change = e.trk_dir().join("types/code-change.toml");
    let original_code_change = std::fs::read_to_string(&code_change).unwrap();
    for (relative, old) in old_types {
        let path = e.trk_dir().join(relative);
        let installed = std::fs::read_to_string(&path).unwrap();
        assert!(installed.contains("worktree = false"), "{relative}");
        std::fs::write(path, old).unwrap();
    }

    let installed = e.ok(&["install"]);
    assert_eq!(
        installed["differ"],
        json!(["types/research.toml", "types/review.toml"])
    );
    for (relative, old) in old_types {
        assert!(
            installed["kept"]
                .as_array()
                .unwrap()
                .contains(&json!(relative))
        );
        assert!(
            !installed["written"]
                .as_array()
                .unwrap()
                .contains(&json!(relative))
        );
        assert_eq!(
            std::fs::read_to_string(e.trk_dir().join(relative)).unwrap(),
            old
        );
    }
    assert_eq!(
        std::fs::read_to_string(code_change).unwrap(),
        original_code_change
    );
}
