//! Rendering. Every command builds one typed result; pretty output is a view
//! of the same JSON, never a separate query.

use std::io::Write;

use comfy_table::{Attribute, Cell, Color, Table, presets};
use owo_colors::OwoColorize;
use serde_json::{Value, json};
use trk_core::config::fmt_duration;
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
                for key in ["blocked_by", "candidates", "allowed", "problems"] {
                    if let Some(Value::Array(a)) = j.get(key)
                        && !a.is_empty()
                    {
                        let items: Vec<String> = a
                            .iter()
                            .map(|v| match v {
                                Value::String(s) => s.clone(),
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

    pub fn state_label(&self, state: &str) -> String {
        let s = format!("{} {state}", state_glyph(state));
        if !self.color {
            return s;
        }
        match state {
            "open" => s.green().to_string(),
            "claimed" => s.yellow().to_string(),
            "waiting" => s.cyan().to_string(),
            "closed" => s.green().dimmed().to_string(),
            _ => s,
        }
    }

    fn priority_label(&self, priority: &str) -> String {
        if !self.color {
            return priority.to_string();
        }
        match priority {
            "P0" => priority.red().bold().to_string(),
            "P1" => priority.yellow().to_string(),
            "P2" => priority.cyan().to_string(),
            "P3" => priority.dimmed().to_string(),
            _ => priority.to_string(),
        }
    }

    fn priority_cell(&self, priority: &str) -> Cell {
        let cell = Cell::new(priority);
        if !self.color {
            return cell;
        }
        match priority {
            "P0" => cell.fg(Color::Red).add_attribute(Attribute::Bold),
            "P1" => cell.fg(Color::Yellow),
            "P2" => cell.fg(Color::Cyan),
            "P3" => cell.fg(Color::DarkGrey),
            _ => cell,
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

    /// The DETAIL column: the claimant and its age, what a waiting issue
    /// waits for, a blocked marker, or blocker progress.
    fn detail_cell(&self, i: &Value) -> String {
        if let Some(c) = i.get("claim").filter(|c| !c.is_null()) {
            let actor = c["actor"].as_str().unwrap_or("");
            let since = c["claimed_at"].as_str().map(age).unwrap_or_default();
            return format!("{} {}", short_actor(actor), since);
        }
        if let Some(w) = i.get("waiting").filter(|w| !w.is_null()) {
            return waiting_summary(w);
        }
        let d = &i["derived"];
        if i["state"] != "closed" {
            if d["blocked_by"].as_array().is_some_and(|a| !a.is_empty()) {
                return match d.get("progress") {
                    Some(p) => format!("⛔ blocked ({}/{} done)", p["closed"], p["total"]),
                    None => "⛔ blocked".into(),
                };
            }
            return String::new();
        }
        i["close_reason"].as_str().unwrap_or("").to_string()
    }

    pub fn table(&self, items: &[Value], fields: Option<&[String]>) -> String {
        if items.is_empty() {
            return self.dim("no issues");
        }
        let default = ["id", "priority", "type", "state", "detail", "title"];
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
                    "priority" => self.priority_cell(i["priority"].as_str().unwrap_or("")),
                    "state" | "status" => {
                        let st = i["state"].as_str().unwrap_or("open");
                        let cell = Cell::new(format!("{} {st}", state_glyph(st)));
                        match st {
                            "open" => cell.fg(Color::Green),
                            "claimed" => cell.fg(Color::Yellow),
                            "waiting" => cell.fg(Color::Cyan),
                            "closed" => cell.fg(Color::DarkGreen),
                            _ => cell,
                        }
                    }
                    "detail" | "claim" => Cell::new(self.detail_cell(i)),
                    "waiting" => Cell::new(
                        i.get("waiting")
                            .filter(|w| !w.is_null())
                            .map(waiting_summary)
                            .unwrap_or_default(),
                    ),
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
            self.priority_label(i["priority"].as_str().unwrap_or("")),
            self.state_label(i["state"].as_str().unwrap_or("")),
            i["close_reason"]
                .as_str()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ));
        s.push_str(&format!(
            "{}\n",
            self.bold(i["title"].as_str().unwrap_or(""))
        ));
        let repo = &i["repo"];
        let meta = format!(
            "project {}{} · created {} by {}{}",
            i["project"].as_str().unwrap_or(""),
            repo["remote_url"]
                .as_str()
                .or(repo["local_path"].as_str())
                .map(|r| format!(" · repo {r}"))
                .unwrap_or_default(),
            i["created_at"].as_str().map(age).unwrap_or_default(),
            i["created_by"].as_str().unwrap_or(""),
            i["type"]
                .as_str()
                .map(|t| format!(" · type {t}"))
                .unwrap_or_default()
        );
        s.push_str(&format!("{}\n", self.dim(&meta)));
        if let Some(routing) = i.get("routing").filter(|r| !r.is_null()) {
            s.push_str(&format!("\n{}\n", self.bold("Routing")));
            if let Some(agent) = routing["agent"].as_str() {
                s.push_str(&format!("  agent   {agent}\n"));
            }
            if let Some(sources) = routing["type_sources"].as_array() {
                for source in sources.iter().filter_map(Value::as_str) {
                    s.push_str(&format!("  type    {source}\n"));
                }
            }
            if let Some(source) = routing["agent_source"].as_str() {
                s.push_str(&format!("  source  {source}\n"));
            }
            if let Some(directory) = routing["prompt_directory"].as_str() {
                s.push_str(&format!("  prompts {directory}\n"));
            }
            s.push_str(if routing["requires_trust"].as_bool() == Some(true) {
                "  trust   required (--trust-repo)\n"
            } else {
                "  trust   not required\n"
            });
        }
        let description = i["description"].as_str().unwrap_or("");
        if !description.trim().is_empty() {
            s.push('\n');
            s.push_str(&markdown(description, self.color));
            if !s.ends_with('\n') {
                s.push('\n');
            }
        }
        if let Some(w) = i.get("waiting").filter(|w| !w.is_null()) {
            s.push_str(&format!("\n{}\n", self.bold("Waiting")));
            if let Some(r) = w["reason"].as_str() {
                s.push_str(&format!("  for    {r}\n"));
            }
            match w["check"].as_str() {
                Some(c) => {
                    // The command runs unattended: always shown in full.
                    let mut lines = c.lines();
                    s.push_str(&format!("  check  {}\n", lines.next().unwrap_or("")));
                    for l in lines {
                        s.push_str(&format!("         {l}\n"));
                    }
                    if let Some(cwd) = w["cwd"].as_str() {
                        s.push_str(&format!("  in     {cwd}\n"));
                    }
                    s.push_str(&format!(
                        "  every  {}{}\n",
                        w["interval"].as_i64().map(fmt_duration).unwrap_or_default(),
                        w["next_check_at"]
                            .as_str()
                            .map(|t| format!(" · next {}", until(t)))
                            .unwrap_or_default()
                    ));
                    if let Some(t) = w["last_check_at"].as_str() {
                        s.push_str(&format!(
                            "  last   {} ago, exit {} (not yet)\n",
                            age(t),
                            w["last_check_exit"]
                        ));
                    }
                }
                None => s.push_str(&format!(
                    "  {}\n",
                    self.dim(&format!(
                        "no check: a human resolves it with `trk close {id}` or `trk reopen {id}`"
                    ))
                )),
            }
        }
        if let Some(deps) = i.get("deps") {
            let by = deps["blocked_by"].as_array().cloned().unwrap_or_default();
            let blocks = deps["blocks"].as_array().cloned().unwrap_or_default();
            if !by.is_empty() || !blocks.is_empty() {
                s.push_str(&format!("\n{}\n", self.bold("Dependencies")));
                for e in &by {
                    s.push_str(&format!("  blocked by {}\n", self.edge(e)));
                }
                for e in &blocks {
                    s.push_str(&format!("  blocks     {}\n", self.edge(e)));
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
                        self.priority_label(item["priority"].as_str().unwrap_or(""))
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        if let Some(p) = d.get("progress") {
            flags.push(format!("progress {}/{}", p["closed"], p["total"]));
        }
        if d["due"].as_bool() == Some(true) {
            flags.push("check due: `trk check` will run it".to_string());
        }
        if !flags.is_empty() {
            s.push_str(&format!("\n{}\n", self.bold("State")));
            for f in flags {
                s.push_str(&format!("  {f}\n"));
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
            "{}  {}  {}",
            self.bold(e["id"].as_str().unwrap_or("")),
            self.state_label(e["state"].as_str().unwrap_or("")),
            e["title"].as_str().unwrap_or("")
        )
    }

    pub fn event_line(&self, e: &Value) -> String {
        let p = &e["payload"];
        let op = e["op"].as_str().unwrap_or("");
        let what = match op {
            "create" => format!(
                "created \"{}\"{}",
                p["title"].as_str().unwrap_or(""),
                p["followup_of"]
                    .as_str()
                    .map(|f| format!(" as a follow-up of {f}"))
                    .unwrap_or_default()
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
            "claim" => format!(
                "claimed{}",
                p["stolen_from"]
                    .as_str()
                    .map(|s| format!(", stolen from {s}"))
                    .unwrap_or_default()
            ),
            "wait" => format!(
                "waiting{}{}",
                p["reason"]
                    .as_str()
                    .map(|r| format!(" for {}", short(r, 60)))
                    .unwrap_or_default(),
                match p["check"].as_str() {
                    Some(c) => format!(
                        "; check every {}: {}",
                        p["interval"].as_i64().map(fmt_duration).unwrap_or_default(),
                        short(c.lines().next().unwrap_or(""), 60)
                    ),
                    None => "; no check, a human decides".to_string(),
                }
            ),
            "check" => match p["result"].as_str() {
                Some("passed") => "check passed".to_string(),
                _ if p["timed_out"].as_bool() == Some(true) => "check timed out".to_string(),
                _ => match p["exit"].as_i64() {
                    Some(c) => format!("check failed (exit {c})"),
                    None => "check failed".to_string(),
                },
            },
            "reopen" => format!(
                "reopened from {}{}",
                p["from"].as_str().unwrap_or(""),
                if p["by_check"].as_bool() == Some(true) {
                    " by its check"
                } else {
                    ""
                }
            ),
            "release" => format!(
                "released claim of {} ({})",
                short_actor(p["claimant"].as_str().unwrap_or("")),
                p["reason"].as_str().unwrap_or("")
            ),
            "close" => format!(
                "closed as {}{}",
                p["reason"].as_str().unwrap_or(""),
                if p["by_check"].as_bool() == Some(true) {
                    " by its check"
                } else {
                    ""
                }
            ),
            "dep_add" => format!(
                "+ {} blocks {}",
                p["blocker"].as_str().unwrap_or(""),
                p["blocked"].as_str().unwrap_or("")
            ),
            "dep_rm" => format!(
                "- {} blocks {}",
                p["blocker"].as_str().unwrap_or(""),
                p["blocked"].as_str().unwrap_or("")
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

fn state_glyph(state: &str) -> &'static str {
    match state {
        "claimed" => "●",
        "waiting" => "◷",
        "closed" => "✓",
        _ => "○",
    }
}

/// One line for a waiting issue: what it waits for and when it is checked next.
fn waiting_summary(w: &Value) -> String {
    let reason = w["reason"].as_str().map(|r| short(r, 40));
    let when = match (w["check"].as_str(), w["next_check_at"].as_str()) {
        (Some(_), Some(t)) => format!("check {}", until(t)),
        (Some(_), None) => "check due".to_string(),
        (None, _) => "needs a human".to_string(),
    };
    match reason {
        Some(r) => format!("{r} · {when}"),
        None => when,
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

fn now() -> jiff::Timestamp {
    std::env::var("TRK_NOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(jiff::Timestamp::now)
}

fn span(secs: i64) -> String {
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

/// "12m", "3h", "2d" since an RFC 3339 timestamp.
pub fn age(ts: &str) -> String {
    let Ok(t) = ts.parse::<jiff::Timestamp>() else {
        return ts.to_string();
    };
    span((now() - t).as_secs().max(0))
}

/// "in 4m", or "now" when the time has passed.
pub fn until(ts: &str) -> String {
    let Ok(t) = ts.parse::<jiff::Timestamp>() else {
        return ts.to_string();
    };
    let secs = (t - now()).as_secs();
    if secs <= 0 {
        return "now".into();
    }
    format!("in {}", span(secs))
}

pub fn cell_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(cell_text).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// A value for `--fields` NAME from an issue object.
pub fn field_value(i: &Value, name: &str) -> Value {
    match name {
        "status" => i["state"].clone(),
        "reason" => i["close_reason"].clone(),
        "created" => i["created_at"].clone(),
        "updated" => i["updated_at"].clone(),
        "closed" => i["closed_at"].clone(),
        "claimed-by" => i["claim"]["actor"].clone(),
        "lease" => i["claim"]["lease_until"].clone(),
        "ready" => i["derived"]["ready"].clone(),
        "due" => i["derived"]["due"].clone(),
        "blocked-by" => i["derived"]["blocked_by"].clone(),
        "check" => i["waiting"]["check"].clone(),
        "next-check" => i["waiting"]["next_check_at"].clone(),
        "repo" => {
            let r = &i["repo"];
            if r["remote_url"].is_null() {
                r["local_path"].clone()
            } else {
                r["remote_url"].clone()
            }
        }
        n => i.get(n).cloned().unwrap_or(Value::Null),
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
