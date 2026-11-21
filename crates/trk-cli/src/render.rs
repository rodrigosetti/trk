//! Rendering. Every command builds one typed result; pretty output is a view
//! of the same JSON, never a separate query.

use std::io::Write;

use comfy_table::{Attribute, Cell, Color, Table, presets};
use owo_colors::OwoColorize;
use serde_json::{Value, json};
use trk_core::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Pretty,
    Json,
    Jsonl,
}

pub enum Out {
    Issue(Value),
    /// Several full issues (`show A B`): a JSON array.
    IssueList(Vec<Value>),
    Issues {
        items: Vec<Value>,
        fields: Option<Vec<String>>,
    },
    Log {
        issue: String,
        events: Vec<Value>,
    },
    /// Pretty text and the JSON data it shows.
    Text(String, Value),
    /// Pretty-printed as indented key/values.
    Data(Value),
    /// Nothing on stdout in pretty mode (e.g. after `attach`).
    Silent(Value),
}

pub struct Renderer {
    pub format: Format,
    pub color: bool,
}

impl Renderer {
    pub fn emit(&self, out: Out) {
        let stdout = std::io::stdout();
        let mut w = stdout.lock();
        match self.format {
            Format::Json => {
                let data = json_data(out);
                let env =
                    json!({"ok": true, "schema_version": trk_core::SCHEMA_VERSION, "data": data});
                let _ = writeln!(
                    w,
                    "{}",
                    serde_json::to_string_pretty(&env).unwrap_or_default()
                );
            }
            Format::Jsonl => match out {
                Out::Issues { items, fields } => {
                    for i in items {
                        let v = match &fields {
                            Some(f) => project_fields(&i, f),
                            None => i,
                        };
                        let _ = writeln!(w, "{}", serde_json::to_string(&v).unwrap_or_default());
                    }
                }
                Out::Log { events, .. } => {
                    for e in events {
                        let _ = writeln!(w, "{}", serde_json::to_string(&e).unwrap_or_default());
                    }
                }
                other => {
                    let _ = writeln!(
                        w,
                        "{}",
                        serde_json::to_string(&json_data(other)).unwrap_or_default()
                    );
                }
            },
            Format::Pretty => {
                let text = self.pretty(out);
                if !text.is_empty() {
                    let mut s = anstream::AutoStream::new(
                        w,
                        if self.color {
                            anstream::ColorChoice::Always
                        } else {
                            anstream::ColorChoice::Never
                        },
                    );
                    let _ = write!(s, "{text}");
                    if !text.ends_with('\n') {
                        let _ = writeln!(s);
                    }
                }
            }
        }
    }

    pub fn error(&self, e: &Error) {
        match self.format {
            Format::Json | Format::Jsonl => {
                let env = json!({"ok": false, "schema_version": trk_core::SCHEMA_VERSION, "error": e.to_json()});
                println!("{}", serde_json::to_string_pretty(&env).unwrap_or_default());
            }
            Format::Pretty => {
                let mut s = String::new();
                let label = if self.color {
                    "error:".red().bold().to_string()
                } else {
                    "error:".into()
                };
                s.push_str(&format!("{label} {}\n", e.message()));
                let j = e.to_json();
                for key in [
                    "missing_fields",
                    "missing_edges",
                    "unmet_gates",
                    "candidates",
                    "allowed",
                    "problems",
                ] {
                    if let Some(Value::Array(a)) = j.get(key)
                        && !a.is_empty()
                    {
                        let items: Vec<String> = a
                            .iter()
                            .map(|v| match v {
                                Value::String(s) => s.clone(),
                                Value::Object(o) if o.contains_key("src") => format!(
                                    "{} ({} {}, needs {})",
                                    o["src"].as_str().unwrap_or(""),
                                    o["kind"].as_str().unwrap_or(""),
                                    o["src_status"].as_str().unwrap_or(""),
                                    o["required"].as_str().unwrap_or("")
                                ),
                                other => other.to_string(),
                            })
                            .collect();
                        if key == "problems" {
                            for i in items {
                                s.push_str(&format!("  - {i}\n"));
                            }
                        } else {
                            s.push_str(&format!(
                                "  {}: {}\n",
                                key.replace('_', " "),
                                items.join(", ")
                            ));
                        }
                    }
                }
                if let Some(h) = e.hint() {
                    let label = if self.color {
                        "hint:".cyan().to_string()
                    } else {
                        "hint:".into()
                    };
                    s.push_str(&format!("  {label} {h}\n"));
                }
                let mut err = anstream::AutoStream::new(
                    std::io::stderr(),
                    if self.color {
                        anstream::ColorChoice::Always
                    } else {
                        anstream::ColorChoice::Never
                    },
                );
                let _ = write!(err, "{s}");
            }
        }
    }

    fn pretty(&self, out: Out) -> String {
        match out {
            Out::Issue(v) => self.issue(&v),
            Out::IssueList(v) => v
                .iter()
                .map(|i| self.issue(i))
                .collect::<Vec<_>>()
                .join(&format!("\n{}\n\n", self.dim(&"─".repeat(40)))),
            Out::Issues { items, fields } => self.table(&items, fields.as_deref()),
            Out::Log { issue, events } => self.log(&issue, &events),
            Out::Text(t, _) => t,
            Out::Data(v) => kv(&v, 0),
            Out::Silent(_) => String::new(),
        }
    }

    pub fn status_label(&self, status: &str, category: &str) -> String {
        let (glyph, s) = match category {
            "active" => ("●", format!("● {status}")),
            "closed" => ("✓", format!("✓ {status}")),
            _ => ("○", format!("○ {status}")),
        };
        let _ = glyph;
        if !self.color {
            return s;
        }
        match category {
            "active" => s.yellow().to_string(),
            "closed" => s.green().dimmed().to_string(),
            _ => s,
        }
    }

    fn dim(&self, s: &str) -> String {
        if self.color {
            s.dimmed().to_string()
        } else {
            s.to_string()
        }
    }

    fn bold(&self, s: &str) -> String {
        if self.color {
            s.bold().to_string()
        } else {
            s.to_string()
        }
    }

    /// The CLAIM column: claimant and age, or a blocked/gated marker, or milestone progress.
    fn claim_cell(&self, i: &Value) -> String {
        if let Some(c) = i.get("claim").filter(|c| !c.is_null()) {
            let actor = c["actor"].as_str().unwrap_or("");
            let since = c["claimed_at"].as_str().map(age).unwrap_or_default();
            return format!("{} {}", short_actor(actor), since);
        }
        let d = &i["derived"];
        if let Some(p) = d.get("progress") {
            return format!("{}/{} done", p["closed"], p["total"]);
        }
        if d["blocked_by"].as_array().is_some_and(|a| !a.is_empty()) {
            return "⛔ blocked".into();
        }
        if let Some(g) = d["gates_unmet"].as_array().filter(|a| !a.is_empty()) {
            let types: Vec<String> = g
                .iter()
                .filter_map(|x| x["src_type"].as_str().map(String::from))
                .collect();
            let mut types = types;
            types.dedup();
            return format!("⛔ gated ({})", types.join(","));
        }
        String::new()
    }

    pub fn table(&self, items: &[Value], fields: Option<&[String]>) -> String {
        if items.is_empty() {
            return self.dim("no issues");
        }
        let default = ["id", "priority", "type", "status", "claim", "title"];
        let cols: Vec<String> = match fields {
            Some(f) => f.to_vec(),
            None => default.iter().map(|s| s.to_string()).collect(),
        };
        let mut t = Table::new();
        t.load_style(presets::NOTHING);
        if self.color {
            t.enforce_styling();
        } else {
            t.force_no_tty();
        }
        t.set_header(
            cols.iter()
                .map(|c| Cell::new(c.to_uppercase()).add_attribute(Attribute::Bold)),
        );
        for i in items {
            let row: Vec<Cell> = cols
                .iter()
                .map(|c| match c.as_str() {
                    "status" => {
                        let cat = i["status_category"].as_str().unwrap_or("open");
                        let label = match cat {
                            "active" => format!("● {}", i["status"].as_str().unwrap_or("")),
                            "closed" => format!("✓ {}", i["status"].as_str().unwrap_or("")),
                            _ => format!("○ {}", i["status"].as_str().unwrap_or("")),
                        };
                        let cell = Cell::new(label);
                        match cat {
                            "active" => cell.fg(Color::Yellow),
                            "closed" => cell.fg(Color::DarkGreen),
                            _ => cell,
                        }
                    }
                    "claim" => Cell::new(self.claim_cell(i)),
                    "id" => {
                        Cell::new(i["id"].as_str().unwrap_or("")).add_attribute(Attribute::Bold)
                    }
                    other => Cell::new(cell_text(&field_value(i, other))),
                })
                .collect();
            t.add_row(row);
        }
        t.to_string()
    }

    pub fn issue(&self, i: &Value) -> String {
        let mut s = String::new();
        let id = i["id"].as_str().unwrap_or("");
        s.push_str(&format!(
            "{}  {}  {}{}\n",
            self.bold(id),
            i["type"].as_str().unwrap_or(""),
            self.status_label(
                i["status"].as_str().unwrap_or(""),
                i["status_category"].as_str().unwrap_or("")
            ),
            i["resolution"]
                .as_str()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ));
        s.push_str(&format!(
            "{}\n",
            self.bold(i["title"].as_str().unwrap_or(""))
        ));
        let meta = format!(
            "project {} · created {} by {}{}",
            i["project"].as_str().unwrap_or(""),
            i["created_at"].as_str().map(age).unwrap_or_default(),
            i["created_by"].as_str().unwrap_or(""),
            i["owner"]
                .as_str()
                .map(|o| format!(" · owner {o}"))
                .unwrap_or_default()
        );
        s.push_str(&format!("{}\n\n", self.dim(&meta)));
        s.push_str(&markdown(i["context"].as_str().unwrap_or(""), self.color));
        if !s.ends_with('\n') {
            s.push('\n');
        }
        if let Some(o) = i["outcome"].as_str() {
            s.push_str(&format!("\n{}\n", self.bold("Outcome")));
            s.push_str(&markdown(o, self.color));
        }
        let mut rows = vec![];
        let mut unset = vec![];
        if let Some(f) = i["fields"].as_object() {
            for (k, v) in f {
                let t = cell_text(v);
                if t.is_empty() {
                    unset.push(k.clone());
                } else {
                    rows.push((k.clone(), t));
                }
            }
        }
        if let Some(t) = i["tags"].as_array().filter(|a| !a.is_empty()) {
            rows.push(("tags".into(), cell_text(&Value::Array(t.clone()))));
        }
        if let Some(l) = i["links"].as_array() {
            for link in l {
                rows.push((
                    format!("link:{}", link["kind"].as_str().unwrap_or("link")),
                    format!(
                        "{}{}",
                        link["url"].as_str().unwrap_or(""),
                        link["note"]
                            .as_str()
                            .map(|n| format!("  {n}"))
                            .unwrap_or_default()
                    ),
                ));
            }
        }
        if !rows.is_empty() || !unset.is_empty() {
            s.push_str(&format!("\n{}\n", self.bold("Fields")));
            let w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
            for (k, v) in rows {
                s.push_str(&format!(
                    "  {:w$}  {}\n",
                    k,
                    v.replace('\n', &format!("\n  {:w$}  ", ""))
                ));
            }
            if !unset.is_empty() {
                s.push_str(&format!(
                    "  {}\n",
                    self.dim(&format!("unset: {}", unset.join(", ")))
                ));
            }
        }
        if let Some(deps) = i.get("deps") {
            let out = deps["out"].as_array().cloned().unwrap_or_default();
            let inn = deps["in"].as_array().cloned().unwrap_or_default();
            if !out.is_empty() || !inn.is_empty() {
                s.push_str(&format!("\n{}\n", self.bold("Dependencies")));
                for e in &out {
                    s.push_str(&format!(
                        "  {} {}\n",
                        e["kind"].as_str().unwrap_or(""),
                        self.edge(e)
                    ));
                }
                for e in &inn {
                    s.push_str(&format!(
                        "  {} {}\n",
                        inverse_label(e["kind"].as_str().unwrap_or("")),
                        self.edge(e)
                    ));
                }
            }
        }
        if let Some(c) = i.get("claim").filter(|c| !c.is_null()) {
            s.push_str(&format!("\n{}\n", self.bold("Claim")));
            s.push_str(&format!("  {}", c["actor"].as_str().unwrap_or("")));
            if let Some(o) = c["on_behalf_of"].as_str() {
                s.push_str(&format!(" on behalf of {o}"));
            }
            s.push_str(&format!(
                " · since {}",
                c["claimed_at"].as_str().map(age).unwrap_or_default()
            ));
            if let Some(l) = c["lease_until"].as_str() {
                s.push_str(&format!(" · lease until {l}"));
            }
            s.push('\n');
            for k in ["agent", "terminal", "worktree", "host"] {
                if let Some(v) = c[k].as_str() {
                    s.push_str(&format!("  {k}: {v}\n"));
                }
            }
        }
        let d = &i["derived"];
        let mut flags = vec![];
        if d["ready"].as_bool() == Some(true) {
            flags.push("ready".to_string());
        }
        if let Some(b) = d["blocked_by"].as_array().filter(|a| !a.is_empty()) {
            flags.push(format!(
                "⛔ blocked by {}",
                b.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(g) = d["gates_unmet"].as_array().filter(|a| !a.is_empty()) {
            for x in g {
                flags.push(format!(
                    "⛔ gated: entering {} needs {} closed as {}",
                    x["entering"].as_str().unwrap_or(""),
                    x["src"].as_str().unwrap_or(""),
                    x["required"].as_str().unwrap_or("")
                ));
            }
        }
        if let Some(unblocks) = d["unblocks_higher_priority"]
            .as_array()
            .filter(|items| !items.is_empty())
        {
            flags.push(format!(
                "unblocks higher-priority work: {}",
                unblocks
                    .iter()
                    .map(|item| format!(
                        "{} ({})",
                        item["id"].as_str().unwrap_or(""),
                        item["priority"].as_str().unwrap_or("")
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        if let Some(p) = d.get("progress") {
            flags.push(format!("progress {}/{}", p["closed"], p["total"]));
        }
        let mut next_lines = vec![];
        if let Some(n) = d["next"].as_object() {
            for (status, req) in n {
                match req {
                    Value::Array(a) => next_lines.push(format!("{status}: {}", missing_text(a))),
                    Value::Object(o) => {
                        for (res, m) in o {
                            next_lines.push(format!(
                                "{status} as {res}: {}",
                                missing_text(m.as_array().map(|v| v.as_slice()).unwrap_or(&[]))
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        if !flags.is_empty() || !next_lines.is_empty() {
            s.push_str(&format!("\n{}\n", self.bold("State")));
            for f in flags {
                s.push_str(&format!("  {f}\n"));
            }
            for n in next_lines {
                s.push_str(&format!("  next → {n}\n"));
            }
        }
        if let Some(notes) = i["notes"].as_array().filter(|a| !a.is_empty()) {
            s.push_str(&format!("\n{}\n", self.bold("Notes")));
            for n in notes {
                s.push_str(&format!(
                    "  {} {} {}{}\n",
                    self.dim(&format!("#{}", n["seq"])),
                    self.dim(n["at"].as_str().unwrap_or("")),
                    short_actor(n["actor"].as_str().unwrap_or("")),
                    n["amends"]
                        .as_i64()
                        .map(|a| format!(" (amends #{a})"))
                        .unwrap_or_default()
                ));
                for line in n["body"].as_str().unwrap_or("").lines() {
                    s.push_str(&format!("    {line}\n"));
                }
            }
        }
        if let Some(ev) = i["events"].as_array().filter(|a| !a.is_empty()) {
            s.push_str(&format!("\n{}\n", self.bold("Timeline")));
            for e in ev {
                s.push_str(&format!("  {}\n", self.event_line(e)));
            }
        }
        if let Some(cl) = i["claims"].as_array().filter(|a| !a.is_empty()) {
            s.push_str(&format!("\n{}\n", self.bold("Claim history")));
            for c in cl {
                s.push_str(&format!(
                    "  {} {} → {} ({}){}\n",
                    c["actor"].as_str().unwrap_or(""),
                    c["claimed_at"].as_str().unwrap_or(""),
                    c["released_at"].as_str().unwrap_or("active"),
                    c["release_reason"].as_str().unwrap_or("-"),
                    c["worktree"]
                        .as_str()
                        .map(|w| format!(" · {w}"))
                        .unwrap_or_default()
                ));
            }
        }
        s
    }

    fn edge(&self, e: &Value) -> String {
        format!(
            "{}  {} {}  {}",
            self.bold(e["id"].as_str().unwrap_or("")),
            e["type"].as_str().unwrap_or(""),
            self.status_label(
                e["status"].as_str().unwrap_or(""),
                e["status_category"].as_str().unwrap_or("")
            ),
            e["title"].as_str().unwrap_or("")
        )
    }

    pub fn event_line(&self, e: &Value) -> String {
        let p = &e["payload"];
        let op = e["op"].as_str().unwrap_or("");
        let what = match op {
            "create" => format!(
                "created {} \"{}\"",
                p["type"].as_str().unwrap_or(""),
                p["title"].as_str().unwrap_or("")
            ),
            "set" => {
                let ch: Vec<String> = p["changes"]
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| format!("{k}={}", short(&cell_text(&v["to"]), 40)))
                            .collect()
                    })
                    .unwrap_or_default();
                format!("set {}", ch.join(", "))
            }
            "move" => format!(
                "moved {} → {}",
                p["from"].as_str().unwrap_or(""),
                p["to"].as_str().unwrap_or("")
            ),
            "claim" => format!(
                "claimed ({} → {}){}",
                p["from"].as_str().unwrap_or(""),
                p["to"].as_str().unwrap_or(""),
                p["stolen_from"]
                    .as_str()
                    .map(|s| format!(", stolen from {s}"))
                    .unwrap_or_default()
            ),
            "release" => format!(
                "released claim of {} ({})",
                short_actor(p["claimant"].as_str().unwrap_or("")),
                p["reason"].as_str().unwrap_or("")
            ),
            "close" => format!("closed as {}", p["resolution"].as_str().unwrap_or("")),
            "dep_add" => format!(
                "+ {} {} {}",
                p["src"].as_str().unwrap_or(""),
                p["kind"].as_str().unwrap_or(""),
                p["dst"].as_str().unwrap_or("")
            ),
            "dep_rm" => format!(
                "- {} {} {}",
                p["src"].as_str().unwrap_or(""),
                p["kind"].as_str().unwrap_or(""),
                p["dst"].as_str().unwrap_or("")
            ),
            "note" => format!(
                "note{}: {}",
                if p["to_agent"].as_bool() == Some(true) {
                    " (to agent)"
                } else {
                    ""
                },
                short(
                    p["body"]
                        .as_str()
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or(""),
                    80
                )
            ),
            other => other.to_string(),
        };
        let forced = if p["forced"].as_bool() == Some(true) {
            " [forced]"
        } else {
            ""
        };
        format!(
            "{} {} {}  {}{}",
            self.dim(&format!("#{:<4}", e["seq"])),
            self.dim(e["at"].as_str().unwrap_or("")),
            short_actor(e["actor"].as_str().unwrap_or("")),
            what,
            forced
        )
    }

    fn log(&self, issue: &str, events: &[Value]) -> String {
        let mut s = format!("{}\n", self.bold(issue));
        for e in events {
            s.push_str(&format!("{}\n", self.event_line(e)));
            if e["op"] == "note" {
                for line in e["payload"]["body"].as_str().unwrap_or("").lines().skip(1) {
                    s.push_str(&format!("        {line}\n"));
                }
            }
        }
        s
    }
}

fn missing_text(a: &[Value]) -> String {
    if a.is_empty() {
        "ready".into()
    } else {
        format!(
            "needs {}",
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn inverse_label(kind: &str) -> String {
    match kind {
        "blocks" => "blocked by".into(),
        "approves" => "approved by".into(),
        "informs" => "informed by".into(),
        "implements" => "implemented by".into(),
        "discovered-from" => "spawned".into(),
        "supersedes" => "superseded by".into(),
        "duplicates" => "duplicated by".into(),
        "reverts" => "reverted by".into(),
        k => format!("{k} (in)"),
    }
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Agent ids are shortened in tables: `claude-code:1293…`; humans show the
/// local part of their email.
pub fn short_actor(a: &str) -> String {
    if let Some(rest) = a.strip_prefix("agent:") {
        let (h, sess) = rest.split_once(':').unwrap_or((rest, ""));
        return format!("{h}:{}", short(sess, 4));
    }
    if let Some(e) = a.strip_prefix("human:") {
        return e.split('@').next().unwrap_or(e).to_string();
    }
    a.to_string()
}

/// "12m", "3h", "2d" since an RFC 3339 timestamp.
pub fn age(ts: &str) -> String {
    let Ok(t) = ts.parse::<jiff::Timestamp>() else {
        return ts.to_string();
    };
    let now = std::env::var("TRK_NOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(jiff::Timestamp::now);
    let secs = (now - t).as_secs().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

pub fn cell_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| match x {
                Value::Object(o) if o.contains_key("url") => {
                    o["url"].as_str().unwrap_or("").to_string()
                }
                other => cell_text(other),
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// A value for `--fields` NAME from an issue object.
pub fn field_value(i: &Value, name: &str) -> Value {
    match name {
        "id" | "type" | "project" | "title" | "context" | "status" | "status_category"
        | "resolution" | "outcome" | "owner" | "links" | "tags" | "created_by" | "created_at"
        | "updated_at" | "closed_at" | "claim" | "deps" | "derived" => i[name].clone(),
        "category" => i["status_category"].clone(),
        "created" => i["created_at"].clone(),
        "updated" => i["updated_at"].clone(),
        "closed" => i["closed_at"].clone(),
        "claimed-by" => i["claim"]["actor"].clone(),
        "lease" => i["claim"]["lease_until"].clone(),
        "ready" => i["derived"]["ready"].clone(),
        "blocked-by" => i["derived"]["blocked_by"].clone(),
        n => {
            let n = n.strip_prefix("field.").unwrap_or(n);
            i["fields"].get(n).cloned().unwrap_or(Value::Null)
        }
    }
}

pub fn project_fields(i: &Value, fields: &[String]) -> Value {
    let mut m = serde_json::Map::new();
    for f in fields {
        m.insert(f.clone(), field_value(i, f));
    }
    Value::Object(m)
}

fn json_data(out: Out) -> Value {
    match out {
        Out::Issue(v) | Out::Data(v) | Out::Text(_, v) | Out::Silent(v) => v,
        Out::IssueList(v) => Value::Array(v),
        Out::Issues { items, fields } => Value::Array(match fields {
            Some(f) => items.iter().map(|i| project_fields(i, &f)).collect(),
            None => items,
        }),
        Out::Log { events, .. } => Value::Array(events),
    }
}

fn markdown(md: &str, color: bool) -> String {
    if !color {
        return md.to_string();
    }
    let skin = termimad::MadSkin::default();
    let width = termimad::terminal_size().0.clamp(40, 120) as usize;
    skin.text(md, Some(width)).to_string()
}

/// Indented key/value text for arbitrary JSON.
pub fn kv(v: &Value, indent: usize) -> String {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => {
            let mut s = String::new();
            for (k, x) in m {
                match x {
                    Value::Object(o) if !o.is_empty() => {
                        s.push_str(&format!("{pad}{k}:\n{}", kv(x, indent + 2)))
                    }
                    Value::Array(a) if a.iter().any(|y| y.is_object()) => {
                        s.push_str(&format!("{pad}{k}:\n"));
                        for y in a {
                            let inner = kv(y, indent + 4);
                            s.push_str(&format!(
                                "{pad}  - {}\n",
                                inner
                                    .trim_start()
                                    .trim_end()
                                    .replace('\n', &format!("\n{pad}    "))
                            ));
                        }
                    }
                    _ => s.push_str(&format!("{pad}{k}: {}\n", cell_text(x))),
                }
            }
            s
        }
        Value::Array(a) => {
            let mut s = String::new();
            for y in a {
                s.push_str(&format!("{pad}- {}\n", kv(y, indent + 2).trim()));
            }
            s
        }
        other => format!("{pad}{}\n", cell_text(other)),
    }
}
