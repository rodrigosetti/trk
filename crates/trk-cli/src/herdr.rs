//! Launch an agent directly in its own Herdr space.

use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Placement {
    Visible {
        space: String,
        tab: String,
        pane: String,
    },
    Failed {
        reason: String,
    },
}

impl Placement {
    pub fn description(&self) -> String {
        match self {
            Self::Visible { space, .. } => format!("Herdr space {space}"),
            Self::Failed { reason } => {
                format!("Herdr placement failed: {reason}; session and claim retained")
            }
        }
    }

    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Failed { .. } => Some(self.description()),
            _ => None,
        }
    }
}

fn command(program: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} {}: {} {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim(),
        ));
    }
    Ok(out.stdout)
}

fn json_command(program: &str, args: &[&str]) -> Result<Value, String> {
    serde_json::from_slice(&command(program, args)?)
        .map_err(|e| format!("invalid JSON from {program}: {e}"))
}

// A direct pane in the newly created space is the visibility proof.
fn visible_pane<'a>(
    ui: &'a Value,
    space: &str,
    cwd: &str,
    title: &str,
    terminal: &str,
) -> Option<(&'a str, &'a str)> {
    let space = ui["spaces"].as_array()?.iter().find(|s| s["id"] == space)?;
    if space["folder"] != cwd || space["name"] != title {
        return None;
    }
    for tab in space["tabs"].as_array()? {
        for pane in tab["terminals"].as_array()? {
            if pane["terminal"] == terminal {
                return Some((tab["id"].as_str()?, pane["pane"].as_str()?));
            }
        }
    }
    None
}

pub fn available() -> Result<(), String> {
    json_command("herdr-app", &["ls"]).map(|_| ())
}

/// Find the space containing the exact terminal address, not a name or pane ID.
fn terminal_space<'a>(ui: &'a Value, terminal: &str) -> Option<&'a Value> {
    ui["spaces"].as_array()?.iter().find(|space| {
        space["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|tab| tab["terminals"].as_array().into_iter().flatten())
            .any(|pane| pane["terminal"] == terminal)
    })
}

pub fn has_terminal(terminal: &str) -> Result<bool, String> {
    let ui = json_command("herdr-app", &["ls"])?;
    Ok(terminal_space(&ui, terminal).is_some())
}

/// Mark the current visible space name without replacing a user's rename.
pub fn mark_closed(terminal: &str) -> Result<(), String> {
    let ui = json_command("herdr-app", &["ls"])?;
    let Some(space) = terminal_space(&ui, terminal) else {
        return Ok(());
    };
    let id = space["id"].as_str().ok_or("space omitted its ID")?;
    let name = space["name"].as_str().ok_or("space omitted its name")?;
    let title = name.trim_end_matches(" [closed]");
    let prefixed;
    let closed_name = if title.starts_with("[closed] ") {
        title
    } else {
        prefixed = format!("[closed] {title}");
        &prefixed
    };
    if closed_name != name {
        command(
            "herdr-app",
            &[
                "do",
                "space.rename",
                "--id",
                id,
                "--name",
                closed_name,
            ],
        )?;
    }
    Ok(())
}

/// Never retry a space creation: an uncertain reply may already have started it.
pub fn start(title: &str, cwd: &str, shell_command: &str) -> Result<(String, Placement), String> {
    let argv = serde_json::to_string(&["sh", "-c", shell_command]).map_err(|e| e.to_string())?;
    let created = json_command(
        "herdr-app",
        &[
            "do",
            "space.new",
            "--name",
            title,
            "--folder",
            cwd,
            "--argv",
            &argv,
        ],
    )?;
    let terminal = created["terminal"]
        .as_str()
        .ok_or("space.new omitted the terminal address; session and claim retained")?;
    let space = created["space"]
        .as_str()
        .ok_or("space.new omitted the space ID; session and claim retained")?;
    let observe = || -> Result<Placement, String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let ui = json_command("herdr-app", &["ls"])?;
            if let Some((tab, pane)) = visible_pane(&ui, space, cwd, title, terminal) {
                return Ok(Placement::Visible {
                    space: space.into(),
                    tab: tab.into(),
                    pane: pane.into(),
                });
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "no pane for {terminal} observed in new space {space} with the issue title and worktree within 2s; placement is unconfirmed"
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let placement = observe().unwrap_or_else(|reason| Placement::Failed { reason });
    Ok((terminal.into(), placement))
}

#[cfg(test)]
mod tests {
    use super::{terminal_space, visible_pane};
    use serde_json::json;

    #[test]
    fn herdr_visibility_requires_the_new_space_title_folder_and_direct_pane() {
        let mut ui = json!({"spaces": [
            {"id": "old", "name": "Issue title", "folder": "/wt", "tabs": [
                {"id": "old-tab", "terminals": [{"terminal": "agent", "pane": "p1"}]}
            ]},
            {"id": "new", "name": "Issue title", "folder": "/wt", "tabs": [
                {"id": "tab", "terminals": [{"terminal": "shell", "pane": "p2",
                    "sub_agents": [{"terminal": "agent"}]}]}
            ]}
        ]});
        assert_eq!(
            visible_pane(&ui, "new", "/wt", "Issue title", "agent"),
            None
        );
        ui["spaces"][1]["tabs"][0]["terminals"] = json!([{"terminal": "agent", "pane": "p3"}]);
        assert_eq!(
            visible_pane(&ui, "new", "/wt", "Issue title", "agent"),
            Some(("tab", "p3"))
        );
        assert_eq!(
            visible_pane(&ui, "new", "/other", "Issue title", "agent"),
            None
        );
        assert_eq!(
            visible_pane(&ui, "new", "/wt", "Other title", "agent"),
            None
        );
    }
    #[test]
    fn terminal_lookup_requires_the_exact_live_address() {
        let ui = json!({"spaces": [{"tabs": [{"terminals": [
            {"terminal": "herdr://space-a/pane-1", "pane": "pane-1"}
        ]}]}]});
        assert!(terminal_space(&ui, "herdr://space-a/pane-1").is_some());
        assert!(terminal_space(&ui, "herdr://space-b/pane-1").is_none());
        assert!(terminal_space(&ui, "pane-1").is_none());
    }
}
