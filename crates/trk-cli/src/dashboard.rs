//! `trk dashboard`: a live overview of the project in the terminal.
//!
//! The view is `trk_core::dashboard`'s snapshot, re-read when the database
//! changes. Quick writes (note, close, reopen, priority) go through the
//! store here; commands that take seconds or own the terminal (`run`,
//! `send`, `edit`, `create`, attach) run as `trk` itself, so they behave
//! exactly as they do from the shell.

mod ui;

use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use serde_json::Value;
use trk_core::dashboard::Dashboard;
use trk_core::error::{Code, Error, Result};
use trk_core::ops::Edit;
use trk_core::{State, Store};

use crate::env::{Env, hostname};
use crate::render::Out;
use crate::run;
use ui::{Confirm, Input, Mode, Status, Ui};

/// Events shown in the activity pane.
const ACTIVITY: usize = 30;
/// How often to look for changes, and to re-read regardless (clocks, session liveness).
const POLL: Duration = Duration::from_millis(250);
const REFRESH: Duration = Duration::from_secs(2);
const STATUS_TTL: Duration = Duration::from_secs(8);

/// Whether a process exists on this host. EPERM means it does.
fn pid_exists(pid: i64) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    (unsafe { libc::kill(pid, 0) }) == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Liveness of a session's MCP server from its actor metadata, as
/// `run::mcp_alive` tells it, without a process per session.
fn session_alive(host: Option<&str>, meta: &Value) -> Option<bool> {
    if meta["mcp_exited_at"].is_string() {
        return Some(false);
    }
    let pid = meta["mcp_pid"].as_i64()?;
    if let Some(h) = meta["mcp_host"].as_str()
        && Some(h) != host
    {
        return None;
    }
    Some(pid_exists(pid))
}

pub fn snapshot(store: &Store, host: Option<&str>) -> Result<Dashboard> {
    store.read(None, |c| {
        c.dashboard(ACTIVITY, &|meta| session_alive(host, meta))
    })
}

struct Job {
    label: String,
    result: std::result::Result<String, String>,
}

struct App<'a> {
    env: &'a Env,
    store: Store,
    host: Option<String>,
    ui: Ui,
    data_version: i64,
    refreshed: Instant,
    status_at: Instant,
    jobs: mpsc::Receiver<Job>,
    jobs_tx: mpsc::Sender<Job>,
    quit: bool,
}

/// `trk dashboard`. Without a terminal, or with a JSON format, it prints the
/// snapshot once.
pub fn dashboard(env: &Env, json: bool) -> Result<Out> {
    let store = env.store()?;
    let host = hostname();
    let snap = snapshot(&store, host.as_deref())?;
    if json || !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        return Ok(Out::Data(serde_json::to_value(&snap)?));
    }
    let project = env
        .root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let (jobs_tx, jobs) = mpsc::channel();
    let mut app = App {
        ui: Ui::new(project, snap, store.now()),
        env,
        store,
        host,
        data_version: 0,
        refreshed: Instant::now(),
        status_at: Instant::now(),
        jobs,
        jobs_tx,
        quit: false,
    };
    app.data_version = app.read_version();
    app.after_snapshot();
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result.map(|()| Out::Silent(Value::Null))
}

impl App<'_> {
    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.tick();
            terminal.draw(|f| ui::draw(f, &mut self.ui))?;
            if event::poll(POLL)?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.key(key, terminal)?;
            }
        }
        Ok(())
    }

    /// Changes whenever another connection commits.
    fn read_version(&self) -> i64 {
        self.store
            .conn()
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap_or(0)
    }

    fn tick(&mut self) {
        while let Ok(job) = self.jobs.try_recv() {
            self.ui.busy.retain(|l| *l != job.label);
            match job.result {
                Ok(text) => self.say(text),
                Err(text) => self.fail(text),
            }
            self.refresh();
        }
        if self.ui.status.is_some() && self.status_at.elapsed() > STATUS_TTL {
            self.ui.status = None;
        }
        let version = self.read_version();
        if version != self.data_version || self.refreshed.elapsed() >= REFRESH {
            self.data_version = version;
            self.refresh();
        }
        self.ui.now = self.store.now();
    }

    fn refresh(&mut self) {
        self.refreshed = Instant::now();
        match snapshot(&self.store, self.host.as_deref()) {
            Ok(snap) => {
                let keep = self.ui.issue().map(|i| i.id.clone());
                self.ui.snap = snap;
                self.ui.refilter(keep.as_deref());
                self.after_snapshot();
            }
            Err(e) => self.fail(e.message()),
        }
    }

    /// Load what the snapshot leaves out: the selected issue's text, and
    /// whether the check runner still exists.
    fn after_snapshot(&mut self) {
        self.ui.now = self.store.now();
        self.ui.runner_alive = self
            .ui
            .snap
            .check_runner
            .as_ref()
            .and_then(|r| (r.host.is_none() || r.host == self.host).then(|| pid_exists(r.pid)));
        self.load_detail();
    }

    fn load_detail(&mut self) {
        let Some(id) = self.ui.issue().map(|i| i.id.clone()) else {
            self.ui.detail = None;
            return;
        };
        if self.ui.detail.as_ref().is_none_or(|(d, _)| *d != id) {
            self.ui.detail_scroll = 0;
        }
        self.ui.detail = self
            .store
            .read(None, |c| c.dashboard_detail(&id))
            .ok()
            .map(|d| (id, d));
    }

    fn say(&mut self, text: impl Into<String>) {
        self.status_at = Instant::now();
        self.ui.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.status_at = Instant::now();
        self.ui.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    fn selected(&self) -> Option<(String, State)> {
        self.ui.issue().map(|i| (i.id.clone(), i.state))
    }

    // ------------------------------------------------------------------
    // Keys
    // ------------------------------------------------------------------

    fn key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return Ok(());
        }
        match self.ui.mode.clone() {
            Mode::Normal => self.normal_key(key, terminal)?,
            Mode::Help => self.ui.mode = Mode::Normal,
            Mode::Filter => match key.code {
                KeyCode::Esc => {
                    self.ui.filter.clear();
                    self.ui.mode = Mode::Normal;
                    self.refilter();
                }
                KeyCode::Enter => self.ui.mode = Mode::Normal,
                KeyCode::Backspace => {
                    self.ui.filter.pop();
                    self.refilter();
                }
                KeyCode::Char(c) => {
                    self.ui.filter.push(c);
                    self.refilter();
                }
                _ => {}
            },
            Mode::Input { kind, mut text } => match key.code {
                KeyCode::Esc => self.ui.mode = Mode::Normal,
                KeyCode::Enter => {
                    self.ui.mode = Mode::Normal;
                    if !text.trim().is_empty() {
                        self.submit(kind, text);
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.ui.mode = Mode::Input { kind, text };
                }
                KeyCode::Char(c) => {
                    text.push(c);
                    self.ui.mode = Mode::Input { kind, text };
                }
                _ => {}
            },
            Mode::Confirm(what) => {
                self.ui.mode = Mode::Normal;
                if matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter) {
                    self.confirmed(what);
                }
            }
            Mode::CloseReason => {
                self.ui.mode = Mode::Normal;
                let reason = match key.code {
                    KeyCode::Char('d') => "done",
                    KeyCode::Char('a') => "abandoned",
                    _ => return Ok(()),
                };
                if let Some((id, state)) = self.selected() {
                    // The prompt said so when the issue is claimed.
                    let force = state == State::Claimed;
                    self.write(format!("closed {id} ({reason})"), |c| {
                        c.close(&id, reason, None, force)
                    });
                }
            }
            Mode::Priority => {
                self.ui.mode = Mode::Normal;
                if let KeyCode::Char(c @ '0'..='3') = key.code
                    && let Some((id, _)) = self.selected()
                {
                    let priority = format!("P{c}");
                    self.write(format!("{id} is now {priority}"), |c| {
                        c.edit(
                            &id,
                            Edit {
                                priority: Some(priority.clone()),
                                ..Default::default()
                            },
                        )
                        .map(|_| ())
                    });
                }
            }
        }
        Ok(())
    }

    fn refilter(&mut self) {
        let keep = self.ui.issue().map(|i| i.id.clone());
        self.ui.refilter(keep.as_deref());
        self.load_detail();
    }

    fn select(&mut self, to: usize) {
        let to = to.min(self.ui.rows.len().saturating_sub(1));
        if to != self.ui.selected {
            self.ui.selected = to;
            self.load_detail();
        }
    }

    fn scroll(&mut self, by: i32) {
        self.ui.detail_scroll = self.ui.detail_scroll.saturating_add_signed(by as i16);
    }

    fn normal_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> Result<()> {
        let page = (terminal.size()?.height / 2).max(1) as i32;
        // With the details full screen, the plain movement keys scroll them.
        let zoom = self.ui.zoom;
        let issue = self.selected();
        let session_terminal = self
            .ui
            .issue()
            .and_then(|i| i.session.as_ref())
            .is_some_and(|s| s.terminal.is_some());
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc if zoom => self.ui.zoom = false,
            KeyCode::Esc if !self.ui.filter.is_empty() => {
                self.ui.filter.clear();
                self.refilter();
            }
            KeyCode::Esc => self.ui.status = None,
            KeyCode::Enter => self.ui.zoom = !zoom && issue.is_some(),
            KeyCode::Char('?') => self.ui.mode = Mode::Help,
            KeyCode::Char('/') => {
                self.ui.zoom = false;
                self.ui.mode = Mode::Filter;
            }
            KeyCode::Char('j') | KeyCode::Down if zoom => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up if zoom => self.scroll(-1),
            KeyCode::Char('j') | KeyCode::Down => self.select(self.ui.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.ui.selected.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home if zoom => self.ui.detail_scroll = 0,
            KeyCode::Char('G') | KeyCode::End if zoom => self.ui.detail_scroll = u16::MAX,
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Char('J') | KeyCode::PageDown | KeyCode::Char(' ') => self.scroll(page),
            KeyCode::Char('K') | KeyCode::PageUp => self.scroll(-page),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll(page)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll(-page)
            }
            KeyCode::Char('R') => self.ui.mode = Mode::Confirm(Confirm::Reap),
            KeyCode::Char('c') => {
                self.shell(terminal, &["create"])?;
            }
            _ => {
                let Some((id, state)) = issue else {
                    return Ok(());
                };
                match key.code {
                    KeyCode::Char('r') => match self.ui.issue().filter(|i| !ui::runnable(i)) {
                        None => self.ui.mode = Mode::Confirm(Confirm::Run),
                        Some(i) if i.state == State::Open => {
                            self.fail(format!("{id} is blocked: close its blockers first"))
                        }
                        Some(_) => self.fail(format!("{id} is {state}: only open issues can run")),
                    },
                    KeyCode::Char('a') if session_terminal => self.go_to_session(terminal, &id)?,
                    KeyCode::Char('s') if session_terminal => {
                        self.ui.mode = Mode::Input {
                            kind: Input::Send,
                            text: String::new(),
                        }
                    }
                    KeyCode::Char('a' | 's') => self.fail(format!("{id} has no session terminal")),
                    KeyCode::Char('m') => {
                        self.ui.mode = Mode::Input {
                            kind: Input::Note,
                            text: String::new(),
                        }
                    }
                    KeyCode::Char('M') => {
                        self.ui.mode = Mode::Input {
                            kind: Input::NoteToAgent,
                            text: String::new(),
                        }
                    }
                    KeyCode::Char('x') => self.ui.mode = Mode::CloseReason,
                    KeyCode::Char('o') if state == State::Waiting => {
                        self.ui.mode = Mode::Confirm(Confirm::Reopen)
                    }
                    KeyCode::Char('o') => self.fail(format!("{id} is {state}: nothing to reopen")),
                    KeyCode::Char('u') if state == State::Claimed => {
                        self.ui.mode = Mode::Confirm(Confirm::Unclaim)
                    }
                    KeyCode::Char('u') => self.fail(format!("{id} is not claimed")),
                    KeyCode::Char('p') => self.ui.mode = Mode::Priority,
                    KeyCode::Char('e') => {
                        self.shell(terminal, &["edit", &id])?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Actions
    // ------------------------------------------------------------------

    /// One write transaction as the person, then a fresh snapshot.
    fn write(&mut self, done: String, f: impl FnOnce(&mut trk_core::Ctx) -> Result<()>) {
        let result = self
            .env
            .actor()
            .map(str::to_string)
            .and_then(|actor| self.store.write(&actor, false, f));
        match result {
            Ok(()) => self.say(done),
            Err(e) => self.fail(error_text(&e)),
        }
        self.refresh();
    }

    fn submit(&mut self, kind: Input, text: String) {
        let Some((id, _)) = self.selected() else {
            return;
        };
        match kind {
            Input::Note | Input::NoteToAgent => {
                let to_agent = kind == Input::NoteToAgent;
                let done = if to_agent {
                    format!("note on {id} queued for the agent")
                } else {
                    format!("note added to {id}")
                };
                self.write(done, |c| c.note(&id, &text, None, to_agent).map(|_| ()));
            }
            Input::Send => self.background(
                format!("sending to {id}"),
                vec!["send".into(), id.clone(), text],
                format!("sent to the session of {id}"),
            ),
        }
    }

    fn confirmed(&mut self, what: Confirm) {
        if what == Confirm::Reap {
            match run::reap(self.env) {
                Ok(Out::Text(text, _)) => self.say(text.replace('\n', "; ")),
                Ok(_) => {}
                Err(e) => self.fail(error_text(&e)),
            }
            self.refresh();
            return;
        }
        let Some((id, _)) = self.selected() else {
            return;
        };
        match what {
            Confirm::Run => self.background(
                format!("starting {id}"),
                vec!["run".into(), id.clone()],
                format!("session started on {id}"),
            ),
            Confirm::Reopen => {
                self.write(format!("{id} is open again"), |c| c.reopen(&id, None));
            }
            Confirm::Unclaim => {
                self.write(format!("claim on {id} released"), |c| {
                    c.unclaim(&id, true, None)
                });
            }
            Confirm::Reap => {}
        }
    }

    fn trk(&self) -> Command {
        let exe = std::env::current_exe().unwrap_or_else(|_| "trk".into());
        let mut cmd = Command::new(exe);
        cmd.env("TRK_DIR", self.env.dir_env())
            .env_remove("TRK_FORMAT");
        if let Some(actor) = &self.env.actor {
            cmd.env("TRK_ACTOR", actor);
        }
        cmd
    }

    /// Run `trk <args> --json` off the UI thread and report how it went.
    fn background(&mut self, label: String, args: Vec<String>, done: String) {
        let mut cmd = self.trk();
        cmd.args(&args)
            .arg("--json")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.ui.busy.push(label.clone());
        let tx = self.jobs_tx.clone();
        std::thread::spawn(move || {
            let result = match cmd.output() {
                Err(e) => Err(format!("cannot run trk: {e}")),
                Ok(out) => {
                    let reply: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
                    if out.status.success() && reply["ok"] == true {
                        match reply["data"]["warnings"][0].as_str() {
                            Some(w) => Ok(format!("{done} (warning: {w})")),
                            None => Ok(done),
                        }
                    } else {
                        let e = &reply["error"];
                        Err(match (e["message"].as_str(), e["hint"].as_str()) {
                            (Some(m), Some(h)) => format!("{m} ({h})"),
                            (Some(m), None) => m.to_string(),
                            _ => String::from_utf8_lossy(&out.stderr)
                                .lines()
                                .last()
                                .unwrap_or("trk failed")
                                .to_string(),
                        })
                    }
                }
            };
            let _ = tx.send(Job { label, result });
        });
    }

    /// Give the terminal to `f`, then take it back.
    fn suspended<T>(
        &mut self,
        terminal: &mut DefaultTerminal,
        f: impl FnOnce(&Self) -> T,
    ) -> Result<T> {
        disable_raw_mode()?;
        execute!(std::io::stdout(), LeaveAlternateScreen)?;
        let out = f(self);
        enable_raw_mode()?;
        execute!(std::io::stdout(), EnterAlternateScreen)?;
        terminal.clear()?;
        // Keys typed into the other program are not for the dashboard.
        while event::poll(Duration::ZERO)? {
            event::read()?;
        }
        Ok(out)
    }

    /// Run an interactive `trk` command (`edit`, `create`) in the terminal.
    fn shell(&mut self, terminal: &mut DefaultTerminal, args: &[&str]) -> Result<()> {
        let ok = self.suspended(terminal, |app| {
            let ok = app.trk().args(args).status().is_ok_and(|s| s.success());
            if !ok {
                // The error is on the normal screen: hold it there.
                print!("\nPress Enter to return to the dashboard ");
                let _ = std::io::stdout().flush();
                let _ = std::io::stdin().read_line(&mut String::new());
            }
            ok
        })?;
        if ok {
            self.say(format!("trk {} done", args[0]));
        }
        self.refresh();
        Ok(())
    }

    /// Show the session: its own pane when the dashboard runs inside the
    /// Herdr window (a second view of the same terminal would fight over
    /// its size), otherwise attach to it here.
    fn go_to_session(&mut self, terminal: &mut DefaultTerminal, id: &str) -> Result<()> {
        let name = self
            .ui
            .issue()
            .and_then(|i| i.session.as_ref())
            .and_then(|s| s.terminal.clone())
            .unwrap_or_default();
        let herdr = run::launcher(self.env).is_ok_and(|l| l.herdr);
        if herdr {
            match crate::herdr::focus(&name) {
                Ok(true) => {
                    self.say(format!("showing the pane of {id} ({name})"));
                    return Ok(());
                }
                Ok(false) => {}
                Err(e) => {
                    self.fail(e);
                    return Ok(());
                }
            }
        }
        let attached = self.suspended(terminal, |app| run::attach(app.env, id))?;
        match attached {
            Ok(_) => self.say(format!("detached from {id}")),
            Err(e) => self.fail(error_text(&e)),
        }
        self.refresh();
        Ok(())
    }
}

fn error_text(e: &Error) -> String {
    let hint = match e.code() {
        // The dashboard has no flags to pass.
        Code::NotClaimant => None,
        _ => e.hint(),
    };
    match hint {
        Some(h) => format!("{} ({h})", e.message()),
        None => e.message(),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use trk_core::ops::{ClaimOpts, NewIssue, WaitOpts};
    use trk_core::{Store, StoreOpts};
    use unicode_width::UnicodeWidthStr;

    use super::ui::{self, Mode, Ui};

    const HUMAN: &str = "human:alice@example.com";
    const AGENT: &str = "agent:omp:0123456789";

    fn screen(ui: &mut Ui, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| ui::draw(f, ui)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let opts = StoreOpts {
            now: Some("2026-11-20T10:00:00Z".parse().unwrap()),
            prefix: "t".into(),
            ..Default::default()
        };
        let mut s = Store::install(&dir.path().join("trk.db"), opts).unwrap();
        let mut create = |title: &str| {
            s.write(HUMAN, false, |c| {
                c.create(NewIssue {
                    title: title.into(),
                    description: "# Why\nBecause.\n```\ncode\n```".into(),
                    ..Default::default()
                })
            })
            .unwrap()
        };
        let (base, blocked, running, landing) = (
            create("Lay the foundation"),
            create("Build the roof"),
            create("Paint the walls"),
            create("Fit the windows"),
        );
        s.write(HUMAN, false, |c| c.dep_add(&base, &blocked))
            .unwrap();
        for id in [&running, &landing] {
            s.write(AGENT, false, |c| {
                c.claim(
                    id,
                    ClaimOpts {
                        agent: Some("implementer".into()),
                        terminal: Some("t7".into()),
                        ..Default::default()
                    },
                )
                .map(|_| ())
            })
            .unwrap();
        }
        s.write(AGENT, false, |c| {
            c.wait(
                &landing,
                WaitOpts {
                    check: Some("git merge --ff-only windows".into()),
                    reason: Some("the merge into main".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
        (dir, s)
    }

    fn ui_of(s: &Store) -> Ui {
        let snap = super::snapshot(s, None).unwrap();
        let mut ui = Ui::new("house".into(), snap, s.now());
        let id = ui.issue().unwrap().id.clone();
        let detail = s.read(None, |c| c.dashboard_detail(&id)).unwrap();
        ui.detail = Some((id, detail));
        ui
    }

    #[test]
    fn draws_groups_with_their_details_in_the_list_and_activity() {
        let (_d, s) = sample();
        let mut ui = ui_of(&s);
        for (i, issue_type) in
            ui.snap
                .issues
                .iter_mut()
                .zip(["code-change", "bug", "feature", "task"])
        {
            i.issue_type = issue_type.into();
        }
        let text = screen(&mut ui, 140, 40);
        for want in [
            "house",
            "▶ IN FLIGHT 1",
            "◷ WAITING 1",
            "○ READY 1",
            "⊘ BLOCKED 1",
            "Paint the walls",
            "implementer · lease 30m",
            "implementer (omp) in t7 · server unknown · since 0s ago",
            "check due",
            "for the merge into main · check `git merge --ff-only windows`",
            "unblocks 1",
            "blocked by t-",
            "Lay the foundation (open)",
            "checks: no runner, 1 due",
            "Activity",
            "waiting on a check: the merge into main",
            "omp:012345",
            "a session",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(text.contains("▸ P2"));
        for i in &ui.snap.issues {
            let prefix = format!("{} {} ", i.priority, i.id);
            let row = text.lines().find(|line| line.contains(&prefix)).unwrap();
            assert!(
                row.contains(&format!("{}  {:<11}  {}", i.id, i.issue_type, i.title)),
                "id, type and title in order:\n{row}"
            );
        }
        let title_column = text
            .lines()
            .find_map(|line| {
                line.split_once("Paint the walls")
                    .map(|(prefix, _)| prefix.width())
            })
            .unwrap();
        let more_column = text
            .lines()
            .find_map(|line| {
                line.split_once("implementer (omp)")
                    .map(|(prefix, _)| prefix.width())
            })
            .unwrap();
        assert_eq!(
            title_column, more_column,
            "continuation aligned under title"
        );
        // The list has the whole width: the issue's text is behind Enter.
        assert!(!text.contains("Description"));
    }

    #[test]
    fn enter_opens_the_details_with_description_and_history() {
        let (_d, s) = sample();
        let mut ui = ui_of(&s);
        ui.zoom = true;
        let text = screen(&mut ui, 100, 40);
        for want in [
            "Session",
            "terminal  t7",
            "Description",
            "Because.",
            "History · newest first",
            "claimed (implementer)",
            "⏎ list",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        // No list, no activity pane, and the code fence markers are not shown.
        assert!(!text.contains("IN FLIGHT 1") && !text.contains("Activity"));
        assert!(!text.contains("```"));
    }

    #[test]
    fn the_selection_stays_in_view_with_both_of_its_lines() {
        let (_d, s) = sample();
        let mut ui = ui_of(&s);
        ui.selected = ui.rows.len() - 1;
        let text = screen(&mut ui, 90, 9);
        assert!(text.contains("▸ P2") && text.contains("Build the roof"));
        assert!(text.contains("blocked by t-"), "second line shown:\n{text}");
        ui.selected = 0;
        let text = screen(&mut ui, 90, 9);
        assert!(text.contains("▶ IN FLIGHT 1") && text.contains("Paint the walls"));
    }

    #[test]
    fn filter_keeps_the_selection_and_prompts_are_drawn() {
        let (_d, s) = sample();
        let mut ui = ui_of(&s);
        ui.selected = 2;
        let keep = ui.issue().unwrap().id.clone();
        ui.filter = "the".into();
        ui.refilter(Some(&keep));
        assert_eq!(ui.issue().unwrap().id, keep);
        ui.filter = "roof".into();
        ui.refilter(Some(&keep));
        assert_eq!(ui.rows.len(), 1);
        assert_eq!(ui.issue().unwrap().title, "Build the roof");
        ui.mode = Mode::Filter;
        let text = screen(&mut ui, 120, 30);
        assert!(text.contains("Issues · 1 of 4 · /roof"));
        assert!(text.contains("← t-"), "blocker shown:\n{text}");
        ui.mode = Mode::CloseReason;
        assert!(screen(&mut ui, 120, 30).contains("d done    a abandoned"));
        ui.mode = Mode::Help;
        assert!(screen(&mut ui, 120, 30).contains("start an agent session"));
        ui.filter = "nothing-like-this".into();
        ui.refilter(None);
        ui.mode = Mode::Normal;
        assert!(screen(&mut ui, 120, 30).contains("Nothing matches the filter"));
    }
}
