use std::path::Path;

use trk_core::config::{ConfigDir, UserConfig};
use trk_core::error::Code;

fn config(root: &Path, text: &str) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("config.toml"), text).unwrap();
}

#[test]
fn layers_merge_keys_recursively_and_replace_arrays() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    config(
        &user,
        r#"
[user]
email = "global@example.com"
[issues]
prefix = "global"
[check]
interval = "2m"
gc = false
[harness]
prefer = ["omp", "codex"]
[harness.omp]
command = "omp"
args = ["--global", "discard"]
model = "global-model"
env = { SHARED = "user", USER_ONLY = "yes" }
builtin_tools = { read-only = ["--readonly"], none = ["--none"] }
[views.mine]
query = "state=open"
fields = ["id", "title"]
sort = "priority"
[views.global]
query = "state=waiting"
"#,
    );
    config(
        &project,
        r#"
[user]
email = "project@example.com"
[issues]
prefix = "project"
[check]
timeout = "1m"
[harness]
prefer = ["codex"]
[harness.omp]
args = ["--project"]
env = { SHARED = "project" }
builtin_tools = { read-only = [] }
[views.mine]
fields = ["id"]
"#,
    );
    let merged = ConfigDir::with_user_dir(project, Some(user))
        .user_config()
        .unwrap();
    assert_eq!(merged.user.email.as_deref(), Some("project@example.com"));
    assert_eq!(merged.issues.prefix, "project");
    assert_eq!(merged.check.interval, "2m");
    assert_eq!(merged.check.timeout, "1m");
    assert!(!merged.check.gc);
    assert_eq!(merged.check.gc_grace, "1h");
    assert_eq!(
        merged.harness["prefer"],
        toml::Value::try_from(vec!["codex"]).unwrap()
    );
    let omp = &merged.harness["omp"];
    assert_eq!(omp["command"].as_str(), Some("omp"));
    assert_eq!(omp["model"].as_str(), Some("global-model"));
    assert_eq!(
        omp["args"],
        toml::Value::try_from(vec!["--project"]).unwrap()
    );
    assert_eq!(omp["env"]["SHARED"].as_str(), Some("project"));
    assert_eq!(omp["env"]["USER_ONLY"].as_str(), Some("yes"));
    assert!(
        omp["builtin_tools"]["read-only"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        omp["builtin_tools"]["none"],
        toml::Value::try_from(vec!["--none"]).unwrap()
    );
    assert_eq!(merged.views["mine"].query, "state=open");
    assert_eq!(merged.views["mine"].fields.as_deref().unwrap(), ["id"]);
    assert_eq!(merged.views["mine"].sort.as_deref(), Some("priority"));
    assert_eq!(merged.views["global"].query, "state=waiting");
}

#[test]
fn project_can_inherit_identity_and_prefix_without_changing_other_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    config(
        &user,
        "[user]\nemail = 'global@example.com'\n[issues]\nprefix = 'global'\n",
    );
    config(&project, "[check]\nverify = 'cargo test'\n");
    let merged = ConfigDir::with_user_dir(project, Some(user))
        .user_config()
        .unwrap();
    assert_eq!(merged.user.email.as_deref(), Some("global@example.com"));
    assert_eq!(merged.issues.prefix, "global");
    assert_eq!(merged.check.verify, "cargo test");
    assert_eq!(
        merged.claims.agent_lease,
        UserConfig::default().claims.agent_lease
    );
}

#[test]
fn missing_user_directory_preserves_complete_project_config() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let text = trk_core::seed::CONFIG_TOML
        .replace("{{EMAIL_LINE}}", "email = 'project@example.com'")
        .replace("{{PREFIX}}", "project")
        .replace("{{BASE}}", "'main'");
    config(&project, &text);
    let expected: UserConfig = toml::from_str(&text).unwrap();
    let absent = ConfigDir::with_user_dir(&project, Some(dir.path().join("absent")))
        .user_config()
        .unwrap();
    let disabled = ConfigDir::with_user_dir(&project, None)
        .user_config()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&absent).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&disabled).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
}

#[test]
fn user_resources_do_not_install_a_project_implicitly() {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("user");
    config(&user, "[issues]\nprefix = 'global'\n");
    let error = ConfigDir::with_user_dir(dir.path().join("project"), Some(user))
        .user_config()
        .unwrap_err();
    assert_eq!(error.code(), Code::NotInstalled);
}

#[test]
fn malformed_user_config_is_not_silently_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let user = dir.path().join("user");
    config(&project, "[issues]\nprefix = 'project'\n");
    config(&user, "not valid TOML");
    let error = ConfigDir::with_user_dir(project, Some(user.clone()))
        .user_config()
        .unwrap_err();
    assert_eq!(error.code(), Code::ConfigInvalid);
    assert!(
        error
            .to_string()
            .contains(&user.join("config.toml").display().to_string())
    );
}
