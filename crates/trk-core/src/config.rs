//! The config directory: TOML files compiled into a [`Schema`], plus the user
//! settings in `config.toml`.
//!
//! ```text
//! config.toml      user settings, launcher templates, harness profiles, views
//! fields.toml      field definitions
//! dep-kinds.toml   dependency kinds
//! types/*.toml     one file per issue type
//! agents/*.toml    one file per agent (parsed by trk-agent)
//! prompts/*.j2     templates (rendered by trk-agent)
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Code, Error, Result};
use crate::registry::*;

#[derive(Debug, Clone)]
pub struct ConfigDir {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
}

impl Diagnostic {
    pub fn error(file: Option<&str>, message: impl Into<String>) -> Self {
        Diagnostic {
            level: Level::Error,
            file: file.map(String::from),
            message: message.into(),
        }
    }
    pub fn warning(file: Option<&str>, message: impl Into<String>) -> Self {
        Diagnostic {
            level: Level::Warning,
            file: file.map(String::from),
            message: message.into(),
        }
    }
}

// ----------------------------------------------------------------------
// User config (config.toml)
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct UserConfig {
    #[serde(default)]
    pub user: UserSection,
    #[serde(default)]
    pub claims: ClaimsSection,
    #[serde(default)]
    pub run: RunSection,
    /// Launcher templates; parsed by trk-cli.
    #[serde(default)]
    pub launcher: Option<toml::Value>,
    /// Harness profiles keyed by harness name; parsed by trk-agent.
    #[serde(default)]
    pub harness: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub views: BTreeMap<String, View>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct UserSection {
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaimsSection {
    /// Default lease for agent claims, e.g. "30m".
    #[serde(default = "default_lease")]
    pub agent_lease: String,
}

impl Default for ClaimsSection {
    fn default() -> Self {
        ClaimsSection {
            agent_lease: default_lease(),
        }
    }
}

fn default_lease() -> String {
    "30m".into()
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RunSection {
    /// Where `trk run` creates worktrees; default `<data dir>/worktrees`.
    pub worktrees: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct View {
    pub query: String,
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

// ----------------------------------------------------------------------
// File formats
// ----------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldFile {
    kind: String,
    #[serde(default)]
    shared: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    values: Option<Vec<String>>,
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    min: Option<i64>,
    #[serde(default)]
    max: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DepKindFile {
    inverse: String,
    #[serde(default)]
    blocks_ready: bool,
    #[serde(default = "yes")]
    acyclic: bool,
    #[serde(default)]
    same_type: bool,
    #[serde(default)]
    description: Option<String>,
    allow: Vec<(String, String)>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeFile {
    #[serde(rename = "type")]
    ty: TypeSection,
    /// Kept as a table so the file order is the display order.
    #[serde(default)]
    fields: toml::Table,
    #[serde(default)]
    requires: Vec<RequiresFile>,
    #[serde(default)]
    routing: Option<RoutingFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeSection {
    name: String,
    #[serde(default)]
    description: Option<String>,
    statuses: Vec<StatusFile>,
    resolutions: Vec<String>,
    #[serde(default)]
    transitions: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    tools: BTreeMap<String, ToolNaming>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusFile {
    name: String,
    category: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TypeFieldFile {
    #[serde(default)]
    required_at_create: bool,
    #[serde(default)]
    default: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiresFile {
    entering: String,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    fields: Vec<String>,
    #[serde(default)]
    min_items: Option<MinItemsFile>,
    #[serde(default)]
    out_edge: Option<String>,
    #[serde(default)]
    in_edges_closed: Option<InEdgesFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MinItemsFile {
    field: String,
    n: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InEdgesFile {
    kind: String,
    #[serde(default)]
    src_resolution: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingFile {
    #[serde(default)]
    agent: Option<String>,
}

pub fn toml_to_json(v: &toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            Code::NotFound,
            format!("cannot read {}: {e}", path.display()),
        )
        .with_hint("run `trk init` to write the default config")
    })
}

fn parse_toml<T: serde::de::DeserializeOwned>(path: &Path, text: &str) -> Result<T> {
    toml::from_str(text)
        .map_err(|e| Error::new(Code::SchemaInvalid, format!("{}: {e}", path.display())))
}

impl ConfigDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        ConfigDir { path: path.into() }
    }

    pub fn exists(&self) -> bool {
        self.path.join("types").is_dir()
    }

    pub fn user_config(&self) -> Result<UserConfig> {
        let p = self.path.join("config.toml");
        if !p.exists() {
            return Ok(UserConfig::default());
        }
        parse_toml(&p, &read(&p)?)
    }

    /// Files in a subdirectory with the given extension, sorted.
    pub fn files_in(&self, sub: &str, ext: &str) -> Vec<PathBuf> {
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(self.path.join(sub)) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some(ext) {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    /// The git commit of the config directory, with `+dirty` when it has
    /// uncommitted changes. `None` when it is not a git repository.
    pub fn git_commit(&self) -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let mut commit = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let dirty = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .is_some_and(|o| !o.stdout.is_empty());
        if dirty {
            commit.push_str("+dirty");
        }
        Some(commit)
    }

    /// Compile fields.toml, dep-kinds.toml and types/*.toml into a schema.
    /// Structural problems that prevent building the schema are errors; the
    /// rest are reported by [`check_schema`].
    pub fn compile(&self) -> Result<Schema> {
        let mut schema = Schema::default();

        let fp = self.path.join("fields.toml");
        let fields: BTreeMap<String, FieldFile> = parse_toml(&fp, &read(&fp)?)?;
        for (name, f) in fields {
            let kind = FieldKind::parse(&f.kind).ok_or_else(|| {
                Error::new(
                    Code::SchemaInvalid,
                    format!("fields.toml: field '{name}' has unknown kind '{}'", f.kind),
                )
            })?;
            let mut config = serde_json::Map::new();
            if let Some(v) = f.values {
                config.insert("values".into(), serde_json::json!(v));
            }
            if let Some(v) = f.item {
                config.insert("item".into(), serde_json::json!(v));
            }
            if let Some(v) = f.min {
                config.insert("min".into(), serde_json::json!(v));
            }
            if let Some(v) = f.max {
                config.insert("max".into(), serde_json::json!(v));
            }
            schema.fields.insert(
                name.clone(),
                FieldDef {
                    name,
                    kind,
                    config: Value::Object(config),
                    shared: f.shared,
                    description: f.description,
                },
            );
        }

        let dp = self.path.join("dep-kinds.toml");
        let kinds: BTreeMap<String, DepKindFile> = parse_toml(&dp, &read(&dp)?)?;
        for (name, k) in kinds {
            schema.dep_kinds.insert(
                name.clone(),
                DepKindDef {
                    name,
                    inverse_label: k.inverse,
                    blocks_ready: k.blocks_ready,
                    acyclic: k.acyclic,
                    same_type: k.same_type,
                    description: k.description,
                    pairs: k.allow,
                },
            );
        }

        for path in self.files_in("types", "toml") {
            let tf: TypeFile = parse_toml(&path, &read(&path)?)?;
            let fname = path.file_name().unwrap().to_string_lossy().to_string();
            let mut statuses = vec![];
            for (i, s) in tf.ty.statuses.iter().enumerate() {
                let category = Category::parse(&s.category).ok_or_else(|| {
                    Error::new(
                        Code::SchemaInvalid,
                        format!(
                            "{fname}: status '{}' has unknown category '{}'",
                            s.name, s.category
                        ),
                    )
                })?;
                statuses.push(StatusDef {
                    name: s.name.clone(),
                    category,
                    ord: i as i64,
                });
            }
            let mut fields = vec![];
            for (name, v) in &tf.fields {
                let f: TypeFieldFile = v.clone().try_into().map_err(|e| {
                    Error::new(
                        Code::SchemaInvalid,
                        format!("{fname}: [fields] {name}: {e}"),
                    )
                })?;
                fields.push(TypeField {
                    field: name.clone(),
                    required_at_create: f.required_at_create,
                    default: f.default.as_ref().map(toml_to_json),
                });
            }
            let mut requirements = vec![];
            for r in &tf.requires {
                let base = |kind| Requirement {
                    to_status: r.entering.clone(),
                    resolution: r.resolution.clone(),
                    kind,
                };
                for f in &r.fields {
                    requirements.push(base(ReqKind::Field { field: f.clone() }));
                }
                if let Some(m) = &r.min_items {
                    requirements.push(base(ReqKind::MinItems {
                        field: m.field.clone(),
                        n: m.n,
                    }));
                }
                if let Some(k) = &r.out_edge {
                    requirements.push(base(ReqKind::OutEdge {
                        dep_kind: k.clone(),
                    }));
                }
                if let Some(e) = &r.in_edges_closed {
                    requirements.push(base(ReqKind::InEdgesClosed {
                        dep_kind: e.kind.clone(),
                        src_resolution: e.src_resolution.clone(),
                    }));
                }
            }
            let name = tf.ty.name.clone();
            if schema.types.contains_key(&name) {
                return Err(Error::new(
                    Code::SchemaInvalid,
                    format!("{fname}: type '{name}' defined twice"),
                ));
            }
            schema.types.insert(
                name.clone(),
                TypeDef {
                    name,
                    description: tf.ty.description,
                    statuses,
                    transitions: tf.ty.transitions,
                    resolutions: tf.ty.resolutions,
                    fields,
                    requirements,
                    agent: tf.routing.and_then(|r| r.agent),
                    tools: tf.ty.tools,
                },
            );
        }
        Ok(schema)
    }
}

/// Consistency checks over a compiled schema (the trk-core half of
/// `trk schema check`; trk-agent adds agent and template checks).
pub fn check_schema(schema: &Schema) -> Vec<Diagnostic> {
    let mut d = vec![];
    let err = |d: &mut Vec<Diagnostic>, file: &str, msg: String| {
        d.push(Diagnostic::error(Some(file), msg))
    };

    // Shared fields are defined for every issue type and may use a common
    // issue column or the JSON fields object for storage.
    for col in SHARED_COLUMNS {
        match schema.fields.get(*col) {
            Some(f) if f.shared => {}
            Some(_) => err(
                &mut d,
                "fields.toml",
                format!("field '{col}' is shared and must be `shared = true`"),
            ),
            None => err(
                &mut d,
                "fields.toml",
                format!("shared field '{col}' is not defined"),
            ),
        }
    }
    for f in schema.fields.values() {
        if f.shared && !SHARED_COLUMNS.contains(&f.name.as_str()) {
            err(
                &mut d,
                "fields.toml",
                format!(
                    "field '{}' is marked shared but is not a shared field ({})",
                    f.name,
                    SHARED_COLUMNS.join(", ")
                ),
            );
        }
        if f.kind == FieldKind::Enum && f.enum_values().is_empty() {
            err(
                &mut d,
                "fields.toml",
                format!("enum field '{}' has no `values`", f.name),
            );
        }
        if f.kind == FieldKind::List
            && let Some(item) = f.config.get("item").and_then(|v| v.as_str())
            && (FieldKind::parse(item).is_none() || item == "list")
        {
            err(
                &mut d,
                "fields.toml",
                format!("list field '{}' has invalid item kind '{item}'", f.name),
            );
        }
        if !f
            .name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
            || !f
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            err(
                &mut d,
                "fields.toml",
                format!("field name '{}' must match [a-z][a-z0-9_]*", f.name),
            );
        }
    }

    for k in schema.dep_kinds.values() {
        for (s, t) in &k.pairs {
            for ty in [s, t] {
                if ty != "*" && !schema.types.contains_key(ty) {
                    err(
                        &mut d,
                        "dep-kinds.toml",
                        format!("dependency kind '{}' allows unknown type '{ty}'", k.name),
                    );
                }
            }
        }
        if k.pairs.is_empty() {
            d.push(Diagnostic::warning(
                Some("dep-kinds.toml"),
                format!("dependency kind '{}' allows no type pairs", k.name),
            ));
        }
    }

    for t in schema.types.values() {
        let file = format!("types/{}.toml", t.name);
        let file = file.as_str();
        let opens = t
            .statuses
            .iter()
            .filter(|s| s.category == Category::Open)
            .count();
        let closeds = t
            .statuses
            .iter()
            .filter(|s| s.category == Category::Closed)
            .count();
        if opens != 1 {
            err(
                &mut d,
                file,
                format!(
                    "type '{}' must have exactly one status with category 'open' (has {opens})",
                    t.name
                ),
            );
        }
        if closeds != 1 {
            err(
                &mut d,
                file,
                format!(
                    "type '{}' must have exactly one status with category 'closed' (has {closeds})",
                    t.name
                ),
            );
        }
        let names: BTreeSet<&str> = t.statuses.iter().map(|s| s.name.as_str()).collect();
        if names.len() != t.statuses.len() {
            err(
                &mut d,
                file,
                format!("type '{}' has duplicate status names", t.name),
            );
        }
        for (from, tos) in &t.transitions {
            if !names.contains(from.as_str()) {
                err(
                    &mut d,
                    file,
                    format!("transition from unknown status '{from}'"),
                );
            }
            for to in tos {
                if !names.contains(to.as_str()) {
                    err(&mut d, file, format!("transition to unknown status '{to}'"));
                }
                if t.category_of(to) == Some(Category::Open) {
                    d.push(Diagnostic::warning(
                        Some(file),
                        format!("transition {from} → {to}: returning to the open status is done by unclaim, the transition is ignored"),
                    ));
                }
            }
        }
        if t.resolutions.is_empty() {
            err(
                &mut d,
                file,
                format!("type '{}' has no resolutions", t.name),
            );
        }
        // Reachability: open --claim--> first active, then transitions.
        if let (Some(open), Some(closed)) = (t.open_status(), t.closed_status()) {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut stack = vec![open.name.clone()];
            if let Some(a) = t.first_active() {
                stack.push(a.name.clone());
            }
            while let Some(s) = stack.pop() {
                if !seen.insert(s.clone()) {
                    continue;
                }
                if let Some(tos) = t.transitions.get(&s) {
                    stack.extend(tos.iter().cloned());
                }
            }
            for s in &t.statuses {
                if !seen.contains(&s.name) {
                    err(&mut d, file, format!("status '{}' is unreachable", s.name));
                }
            }
            if !seen.contains(&closed.name) {
                err(
                    &mut d,
                    file,
                    format!("type '{}' has no path to '{}'", t.name, closed.name),
                );
            }
            // Every active status should be able to reach closed.
            for s in t.statuses.iter().filter(|s| s.category == Category::Active) {
                let mut seen2 = BTreeSet::new();
                let mut st = vec![s.name.clone()];
                while let Some(x) = st.pop() {
                    if !seen2.insert(x.clone()) {
                        continue;
                    }
                    if let Some(tos) = t.transitions.get(&x) {
                        st.extend(tos.iter().cloned());
                    }
                }
                if !seen2.contains(&closed.name) {
                    err(
                        &mut d,
                        file,
                        format!("status '{}' has no path to '{}'", s.name, closed.name),
                    );
                }
            }
        }
        for tf in &t.fields {
            match schema.fields.get(&tf.field) {
                None => err(
                    &mut d,
                    file,
                    format!("field '{}' is not defined in fields.toml", tf.field),
                ),
                Some(f) if f.shared => err(
                    &mut d,
                    file,
                    format!(
                        "field '{}' is shared; it applies to every type and cannot be listed",
                        tf.field
                    ),
                ),
                Some(_) => {}
            }
        }
        for r in &t.requirements {
            if !names.contains(r.to_status.as_str()) {
                err(
                    &mut d,
                    file,
                    format!("requirement enters unknown status '{}'", r.to_status),
                );
            }
            if let Some(res) = &r.resolution {
                if !t.resolutions.contains(res) {
                    err(
                        &mut d,
                        file,
                        format!("requirement names unknown resolution '{res}'"),
                    );
                }
                if t.category_of(&r.to_status) != Some(Category::Closed) {
                    err(
                        &mut d,
                        file,
                        format!(
                            "requirement with a resolution must enter the closed status, not '{}'",
                            r.to_status
                        ),
                    );
                }
            }
            match &r.kind {
                ReqKind::Field { field } | ReqKind::MinItems { field, .. } => {
                    let applies = t.type_field(field).is_some()
                        || schema.fields.get(field).is_some_and(|f| f.shared);
                    if !applies {
                        err(
                            &mut d,
                            file,
                            format!(
                                "requirement names field '{field}', which type '{}' does not have",
                                t.name
                            ),
                        );
                    }
                    if let ReqKind::MinItems { .. } = r.kind
                        && schema
                            .fields
                            .get(field)
                            .is_some_and(|f| f.kind != FieldKind::List)
                    {
                        err(
                            &mut d,
                            file,
                            format!("min_items on '{field}', which is not a list"),
                        );
                    }
                }
                ReqKind::OutEdge { dep_kind } => {
                    if !schema.dep_kinds.contains_key(dep_kind) {
                        err(
                            &mut d,
                            file,
                            format!("requirement names unknown dependency kind '{dep_kind}'"),
                        );
                    }
                }
                ReqKind::InEdgesClosed { dep_kind, .. } => {
                    if !schema.dep_kinds.contains_key(dep_kind) {
                        err(
                            &mut d,
                            file,
                            format!("requirement names unknown dependency kind '{dep_kind}'"),
                        );
                    }
                }
            }
        }
        for status in t.tools.keys() {
            match t.category_of(status) {
                Some(Category::Active) => {}
                Some(_) => err(
                    &mut d,
                    file,
                    format!(
                        "[type.tools] names status '{status}'; only intermediate statuses get transition tools"
                    ),
                ),
                None => err(
                    &mut d,
                    file,
                    format!("[type.tools] names unknown status '{status}'"),
                ),
            }
        }
        if t.agent.is_some() && !t.claimable() {
            d.push(Diagnostic::warning(
                Some(file),
                format!("type '{}' routes to an agent but has no intermediate status, so it cannot be claimed", t.name),
            ));
        }
    }
    d
}

// ----------------------------------------------------------------------
// Plan: diff two schemas
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub op: &'static str, // add | remove | change
    pub what: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn diff_schemas(old: &Schema, new: &Schema) -> Vec<Change> {
    let mut out = vec![];
    let add = |out: &mut Vec<Change>, what: String| {
        out.push(Change {
            op: "add",
            what,
            detail: None,
        })
    };
    let rm = |out: &mut Vec<Change>, what: String| {
        out.push(Change {
            op: "remove",
            what,
            detail: None,
        })
    };

    for (n, f) in &new.fields {
        match old.fields.get(n) {
            None => add(&mut out, format!("field {n} ({})", f.kind.as_str())),
            Some(o) if o != f => out.push(Change {
                op: "change",
                what: format!("field {n}"),
                detail: Some(format!(
                    "{} {} → {} {}",
                    o.kind.as_str(),
                    o.config,
                    f.kind.as_str(),
                    f.config
                )),
            }),
            _ => {}
        }
    }
    for n in old.fields.keys() {
        if !new.fields.contains_key(n) {
            rm(&mut out, format!("field {n}"));
        }
    }
    for (n, k) in &new.dep_kinds {
        match old.dep_kinds.get(n) {
            None => add(&mut out, format!("dep kind {n}")),
            Some(o) if o != k => out.push(Change {
                op: "change",
                what: format!("dep kind {n}"),
                detail: None,
            }),
            _ => {}
        }
    }
    for n in old.dep_kinds.keys() {
        if !new.dep_kinds.contains_key(n) {
            rm(&mut out, format!("dep kind {n}"));
        }
    }
    for (n, t) in &new.types {
        let Some(o) = old.types.get(n) else {
            add(&mut out, format!("type {n}"));
            continue;
        };
        if o.description != t.description {
            out.push(Change {
                op: "change",
                what: format!("type {n} description"),
                detail: None,
            });
        }
        if o.agent != t.agent {
            out.push(Change {
                op: "change",
                what: format!("type {n} routing.agent"),
                detail: Some(format!("{:?} → {:?}", o.agent, t.agent)),
            });
        }
        for s in &t.statuses {
            match o.status(&s.name) {
                None => add(
                    &mut out,
                    format!("status {n}.{} ({})", s.name, s.category.as_str()),
                ),
                Some(os) if os.category != s.category || os.ord != s.ord => out.push(Change {
                    op: "change",
                    what: format!("status {n}.{}", s.name),
                    detail: Some(format!(
                        "{}#{} → {}#{}",
                        os.category.as_str(),
                        os.ord,
                        s.category.as_str(),
                        s.ord
                    )),
                }),
                _ => {}
            }
        }
        for s in &o.statuses {
            if t.status(&s.name).is_none() {
                rm(&mut out, format!("status {n}.{}", s.name));
            }
        }
        let pairs = |t: &TypeDef| -> BTreeSet<(String, String)> {
            t.transitions
                .iter()
                .flat_map(|(f, ts)| ts.iter().map(move |to| (f.clone(), to.clone())))
                .collect()
        };
        let (np, op) = (pairs(t), pairs(o));
        for (f, to) in np.difference(&op) {
            add(&mut out, format!("transition {n}: {f} → {to}"));
        }
        for (f, to) in op.difference(&np) {
            rm(&mut out, format!("transition {n}: {f} → {to}"));
        }
        for r in &t.resolutions {
            if !o.resolutions.contains(r) {
                add(&mut out, format!("resolution {n}.{r}"));
            }
        }
        for r in &o.resolutions {
            if !t.resolutions.contains(r) {
                rm(&mut out, format!("resolution {n}.{r}"));
            }
        }
        for f in &t.fields {
            match o.type_field(&f.field) {
                None => add(&mut out, format!("type field {n}.{}", f.field)),
                Some(of) if of != f => out.push(Change {
                    op: "change",
                    what: format!("type field {n}.{}", f.field),
                    detail: None,
                }),
                _ => {}
            }
        }
        for f in &o.fields {
            if t.type_field(&f.field).is_none() {
                rm(&mut out, format!("type field {n}.{}", f.field));
            }
        }
        let nr: BTreeSet<&Requirement> = t.requirements.iter().collect();
        let or: BTreeSet<&Requirement> = o.requirements.iter().collect();
        for r in nr.difference(&or) {
            add(&mut out, format!("requirement {n}: {}", r.describe()));
        }
        for r in or.difference(&nr) {
            rm(&mut out, format!("requirement {n}: {}", r.describe()));
        }
        if o.tools != t.tools {
            out.push(Change {
                op: "change",
                what: format!("type {n} tool names"),
                detail: None,
            });
        }
    }
    for n in old.types.keys() {
        if !new.types.contains_key(n) {
            rm(&mut out, format!("type {n}"));
        }
    }
    out
}

/// Parse a duration like `30m`, `2h`, `90s`, `1d`.
pub fn parse_duration(s: &str) -> Result<jiff::SignedDuration> {
    let s = s.trim();
    let bad = || {
        Error::new(
            Code::BadArgs,
            format!("invalid duration '{s}' (expected e.g. 30m, 2h, 90s, 1d)"),
        )
    };
    if s.is_empty() {
        return Err(bad());
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" | "" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        "w" => n * 7 * 86400,
        _ => return Err(bad()),
    };
    Ok(jiff::SignedDuration::from_secs(secs))
}
