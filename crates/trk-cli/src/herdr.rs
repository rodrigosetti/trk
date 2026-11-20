//! Optional UI placement for the hc launcher. Launch success is not visibility.

use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Placement {
    Headless {
        reason: String,
    },
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
            Self::Headless { reason } => format!("headless: {reason}"),
            Self::Visible { space, tab, .. } => format!("Herdr tab {tab} in space {space}"),
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

// Only a direct pane in a different tab counts. An agent list/sub-agent entry
// or a pane in another space does not prove the requested placement happened.
fn visible_pane<'a>(
    ui: &'a Value,
    space: &str,
    parent_tab: Option<&str>,
    terminal: &str,
) -> Option<(&'a str, &'a str)> {
    let space = ui["spaces"].as_array()?.iter().find(|s| s["id"] == space)?;
    for tab in space["tabs"].as_array()? {
        let id = tab["id"].as_str()?;
        if Some(id) == parent_tab {
            continue;
        }
        for pane in tab["terminals"].as_array()? {
            if pane["terminal"] == terminal {
                return Some((id, pane["pane"].as_str()?));
            }
        }
    }
    None
}

fn place_in_ui(terminal: &str, parent: &str) -> Result<Placement, String> {
    let parent = json_command("hc", &["get", parent, "--json"])?;
    let Some(space) = parent["labels"]["herdr.space"]
        .as_str()
        .filter(|s| !s.is_empty())
    else {
        return Ok(Placement::Headless {
            reason: "launching terminal has no herdr.space label".into(),
        });
    };
    let parent_tab = parent["labels"]["herdr.tab"].as_str();
    // Do not attach a stale label to a session when its space/window is gone.
    let ui = json_command("herdr-app", &["ls"])?;
    if !ui["spaces"]
        .as_array()
        .is_some_and(|spaces| spaces.iter().any(|s| s["id"] == space))
    {
        return Err(format!(
            "launching space {space} is not in the open Herdr window"
        ));
    }
    // Never copy herdr.tab: omitting it asks Herdr for a new tab. Labeling
    // after start keeps the configurable start argv independent of placement.
    command("hc", &["label", terminal, &format!("herdr.space={space}")])?;
    let address = json_command("hc", &["get", terminal, "--json"])?;
    let address = address["address"]
        .as_str()
        .ok_or("hc get omitted the terminal address")?;
    // The window consumes label events asynchronously. Bound observation, not
    // launch retries: no failure here may start a second session or unclaim it.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let ui = json_command("herdr-app", &["ls"])?;
        if let Some((tab, pane)) = visible_pane(&ui, space, parent_tab, address) {
            return Ok(Placement::Visible {
                space: space.into(),
                tab: tab.into(),
                pane: pane.into(),
            });
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no new tab for {address} observed in space {space} within 2s; placement is unconfirmed"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn place(terminal: &str) -> Placement {
    let Some(parent) = std::env::var("HC_TERMINAL_ID")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return Placement::Headless {
            reason: "not launched from an hc terminal".into(),
        };
    };
    place_in_ui(terminal, &parent).unwrap_or_else(|reason| Placement::Failed { reason })
}

#[cfg(test)]
mod tests {
    use super::visible_pane;
    use serde_json::json;

    #[test]
    fn herdr_visibility_requires_a_new_tab_in_the_launchers_space() {
        let ui = json!({"spaces": [
            {"id": "other", "tabs": [{"id": "wrong-space", "terminals": [
                {"terminal": "agent", "pane": "p1"}
            ]}]},
            {"id": "target", "tabs": [
                {"id": "original", "terminals": [{"terminal": "agent", "pane": "p2"}]},
                {"id": "new", "terminals": [{"terminal": "parent", "pane": "p3",
                    "sub_agents": [{"terminal": "agent"}]}]}
            ]}
        ]});
        assert_eq!(visible_pane(&ui, "target", Some("original"), "agent"), None);
        let mut placed = ui;
        placed["spaces"][1]["tabs"][1]["terminals"] = json!([
            {"terminal": "agent", "pane": "p4"}
        ]);
        assert_eq!(
            visible_pane(&placed, "target", Some("original"), "agent"),
            Some(("new", "p4"))
        );
    }
}
