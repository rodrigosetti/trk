//! Drawing the dashboard. Everything here is a pure view of `Ui`: no store,
//! no processes, so a test can render it into a buffer.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use serde_json::Value;
use trk_core::State;
use trk_core::config::fmt_duration;
use trk_core::dashboard::{DashIssue, Dashboard, Detail, Group};
use trk_core::model::{Edge, Event, parse_ts};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Text the person is typing in the footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Note,
    NoteToAgent,
    Send,
    Resume { id: String },
}

/// Something that asks for a yes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Run,
    Reopen,
    Unclaim,
    Reap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Input { kind: Input, text: String },
    Confirm(Confirm),
    CloseReason,
    Priority,
    Help,
}

pub struct Status {
    pub text: String,
    pub error: bool,
}

pub struct Ui {
    pub project: String,
    pub snap: Dashboard,
    pub now: jiff::Timestamp,
    pub filter: String,
    /// Indexes into `snap.issues` that pass the filter, in display order.
    pub rows: Vec<usize>,
    /// Index into `rows`.
    pub selected: usize,
    pub detail: Option<(String, Detail)>,
    pub detail_scroll: u16,
    /// The detail pane fills the screen.
    pub zoom: bool,
    pub mode: Mode,
    pub status: Option<Status>,
    /// Labels of background commands still running.
    pub busy: Vec<String>,
    /// Whether the check runner's process exists, when that can be told.
    pub runner_alive: Option<bool>,
    /// First list line shown; kept so the selection scrolls smoothly.
    pub list_top: usize,
}

impl Ui {
    pub fn new(project: String, snap: Dashboard, now: jiff::Timestamp) -> Ui {
        let mut ui = Ui {
            project,
            snap,
            now,
            filter: String::new(),
            rows: vec![],
            selected: 0,
            detail: None,
            detail_scroll: 0,
            zoom: false,
            mode: Mode::Normal,
            status: None,
            busy: vec![],
            runner_alive: None,
            list_top: 0,
        };
        ui.refilter(None);
        ui
    }

    pub fn issue(&self) -> Option<&DashIssue> {
        self.rows
            .get(self.selected)
            .and_then(|i| self.snap.issues.get(*i))
    }

    /// Recompute the visible rows, keeping the selection on `keep` when it
    /// is still visible.
    pub fn refilter(&mut self, keep: Option<&str>) {
        let needle = self.filter.to_lowercase();
        self.rows = self
            .snap
            .issues
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                needle.is_empty()
                    || i.id.to_lowercase().contains(&needle)
                    || i.title.to_lowercase().contains(&needle)
                    || i.priority.to_lowercase() == needle
                    || i.group.label().to_lowercase().contains(&needle)
            })
            .map(|(n, _)| n)
            .collect();
        let kept = keep.and_then(|id| self.rows.iter().position(|i| self.snap.issues[*i].id == id));
        self.selected = kept
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }
}

// ----------------------------------------------------------------------
// Small formatting helpers
// ----------------------------------------------------------------------

/// `s` cut to at most `w` columns, with an ellipsis when cut.
pub fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    if w > 0 {
        out.push('…');
    }
    out
}

fn span_secs(secs: i64) -> String {
    let s = secs.abs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..172_800 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// Seconds from now to `ts`: negative in the past.
fn until(ts: &str, now: jiff::Timestamp) -> Option<i64> {
    Some(parse_ts(ts)?.as_second() - now.as_second())
}

/// `3m ago` or `in 3m`.
pub fn relative(ts: &str, now: jiff::Timestamp) -> String {
    match until(ts, now) {
        None => ts.to_string(),
        Some(d) if d > 0 => format!("in {}", span_secs(d)),
        Some(d) => format!("{} ago", span_secs(d)),
    }
}

/// `alice` for a human, `omp:1a2b3c` for an agent session.
pub fn short_actor(actor: &str) -> String {
    if let Some(email) = actor.strip_prefix("human:") {
        return email.split('@').next().unwrap_or(email).to_string();
    }
    if let Some(rest) = actor.strip_prefix("agent:") {
        let (harness, session) = rest.split_once(':').unwrap_or((rest, ""));
        return format!("{harness}:{}", session.chars().take(6).collect::<String>());
    }
    actor.to_string()
}

/// A path with the home directory written as `~`.
fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match path.strip_prefix(&home) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_string(),
        },
        _ => path.to_string(),
    }
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// One event as a short phrase.
pub fn describe(e: &Event) -> String {
    let p = &e.payload;
    let reason = p["reason"].as_str();
    match e.op.as_str() {
        "create" => "created".into(),
        "resume_chat" => "session reported a human chat reply; resumed".into(),
        "claim" if p["by_check"] == true => "resumed after a failed check".into(),
        "claim" => match p["agent"].as_str() {
            Some(a) => format!("claimed ({a})"),
            None => "claimed".into(),
        },
        "release" => match reason {
            Some("waiting") => "turn ended, waiting".into(),
            Some("closed") => "claim released (closed)".into(),
            Some("expired") => "lease expired, open again".into(),
            Some("stolen") => "claim taken over".into(),
            _ if p["reaped"] == true => "reaped, open again".into(),
            _ => "unclaimed, open again".into(),
        },
        "wait" => match (p["check"].is_string(), reason) {
            (true, Some(r)) => format!("waiting on a check: {}", first_line(r)),
            (true, None) => "waiting on a check".into(),
            (false, Some(r)) => format!("waiting for a decision: {}", first_line(r)),
            (false, None) => "waiting for a decision".into(),
        },
        "check" => match (p["result"].as_str(), p["exit"].as_i64()) {
            (Some("passed"), _) => "check passed".into(),
            (_, _) if p["timed_out"] == true => "check timed out".into(),
            (_, Some(code)) => format!("check failed (exit {code})"),
            _ => "check failed".into(),
        },
        "close" if p["by_check"] == true => "closed by its check".into(),
        "close" => format!("closed ({})", reason.unwrap_or("done")),
        "reopen" if p["by_check"] == true => "open again after a failed check".into(),
        "reopen" => "reopened".into(),
        "note" => {
            let to = if p["to_agent"] == true {
                " to agent"
            } else {
                ""
            };
            format!("note{to}: {}", first_line(p["body"].as_str().unwrap_or("")))
        }
        "set" => match p["changes"].as_object() {
            Some(c) => format!(
                "changed {}",
                c.keys().cloned().collect::<Vec<_>>().join(", ")
            ),
            None => "changed".into(),
        },
        "dep_add" => format!(
            "{} blocks {}",
            p["blocker"].as_str().unwrap_or("?"),
            p["blocked"].as_str().unwrap_or("?")
        ),
        "dep_rm" => format!(
            "{} no longer blocks {}",
            p["blocker"].as_str().unwrap_or("?"),
            p["blocked"].as_str().unwrap_or("?")
        ),
        other => other.to_string(),
    }
}

fn group_color(g: Group) -> Color {
    match g {
        Group::Attention => Color::Red,
        Group::Running => Color::Green,
        Group::Waiting => Color::Cyan,
        Group::Ready => Color::Blue,
        Group::Blocked => Color::DarkGray,
    }
}

fn group_glyph(g: Group) -> &'static str {
    match g {
        Group::Attention => "!",
        Group::Running => "▶",
        Group::Waiting => "◷",
        Group::Ready => "○",
        Group::Blocked => "⊘",
    }
}

fn priority_style(p: &str) -> Style {
    match p {
        "P0" => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        "P1" => Style::new().fg(Color::Yellow),
        "P3" => Style::new().fg(Color::DarkGray),
        _ => Style::new(),
    }
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A person can start a session on it.
pub fn runnable(i: &DashIssue) -> bool {
    i.state == State::Open && i.blocked_by.iter().all(|b| b.state == State::Closed)
}

/// The issue's first link as a row chip: `↗ PR #12`, with `+N` for the rest.
fn link_chip(i: &DashIssue) -> Option<String> {
    let first = i.links.first()?;
    let more = match i.links.len() - 1 {
        0 => String::new(),
        n => format!(" +{n}"),
    };
    Some(format!("↗ {}{more}", fit(first.text(), 24)))
}

/// The short status at the right of a list row.
fn row_info(i: &DashIssue, now: jiff::Timestamp) -> String {
    if let Some(why) = i.attention.first() {
        return why.clone();
    }
    match i.group {
        Group::Running => {
            let who = i
                .session
                .as_ref()
                .map(|s| s.agent.clone().unwrap_or_else(|| short_actor(&s.actor)))
                .unwrap_or_default();
            match i.session.as_ref().and_then(|s| s.lease_until.as_deref()) {
                Some(l) => format!("{who} · lease {}", span_secs(until(l, now).unwrap_or(0))),
                None => who,
            }
        }
        Group::Waiting => match i.waiting.as_ref() {
            Some(w) if w.due => "check due".into(),
            Some(w) => match w.next_check_at.as_deref() {
                Some(t) => format!("check {}", relative(t, now)),
                None => "check".into(),
            },
            None => String::new(),
        },
        Group::Blocked => {
            let open: Vec<&str> = i
                .blocked_by
                .iter()
                .filter(|b| b.state != State::Closed)
                .map(|b| b.id.as_str())
                .collect();
            format!("← {}", open.join(", "))
        }
        Group::Ready if !i.blocks.is_empty() => format!("unblocks {}", i.blocks.len()),
        _ => String::new(),
    }
}

// ----------------------------------------------------------------------
// Panes
// ----------------------------------------------------------------------

fn header(ui: &Ui) -> Line<'static> {
    let c = &ui.snap.counts;
    let mut spans = vec![
        Span::styled(" trk ", bold().add_modifier(Modifier::REVERSED)),
        Span::styled(format!(" {}  ", ui.project), bold()),
    ];
    for (g, n) in [
        (Group::Attention, c.attention),
        (Group::Running, c.running),
        (Group::Waiting, c.waiting),
        (Group::Ready, c.ready),
        (Group::Blocked, c.blocked),
    ] {
        let style = if n == 0 {
            dim()
        } else {
            Style::new().fg(group_color(g))
        };
        spans.push(Span::styled(
            format!("{} {n} {}  ", group_glyph(g), g.label().to_lowercase()),
            style,
        ));
    }
    spans.push(Span::styled(format!("✓ {} closed", c.closed), dim()));

    let due = ui
        .snap
        .issues
        .iter()
        .filter(|i| i.waiting.as_ref().is_some_and(|w| w.due))
        .count();
    let checks = ui
        .snap
        .issues
        .iter()
        .filter(|i| i.waiting.as_ref().is_some_and(|w| w.check.is_some()))
        .count();
    spans.push(Span::raw("   "));
    match (&ui.snap.check_runner, ui.runner_alive) {
        (Some(r), alive) if alive != Some(false) => spans.push(Span::styled(
            format!(
                "checks: runner {} (pid {})",
                if r.busy { "running a check" } else { "idle" },
                r.pid
            ),
            Style::new().fg(Color::Green),
        )),
        _ if checks > 0 => spans.push(Span::styled(
            format!("checks: no runner, {due} due (trk check --watch)"),
            Style::new().fg(Color::Yellow),
        )),
        _ => spans.push(Span::styled("checks: none waiting", dim())),
    }
    if !ui.busy.is_empty() {
        spans.push(Span::styled(
            format!("   … {}", ui.busy.join(", ")),
            Style::new().fg(Color::Magenta),
        ));
    }
    Line::from(spans)
}

/// The line under an issue's title: who holds it, what it waits for, and
/// what stands in its way.
fn row_more(i: &DashIssue, ui: &Ui) -> String {
    let now = ui.now;
    let mut parts: Vec<String> = i.attention.iter().skip(1).cloned().collect();
    if let Some(w) = &i.waiting {
        if let Some(r) = &w.reason {
            parts.push(format!("for {}", first_line(r)));
        }
        if let Some(check) = &w.check {
            parts.push(format!("check `{}`", first_line(check)));
            if i.group == Group::Attention {
                parts.push(match w.next_check_at.as_deref() {
                    Some(t) if !w.due => format!("next {}", relative(t, now)),
                    _ => "due now".into(),
                });
            }
            if let Some(at) = &w.last_check_at {
                parts.push(match w.last_check_exit {
                    Some(75) => format!("not yet {}", relative(at, now)),
                    Some(code) => format!("exit {code} {}", relative(at, now)),
                    None => format!("ran {}", relative(at, now)),
                });
            }
        }
    }
    if i.wake_count > 0 {
        parts.push(format!("wakes {}/{}", i.wake_count, ui.snap.max_wakes));
    }
    if let Some(s) = &i.session {
        if s.actor.starts_with("agent:") {
            let mut who = match (&s.agent, &s.harness) {
                (Some(a), Some(h)) => format!("{a} ({h})"),
                (Some(a), None) => a.clone(),
                _ => short_actor(&s.actor),
            };
            if let Some(t) = &s.terminal {
                who.push_str(&format!(" in {t}"));
            }
            parts.push(who);
            parts.push(
                match (s.alive, s.mcp_alive) {
                    (Some(true), Some(false)) => "dormant (MCP stopped)",
                    (Some(true), Some(true)) => "session alive · MCP running",
                    (Some(true), None) => "session alive · MCP unknown",
                    (Some(false), _) => "session ended",
                    (None, Some(true)) => "session unknown · MCP running",
                    (None, Some(false)) => "session unknown · MCP stopped",
                    (None, None) => "session unknown · MCP unknown",
                }
                .into(),
            );
        } else {
            parts.push(format!("claimed by {}", short_actor(&s.actor)));
        }
        if s.active {
            parts.push(format!("since {}", relative(&s.claimed_at, now)));
            // The lease of an agent in flight is already at the right.
            if i.group != Group::Running
                && let Some(l) = &s.lease_until
            {
                let left = until(l, now).unwrap_or(0);
                parts.push(if left >= 0 {
                    format!("lease {}", span_secs(left))
                } else {
                    format!("lease expired {} ago", span_secs(left))
                });
            }
        }
    }
    let open: Vec<String> = i
        .blocked_by
        .iter()
        .filter(|b| b.state != State::Closed)
        .map(|b| format!("{} {} ({})", b.id, b.title, b.state))
        .collect();
    if !open.is_empty() {
        parts.push(format!("blocked by {}", open.join(", ")));
    }
    if !i.blocks.is_empty() {
        let ids: Vec<&str> = i.blocks.iter().map(|b| b.id.as_str()).collect();
        parts.push(format!("blocks {}", ids.join(", ")));
    }
    parts.join(" · ")
}

enum Row {
    Group(Group, usize),
    Issue(usize),
    /// The second line of an issue.
    More(String),
}

fn list(frame: &mut Frame, area: Rect, ui: &mut Ui) {
    let title = if ui.filter.is_empty() {
        format!(" Issues · {} ", ui.rows.len())
    } else {
        format!(
            " Issues · {} of {} · /{} ",
            ui.rows.len(),
            ui.snap.issues.len(),
            ui.filter
        )
    };
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if ui.rows.is_empty() {
        let text = if ui.snap.issues.is_empty() {
            "No open issues. Press c to create one."
        } else {
            "Nothing matches the filter. Esc clears it."
        };
        frame.render_widget(Paragraph::new(text).style(dim()), inner);
        return;
    }

    let mut rows = vec![];
    let mut selected_line = 0;
    let mut last = None;
    for (n, idx) in ui.rows.iter().enumerate() {
        let g = ui.snap.issues[*idx].group;
        if last != Some(g) {
            let count = ui
                .rows
                .iter()
                .filter(|i| ui.snap.issues[**i].group == g)
                .count();
            rows.push(Row::Group(g, count));
            last = Some(g);
        }
        if n == ui.selected {
            selected_line = rows.len();
        }
        rows.push(Row::Issue(n));
        let more = row_more(&ui.snap.issues[*idx], ui);
        if !more.is_empty() {
            rows.push(Row::More(more));
        }
    }

    // Keep the selection in view: both of its lines, and its group heading
    // when it is the first of its group.
    let height = inner.height as usize;
    let first_wanted = selected_line.saturating_sub(1);
    let last_wanted = match rows.get(selected_line + 1) {
        Some(Row::More(_)) => selected_line + 1,
        _ => selected_line,
    };
    if last_wanted >= ui.list_top + height {
        ui.list_top = last_wanted + 1 - height;
    }
    if first_wanted < ui.list_top {
        ui.list_top = first_wanted;
    }
    ui.list_top = ui.list_top.min(rows.len().saturating_sub(height));

    let width = inner.width as usize;
    let (id_width, type_width) = ui.rows.iter().fold((0, 0), |(id_width, type_width), idx| {
        let i = &ui.snap.issues[*idx];
        (
            id_width.max(i.id.width()),
            type_width.max(i.issue_type.width()),
        )
    });
    let title_indent = 2 + 2 + 1 + id_width + 2 + type_width + 2;
    let lines: Vec<Line> = rows
        .iter()
        .skip(ui.list_top)
        .take(height)
        .map(|row| match row {
            Row::Group(g, count) => Line::from(vec![
                Span::styled(
                    format!("{} {} ", group_glyph(*g), g.label().to_uppercase()),
                    Style::new()
                        .fg(group_color(*g))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{count}"), dim()),
            ]),
            Row::Issue(n) => {
                let i = &ui.snap.issues[ui.rows[*n]];
                let selected = *n == ui.selected;
                // A reversed row carries no colours of its own.
                let st = |s: Style| if selected { Style::new() } else { s };
                let info = fit(&row_info(i, ui.now), (width * 2 / 5).min(36));
                let room = width.saturating_sub(title_indent + info.width() + 1);
                // The chip gives way before the title does.
                let chip = link_chip(i)
                    .map(|c| format!("  {c}"))
                    .filter(|c| room >= c.width() + 16)
                    .unwrap_or_default();
                let title = fit(&i.title, room - chip.width());
                let pad = room.saturating_sub(title.width() + chip.width()) + 1;
                let info_style = if i.group == Group::Attention {
                    Style::new().fg(Color::Red)
                } else {
                    dim()
                };
                let line = Line::from(vec![
                    Span::raw(if selected { "▸ " } else { "  " }),
                    Span::styled(i.priority.clone(), st(priority_style(&i.priority))),
                    Span::raw(" "),
                    Span::styled(format!("{:<id_width$}", i.id), st(dim())),
                    Span::raw("  "),
                    Span::styled(format!("{:<type_width$}", i.issue_type), st(dim())),
                    Span::raw("  "),
                    Span::raw(title),
                    Span::styled(chip, st(Style::new().fg(Color::Cyan))),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(info, st(info_style)),
                ]);
                if selected {
                    line.style(Style::new().add_modifier(Modifier::REVERSED))
                } else {
                    line
                }
            }
            Row::More(more) => {
                // Under the title.
                let indent = title_indent;
                Line::from(vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled(fit(more, width.saturating_sub(indent)), dim()),
                ])
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn field(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<10}"), dim()),
        Span::raw(value.into()),
    ])
}

fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
    ))
}

fn edge(e: &Edge) -> Line<'static> {
    let (glyph, style) = match e.state {
        State::Closed => ("✓", dim()),
        State::Claimed => ("▶", Style::new().fg(Color::Green)),
        State::Waiting => ("◷", Style::new().fg(Color::Cyan)),
        State::Open => ("○", Style::new()),
    };
    Line::from(vec![
        Span::styled(format!("  {glyph} "), style),
        Span::styled(format!("{} ", e.id), dim()),
        Span::styled(format!("{} ", e.priority), priority_style(&e.priority)),
        Span::styled(e.title.clone(), style),
        Span::styled(format!("  {}", e.state), dim()),
    ])
}

/// Markdown shown as text: headings bold, code dimmed, the rest as written.
fn markdown(text: &str, out: &mut Vec<Line<'static>>) {
    let mut code = false;
    for l in text.lines() {
        if l.trim_start().starts_with("```") {
            code = !code;
            continue;
        }
        let style = if code {
            Style::new().fg(Color::Cyan)
        } else if l.starts_with('#') {
            bold()
        } else {
            Style::new()
        };
        out.push(Line::from(Span::styled(format!("  {l}"), style)));
    }
}

fn detail_lines(i: &DashIssue, detail: Option<&Detail>, ui: &Ui) -> Vec<Line<'static>> {
    let now = ui.now;
    let mut out = vec![
        Line::from(Span::styled(i.title.clone(), bold())),
        Line::from(vec![
            Span::styled(i.priority.clone(), priority_style(&i.priority)),
            Span::styled(format!(" · {} · ", i.issue_type), dim()),
            Span::styled(
                format!(
                    "{} {}",
                    group_glyph(i.group),
                    i.group.label().to_lowercase()
                ),
                Style::new().fg(group_color(i.group)),
            ),
            Span::styled(
                format!(" · {} · updated {}", i.state, relative(&i.updated_at, now)),
                dim(),
            ),
        ]),
    ];
    for why in &i.attention {
        out.push(Line::from(Span::styled(
            format!("! {why}"),
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }

    if !i.links.is_empty() {
        out.push(Line::default());
        out.push(section("Links"));
        for (n, l) in i.links.iter().enumerate() {
            let mut spans = vec![Span::styled("  ↗ ", Style::new().fg(Color::Cyan))];
            if let Some(label) = &l.label {
                spans.push(Span::raw(format!("{label}  ")));
            }
            spans.push(Span::styled(
                l.url.clone(),
                Style::new()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED),
            ));
            if n == 0 {
                spans.push(Span::styled("  (l opens it)", dim()));
            }
            out.push(Line::from(spans));
        }
    }

    if let Some(s) = &i.session {
        out.push(Line::default());
        let agent = s.actor.starts_with("agent:");
        out.push(section(match (agent, s.active) {
            (true, true) => "Session",
            (true, false) => "Session (ended its turn, waiting)",
            (false, true) => "Claim",
            (false, false) => "Claim (released to wait)",
        }));
        if agent {
            out.push(field(
                "agent",
                match (&s.agent, &s.harness) {
                    (Some(a), Some(h)) => format!("{a} ({h})"),
                    (Some(a), None) => a.clone(),
                    _ => short_actor(&s.actor),
                },
            ));
        }
        out.push(field("actor", s.actor.clone()));
        if let Some(t) = &s.terminal {
            out.push(field("terminal", t.clone()));
        }
        if let Some(w) = &s.worktree {
            out.push(field("worktree", tilde(w)));
        }
        out.push(field("claimed", relative(&s.claimed_at, now)));
        if s.active
            && let Some(l) = &s.lease_until
        {
            let left = until(l, now).unwrap_or(0);
            out.push(field(
                "lease",
                if left >= 0 {
                    format!("expires in {}", span_secs(left))
                } else {
                    format!("expired {} ago", span_secs(left))
                },
            ));
        }
        if agent {
            let (text, style) = match s.alive {
                Some(true) => ("running", Style::new().fg(Color::Green)),
                Some(false) => ("gone", Style::new().fg(Color::Red)),
                None => ("unknown (another host, or not started yet)", dim()),
            };
            out.push(Line::from(vec![
                Span::styled(format!("  {:<10}", "server"), dim()),
                Span::styled(text, style),
            ]));
        }
    }

    if let Some(w) = &i.waiting {
        out.push(Line::default());
        out.push(section("Waiting"));
        if let Some(r) = &w.reason {
            out.push(field("for", r.clone()));
        }
        match &w.check {
            None => out.push(field("check", "none: a person decides (x close, o reopen)")),
            Some(check) => {
                let mut lines = check.lines();
                out.push(Line::from(vec![
                    Span::styled(format!("  {:<10}", "check"), dim()),
                    Span::styled(
                        lines.next().unwrap_or("").to_string(),
                        Style::new().fg(Color::Cyan),
                    ),
                ]));
                for l in lines {
                    out.push(Line::from(Span::styled(
                        format!("  {:<10}{l}", ""),
                        Style::new().fg(Color::Cyan),
                    )));
                }
                let next = match w.next_check_at.as_deref() {
                    _ if w.due => "due now".to_string(),
                    Some(t) => relative(t, now),
                    None => "due now".to_string(),
                };
                let every = w
                    .interval
                    .map(|s| format!(" · every {}", fmt_duration(s)))
                    .unwrap_or_default();
                out.push(field("next", format!("{next}{every}")));
                if let Some(at) = &w.last_check_at {
                    let exit = match w.last_check_exit {
                        Some(75) => "not yet (exit 75)".to_string(),
                        Some(c) => format!("exit {c}"),
                        None => "ran".to_string(),
                    };
                    out.push(field("last", format!("{exit}, {}", relative(at, now))));
                }
                if let Some(cwd) = &w.cwd {
                    out.push(field("cwd", tilde(cwd)));
                }
            }
        }
    }
    if i.wake_count > 0 {
        out.push(field(
            "wakes",
            format!(
                "{} of {} failed-check wakes used",
                i.wake_count, ui.snap.max_wakes
            ),
        ));
    }

    if !i.blocked_by.is_empty() {
        out.push(Line::default());
        out.push(section("Blocked by"));
        out.extend(i.blocked_by.iter().map(edge));
    }
    if !i.blocks.is_empty() {
        out.push(Line::default());
        out.push(section("Blocks"));
        out.extend(i.blocks.iter().map(edge));
    }

    let Some(d) = detail else {
        return out;
    };
    out.push(Line::default());
    out.push(section("Description"));
    if d.description.trim().is_empty() {
        out.push(Line::from(Span::styled("  (none)", dim())));
    } else {
        markdown(&d.description, &mut out);
    }

    let notes: Vec<&Value> = d.notes.iter().filter(|n| n["body"].is_string()).collect();
    if !notes.is_empty() {
        out.push(Line::default());
        out.push(section(&format!("Notes · {} · newest first", notes.len())));
        for (nth, n) in notes.iter().rev().enumerate() {
            if nth > 0 {
                out.push(Line::default());
            }
            out.push(Line::from(vec![
                Span::styled(
                    format!(
                        "  {} · {}",
                        relative(n["at"].as_str().unwrap_or(""), now),
                        short_actor(n["actor"].as_str().unwrap_or(""))
                    ),
                    Style::new().fg(Color::Yellow),
                ),
                Span::styled(
                    if n["to_agent"] == true {
                        " · to agent"
                    } else {
                        ""
                    },
                    dim(),
                ),
            ]));
            markdown(n["body"].as_str().unwrap_or(""), &mut out);
        }
    }

    out.push(Line::default());
    out.push(section("History · newest first"));
    for e in d.events.iter().rev().filter(|e| e.op != "note") {
        out.push(Line::from(vec![
            Span::styled(format!("  {:>8}  ", relative(&e.at, now)), dim()),
            Span::raw(describe(e)),
            Span::styled(format!("  {}", short_actor(&e.actor)), dim()),
        ]));
    }
    out.push(field(
        "created",
        format!("{} by {}", relative(&i.created_at, now), d.created_by),
    ));
    out
}

fn detail(frame: &mut Frame, area: Rect, ui: &mut Ui) {
    let Some(i) = ui.issue() else {
        frame.render_widget(Block::bordered().title(" Details "), area);
        return;
    };
    let loaded = ui
        .detail
        .as_ref()
        .filter(|(id, _)| *id == i.id)
        .map(|(_, d)| d);
    let lines = detail_lines(i, loaded, ui);
    let block = Block::bordered().title(format!(" {} ", i.id));
    let inner = block.inner(area);
    // Wrapping only adds lines, so this bound never hides the end.
    let max = (lines.len() as u16).saturating_sub(inner.height.min(3));
    ui.detail_scroll = ui.detail_scroll.min(max);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((ui.detail_scroll, 0))
            .block(block),
        area,
    );
}

fn activity(frame: &mut Frame, area: Rect, ui: &Ui) {
    let block = Block::bordered().title(" Activity ");
    let inner = block.inner(area);
    let width = inner.width as usize;
    let lines: Vec<Line> = ui
        .snap
        .activity
        .iter()
        .take(inner.height as usize)
        .map(|e| {
            let when = format!("{:>8}  ", relative(&e.at, ui.now));
            let id = format!("{}  ", e.issue_id.as_deref().unwrap_or(""));
            let who = format!("  {}", short_actor(&e.actor));
            let room = width.saturating_sub(when.width() + id.width() + who.width());
            let color = match e.op.as_str() {
                "check" if e.payload["result"] == "passed" => Color::Green,
                "check" => Color::Red,
                "close" => Color::Green,
                "claim" => Color::Cyan,
                "note" => Color::Yellow,
                _ => Color::Reset,
            };
            Line::from(vec![
                Span::styled(when, dim()),
                Span::styled(id, dim()),
                Span::styled(fit(&describe(e), room), Style::new().fg(color)),
                Span::styled(who, dim()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn hints(ui: &Ui) -> String {
    let mut keys = vec!["j/k move"];
    keys.push(if ui.zoom { "⏎ list" } else { "⏎ details" });
    if let Some(i) = ui.issue() {
        if runnable(i) {
            keys.push("r run");
        }
        if i.session.as_ref().is_some_and(|s| s.terminal.is_some()) {
            keys.push("a session");
            keys.push("s send");
        }
        if !i.links.is_empty() {
            keys.push("l link");
        }
        keys.push("m note");
        keys.push("x close");
        if i.state == State::Waiting {
            keys.push("v resume");
            keys.push("o reopen");
        }
        if i.state == State::Claimed {
            keys.push("u unclaim");
        }
        keys.push("p priority");
        keys.push("e edit");
    }
    keys.extend(["c new", "/ filter", "? help", "q quit"]);
    format!(" {}", keys.join("  "))
}

fn footer(frame: &mut Frame, area: Rect, ui: &Ui) {
    let prompt = |label: &str, text: &str| -> (Line<'static>, usize) {
        let label = format!(" {label} ");
        let x = label.width() + text.width();
        (
            Line::from(vec![
                Span::styled(label, bold().add_modifier(Modifier::REVERSED)),
                Span::raw(format!(" {text}")),
            ]),
            x + 1,
        )
    };
    let typed = match &ui.mode {
        Mode::Filter => Some(prompt("filter", &ui.filter)),
        Mode::Input { kind, text } => Some(prompt(
            match kind {
                Input::Note => "note",
                Input::NoteToAgent => "note to agent",
                Input::Send => "send text only",
                Input::Resume { .. } => "resume + send (Enter submit · Esc cancel)",
            },
            text,
        )),
        _ => None,
    };
    if let Some((line, x)) = typed {
        frame.render_widget(Paragraph::new(line), area);
        frame.set_cursor_position(Position::new(
            area.x + (x as u16).min(area.width.saturating_sub(1)),
            area.y,
        ));
        return;
    }
    let line = match &ui.status {
        Some(s) => Line::from(Span::styled(
            format!(" {}", first_line(&s.text)),
            if s.error {
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(Color::Green)
            },
        )),
        None => Line::from(Span::styled(hints(ui), dim())),
    };
    frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), area);
}

fn popup(frame: &mut Frame, title: &str, lines: Vec<Line<'static>>) {
    let area = frame.area();
    let width = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .max(title.width() + 2) as u16
        + 4;
    let width = width.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 3,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(format!(" {title} "))
                .border_style(Style::new().fg(Color::Yellow))
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        rect,
    );
}

const HELP: &[(&str, &str)] = &[
    ("j k ↑ ↓", "move the selection"),
    ("g G", "first, last issue"),
    (
        "Enter",
        "open the details: description, notes, history; again to go back",
    ),
    ("j k  PgDn PgUp", "scroll the details when they are open"),
    ("/", "filter by id, title, priority or group; Esc clears"),
    ("", ""),
    ("r", "run: start an agent session on a ready issue"),
    ("a", "go to the session's terminal (Herdr pane, or attach)"),
    ("s", "send text only; does not restore a waiting claim"),
    (
        "v",
        "resume waiting: restore original ownership + send instructions",
    ),
    ("l", "open the issue's first link in the browser"),
    ("m  M", "add a note; M queues it for the agent"),
    ("x", "close as done or abandoned"),
    ("o", "reopen a waiting issue"),
    ("u", "unclaim: release the claim, open again"),
    ("R", "reap: release claims whose session has ended"),
    ("p", "set the priority"),
    ("e  c", "edit this issue, create one ($EDITOR)"),
    ("", ""),
    ("?", "this help"),
    ("q  Ctrl-C", "quit"),
];

fn overlay(frame: &mut Frame, ui: &Ui) {
    let id = ui.issue().map(|i| i.id.clone()).unwrap_or_default();
    let keys = |text: &str| Line::from(Span::styled(text.to_string(), dim()));
    match &ui.mode {
        Mode::Help => popup(
            frame,
            "Keys",
            HELP.iter()
                .map(|(k, what)| {
                    Line::from(vec![
                        Span::styled(format!("{k:<16}"), Style::new().fg(Color::Yellow)),
                        Span::raw(what.to_string()),
                    ])
                })
                .collect(),
        ),
        Mode::Confirm(c) => {
            let (title, text) = match c {
                Confirm::Run => ("Run", format!("Start an agent session on {id}?")),
                Confirm::Reopen => ("Reopen", format!("Return {id} to open and drop its check?")),
                Confirm::Unclaim => (
                    "Unclaim",
                    format!("Release the claim on {id}? Its session is not stopped."),
                ),
                Confirm::Reap => (
                    "Reap",
                    "Release every claim whose session has ended?".to_string(),
                ),
            };
            popup(
                frame,
                title,
                vec![
                    Line::from(text),
                    Line::default(),
                    keys("y yes    n / Esc no"),
                ],
            );
        }
        Mode::CloseReason => {
            let mut lines = vec![Line::from(format!("Close {id} as:"))];
            if ui.issue().is_some_and(|i| i.state == State::Claimed) {
                lines.push(Line::from(Span::styled(
                    "It is claimed: closing overrides the claimant.",
                    Style::new().fg(Color::Red),
                )));
            }
            lines.push(Line::default());
            lines.push(keys("d done    a abandoned    Esc cancel"));
            popup(frame, "Close", lines);
        }
        Mode::Priority => popup(
            frame,
            "Priority",
            vec![
                Line::from(format!("Priority of {id}:")),
                Line::default(),
                keys("0 urgent   1 high   2 normal   3 low   Esc cancel"),
            ],
        ),
        _ => {}
    }
}

pub fn draw(frame: &mut Frame, ui: &mut Ui) {
    let area = frame.area();
    let activity_height = match area.height {
        _ if ui.zoom => 0,
        0..18 => 0,
        18..30 => 6,
        _ => 9,
    };
    // Leave room for actionable command-error hints rather than clipping
    // them off the right edge of the terminal.
    let footer_height =
        if matches!(ui.mode, Mode::Normal) && ui.status.as_ref().is_some_and(|s| s.error) {
            4.min((area.height / 3).max(1))
        } else {
            1
        };
    let [top, body, feed, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(activity_height),
        Constraint::Length(footer_height),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(header(ui)), top);
    if ui.zoom {
        detail(frame, body, ui);
    } else {
        list(frame, body, ui);
    }
    if activity_height > 0 {
        activity(frame, feed, ui);
    }
    footer(frame, bottom, ui);
    overlay(frame, ui);
}
