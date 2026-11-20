//! The configurable schema, held in memory. Loaded from the registry tables at
//! the start of every transaction, or compiled from config files by
//! [`crate::config`]. Both paths produce the same [`Schema`], which is what
//! `schema plan` diffs.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Code, Error, Result};

/// Shared field names; the stored representation may be an issue column or a
/// value in the JSON `fields` object.
pub const SHARED_COLUMNS: &[&str] = &[
    "title", "context", "outcome", "owner", "links", "tags", "priority",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Open,
    Active,
    Closed,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Open => "open",
            Category::Active => "active",
            Category::Closed => "closed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Category::Open),
            "active" => Some(Category::Active),
            "closed" => Some(Category::Closed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Markdown,
    Int,
    Bool,
    Date,
    Datetime,
    Url,
    Enum,
    Actor,
    Repo,
    Link,
    List,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Markdown => "markdown",
            FieldKind::Int => "int",
            FieldKind::Bool => "bool",
            FieldKind::Date => "date",
            FieldKind::Datetime => "datetime",
            FieldKind::Url => "url",
            FieldKind::Enum => "enum",
            FieldKind::Actor => "actor",
            FieldKind::Repo => "repo",
            FieldKind::Link => "link",
            FieldKind::List => "list",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.to_string())).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldDef {
    pub name: String,
    pub kind: FieldKind,
    /// enum: {"values":[...]}; list: {"item":"url"}; int: {"min":0}
    pub config: Value,
    pub shared: bool,
    pub description: Option<String>,
}

impl FieldDef {
    pub fn enum_values(&self) -> Vec<String> {
        self.config
            .get("values")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn list_item(&self) -> FieldKind {
        self.config
            .get("item")
            .and_then(|v| v.as_str())
            .and_then(FieldKind::parse)
            .unwrap_or(FieldKind::Text)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusDef {
    pub name: String,
    pub category: Category,
    pub ord: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypeField {
    pub field: String,
    pub required_at_create: bool,
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReqKind {
    Field {
        field: String,
    },
    MinItems {
        field: String,
        n: i64,
    },
    OutEdge {
        dep_kind: String,
    },
    InEdgesClosed {
        dep_kind: String,
        src_resolution: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, PartialOrd, Ord)]
pub struct Requirement {
    pub to_status: String,
    pub resolution: Option<String>,
    #[serde(flatten)]
    pub kind: ReqKind,
}

impl Requirement {
    pub fn describe(&self) -> String {
        let when = match &self.resolution {
            Some(r) => format!("entering {} as {}", self.to_status, r),
            None => format!("entering {}", self.to_status),
        };
        let what = match &self.kind {
            ReqKind::Field { field } => format!("field {field}"),
            ReqKind::MinItems { field, n } => format!("at least {n} {field}"),
            ReqKind::OutEdge { dep_kind } => format!("an outgoing {dep_kind} edge"),
            ReqKind::InEdgesClosed {
                dep_kind,
                src_resolution,
            } => match src_resolution {
                Some(r) => format!("every incoming {dep_kind} source closed as {r}"),
                None => format!("every incoming {dep_kind} source closed"),
            },
        };
        format!("{when}: {what}")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolNaming {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypeDef {
    pub name: String,
    pub description: Option<String>,
    /// Ordered by `ord`.
    pub statuses: Vec<StatusDef>,
    /// from -> [to]
    pub transitions: BTreeMap<String, Vec<String>>,
    pub resolutions: Vec<String>,
    pub fields: Vec<TypeField>,
    pub requirements: Vec<Requirement>,
    /// routing.agent
    pub agent: Option<String>,
    /// target status -> generated tool naming
    pub tools: BTreeMap<String, ToolNaming>,
}

impl TypeDef {
    pub fn status(&self, name: &str) -> Option<&StatusDef> {
        self.statuses.iter().find(|s| s.name == name)
    }
    pub fn category_of(&self, status: &str) -> Option<Category> {
        self.status(status).map(|s| s.category)
    }
    pub fn open_status(&self) -> Option<&StatusDef> {
        self.statuses.iter().find(|s| s.category == Category::Open)
    }
    pub fn closed_status(&self) -> Option<&StatusDef> {
        self.statuses
            .iter()
            .find(|s| s.category == Category::Closed)
    }
    /// The status `claim` moves an issue into.
    pub fn first_active(&self) -> Option<&StatusDef> {
        self.statuses
            .iter()
            .find(|s| s.category == Category::Active)
    }
    pub fn claimable(&self) -> bool {
        self.first_active().is_some()
    }
    pub fn has_transition(&self, from: &str, to: &str) -> bool {
        self.transitions
            .get(from)
            .is_some_and(|t| t.iter().any(|x| x == to))
    }
    pub fn type_field(&self, name: &str) -> Option<&TypeField> {
        self.fields.iter().find(|f| f.field == name)
    }
    /// Requirements checked when entering `to_status` (with `resolution` when closing).
    pub fn requirements_for<'a>(
        &'a self,
        to_status: &'a str,
        resolution: Option<&'a str>,
    ) -> impl Iterator<Item = &'a Requirement> + 'a {
        self.requirements.iter().filter(move |r| {
            r.to_status == to_status
                && match (&r.resolution, resolution) {
                    (None, _) => true,
                    (Some(a), Some(b)) => a == b,
                    (Some(_), None) => false,
                }
        })
    }
    /// Name of the generated tool that moves an issue into `status`.
    pub fn transition_tool_name(&self, status: &str) -> String {
        self.tools
            .get(status)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| format!("move_to_{status}"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DepKindDef {
    pub name: String,
    pub inverse_label: String,
    pub blocks_ready: bool,
    pub acyclic: bool,
    pub same_type: bool,
    pub description: Option<String>,
    /// Allowed (src_type, dst_type); '*' = any.
    pub pairs: Vec<(String, String)>,
}

impl DepKindDef {
    pub fn allows(&self, src_type: &str, dst_type: &str) -> bool {
        if self.same_type && src_type != dst_type {
            return false;
        }
        self.pairs
            .iter()
            .any(|(s, d)| (s == "*" || s == src_type) && (d == "*" || d == dst_type))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Schema {
    pub types: BTreeMap<String, TypeDef>,
    pub fields: BTreeMap<String, FieldDef>,
    pub dep_kinds: BTreeMap<String, DepKindDef>,
}

impl Schema {
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn ty(&self, name: &str) -> Result<&TypeDef> {
        self.types.get(name).ok_or_else(|| {
            Error::new(Code::NotFound, format!("unknown issue type '{name}'")).with_hint(format!(
                "known types: {}",
                self.types.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        })
    }

    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.get(name)
    }

    pub fn dep_kind(&self, name: &str) -> Result<&DepKindDef> {
        self.dep_kinds.get(name).ok_or_else(|| {
            Error::new(Code::NotFound, format!("unknown dependency kind '{name}'")).with_hint(
                format!(
                    "known kinds: {}",
                    self.dep_kinds
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })
    }

    /// Fails with a hint when no schema has been applied yet.
    pub fn require_applied(&self) -> Result<()> {
        if self.is_empty() {
            return Err(
                Error::new(Code::SchemaInvalid, "no schema has been applied").with_hint(
                    "run `trk init` to write the default config, then `trk schema apply`",
                ),
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Load from the registry tables
    // ------------------------------------------------------------------

    pub fn load(conn: &Connection) -> Result<Schema> {
        let mut schema = Schema::default();

        let mut st = conn.prepare("SELECT name, kind, config, shared, description FROM field")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (name, kind, config, shared, description) = row?;
            let kind = FieldKind::parse(&kind)
                .ok_or_else(|| Error::new(Code::DbCorrupt, format!("bad field kind {kind}")))?;
            schema.fields.insert(
                name.clone(),
                FieldDef {
                    name,
                    kind,
                    config: serde_json::from_str(&config)?,
                    shared,
                    description,
                },
            );
        }

        let mut st = conn.prepare("SELECT name, description, agent, tools FROM issue_type")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (name, description, agent, tools) = row?;
            schema.types.insert(
                name.clone(),
                TypeDef {
                    name,
                    description,
                    statuses: vec![],
                    transitions: BTreeMap::new(),
                    resolutions: vec![],
                    fields: vec![],
                    requirements: vec![],
                    agent,
                    tools: serde_json::from_str(&tools)?,
                },
            );
        }

        let mut st =
            conn.prepare("SELECT type, name, category, ord FROM status ORDER BY type, ord, name")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (ty, name, cat, ord) = row?;
            if let Some(t) = schema.types.get_mut(&ty) {
                t.statuses.push(StatusDef {
                    name,
                    category: Category::parse(&cat).unwrap_or(Category::Open),
                    ord,
                });
            }
        }

        let mut st =
            conn.prepare("SELECT type, from_status, to_status FROM transition ORDER BY rowid")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (ty, from, to) = row?;
            if let Some(t) = schema.types.get_mut(&ty) {
                t.transitions.entry(from).or_default().push(to);
            }
        }

        let mut st = conn.prepare("SELECT type, name FROM resolution ORDER BY rowid")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (ty, name) = row?;
            if let Some(t) = schema.types.get_mut(&ty) {
                t.resolutions.push(name);
            }
        }

        let mut st = conn.prepare(
            "SELECT type, field, required_at_create, default_value FROM type_field ORDER BY rowid",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (ty, field, req, def) = row?;
            if let Some(t) = schema.types.get_mut(&ty) {
                t.fields.push(TypeField {
                    field,
                    required_at_create: req,
                    default: def.map(|d| serde_json::from_str(&d)).transpose()?,
                });
            }
        }

        let mut st = conn.prepare(
            "SELECT type, to_status, resolution, kind, field, n, dep_kind, src_resolution FROM requirement ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?;
        for row in rows {
            let (ty, to_status, resolution, kind, field, n, dep_kind, src_res) = row?;
            let kind = match kind.as_str() {
                "field" => ReqKind::Field {
                    field: field.unwrap_or_default(),
                },
                "min_items" => ReqKind::MinItems {
                    field: field.unwrap_or_default(),
                    n: n.unwrap_or(1),
                },
                "out_edge" => ReqKind::OutEdge {
                    dep_kind: dep_kind.unwrap_or_default(),
                },
                _ => ReqKind::InEdgesClosed {
                    dep_kind: dep_kind.unwrap_or_default(),
                    src_resolution: src_res,
                },
            };
            if let Some(t) = schema.types.get_mut(&ty) {
                t.requirements.push(Requirement {
                    to_status,
                    resolution,
                    kind,
                });
            }
        }

        let mut st = conn.prepare(
            "SELECT name, inverse_label, blocks_ready, acyclic, same_type, description FROM dep_kind",
        )?;
        let rows = st.query_map([], |r| {
            Ok(DepKindDef {
                name: r.get(0)?,
                inverse_label: r.get(1)?,
                blocks_ready: r.get(2)?,
                acyclic: r.get(3)?,
                same_type: r.get(4)?,
                description: r.get(5)?,
                pairs: vec![],
            })
        })?;
        for row in rows {
            let k = row?;
            schema.dep_kinds.insert(k.name.clone(), k);
        }
        let mut st =
            conn.prepare("SELECT kind, src_type, dst_type FROM dep_kind_types ORDER BY rowid")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (kind, s, d) = row?;
            if let Some(k) = schema.dep_kinds.get_mut(&kind) {
                k.pairs.push((s, d));
            }
        }
        Ok(schema)
    }

    // ------------------------------------------------------------------
    // Write to the registry tables (used only by `schema apply`)
    // ------------------------------------------------------------------

    /// Replaces every registry row. Must run inside a transaction with
    /// `defer_foreign_keys = ON`, since issues reference statuses.
    pub(crate) fn write(&self, conn: &Connection) -> Result<()> {
        for t in [
            "requirement",
            "dep_kind_types",
            "dep_kind",
            "type_field",
            "field",
            "resolution",
            "transition",
            "status",
            "issue_type",
        ] {
            conn.execute(&format!("DELETE FROM {t}"), [])?;
        }
        for f in self.fields.values() {
            conn.execute(
                "INSERT INTO field (name, kind, config, shared, description) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![f.name, f.kind.as_str(), f.config.to_string(), f.shared, f.description],
            )?;
        }
        for t in self.types.values() {
            conn.execute(
                "INSERT INTO issue_type (name, description, agent, tools) VALUES (?1, ?2, ?3, ?4)",
                params![
                    t.name,
                    t.description,
                    t.agent,
                    serde_json::to_string(&t.tools)?
                ],
            )?;
            for s in &t.statuses {
                conn.execute(
                    "INSERT INTO status (type, name, category, ord) VALUES (?1, ?2, ?3, ?4)",
                    params![t.name, s.name, s.category.as_str(), s.ord],
                )?;
            }
            for (from, tos) in &t.transitions {
                for to in tos {
                    conn.execute(
                        "INSERT INTO transition (type, from_status, to_status) VALUES (?1, ?2, ?3)",
                        params![t.name, from, to],
                    )?;
                }
            }
            for r in &t.resolutions {
                conn.execute(
                    "INSERT INTO resolution (type, name) VALUES (?1, ?2)",
                    params![t.name, r],
                )?;
            }
            for f in &t.fields {
                conn.execute(
                    "INSERT INTO type_field (type, field, required_at_create, default_value) VALUES (?1, ?2, ?3, ?4)",
                    params![t.name, f.field, f.required_at_create, f.default.as_ref().map(|d| d.to_string())],
                )?;
            }
            for r in &t.requirements {
                let (kind, field, n, dep_kind, src_res): (
                    &str,
                    Option<&str>,
                    Option<i64>,
                    Option<&str>,
                    Option<&str>,
                ) = match &r.kind {
                    ReqKind::Field { field } => ("field", Some(field), None, None, None),
                    ReqKind::MinItems { field, n } => {
                        ("min_items", Some(field), Some(*n), None, None)
                    }
                    ReqKind::OutEdge { dep_kind } => ("out_edge", None, None, Some(dep_kind), None),
                    ReqKind::InEdgesClosed {
                        dep_kind,
                        src_resolution,
                    } => (
                        "in_edges_closed",
                        None,
                        None,
                        Some(dep_kind),
                        src_resolution.as_deref(),
                    ),
                };
                conn.execute(
                    "INSERT INTO requirement (type, to_status, resolution, kind, field, n, dep_kind, src_resolution)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![t.name, r.to_status, r.resolution, kind, field, n, dep_kind, src_res],
                )?;
            }
        }
        for k in self.dep_kinds.values() {
            conn.execute(
                "INSERT INTO dep_kind (name, inverse_label, blocks_ready, acyclic, same_type, description)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![k.name, k.inverse_label, k.blocks_ready, k.acyclic, k.same_type, k.description],
            )?;
            for (s, d) in &k.pairs {
                conn.execute(
                    "INSERT INTO dep_kind_types (kind, src_type, dst_type) VALUES (?1, ?2, ?3)",
                    params![k.name, s, d],
                )?;
            }
        }
        Ok(())
    }
}
