//! Explicit, curated issue groupings.

use rusqlite::{OptionalExtension, params};
use serde_json::json;

use crate::error::{Code, Error, Result};
use crate::model::Epic;
use crate::store::Ctx;

const EPIC_SELECT: &str = "SELECT e.name, e.description, e.created_by, e.created_at, e.archived_at,
    (SELECT count(*) FROM issue i WHERE i.epic = e.name) FROM epic e";

fn row_to_epic(row: &rusqlite::Row<'_>) -> rusqlite::Result<Epic> {
    Ok(Epic {
        name: row.get(0)?,
        description: row.get(1)?,
        created_by: row.get(2)?,
        created_at: row.get(3)?,
        archived_at: row.get(4)?,
        issue_count: row.get(5)?,
    })
}

fn check_name(name: &str) -> Result<()> {
    let mut bytes = name.bytes();
    if !bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        || !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(
            Error::new(Code::InvalidField, format!("invalid epic name '{name}'")).with_hint(
                "use a lowercase letter followed by lowercase letters, digits, or hyphens",
            ),
        );
    }
    Ok(())
}

impl Ctx<'_> {
    fn epic_named(&self, name: &str) -> Result<Option<Epic>> {
        Ok(self
            .conn
            .query_row(
                &format!("{EPIC_SELECT} WHERE e.name = ?1"),
                [name],
                row_to_epic,
            )
            .optional()?)
    }

    fn require_epic(&self, name: &str) -> Result<Epic> {
        self.epic_named(name)?.ok_or_else(|| {
            Error::new(Code::InvalidField, format!("unknown epic '{name}'"))
                .with_hint("choose an existing epic")
        })
    }

    pub(crate) fn check_epic_assignment(&self, name: &str) -> Result<()> {
        let active: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM epic WHERE name = ?1 AND archived_at IS NULL)",
            [name],
            |row| row.get(0),
        )?;
        if active {
            return Ok(());
        }
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM epic WHERE archived_at IS NULL ORDER BY name")?;
        let allowed = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Err(Error::new(
            Code::InvalidField,
            format!("unknown or archived epic '{name}'"),
        )
        .with_detail("allowed", json!(allowed))
        .with_hint("choose an active epic from allowed, or omit epic if none fits"))
    }

    /// Create a curated epic. No issue claim is required, including for agents.
    pub fn create_epic(&self, name: &str, description: &str) -> Result<Epic> {
        check_name(name)?;
        if self.epic_named(name)?.is_some() {
            return Err(Error::new(
                Code::InvalidField,
                format!("epic '{name}' already exists"),
            ));
        }
        self.conn.execute(
            "INSERT INTO epic (name, description, created_by, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, description, self.actor_str(), self.ts()],
        )?;
        self.event(
            None,
            "epic_create",
            json!({"name": name, "description": description}),
        )?;
        self.require_epic(name)
    }

    /// List epics in name order, counting members in every issue state.
    pub fn epics(&self, include_archived: bool) -> Result<Vec<Epic>> {
        let mut stmt = self.conn.prepare(&format!(
            "{EPIC_SELECT} WHERE ?1 OR e.archived_at IS NULL ORDER BY e.name"
        ))?;
        Ok(stmt
            .query_map([include_archived], row_to_epic)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Rename the epic; its foreign key cascades membership without per-issue edits.
    pub fn rename_epic(&self, old: &str, new: &str) -> Result<Epic> {
        check_name(new)?;
        let epic = self.require_epic(old)?;
        if old == new {
            return Ok(epic);
        }
        if self.epic_named(new)?.is_some() {
            return Err(Error::new(
                Code::InvalidField,
                format!("epic '{new}' already exists"),
            ));
        }
        self.conn.execute(
            "UPDATE epic SET name = ?2 WHERE name = ?1",
            params![old, new],
        )?;
        self.event(None, "epic_rename", json!({"from": old, "to": new}))?;
        self.require_epic(new)
    }

    /// Archive without removing existing members. Repeated archival is a no-op.
    pub fn archive_epic(&self, name: &str) -> Result<Epic> {
        let epic = self.require_epic(name)?;
        if epic.archived_at.is_some() {
            return Ok(epic);
        }
        self.conn.execute(
            "UPDATE epic SET archived_at = ?2 WHERE name = ?1",
            params![name, self.ts()],
        )?;
        self.event(None, "epic_archive", json!({"name": name}))?;
        self.require_epic(name)
    }
}
