//! `trk herdr`: the dashboard as a panel in the Herdr app.
//!
//! Two steps, because that is how Herdr works. The mod is installed once per
//! machine (`trk herdr install`): its files go in Herdr's folder and are
//! embedded in this binary, so the mod always matches the `trk --json` it
//! reads. A space shows it once a pane or tab there holds it
//! (`trk herdr open`); Herdr keeps that pane with its tab, so the panel
//! stays in the space until it is closed.
//!
//! Both mod folders (`mods/trk/`, `mods/trk-tabs/`) are machine-owned.
//! The single trk layout is installed separately from activation; upgrades back up old copies.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use trk_core::error::{Code, Error, Result, bad_args};

use crate::env::{Env, home};
use crate::herdr::{command, json_command, terminal_space};
use crate::render::Out;

pub const MOD: &str = "trk";

/// The mod's files, relative to `mods/trk/`.
const MOD_FILES: &[(&str, &str)] = &[
    ("main.js", include_str!("../../../herdr/mods/trk/main.js")),
    ("mod.json", include_str!("../../../herdr/mods/trk/mod.json")),
    (
        "dashboard.svg",
        include_str!("../../../herdr/mods/trk/dashboard.svg"),
    ),
    (
        "spinner.svg",
        include_str!("../../../herdr/mods/trk/spinner.svg"),
    ),
];

const TABS_FILES: &[(&str, &str)] = &[
    (
        "main.js",
        include_str!("../../../herdr/mods/trk-tabs/main.js"),
    ),
    (
        "mod.json",
        include_str!("../../../herdr/mods/trk-tabs/mod.json"),
    ),
    (
        "project.js",
        include_str!("../../../herdr/mods/trk-tabs/project.js"),
    ),
];

/// The layout's files, relative to `layouts/`; the layout itself first.
const LAYOUT_FILES: &[(&str, &str)] = &[
    ("trk.json", include_str!("../../../herdr/layouts/trk.json")),
    ("trk.svg", include_str!("../../../herdr/layouts/trk.svg")),
];

const MODS: &[(&str, &[(&str, &str)])] = &[("trk", MOD_FILES), ("trk-tabs", TABS_FILES)];

const KEY_SECTION: &str = "layout:trk";
const DISPATCH: &str = "trk-tabs.dispatch:";

fn native_action(action: &Value) -> Option<Value> {
    let (command, args) = if let Some(action) = action.as_str() {
        let (command, argument) = action.split_once(':').unwrap_or((action, ""));
        let mut args = Map::new();
        if !argument.is_empty() {
            let name = match command {
                "tab.select" | "agent.select" => "index",
                "pane.split" | "pane.go" => "direction",
                _ => return None,
            };
            let value = if name == "index" {
                json!(argument.parse::<u64>().ok()?)
            } else {
                json!(argument)
            };
            args.insert(name.into(), value);
        }
        (command, Value::Object(args))
    } else {
        (
            action.get("command")?.as_str()?,
            action.get("args").cloned().unwrap_or(json!({})),
        )
    };
    if !matches!(
        command,
        "tab.next"
            | "tab.prev"
            | "tab.select"
            | "tab.close"
            | "pane.close"
            | "space.close"
            | "tab.new"
            | "pane.split"
            | "terminal.open"
            | "agent.next"
            | "agent.prev"
            | "agent.select"
            | "pane.go"
            | "tab.rename"
    ) {
        return None;
    }
    if command == "tab.select" && !args.get("index").is_some_and(Value::is_u64) {
        return None;
    }
    Some(json!({"command": command, "args": args}))
}

fn read_keys(app: &Path) -> Result<Value> {
    let path = app.join("keys.json");
    if !path.exists() {
        return Ok(json!({}));
    }
    serde_json::from_str(&std::fs::read_to_string(path)?)
        .map_err(|e| bad_args(format!("invalid Herdr keys.json: {e}")))
}

fn write_keys(app: &Path, keys: &Value) -> Result<()> {
    std::fs::write(
        app.join("keys.json"),
        format!("{}\n", serde_json::to_string_pretty(keys).unwrap()),
    )?;
    Ok(())
}

fn install_keys(app: &Path) -> Result<()> {
    let mut keys = read_keys(app)?;
    let before = keys.clone();
    let root = keys
        .as_object_mut()
        .ok_or_else(|| bad_args("Herdr keys.json must be an object"))?;
    let mut effective = root
        .get("global")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(local) = root.get(KEY_SECTION).and_then(Value::as_object) {
        effective.extend(local.clone());
    }
    let local = root
        .entry(KEY_SECTION)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| bad_args("Herdr layout:trk bindings must be an object"))?;
    for (key, action) in effective {
        if let Some(native) = native_action(&action) {
            local.insert(key, json!(format!("{DISPATCH}{native}")));
        }
    }
    if keys != before {
        write_keys(app, &keys)?;
    }
    Ok(())
}

fn uninstall_keys(app: &Path) -> Result<()> {
    if !app.join("keys.json").exists() {
        return Ok(());
    }
    let mut keys = read_keys(app)?;
    let global = keys
        .get("global")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(local) = keys.get_mut(KEY_SECTION).and_then(Value::as_object_mut) {
        let replacements: Vec<_> = local
            .iter()
            .filter_map(|(key, action)| {
                let encoded = action.as_str()?.strip_prefix(DISPATCH)?;
                let native: Value = serde_json::from_str(encoded).ok()?;
                if native_action(&native).as_ref() != Some(&native)
                    || format!("{DISPATCH}{native}") != action.as_str()?
                {
                    return None;
                }
                Some((key.clone(), native))
            })
            .collect();
        for (key, native) in replacements {
            if global.get(&key).and_then(native_action).as_ref() == Some(&native) {
                local.remove(&key);
            } else {
                local.insert(key, native);
            }
        }
    }
    if keys
        .get(KEY_SECTION)
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        keys.as_object_mut().unwrap().remove(KEY_SECTION);
    }
    write_keys(app, &keys)
}

fn migrate_selection(
    app: &Path,
    mut run: impl FnMut(&[&str]) -> std::io::Result<std::process::Output>,
) -> Result<()> {
    let legacy = |name: &str| matches!(name, "main-trk" | "main-trk-tabs");
    if let Ok(status) = run(&["status"]) {
        if status.status.success() {
            let status: Value = serde_json::from_slice(&status.stdout)
                .map_err(|e| bad_args(format!("invalid Herdr status: {e}")))?;
            if status["layout"].as_str().is_some_and(legacy) {
                let switched = run(&["do", "layout.switch", "--name", "trk"])?;
                if !switched.status.success() {
                    return Err(herdr(format!(
                        "cannot migrate active Herdr layout: {}",
                        String::from_utf8_lossy(&switched.stderr)
                    )));
                }
            }
            // The running app owns shared.json; never rewrite its snapshot.
            return Ok(());
        }
    }
    let shared_path = app.join("shared.json");
    if shared_path.exists() {
        let mut shared: Value = serde_json::from_str(&std::fs::read_to_string(&shared_path)?)
            .map_err(|e| bad_args(format!("invalid Herdr shared.json: {e}")))?;
        if shared["layout"].as_str().is_some_and(legacy) {
            shared["layout"] = json!("trk");
            let temporary = tempfile::NamedTempFile::new_in(app)?;
            std::fs::write(
                temporary.path(),
                serde_json::to_vec_pretty(&shared).unwrap(),
            )?;
            temporary.persist(&shared_path).map_err(|e| e.error)?;
        }
    }
    Ok(())
}

fn contains_tabs(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.get("mod").and_then(Value::as_str) == Some("trk-tabs")
                || object.values().any(contains_tabs)
        }
        Value::Array(values) => values.iter().any(contains_tabs),
        _ => false,
    }
}

fn install_layout(app: &Path) -> Result<()> {
    std::fs::create_dir_all(app.join("layouts"))?;
    let manifest_path = app.join("backups/trk-layouts/installed.json");
    let previous: Value = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(json!({}));
    let current_layout = std::fs::read_to_string(app.join("layouts/trk.json")).ok();
    let new_layout = current_layout.as_ref().is_some_and(|text| {
        serde_json::from_str(text)
            .ok()
            .as_ref()
            .is_some_and(contains_tabs)
    });
    let mut preserved = Vec::new();
    // Retire legacy selectable layouts, retaining personal copies outside layouts/.
    for rel in [
        "trk.json",
        "trk.svg",
        "main-trk.json",
        "main-trk.svg",
        "main-trk-tabs.json",
        "main-trk-tabs.svg",
    ] {
        let path = app.join("layouts").join(rel);
        if !path.exists() && !path.is_symlink() {
            continue;
        }
        if rel.starts_with("trk.")
            && (previous
                .get(rel)
                .and_then(Value::as_str)
                .is_some_and(|installed| !same(&path, installed))
                || (previous.get(rel).is_none() && new_layout))
        {
            preserved.push(rel);
            continue;
        }
        if LAYOUT_FILES
            .iter()
            .any(|(name, content)| *name == rel && same(&path, content))
        {
            continue;
        }
        let backup_dir = app.join("backups/trk-layouts");
        std::fs::create_dir_all(&backup_dir)?;
        let mut backup = backup_dir.join(rel);
        let mut version = 1;
        while backup.exists() || backup.is_symlink() {
            backup = backup_dir.join(format!("{rel}.{version}"));
            version += 1;
        }
        std::fs::rename(path, backup)?;
        let state = app.join("layouts/.state").join(rel);
        if state.exists() || state.is_symlink() {
            let mut state_backup = backup_dir.join(format!("{rel}.state"));
            let mut version = 1;
            while state_backup.exists() || state_backup.is_symlink() {
                state_backup = backup_dir.join(format!("{rel}.state.{version}"));
                version += 1;
            }
            std::fs::rename(state, state_backup)?;
        }
    }
    for (rel, content) in LAYOUT_FILES {
        let path = app.join("layouts").join(rel);
        if !preserved.contains(rel) && !same(&path, content) {
            std::fs::write(path, content)?;
        }
    }
    install_keys(app)?;
    std::fs::create_dir_all(manifest_path.parent().unwrap())?;
    let manifest: Map<String, Value> = LAYOUT_FILES
        .iter()
        .map(|(rel, content)| ((*rel).into(), json!(content)))
        .collect();
    std::fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap())?;
    migrate_selection(app, |args| {
        std::process::Command::new("herdr-app")
            .args(args)
            .env("HERDR_APP_HOME", app)
            .output()
    })
}

/// The panels `trk herdr open` made, by space, in the project's `.trk/`.
const PANELS_FILE: &str = "herdr.json";

const KEYS_HINT: &str = "keys      optional, in ~/.config/herdr-app/keys.json:\n            \"global\": { \"ctrl-shift-i\": \"trk.new\" }   (new issue from anywhere a panel shows)";

/// Herdr's folder: the app's whole interface, as files.
pub fn app_home() -> PathBuf {
    home().join(".config/herdr-app")
}

fn same(path: &Path, content: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s == content)
}

fn no_herdr(app: &Path) -> Error {
    Error::new(
        Code::NotFound,
        format!("Herdr is not set up here: {} not found", app.display()),
    )
    .with_hint("start the Herdr app once, then run this again")
}

/// "absent" (no Herdr), "missing", "current", "outdated", or "linked" (a
/// symlink, e.g. a development copy).
pub fn state(app: &Path) -> &'static str {
    if !app.join("mods").is_dir() {
        return "absent";
    }
    if !app.join("mods/trk/main.js").exists() && !app.join("mods/trk").is_symlink() {
        return "missing";
    }
    if MODS.iter().any(|(name, files)| {
        let dir = app.join("mods").join(name);
        !dir.is_symlink()
            && !files
                .iter()
                .all(|(rel, content)| same(&dir.join(rel), content))
    }) {
        "outdated"
    } else if MODS
        .iter()
        .any(|(name, _)| app.join("mods").join(name).is_symlink())
    {
        "linked"
    } else {
        "current"
    }
}

/// Write the mod's files; what happened, in a line.
fn write_mod(app: &Path) -> Result<String> {
    let before = state(app);
    let mut lines = Vec::new();
    for (name, files) in MODS {
        let dir = app.join("mods").join(name);
        if dir.is_symlink() {
            lines.push(format!(
                "{} is a symlink (a development copy?); left alone",
                dir.display()
            ));
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        for (rel, content) in *files {
            let path = dir.join(rel);
            if !same(&path, content) {
                std::fs::write(path, content)?;
            }
        }
        lines.push(format!(
            "mod {} at {}",
            if before == "current" {
                "already current"
            } else {
                "updated"
            },
            dir.display()
        ));
    }
    Ok(lines.join("\n          "))
}

pub struct InstallOpts {
    /// Do nothing unless the mod is installed already.
    pub if_installed: bool,
}

/// `trk herdr install`.
pub fn install(app: &Path, opts: InstallOpts) -> Result<Out> {
    let before = state(app);
    if opts.if_installed && matches!(before, "absent" | "missing") {
        return Ok(Out::Text(
            "the trk Herdr mod is not installed; nothing to refresh".into(),
            json!({"mod_state": before, "changed": false}),
        ));
    }
    if before == "absent" {
        return Err(no_herdr(app));
    }
    let mut lines = vec![write_mod(app)?];
    let layout = app.join("layouts/trk.json");
    install_layout(app)?;
    let layout_state = "written";
    lines.push("layout installed (not activated): herdr-app do layout.switch --name trk; revert: herdr-app do layout.switch --name main".into());
    lines.push("keys: only layout:trk bindings changed; edited legacy layouts backed up under backups/trk-layouts/".into());
    let mut text = format!("herdr     {}", lines.join("\n          "));
    if !opts.if_installed {
        text.push_str(&format!(
            "\n{KEYS_HINT}\n\nNext, in a project:\n  trk herdr open                   (the panel as a tab in this Herdr space)"
        ));
    }
    Ok(Out::Text(
        text,
        json!({"mod": app.join("mods").join(MOD), "mod_state": state(app),
               "changed": before != "current", "layout": layout, "layout_state": layout_state}),
    ))
}

/// For `trk install`: keep an installed mod current, and say the panel
/// exists where Herdr is set up without it. Never installs it: `trk install`
/// is about one project, and the mod is machine-wide.
pub fn refresh(app: &Path) -> Result<Option<(String, Value)>> {
    let before = state(app);
    let line = match before {
        "absent" => return Ok(None),
        "missing" => "Herdr found: `trk herdr open` shows the dashboard as a panel there".into(),
        _ => {
            let line = write_mod(app)?;
            install_layout(app)?;
            line
        }
    };
    Ok(Some((line, json!({"mod_state": state(app)}))))
}

/// `trk herdr uninstall`.
pub fn uninstall(app: &Path, force: bool) -> Result<Out> {
    let mut lines = vec![];
    let mut removed = false;
    for (name, _) in MODS {
        let dir = app.join("mods").join(name);
        if dir.is_symlink() {
            lines.push(format!(
                "{} is a symlink (a development copy?); left alone",
                dir.display()
            ));
        } else if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
            lines.push(format!("mod removed from {}", dir.display()));
            removed = true;
        }
    }
    uninstall_keys(app)?;
    let mut layout_state = "absent";
    for (rel, content) in LAYOUT_FILES {
        let path = app.join("layouts").join(rel);
        if !path.exists() {
            continue;
        }
        if force || same(&path, content) {
            std::fs::remove_file(&path)?;
            if layout_state != "kept" {
                layout_state = "removed";
            }
        } else {
            lines.push(format!(
                "layout kept at {} (you edited it; --force to remove)",
                path.display()
            ));
            layout_state = "kept";
        }
    }
    if removed {
        lines.push("panels still open in Herdr are empty now: close them there".into());
    }
    Ok(Out::Text(
        format!("herdr     {}", lines.join("\n          ")),
        json!({"removed": removed, "layout_state": layout_state}),
    ))
}

/// Where a panel goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    Tab,
    Right,
    Down,
}

/// The space to open in, and this terminal's own tab and pane there.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    space: String,
    active: bool,
    own: Option<(String, String)>,
}

fn tabs(space: &Value) -> impl Iterator<Item = &Value> {
    space["tabs"].as_array().into_iter().flatten()
}

/// The space named with `--space`, else the one this terminal is in, else
/// the only one whose folder is the project.
fn pick_space(
    ui: &Value,
    explicit: Option<&str>,
    own: Option<&str>,
    root: &Path,
) -> Result<Target> {
    let spaces = || ui["spaces"].as_array().into_iter().flatten();
    let own_in = |space: &Value| {
        let own = own?;
        tabs(space).find_map(|tab| {
            let pane = tab["terminals"]
                .as_array()?
                .iter()
                .find(|t| t["terminal"] == own)?;
            Some((
                tab["id"].as_str()?.to_string(),
                pane["pane"].as_str()?.to_string(),
            ))
        })
    };
    let space = if let Some(id) = explicit {
        spaces().find(|s| s["id"] == id).ok_or_else(|| {
            bad_args(format!("no Herdr space with id {id}"))
                .with_hint("`herdr-app ls` lists the spaces")
        })?
    } else if let Some(space) = own.and_then(|t| terminal_space(ui, t)) {
        space
    } else {
        let root = root.to_string_lossy();
        let mut here = spaces().filter(|s| s["machine"].is_null() && s["folder"] == root.as_ref());
        match (here.next(), here.next()) {
            (Some(space), None) => space,
            (None, _) => {
                return Err(bad_args(format!("no Herdr space is in {root}")).with_hint(
                    "run this in a terminal of the space, or pass --space <id> (`herdr-app ls`)",
                ));
            }
            _ => {
                return Err(bad_args(format!("several Herdr spaces are in {root}"))
                    .with_hint("run this in a terminal of the space, or pass --space <id> (`herdr-app ls`)"));
            }
        }
    };
    if !space["machine"].is_null() {
        return Err(bad_args(format!(
            "that space is on another machine ({}); the panel reads this machine's project",
            space["machine"].as_str().unwrap_or_default()
        )));
    }
    Ok(Target {
        space: space["id"].as_str().unwrap_or_default().to_string(),
        active: space["active"] == true,
        own: own_in(space),
    })
}

fn read_panels(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["panels"].as_object().cloned())
        .unwrap_or_default()
}

fn herdr(e: String) -> Error {
    Error::new(Code::Internal, e)
}

/// `trk herdr open`: the panel for this project in a Herdr space. Opening
/// again goes to the panel that is there.
pub fn open(env: &Env, app: &Path, place: Place, space: Option<&str>) -> Result<Out> {
    if state(app) == "absent" {
        return Err(no_herdr(app));
    }
    let installed = (state(app) != "current")
        .then(|| write_mod(app))
        .transpose()?;
    let ui = json_command("herdr-app", &["ls"]).map_err(|e| {
        Error::new(Code::NotFound, format!("the Herdr app is not running: {e}"))
            .with_hint("open Herdr, then run this again")
    })?;
    let own = std::env::var("HC_TERMINAL_ID").ok();
    let target = pick_space(&ui, space, own.as_deref(), &env.root)?;
    let file = env.dir.join(PANELS_FILE);
    let mut panels = read_panels(&file);
    let show_space = || -> Result<()> {
        if !target.active {
            command("herdr-app", &["do", "space.select", "--id", &target.space]).map_err(herdr)?;
        }
        Ok(())
    };

    // The panel made here before, if its pane still exists.
    if let Some(known) = panels.get(&target.space)
        && let (Some(tab), Some(pane)) = (known["tab"].as_str(), known["pane"].as_str())
        && ui["spaces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["id"] == target.space.as_str())
            .flat_map(tabs)
            .any(|t| t["id"] == tab)
    {
        show_space()?;
        if command("herdr-app", &["do", "tab.select", "--id", tab]).is_ok()
            && command("herdr-app", &["do", "pane.focus", "--pane", pane]).is_ok()
        {
            return Ok(Out::Text(
                format!("the trk panel is already open in this space: showing it (tab {tab})"),
                json!({"space": target.space, "tab": tab, "pane": pane, "opened": false}),
            ));
        }
    }

    let props = json!({"folder": env.root}).to_string();
    let made = match place {
        Place::Tab => json_command(
            "herdr-app",
            &["do", "tab.new", "--space", &target.space, "--name", "trk dashboard", "--mod", MOD, "--props", &props],
        ),
        Place::Right | Place::Down => {
            let Some((tab, pane)) = &target.own else {
                return Err(bad_args("--split puts the panel beside this terminal, and this terminal is not a pane of that space")
                    .with_hint("run it in a Herdr pane, or omit --split to open a tab"));
            };
            let direction = if place == Place::Right { "right" } else { "down" };
            json_command(
                "herdr-app",
                &["do", "pane.split", "--pane", pane, "--direction", direction, "--mod", MOD, "--props", &props],
            )
            .map(|mut v| {
                v["tab"] = json!(tab);
                v
            })
        }
    }
    .map_err(herdr)?;
    let (tab, pane) = (made["tab"].clone(), made["pane"].clone());
    panels.insert(target.space.clone(), json!({"tab": tab, "pane": pane}));
    // Best effort: without the file, the next open makes a second panel.
    let _ = std::fs::write(&file, json!({"panels": panels}).to_string());

    let mut text = String::new();
    if let Some(line) = &installed {
        text.push_str(&format!("{line}\n"));
    }
    text.push_str(&match place {
        Place::Tab => format!("opened the trk panel for {} as a tab", env.root.display()),
        _ => format!(
            "opened the trk panel for {} beside this pane",
            env.root.display()
        ),
    });
    text.push_str("\nIt stays in this space until you close it in Herdr.");
    Ok(Out::Text(
        text,
        json!({"space": target.space, "tab": tab, "pane": pane, "opened": true,
               "mod_installed": installed.is_some()}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(if_installed: bool) -> InstallOpts {
        InstallOpts { if_installed }
    }

    fn data(out: Out) -> Value {
        match out {
            Out::Text(_, v) => v,
            _ => panic!("expected text output"),
        }
    }

    fn herdr_home() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("mods")).unwrap();
        tmp
    }

    #[cfg(unix)]
    #[test]
    fn selection_migration_uses_live_commands_without_rewriting_shared() {
        use std::os::unix::process::ExitStatusExt;
        let tmp = herdr_home();
        let path = tmp.path().join("shared.json");
        let persisted = r#"{"layout":"main-trk","unrelated":42}"#;
        std::fs::write(&path, persisted).unwrap();
        for layout in ["main", "main-trk", "main-trk-tabs"] {
            let mut calls = Vec::new();
            migrate_selection(tmp.path(), |args| {
                calls.push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: serde_json::to_vec(&json!({"layout":layout})).unwrap(),
                    stderr: Vec::new(),
                })
            })
            .unwrap();
            assert_eq!(calls.len(), if layout == "main" { 1 } else { 2 });
            if calls.len() == 2 {
                assert_eq!(calls[1], ["do", "layout.switch", "--name", "trk"]);
            }
            assert!(same(&path, persisted));
        }
        migrate_selection(tmp.path(), |_| {
            Err(std::io::Error::from(std::io::ErrorKind::NotConnected))
        })
        .unwrap();
        let shared: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(shared, json!({"layout":"trk","unrelated":42}));
    }

    #[test]
    fn refresh_preserves_personal_edits_to_the_current_layout() {
        let tmp = herdr_home();
        install(tmp.path(), opts(false)).unwrap();
        let layout = tmp.path().join("layouts/trk.json");
        let mut personal: Value =
            serde_json::from_str(&std::fs::read_to_string(&layout).unwrap()).unwrap();
        personal["personal"] = json!(true);
        let text = serde_json::to_string(&personal).unwrap();
        std::fs::write(&layout, &text).unwrap();
        refresh(tmp.path()).unwrap();
        install(tmp.path(), opts(false)).unwrap();
        assert!(same(&layout, &text));
        assert!(!tmp.path().join("backups/trk-layouts/trk.json").exists());
    }

    #[test]
    fn absent_herdr_is_never_created_and_uninstall_preserves_edited_layout() {
        let absent = tempfile::tempdir().unwrap();
        assert!(install(absent.path(), opts(false)).is_err());
        assert!(refresh(absent.path()).unwrap().is_none());
        assert_eq!(
            data(install(absent.path(), opts(true)).unwrap())["changed"],
            false
        );
        assert!(!absent.path().join("mods").exists());

        let tmp = herdr_home();
        install(tmp.path(), opts(false)).unwrap();
        std::fs::write(tmp.path().join("layouts/trk.json"), "{\"mine\":true}").unwrap();
        uninstall(tmp.path(), false).unwrap();
        assert!(same(
            &tmp.path().join("layouts/trk.json"),
            "{\"mine\":true}"
        ));
        uninstall(tmp.path(), true).unwrap();
        assert!(!tmp.path().join("layouts/trk.json").exists());
    }

    #[test]
    fn install_refresh_and_uninstall_manage_both_mods_and_one_layout() {
        let tmp = herdr_home();
        let app = tmp.path();
        refresh(app).unwrap();
        assert!(!app.join("mods/trk-tabs").exists());
        install(app, opts(true)).unwrap();
        assert_eq!(state(app), "missing");
        install(app, opts(false)).unwrap();
        assert_eq!(state(app), "current");
        for (name, files) in MODS {
            for (rel, content) in *files {
                assert!(same(&app.join("mods").join(name).join(rel), content));
            }
        }
        assert!(app.join("layouts/trk.json").exists());
        assert!(!app.join("layouts/main-trk.json").exists());
        std::fs::write(app.join("mods/trk-tabs/project.js"), "old").unwrap();
        std::fs::write(app.join("mods/trk-tabs/personal"), "mine").unwrap();
        assert_eq!(state(app), "outdated");
        refresh(app).unwrap();
        assert_eq!(state(app), "current");
        assert!(same(&app.join("mods/trk-tabs/personal"), "mine"));
        uninstall(app, false).unwrap();
        assert!(!app.join("mods/trk").exists());
        assert!(!app.join("mods/trk-tabs").exists());
        assert!(!app.join("layouts/trk.json").exists());
    }

    #[test]
    fn upgrade_backs_up_personal_layouts_and_migrates_active_legacy_layout() {
        let tmp = herdr_home();
        let app = tmp.path();
        std::fs::create_dir_all(app.join("layouts/.state")).unwrap();
        for rel in [
            "trk.json",
            "main-trk.json",
            ".state/trk.json",
            ".state/main-trk.json",
        ] {
            std::fs::write(app.join("layouts").join(rel), "{\"personal\":true}").unwrap();
        }
        std::fs::write(app.join("layouts/main.json"), "stock").unwrap();
        std::fs::write(app.join("layouts/personal.json"), "personal").unwrap();
        std::fs::write(
            app.join("shared.json"),
            r#"{"layout":"main-trk","other":7}"#,
        )
        .unwrap();
        install(app, opts(false)).unwrap();
        assert!(same(
            &app.join("backups/trk-layouts/trk.json"),
            "{\"personal\":true}"
        ));
        assert!(same(
            &app.join("backups/trk-layouts/main-trk.json"),
            "{\"personal\":true}"
        ));
        assert!(app.join("backups/trk-layouts/trk.json.state").exists());
        assert!(!app.join("layouts/.state/trk.json").exists());
        assert!(!app.join("layouts/main-trk.json").exists());
        assert!(same(&app.join("layouts/main.json"), "stock"));
        assert!(same(&app.join("layouts/personal.json"), "personal"));
        let shared: Value =
            serde_json::from_str(&std::fs::read_to_string(app.join("shared.json")).unwrap())
                .unwrap();
        assert_eq!(shared, json!({"layout":"trk","other":7}));
        install(app, opts(false)).unwrap();
        assert!(!app.join("backups/trk-layouts/trk.json.1").exists());
    }

    #[test]
    fn layout_bindings_wrap_effective_actions_and_uninstall_preserves_overrides() {
        let tmp = herdr_home();
        let app = tmp.path();
        let original = json!({
            "global": {"next":"tab.next", "select":"tab.select:2", "split":"pane.split:right", "close":"tab.close"},
            "layout:other": {"next":"personal.command"},
            "layout:trk": {"next":"personal.next", "local":{"command":"pane.go","args":{"direction":"left","extra":true}}}
        });
        write_keys(app, &original).unwrap();
        install(app, opts(false)).unwrap();
        let keys = read_keys(app).unwrap();
        assert_eq!(keys["global"], original["global"]);
        assert_eq!(keys["layout:other"], original["layout:other"]);
        assert_eq!(keys[KEY_SECTION]["next"], "personal.next");
        let dispatch: Value = serde_json::from_str(
            keys[KEY_SECTION]["select"]
                .as_str()
                .unwrap()
                .strip_prefix(DISPATCH)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(dispatch, json!({"command":"tab.select","args":{"index":2}}));
        let mut edited = keys;
        edited[KEY_SECTION]["close"] = json!("personal.close");
        write_keys(app, &edited).unwrap();
        uninstall(app, false).unwrap();
        let keys = read_keys(app).unwrap();
        assert_eq!(keys[KEY_SECTION]["close"], "personal.close");
        assert_eq!(keys[KEY_SECTION]["next"], "personal.next");
        assert!(keys[KEY_SECTION].get("select").is_none());
        assert_eq!(keys[KEY_SECTION]["local"], original[KEY_SECTION]["local"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_mods_are_never_overwritten_or_removed() {
        let tmp = herdr_home();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("main.js"), "development").unwrap();
        std::os::unix::fs::symlink(source.path(), tmp.path().join("mods/trk-tabs")).unwrap();
        install(tmp.path(), opts(false)).unwrap();
        assert_eq!(state(tmp.path()), "linked");
        refresh(tmp.path()).unwrap();
        uninstall(tmp.path(), false).unwrap();
        assert!(tmp.path().join("mods/trk-tabs").is_symlink());
        assert!(same(&source.path().join("main.js"), "development"));
    }

    fn ui() -> Value {
        json!({"spaces": [
            {"id": "sp-a", "folder": "/src/a", "machine": null, "active": true, "tabs": [
                {"id": "tb-1", "terminals": [{"terminal": "t1", "pane": "p1"}]},
                {"id": "tb-2", "terminals": []}]},
            {"id": "sp-b", "folder": "/src/b", "machine": null, "active": false, "tabs": [
                {"id": "tb-3", "terminals": [{"terminal": "t3", "pane": "p3"}]}]},
            {"id": "sp-b2", "folder": "/src/b", "machine": null, "active": false, "tabs": []},
            {"id": "vps/sp-c", "folder": "/src/c", "machine": "vps", "active": false, "tabs": []},
        ]})
    }

    #[test]
    fn the_space_is_this_terminals_then_the_projects() {
        let ui = ui();
        // This terminal's space wins over the project's folder.
        let t = pick_space(&ui, None, Some("t3"), Path::new("/src/a")).unwrap();
        assert_eq!(t.space, "sp-b");
        assert!(!t.active);
        assert_eq!(t.own, Some(("tb-3".into(), "p3".into())));
        // Outside Herdr: the only space in the project.
        let t = pick_space(&ui, None, None, Path::new("/src/a")).unwrap();
        assert_eq!((t.space.as_str(), t.active, t.own), ("sp-a", true, None));
        // A terminal Herdr does not show falls back the same way.
        let t = pick_space(&ui, None, Some("t9"), Path::new("/src/a")).unwrap();
        assert_eq!(t.space, "sp-a");
        // Ambiguous or absent: say so rather than guess.
        assert!(pick_space(&ui, None, None, Path::new("/src/b")).is_err());
        assert!(pick_space(&ui, None, None, Path::new("/src/zzz")).is_err());
        // --space names it; this terminal's pane counts only when it is there.
        let t = pick_space(&ui, Some("sp-b2"), Some("t1"), Path::new("/src/a")).unwrap();
        assert_eq!((t.space.as_str(), t.own), ("sp-b2", None));
        assert!(pick_space(&ui, Some("nope"), None, Path::new("/src/a")).is_err());
        assert!(pick_space(&ui, Some("vps/sp-c"), None, Path::new("/src/c")).is_err());
    }
}
