//! Explicit recovery of finished hc sessions predating named launcher ownership.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use trk_core::error::Result;
use trk_core::model::Claim;
use trk_core::store::Ctx;

use super::Resource;
use crate::env::Env;
use crate::herdr::{command, json_command};

pub(super) struct Recovery {
    authorized: bool,
    inventory: Option<std::result::Result<BTreeSet<String>, String>>,
}

impl Recovery {
    pub fn new(authorized: bool) -> Self {
        Self {
            authorized,
            inventory: None,
        }
    }
}

pub(super) fn recover(
    env: &Env,
    c: &mut Ctx<'_>,
    claim: &Claim,
    protected: &[String],
    dry_run: bool,
    recovery: &mut Recovery,
) -> Result<Resource> {
    let terminal = claim.terminal.as_deref().expect("filtered terminal");
    // Receipts are GC-only provenance, never a rewrite of historical claims or
    // permission to operate on a different claim/host/terminal after reopening.
    let recorded: bool = c.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM event WHERE issue_id = ?1 AND op = 'note'
         AND json_extract(payload, '$.gc_session.claim') = ?2
         AND json_extract(payload, '$.gc_session.terminal') = ?3
         AND json_extract(payload, '$.gc_session.host') = ?4
         AND json_extract(payload, '$.gc_session.backend') = 'hc')",
        (&claim.issue_id, claim.id, terminal, &claim.host),
        |row| row.get(0),
    )?;
    if !recovery.authorized && !recorded {
        return Ok(Resource::new(
            "skipped",
            "terminal has no named launcher profile; verify hc provenance before using gc ID --legacy-hc",
        ));
    }
    if std::env::var("HC_TERMINAL_ID").is_ok_and(|own| own == terminal) {
        return Ok(Resource::new("skipped", "terminal is the current terminal"));
    }
    if protected.iter().any(|other| other == terminal) {
        return Ok(Resource::new(
            "skipped",
            "terminal belongs to another issue's active or waiting session",
        ));
    }
    let addresses = match recovery.inventory.get_or_insert_with(inventory) {
        Ok(addresses) => addresses,
        Err(reason) => {
            return Ok(Resource::new("skipped", format!("legacy hc recovery: {reason}")));
        }
    };
    let resource = match collect(env, claim, terminal, dry_run, addresses) {
        Ok(resource) => resource,
        Err(reason) => Resource::new("skipped", format!("legacy hc recovery: {reason}")),
    };
    if !dry_run && resource.status == "removed" {
        addresses.remove(terminal);
    }
    if !dry_run && !recorded && matches!(resource.status, "removed" | "absent") {
        c.event(
            Some(&claim.issue_id),
            "note",
            json!({
                "body": format!("trk gc legacy hc recovery for claim {}: {}.", claim.id, resource.reason),
                "gc_session": {
                    "claim": claim.id,
                    "terminal": terminal,
                    "host": claim.host,
                    "backend": "hc",
                },
            }),
        )?;
    }
    Ok(resource)
}

fn inventory() -> std::result::Result<BTreeSet<String>, String> {
    let mut inventory = json_command("hc", &["ls", "--all", "--json"])?;
    let terminals = inventory["terminals"]
        .as_array_mut()
        .ok_or("hc inventory omitted terminals array")?;
    let mut addresses = BTreeSet::new();
    for entry in terminals {
        let Some(Value::String(address)) = entry.get_mut("address").map(Value::take) else {
            return Err("hc inventory omitted an exact terminal address".into());
        };
        if address.is_empty() || !addresses.insert(address) {
            return Err("hc inventory contains empty or duplicate terminal addresses".into());
        }
    }
    Ok(addresses)
}

fn collect(
    env: &Env,
    claim: &Claim,
    terminal: &str,
    dry_run: bool,
    addresses: &BTreeSet<String>,
) -> std::result::Result<Resource, String> {
    if !addresses.contains(terminal) {
        return Ok(Resource::new(
            "absent",
            "exact terminal is absent from hc inventory",
        ));
    }
    // A name/alias match is never identity evidence. Check the exact address,
    // original actor in the launch argv, and recorded working directory.
    let detail = json_command("hc", &["get", terminal, "--json"])?;
    if detail["address"] != terminal {
        return Err("hc returned a different terminal address".into());
    }
    if detail["state"] != "finished" {
        return Ok(Resource::new("skipped", "hc terminal is not finished"));
    }
    let cwd = claim
        .worktree
        .as_deref()
        .map(std::path::Path::new)
        .unwrap_or(&env.root);
    let cwd_matches = detail["cwd"].as_str().is_some_and(|path| {
        let path = std::path::Path::new(path);
        path == cwd
            || super::resolved(path)
                .ok()
                .zip(super::resolved(cwd).ok())
                .is_some_and(|(actual, recorded)| actual == recorded)
    });
    let actor = format!("TRK_ACTOR={}", claim.actor);
    let actor_matches = detail["argv"].as_array().is_some_and(|argv| {
        argv.iter()
            .filter_map(Value::as_str)
            .any(|arg| arg.split_ascii_whitespace().any(|word| word == actor))
    });
    if !cwd_matches || !actor_matches {
        return Ok(Resource::new(
            "skipped",
            "hc terminal does not match the recorded claim actor and working directory",
        ));
    }
    // hc rm refuses live terminals too; there is deliberately no kill/force path.
    if !dry_run {
        command("hc", &["rm", terminal, "--json"])?;
    }
    Ok(Resource::new(
        "removed",
        format!(
            "{} finished hc terminal {terminal}",
            if dry_run { "would remove" } else { "removed" }
        ),
    ))
}
