use super::*;

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn type_guidance_preserves_layered_origins_and_agent_overrides() {
    let e = Env::new();
    write(
        &e.user_dir(),
        "types/review.toml",
        "agent = 'reviewer'\ndescription = 'Review carefully'\nworktree = false\nguidance = '''{% include \"prompts/type-guidance.md.j2\" %}'''",
    );
    std::fs::remove_file(e.trk_dir().join("types/review.toml")).unwrap();
    write(
        &e.user_dir(),
        "prompts/type-guidance.md.j2",
        "USER {{ type.name }} {{ agent.name }}",
    );
    let id = e.create(&["Review guidance", "--type", "review"]);
    let preview = e.ok(&["agent", "preview", "researcher", "--issue", &id]);
    assert_eq!(preview["guidance"], "USER review researcher");
    assert!(
        preview["instructions"]
            .as_str()
            .unwrap()
            .contains("USER review researcher")
    );
    let inline = "types/review.toml#guidance";
    assert_eq!(preview["templates"][inline]["layer"], "user");
    assert_eq!(
        preview["templates"][inline]["source"],
        json!(e.user_dir().join("types/review.toml"))
    );
    assert_eq!(
        preview["templates"]["prompts/type-guidance.md.j2"]["layer"],
        "user"
    );

    write(
        &e.trk_dir(),
        "prompts/type-guidance.md.j2",
        "PROJECT {{ type.name }} {{ agent.name }} {{ issue.id }}",
    );
    let expected = format!("PROJECT review researcher {id}");
    let spec = e.ok(&[
        "run",
        &id,
        "--agent",
        "researcher",
        "--harness",
        "omp",
        "--dry-run",
    ]);
    assert_eq!(spec["guidance"], expected);
    assert!(spec["instructions"].as_str().unwrap().contains(&expected));
    assert_eq!(spec["routing"]["type_layer"], "user");
    assert_eq!(spec["templates"][inline]["layer"], "user");
    assert_eq!(
        spec["templates"]["prompts/type-guidance.md.j2"]["layer"],
        "project"
    );
    let checked = e.ok(&["agent", "check"]);
    let route = checked["resources"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "review")
        .unwrap();
    assert!(
        route["guidance"]
            .as_str()
            .unwrap()
            .starts_with("PROJECT review reviewer ")
    );
    assert_eq!(checked["resources"]["templates"][inline]["layer"], "user");
    let output = e
        .cmd()
        .args([
            "--format",
            "pretty",
            "--no-color",
            "agent",
            "preview",
            "researcher",
            "--issue",
            &id,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&expected));
    assert!(text.contains(&e.user_dir().join("types/review.toml").display().to_string()));
    assert!(
        text.contains(
            &e.trk_dir()
                .join("prompts/type-guidance.md.j2")
                .display()
                .to_string()
        )
    );
    assert!(text.contains("(user)") && text.contains("(project)"));
}

#[test]
fn agent_check_rejects_invalid_or_missing_guidance_in_nondefault_type() {
    let e = Env::new();
    let route = "agent = 'reviewer'\ndescription = 'Review'\nworktree = false\nguidance = '''{% include \"prompts/review-guidance.md.j2\" %}'''";
    write(&e.trk_dir(), "types/review.toml", route);
    let missing = e.err(&["agent", "check"], 4, "config_invalid");
    assert!(
        missing.to_string().contains("review-guidance.md.j2"),
        "{missing:#}"
    );
    write(&e.trk_dir(), "prompts/review-guidance.md.j2", "{% if %}");
    let invalid = e.err(&["agent", "check"], 4, "config_invalid");
    assert!(
        invalid.to_string().contains("review-guidance.md.j2"),
        "{invalid:#}"
    );
    write(
        &e.trk_dir(),
        "prompts/review-guidance.md.j2",
        "REVIEW {{ type.name }}",
    );
    let checked = e.ok(&["agent", "check"]);
    let route = checked["resources"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "review")
        .unwrap();
    assert_eq!(route["guidance"], "REVIEW review");
}

#[test]
fn absent_type_guidance_keeps_original_instructions() {
    let e = Env::new();
    write(
        &e.trk_dir(),
        "agents/implementer.toml",
        "name = 'implementer'\ninstructions = 'prompts/plain.md.j2'\n",
    );
    write(
        &e.trk_dir(),
        "prompts/plain.md.j2",
        "UNCHANGED INSTRUCTIONS",
    );
    let preview = e.ok(&["agent", "preview", "implementer"]);
    assert_eq!(preview["instructions"], "UNCHANGED INSTRUCTIONS");
    assert!(preview["guidance"].is_null());
    assert!(
        preview["templates"]
            .get("types/code-change.toml#guidance")
            .is_none()
    );
    let id = e.create(&["No guidance"]);
    let spec = e.ok(&["run", &id, "--harness", "omp", "--dry-run"]);
    assert_eq!(spec["instructions"], "UNCHANGED INSTRUCTIONS");
    assert!(spec.get("guidance").is_none());
    assert!(spec["routing"].get("guidance").is_none());
}
