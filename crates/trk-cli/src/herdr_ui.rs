//! `trk herdr`: the dashboard as a panel in the Herdr app.
//!
//! Two steps, because that is how Herdr works. The mod is installed once per
//! machine (`trk herdr install`): its files go in Herdr's folder and are
//! embedded in this binary, so the mod always matches the `trk --json` it
//! reads. A space shows it once a pane or tab there holds it
//! (`trk herdr open`); Herdr keeps that pane with its tab, so the panel
//! stays in the space until it is closed.
//!
//! The mod folder (`mods/trk/`) is trk's and is rewritten on install. The
//! layouts are opt-in and the person's once written.

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

/// The layout's files, relative to `layouts/`; the layout itself first.
const LAYOUT_FILES: &[(&str, &str)] = &[
    ("trk.json", include_str!("../../../herdr/layouts/trk.json")),
    ("trk.svg", include_str!("../../../herdr/layouts/trk.svg")),
    (
        "main-trk.json",
        include_str!("../../../herdr/layouts/main-trk.json"),
    ),
];

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
    let dir = app.join("mods").join(MOD);
    if !app.join("mods").is_dir() {
        "absent"
    } else if dir.is_symlink() {
        "linked"
    } else if !dir.join("main.js").exists() {
        "missing"
    } else if MOD_FILES.iter().all(|(rel, c)| same(&dir.join(rel), c)) {
        "current"
    } else {
        "outdated"
    }
}

/// Write the mod's files; what happened, in a line.
fn write_mod(app: &Path) -> Result<String> {
    let dir = app.join("mods").join(MOD);
    let before = state(app);
    if before == "linked" {
        return Ok(format!(
            "{} is a symlink (a development copy?); left alone",
            dir.display()
        ));
    }
    std::fs::create_dir_all(&dir)?;
    // Only what changed: Herdr reloads the mod on every save, and keeps
    // files of its own in the folder.
    for (rel, content) in MOD_FILES {
        let p = dir.join(rel);
        if !same(&p, content) {
            std::fs::write(&p, content)?;
        }
    }
    Ok(match before {
        "current" => format!("mod already current at {}", dir.display()),
        "missing" => format!("mod installed at {}", dir.display()),
        _ => format!("mod updated at {}", dir.display()),
    })
}

pub struct InstallOpts {
    /// Also write the split-view and sidebar-launcher layouts.
    pub layout: bool,
    /// Overwrite a layout that differs.
    pub force: bool,
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
    let mut layout_state = "skipped";
    if opts.layout {
        std::fs::create_dir_all(app.join("layouts"))?;
        layout_state = "written";
        for (rel, content) in LAYOUT_FILES {
            let path = app.join("layouts").join(rel);
            if path.exists() && !opts.force && !same(&path, content) {
                lines.push(format!(
                    "layout kept at {} (differs from this trk's; --force to overwrite)",
                    path.display()
                ));
                layout_state = "kept";
            } else {
                std::fs::write(&path, content)?;
            }
        }
        lines.push(
            "layouts available: herdr-app do layout.switch --name main-trk (sidebar button), or --name trk (split view)"
                .into(),
        );
    }
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
        _ => write_mod(app)?,
    };
    Ok(Some((line, json!({"mod_state": state(app)}))))
}

/// `trk herdr uninstall`.
pub fn uninstall(app: &Path, force: bool) -> Result<Out> {
    let dir = app.join("mods").join(MOD);
    let mut lines = vec![];
    let removed = if dir.is_symlink() {
        lines.push(format!(
            "{} is a symlink (a development copy?); left alone",
            dir.display()
        ));
        false
    } else if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
        lines.push(format!("mod removed from {}", dir.display()));
        true
    } else {
        lines.push("mod was not installed".into());
        false
    };
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

    fn opts(layout: bool, force: bool, if_installed: bool) -> InstallOpts {
        InstallOpts {
            layout,
            force,
            if_installed,
        }
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

    #[test]
    fn nothing_is_written_without_herdr() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(state(tmp.path()), "absent");
        assert!(install(tmp.path(), opts(true, false, false)).is_err());
        assert!(refresh(tmp.path()).unwrap().is_none());
        let v = data(install(tmp.path(), opts(false, false, true)).unwrap());
        assert_eq!(v["changed"], false);
        assert!(!tmp.path().join("mods").exists());
    }

    #[test]
    fn the_mod_is_installed_on_request_and_only_refreshed_otherwise() {
        let tmp = herdr_home();
        let app = tmp.path();

        // `trk install` and `--if-installed` leave a Herdr without the mod alone.
        let (hint, _) = refresh(app).unwrap().unwrap();
        assert!(hint.contains("trk herdr open"), "{hint}");
        data(install(app, opts(false, false, true)).unwrap());
        assert_eq!(state(app), "missing");

        let v = data(install(app, opts(false, false, false)).unwrap());
        assert_eq!(v["mod_state"], "current");
        assert_eq!(v["layout_state"], "skipped");
        assert!(!app.join("layouts/trk.json").exists());

        // Herdr's own files in the mod folder survive a refresh.
        std::fs::write(app.join("mods/trk/jsconfig.json"), "{}").unwrap();
        std::fs::write(app.join("mods/trk/main.js"), "stale").unwrap();
        assert_eq!(state(app), "outdated");
        let (line, _) = refresh(app).unwrap().unwrap();
        assert!(line.contains("updated"), "{line}");
        assert_eq!(state(app), "current");
        assert!(app.join("mods/trk/jsconfig.json").exists());
    }

    #[test]
    fn the_layout_is_opt_in_and_an_edited_one_is_kept() {
        let tmp = herdr_home();
        let app = tmp.path();
        let layout = app.join("layouts/trk.json");

        let v = data(install(app, opts(true, false, false)).unwrap());
        assert_eq!(v["layout_state"], "written");
        std::fs::write(&layout, "{\"mine\": true}").unwrap();
        let v = data(install(app, opts(true, false, false)).unwrap());
        assert_eq!(v["layout_state"], "kept");
        assert!(same(&layout, "{\"mine\": true}"));

        // Uninstall keeps the edited layout too, unless forced.
        let v = data(uninstall(app, false).unwrap());
        assert_eq!(v["removed"], true);
        assert_eq!(v["layout_state"], "kept");
        assert!(!app.join("mods/trk").exists());
        let v = data(uninstall(app, true).unwrap());
        assert_eq!(v["removed"], false);
        assert_eq!(v["layout_state"], "removed");
        assert!(!layout.exists() && !app.join("layouts/trk.svg").exists());
    }

    #[test]
    fn an_edited_sidebar_layout_survives_install_and_uninstall() {
        let tmp = herdr_home();
        let app = tmp.path();
        let sidebar = app.join("layouts/main-trk.json");
        data(install(app, opts(true, false, false)).unwrap());
        assert!(sidebar.exists());
        std::fs::write(&sidebar, "{\"mine\": true}").unwrap();
        let v = data(install(app, opts(true, false, false)).unwrap());
        assert_eq!(v["layout_state"], "kept");
        assert!(same(&sidebar, "{\"mine\": true}"));
        let v = data(uninstall(app, false).unwrap());
        assert_eq!(v["layout_state"], "kept");
        assert!(same(&sidebar, "{\"mine\": true}"));
        data(uninstall(app, true).unwrap());
        assert!(!sidebar.exists());
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
