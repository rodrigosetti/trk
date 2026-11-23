// trk dashboard for Herdr: the issues of the shown space's project, what needs
// you, live sessions and waiting checks. The data is `trk dashboard --json`
// (the snapshot the terminal dashboard draws); every action runs `trk` itself.
//
// Props: { folder?: string, select?: string, focus?: "agent", launcher?: boolean }
//   folder  show this project instead of the shown space's.
//   select  the issue to start on, with its detail open.
//   focus   "agent": selecting an issue shows its session in the layout's
//           agent block (`{ "agent": "focused" }`), as in layouts/trk.json.
//   launcher  only the sidebar button; opens or focuses this space's trk panel.

import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextView } from "gpui-base";
import {
  exec, spaces, panes, agents, terminals, theme, props, run, notify, checking,
  shared, setShared, setBadge, setAgentOrder, nextCommand, watchFolder, layout,
} from "herdr";

/** The envelope version of `trk --json` this mod was written against. */
const SCHEMA = 4;
const GROUPS = [
  { id: "attention", label: "Needs you", color: "blocked" },
  { id: "running", label: "In flight", color: "working" },
  { id: "waiting", label: "Waiting", color: "done" },
  { id: "ready", label: "Ready", color: "foreground" },
  { id: "blocked", label: "Blocked", color: "dim" },
];
const PRIORITIES = ["P0", "P1", "P2", "P3"];
const PROMPTS = {
  create: { label: "New issue", placeholder: "Title", submit: "Create" },
  note: { label: "Note", placeholder: "What happened", submit: "Add note" },
  agent: { label: "Note to the agent", placeholder: "Goes to the session's inbox", submit: "Send note" },
  send: { label: "Type into the session", placeholder: "Text for the session's terminal", submit: "Send" },
  resume: { label: "Resume the waiting session", placeholder: "Authorization and instructions (required)", submit: "Resume" },
  close: { label: "Close", placeholder: "Outcome (optional)", submit: "Done" },
};
const REFRESH_MS = 10000;
const FOLLOW_MS = 300;
const WIDE = 720;

// Attention already announced, per project, shared by every copy of the mod.
const announced = new Map();

function ago(iso, now) {
  const ms = now - Date.parse(iso);
  if (!Number.isFinite(ms)) return "";
  const m = Math.floor(Math.abs(ms) / 60000);
  const text = m < 1 ? "<1m" : m < 60 ? `${m}m` : m < 1440 ? `${Math.floor(m / 60)}h` : `${Math.floor(m / 1440)}d`;
  return ms < 0 ? `in ${text}` : text;
}

// "human:ada@example.com" is ada; "agent:omp:<uuid>" is omp.
const short = (actor) => (actor ?? "").replace(/^human:([^@]+).*$/, "$1").replace(/^agent:([^:]+):.*$/, "$1");

function eventText(e) {
  const p = e.payload ?? {};
  switch (e.op) {
    case "create": return "created";
    case "claim": return "claimed";
    case "release": return `released${p.reason ? ` (${p.reason})` : ""}`;
    case "wait": return "started waiting";
    case "check": return `check ${p.result ?? ""}`.trim();
    case "close": return `closed ${p.reason ?? ""}`.trim();
    case "reopen": return "reopened";
    case "note": return (p.body ?? "note").split("\n")[0];
    case "edit": return "edited";
    case "dep_add": return p.blocker === e.issue_id ? `now blocks ${p.blocked}` : `now blocked by ${p.blocker}`;
    case "dep_rm": return p.blocker === e.issue_id ? `no longer blocks ${p.blocked}` : `no longer blocked by ${p.blocker}`;
    default: return e.op.replace(/_/g, " ");
  }
}

export default class Trk extends View {
  init(_props, cx) {
    if (props()?.launcher) return;
    this.data = null;
    this.error = null;
    this.detail = null;
    this.selected = null;
    this.open = false;
    this.folded = new Set(["blocked"]);
    this.busy = null;
    this.status = null;
    this.confirm = null;
    this.prompt = null;
    this.size = null;
    this.context = null;
    this.root = null;
    this.want = null;
    this.loading = false;
    this.loadedAt = 0;
    this.handle = cx.focus_handle();
    this.input = InputState.new({ placeholder: "" });
    this.input.on("submit", (_e, cx) => this.submit(cx));
    if (checking()) return;
    this.want = this.location();
    this.follow(cx);
    cx.timer.every(REFRESH_MS, (cx) => this.load(cx));
    // render() only notes where the shown space is: work started from render's cx stalls.
    cx.timer.every(FOLLOW_MS, (cx) => this.follow(cx));
    cx.spawn(async (cx) => {
      for (;;) {
        const command = await nextCommand();
        if (command === "focus") this.handle.focus();
        else if (command === "new") this.ask({ kind: "create" });
        else if (command === "refresh") this.load(cx);
        cx.notify();
      }
    });
  }

  async close() {
    if (props()?.launcher) return;
    const root = this.context?.root;
    this.context = null;
    this.publish(root, 0);
  }

  // ---------------------------------------------------------------- data

  /** The given folder, else the shown space's first live terminal (as in Herdr's panels). */
  location() {
    const space = spaces().find((s) => s.active);
    const folder = props()?.folder;
    if (folder) return { cwd: folder, on: null };
    // Mod tabs have no cwd; space.folder is only where the space was created.
    // Resolve in space tab order so focusing the dashboard cannot change its project.
    const all = panes();
    const on = space?.machine ?? null;
    for (const tab of space?.tabs ?? []) {
      const pane = all.find((p) => p.tab === tab.id && p.terminal && p.cwd);
      if (pane) return { cwd: pane.cwd, on };
    }
    return { cwd: space?.folder || null, on };
  }

  async trk(args, timeout = 30000) {
    const { cwd, on } = this.context ?? this.location();
    let r;
    try {
      r = await exec("trk", ["--json", ...args], { cwd: cwd || undefined, on, timeout });
    } catch (e) {
      throw new Error(`cannot run trk: ${e?.message ?? e}`);
    }
    let out = null;
    try { out = JSON.parse(r.stdout); } catch { /* not JSON: reported below */ }
    if (out?.ok) return out;
    const err = out?.error;
    throw new Error((typeof err === "string" ? err : err?.message) || r.stderr.trim() || r.stdout.trim() || `trk exited ${r.code}`);
  }

  /** Follows the shown space. What is on screen stays until the new place answers. */
  follow(cx) {
    const loc = this.want;
    const key = JSON.stringify(loc);
    if (!loc || this.context?.key === key) return;
    this.context = { key, ...loc, root: null };
    this.load(cx, true);
  }

  /** A different project starts over; another folder of the same one keeps the selection. */
  enter(root) {
    if (this.root === root) return;
    this.publish(this.root, 0);
    this.root = root;
    this.detail = null;
    this.selected = root ? props()?.select ?? null : null;
    this.open = Boolean(this.selected);
    this.prompt = null; this.confirm = null; this.status = null;
  }

  load(cx, restart = false) {
    const context = this.context;
    if (!context || (this.loading && !restart)) return;
    this.loading = true;
    cx.spawn(async (cx) => {
      try {
        if (!context.cwd) throw new Error("This space has no folder yet.");
        const [out, who] = await Promise.all([this.trk(["dashboard"]), context.root ? null : this.trk(["whoami"])]);
        if (this.context !== context) return;
        if (who) {
          context.root = who.data.root;
          context.dir = who.data.dir;
          this.enter(context.root);
          this.watch(cx, context);
        }
        this.data = out.data;
        this.error = out.schema_version === SCHEMA ? null : `This mod reads trk's JSON version ${SCHEMA}; the installed trk writes ${out.schema_version}. Run \`trk install\` to refresh the mod.`;
        this.loadedAt = Date.now();
        this.announce(context);
        if (this.selected && !this.data.issues.some((i) => i.id === this.selected)) { this.selected = null; this.detail = null; }
        if (this.selected && this.open) {
          cx.notify();
          await this.loadDetail(context);
        }
      } catch (e) {
        if (this.context === context) { this.enter(null); this.error = e.message; this.data = null; }
      } finally {
        if (this.context === context) { this.loading = false; cx.notify(); }
      }
    });
  }

  /** Reloads when the database changes. Worktrees live under .trk too, so only its own files count. */
  watch(cx, context) {
    if (context.on) return;
    cx.spawn(async (cx) => {
      try {
        while (this.context === context) {
          const paths = await watchFolder(context.dir);
          if (this.context !== context) return;
          const db = `${context.dir}/trk.db`;
          if (!paths.some((p) => p === context.dir || p.startsWith(db))) continue;
          if (Date.now() - this.loadedAt < 400) continue;
          this.load(cx);
        }
      } catch { /* too many watches, or none on this platform: the timer still refreshes */ }
    });
  }

  async loadDetail(context) {
    const id = this.selected;
    try {
      const out = await this.trk(["show", id, "--with", "notes,events,deps"]);
      if (this.context === context && this.selected === id) this.detail = out.data;
    } catch (e) {
      if (this.context === context && this.selected === id) this.detail = { id, failed: e.message };
    }
  }

  /** What Herdr knows about an issue's session terminal. */
  session(issue) {
    const address = issue.session?.terminal;
    if (!address) return null;
    const agent = this.agentsBy.get(address);
    const pane = this.panesBy.get(address);
    const live = this.liveTerminals.has(address);
    return { address, agent, pane, live, status: agent?.status ?? pane?.status ?? null };
  }

  /** Why this issue needs the person: trk's reasons, then an agent Herdr says is blocked. */
  reasons(issue) {
    const s = this.session(issue);
    const blocked = s?.status === "blocked" && issue.state === "claimed";
    return [...issue.attention, ...(blocked ? ["the agent is waiting for you"] : [])];
  }

  groupOf(issue) {
    return this.reasons(issue).length ? "attention" : issue.group;
  }

  /** The Dock badge counts what needs you across every project some copy shows. */
  publish(root, count) {
    if (!root || checking()) return;
    const all = { ...(shared("trk.attention") ?? {}) };
    if ((all[root] ?? 0) === (count ?? 0)) return;
    if (count) all[root] = count; else delete all[root];
    setShared("trk.attention", all);
    const total = Object.values(all).reduce((a, b) => a + b, 0);
    setBadge(total ? String(total) : null);
  }

  /** A toast for each issue that starts needing the person; none on the first read. */
  announce(context) {
    const now = new Map();
    for (const i of this.data.issues) {
      const why = this.reasons(i);
      if (why.length) now.set(i.id, { why: why[0], issue: i });
    }
    const before = announced.get(context.root);
    announced.set(context.root, new Set(now.keys()));
    this.publish(context.root, now.size);
    if (!before) return;
    for (const [id, { why, issue }] of now) {
      if (before.has(id)) continue;
      const terminal = issue.session?.terminal;
      notify({
        title: `${id} needs you`,
        body: `${issue.title}: ${why}`,
        level: "warning",
        action: terminal && this.panesBy?.has(terminal) ? { name: "pane.focus", args: { terminal } } : undefined,
      });
    }
  }

  // ------------------------------------------------------------- actions

  say(text, level = "info") {
    this.status = { text, level };
  }

  /** Runs one trk command, reports it in the status line, and re-reads. */
  act(cx, label, args, timeout) {
    if (this.busy) return;
    this.busy = label;
    this.confirm = null;
    this.status = null;
    const context = this.context;
    cx.notify();
    cx.spawn(async (cx) => {
      let done = null;
      try {
        done = await this.trk(args, timeout);
        if (this.context === context) this.say(`${label}: done`);
      } catch (e) {
        if (this.context === context) this.say(`${label}: ${e.message}`, "error");
      }
      if (this.context !== context) return;
      this.busy = null;
      if (args[0] === "create" && done?.data?.id) { this.selected = done.data.id; this.open = true; this.detail = null; }
      cx.notify();
      this.load(cx, true);
    });
  }

  /** A second click on the same button within the same selection confirms. */
  confirmed(key, cx) {
    if (this.confirm === key) return true;
    this.confirm = key;
    cx.notify();
    return false;
  }

  ask(prompt) {
    this.prompt = prompt;
    this.confirm = null;
    this.input.set_value("");
    this.input.focus();
  }

  submit(cx, variant) {
    const p = this.prompt;
    if (!p) return;
    // Typed text may start with a dash: it is passed so it never reads as an option.
    const text = this.input.value().trim();
    if (!text && p.kind !== "close") return;
    this.prompt = null;
    this.handle.focus();
    const id = p.id;
    if (p.kind === "create") this.act(cx, "create", ["create", `--title=${text}`]);
    else if (p.kind === "note") this.act(cx, `note ${id}`, ["note", id, "--", text]);
    else if (p.kind === "agent") this.act(cx, `note to agent ${id}`, ["note", "--to-agent", id, "--", text]);
    else if (p.kind === "send") this.act(cx, `send ${id}`, ["send", id, "--", text]);
    else if (p.kind === "resume") this.act(cx, `resume ${id}`, ["resume", id, `--message=${text}`]);
    else if (p.kind === "close") {
      const reason = variant ?? "done";
      this.act(cx, `close ${id} (${reason})`, ["close", id, "-r", reason, ...(text ? [`--message=${text}`] : [])]);
    }
  }

  /** Shows the session: its pane where it has one, a peek over the tab otherwise. */
  go(issue) {
    const s = this.session(issue);
    if (!s) return this.say(`${issue.id} has no session terminal`, "error");
    if (!s.live && !s.pane) return this.say(`${issue.id}: its terminal ${s.address} is gone`, "error");
    if (props()?.focus === "agent" && s.agent) return run("agent.focus", { terminal: s.address });
    if (!s.pane) return run("terminal.peek", { terminal: s.address });
    const space = spaces().find((sp) => sp.tabs.some((tab) => tab.id === s.pane.tab));
    if (space && !space.active) run("space.select", { id: space.id });
    run("tab.select", { id: s.pane.tab });
    run("pane.focus", { terminal: s.address });
  }

  select(id, cx, open = true) {
    const same = this.selected === id;
    this.selected = id;
    this.confirm = null;
    this.prompt = null;
    this.open = same && this.open && open ? !this.wide : open;
    if (!same) this.detail = null;
    this.handle.focus();
    const issue = this.data?.issues.find((i) => i.id === id);
    if (issue && props()?.focus === "agent" && this.session(issue)?.agent) run("agent.focus", { terminal: issue.session.terminal });
    cx.notify();
    if (this.open && !this.detail) {
      const context = this.context;
      cx.spawn(async (cx) => { await this.loadDetail(context); cx.notify(); });
    }
  }

  hideDetail(cx) {
    this.open = false;
    this.prompt = null;
    this.confirm = null;
    this.handle.focus();
    cx.notify();
  }

  runnable(issue) {
    return issue.state === "open" && issue.blocked_by.every((b) => b.state === "closed");
  }

  key(e, cx) {
    if (this.prompt) {
      if (e.key === "escape") { this.prompt = null; this.handle.focus(); cx.stop_propagation(); cx.notify(); }
      return;
    }
    if (e.modifiers?.control || e.modifiers?.platform || e.modifiers?.alt) return;
    const order = this.order ?? [];
    const at = order.indexOf(this.selected);
    const issue = this.data?.issues.find((i) => i.id === this.selected);
    const move = (to) => { if (order.length) this.select(order[Math.max(0, Math.min(order.length - 1, to))], cx, this.open); };
    const k = e.key_char ?? e.key;
    if (k === "j" || e.key === "down") move(at + 1);
    else if (k === "k" || e.key === "up") move(at < 0 ? 0 : at - 1);
    else if (k === "g") move(0);
    else if (k === "G") move(order.length - 1);
    else if (e.key === "enter" && issue) this.select(issue.id, cx);
    else if (e.key === "escape") { this.status = null; this.hideDetail(cx); }
    else if (k === "c") { this.ask({ kind: "create" }); cx.notify(); }
    else if (!issue) return;
    else if (k === "a") { this.go(issue); cx.notify(); }
    else if (k === "p" && issue.session?.terminal) run("terminal.peek", { terminal: issue.session.terminal });
    else if (k === "m") { this.open = true; this.ask({ kind: "note", id: issue.id }); cx.notify(); }
    else if (k === "M") { this.open = true; this.ask({ kind: "agent", id: issue.id }); cx.notify(); }
    else if (k === "s" && issue.session?.terminal) { this.open = true; this.ask({ kind: "send", id: issue.id }); cx.notify(); }
    else if (k === "v" && issue.state === "waiting") { this.open = true; this.ask({ kind: "resume", id: issue.id }); cx.notify(); }
    else if (k === "x") { this.open = true; this.ask({ kind: "close", id: issue.id }); cx.notify(); }
    else return;
    cx.stop_propagation();
  }

  // ---------------------------------------------------------------- view

  button(t, id, text, action, { primary = false, danger = false, tip = null } = {}) {
    const armed = this.confirm === id;
    const color = danger || armed ? t.blocked : t.foreground;
    return h_flex().id(id).h(24).px(9).flex_none().items_center().rounded(6)
      .border_1().border_color(armed ? t.blocked : primary ? t.solid : t.input)
      .bg(primary && !armed ? t.solid : t.raised)
      .text_size(11.5).font_medium().text_color(primary && !armed ? t.solid_foreground : color)
      .cursor_pointer().hover((s) => s.opacity(0.85))
      .when(Boolean(tip), (el) => el.tooltip(tip))
      .on_click((_e, cx) => { cx.stop_propagation(); action(cx); })
      .child(armed ? `${text}?` : text);
  }

  issueId(t, id, place, color = t.muted) {
    return div().id(`copy-${place}-${id}`).flex_none().text_size(11).text_color(color)
      .cursor_pointer().hover((s) => s.text_color(t.foreground)).tooltip("Copy issue ID")
      .on_click((_e, cx) => {
        cx.stop_propagation();
        cx.write_to_clipboard(id);
        this.say(`Copied ${id}`);
        cx.notify();
      })
      .child(id);
  }

  /** The right end of a row: why it needs you, the agent's state, or the next check. */
  badge(t, issue, now) {
    const why = this.reasons(issue);
    if (why.length) return div().flex_none().max_w(170).truncate().px(6).h(18).rounded(5).bg(t.blocked + "26").text_size(10.5).font_semibold().text_color(t.blocked).flex().items_center().child(why[0]);
    const s = this.session(issue);
    if (issue.state === "claimed") {
      if (s?.status === "working") return svg("mods/trk/spinner.svg").size(12).mx(1).flex_none().text_color(t.working).spin(900);
      if (s?.status === "done") return div().flex_none().text_size(10.5).text_color(t.done).child("finished");
      const who = issue.session?.agent ?? short(issue.session?.actor);
      return div().flex_none().max_w(120).truncate().text_size(10.5).text_color(t.dim).child(s?.status === "idle" ? `${who} idle` : who);
    }
    if (issue.state === "waiting") {
      const w = issue.waiting;
      const text = !w?.check ? "decision" : w.due ? "check due" : w.next_check_at ? `check ${ago(w.next_check_at, now)}` : "check";
      return div().flex_none().text_size(10.5).text_color(w?.due ? t.done : t.dim).child(text);
    }
    if (issue.group === "blocked") return div().flex_none().max_w(120).truncate().text_size(10.5).text_color(t.dim).child(`by ${issue.blocked_by.filter((b) => b.state !== "closed").map((b) => b.id).join(", ")}`);
    return div().flex_none().text_size(10.5).text_color(t.dim).child(ago(issue.updated_at, now));
  }

  row(t, issue, now) {
    const on = issue.id === this.selected;
    const urgent = issue.priority === "P0" ? t.blocked : issue.priority === "P1" ? t.done : t.dim;
    return h_flex().id(`row-${issue.id}`).h(28).px(8).gap(7).flex_none().items_center().rounded(7)
      .when(on, (el) => el.bg(t.selected)).cursor_pointer().hover((s) => s.bg(t.hover))
      .on_click((_e, cx) => this.select(issue.id, cx))
      .child(div().w(18).flex_none().text_size(10.5).font_semibold().text_color(urgent).child(issue.priority))
      .child(this.issueId(t, issue.id, "row"))
      .child(div().flex_none().text_size(10).text_color(t.dim).child(issue.type))
      .child(div().flex_1().min_w(0).truncate().text_color(on ? t.foreground : issue.group === "blocked" ? t.muted : t.foreground).child(issue.title))
      .child(this.badge(t, issue, now));
  }

  promptBox(t) {
    const p = this.prompt;
    const spec = PROMPTS[p.kind];
    return v_flex().id("prompt").gap(6).p(8).rounded(8).bg(t.surface).border_1().border_color(t.border)
      .child(div().text_size(11).text_color(t.dim).child(p.id ? `${spec.label} · ${p.id}` : spec.label))
      .child(Input.new(this.input).h(30).px(8).rounded(7).border_1().border_color(t.accent).bg(t.background).text_size(12.5).text_color(t.foreground))
      .child(h_flex().gap(6).justify_end()
        .child(this.button(t, "prompt-cancel", "Cancel", (cx) => { this.prompt = null; this.handle.focus(); cx.notify(); }))
        .when(p.kind === "close", (el) => el.child(this.button(t, "prompt-abandon", "Abandoned", (cx) => this.submit(cx, "abandoned"), { danger: true })))
        .child(this.button(t, "prompt-ok", spec.submit, (cx) => this.submit(cx), { primary: true })));
  }

  actions(t, issue) {
    const id = issue.id;
    const s = this.session(issue);
    const reachable = Boolean(s && (s.live || s.pane));
    const list = [];
    const ask = (kind) => (cx) => { this.ask({ kind, id }); cx.notify(); };
    if (reachable) list.push(this.button(t, `go-${id}`, "Go to session", (cx) => { this.go(issue); cx.notify(); }, { primary: true, tip: "Show the session's terminal (a)" }));
    if (reachable && s.pane) list.push(this.button(t, `peek-${id}`, "Peek", () => run("terminal.peek", { terminal: s.address }), { tip: "Peek at it over this tab (p)" }));
    if (this.runnable(issue)) list.push(this.button(t, `run-${id}`, "Run", (cx) => { if (this.confirmed(`run-${id}`, cx)) this.act(cx, `run ${id}`, ["run", id], 120000); }, { primary: true, tip: "Start an agent session on this issue" }));
    if (reachable) list.push(this.button(t, `send-${id}`, "Send…", ask("send"), { tip: "Type text into the session's terminal (s)" }));
    if (issue.state === "waiting") list.push(this.button(t, `resume-${id}`, "Resume…", ask("resume"), { tip: "Authorize the waiting session to continue (v)" }));
    if (issue.state === "waiting" && issue.waiting?.check) list.push(this.button(t, `check-${id}`, "Check now", (cx) => this.act(cx, `check ${id}`, ["check", id], 600000), { tip: "Run this issue's check, due or not" }));
    list.push(this.button(t, `note-${id}`, "Note…", ask("note"), { tip: "Add a note (m)" }));
    if (issue.session) list.push(this.button(t, `agent-${id}`, "Note to agent…", ask("agent"), { tip: "A note queued for the agent's inbox (M)" }));
    list.push(this.button(t, `edit-${id}`, "Edit", () => run("terminal.open", { argv: ["trk", "edit", id], folder: this.context.root, place: "down" }), { tip: "Open trk edit in a pane below" }));
    if (issue.state === "claimed") list.push(this.button(t, `unclaim-${id}`, "Unclaim", (cx) => { if (this.confirmed(`unclaim-${id}`, cx)) this.act(cx, `unclaim ${id}`, ["unclaim", id]); }, { danger: true }));
    if (issue.state === "waiting") list.push(this.button(t, `reopen-${id}`, "Reopen", (cx) => { if (this.confirmed(`reopen-${id}`, cx)) this.act(cx, `reopen ${id}`, ["reopen", id]); }, { danger: true }));
    list.push(this.button(t, `close-${id}`, "Close…", ask("close"), { tip: "Close as done or abandoned (x)" }));
    return h_flex().flex_wrap().gap(6).children(list);
  }

  detailView(t, issue, now) {
    const d = this.detail?.id === issue.id ? this.detail : null;
    const s = this.session(issue);
    const w = issue.waiting;
    const line = (label, text, color = t.muted) => h_flex().gap(8).items_start()
      .child(div().w(64).flex_none().text_size(11).text_color(t.dim).child(label))
      .child(div().flex_1().min_w(0).text_size(12).text_color(color).child(text));
    const edges = (label, list) => h_flex().gap(8).items_start()
      .child(div().w(64).flex_none().text_size(11).text_color(t.dim).child(label))
      .child(v_flex().flex_1().min_w(0).gap(2).children(list.map((b) => h_flex().id(`edge-${label}-${b.id}`).gap(6).text_size(12).cursor_pointer()
        .hover((st) => st.text_color(t.foreground)).text_color(b.state === "closed" ? t.dim : t.muted)
        .on_click((_e, cx) => { cx.stop_propagation(); if (this.data.issues.some((i) => i.id === b.id)) this.select(b.id, cx); })
        .child(div().flex_none().child(b.id)).child(div().flex_1().min_w(0).truncate().child(b.title)).child(div().flex_none().text_color(t.dim).child(b.state)))));
    const session = issue.session;
    const notes = (d?.notes ?? []).slice(-6);
    const events = (d?.events ?? []).slice(-8).reverse();
    return v_flex().id(`detail-${issue.id}`).gap(9).p(10).rounded(8).bg(t.background).border_1().border_color(t.border)
      .child(h_flex().gap(8).items_start()
        .child(div().flex_1().min_w(0).text_size(13.5).font_semibold().text_color(t.foreground).child(issue.title))
        .child(this.button(t, "hide-detail", "Hide details", (cx) => this.hideDetail(cx), { tip: "Hide details and expand the issue list (Esc)" })))
      .child(h_flex().gap(6).items_center().flex_wrap()
        .child(this.issueId(t, issue.id, "detail", t.dim))
        .child(div().text_size(11).text_color(t.dim).child(`· ${issue.type} · ${issue.state} · updated ${ago(issue.updated_at, now)} ago`))
        .child(div().flex_1())
        .children(PRIORITIES.map((p) => div().id(`prio-${issue.id}-${p}`).px(5).h(18).flex().items_center().rounded(5).text_size(10.5).font_semibold()
          .text_color(p === issue.priority ? t.accent_foreground : t.dim).when(p === issue.priority, (el) => el.bg(t.accent))
          .cursor_pointer().hover((st) => st.bg(p === issue.priority ? t.accent : t.hover))
          .on_click((_e, cx) => { cx.stop_propagation(); if (p !== issue.priority) this.act(cx, `${issue.id} → ${p}`, ["edit", issue.id, "-p", p]); })
          .child(p))))
      .children(this.reasons(issue).map((why) => line("needs you", why, t.blocked)))
      .when(Boolean(session), (el) => el.child(line("session", [
        session?.agent ?? short(session?.actor),
        session?.harness,
        s ? (s.status ?? (s.live ? "running" : "terminal gone")) : session?.alive === false ? "ended" : null,
        session?.lease_until ? `lease ${ago(session.lease_until, now)}` : null,
        issue.wake_count ? `${issue.wake_count}/${this.data.max_wakes} wakes` : null,
      ].filter(Boolean).join(" · "))))
      .when(Boolean(w), (el) => el
        .when(Boolean(w?.reason), (el) => el.child(line("waiting", w.reason)))
        .when(Boolean(w?.check), (el) => el.child(line("check", w.check, t.dim))
          .child(line("", [w.due ? "due now" : w.next_check_at ? `next ${ago(w.next_check_at, now)}` : null, w.last_check_at ? `last ${ago(w.last_check_at, now)} ago, exit ${w.last_check_exit}` : "never run"].filter(Boolean).join(" · ")))))
      .when(issue.blocked_by.length > 0, (el) => el.child(edges("blocked by", issue.blocked_by)))
      .when(issue.blocks.length > 0, (el) => el.child(edges("blocks", issue.blocks)))
      .child(this.prompt?.id === issue.id ? this.promptBox(t) : this.actions(t, issue))
      .when(Boolean(d?.failed), (el) => el.child(div().text_size(11.5).text_color(t.blocked).child(d.failed)))
      .when(Boolean(d?.description), (el) => el.child(div().pt(2).text_size(12.5).text_color(t.foreground).child(TextView.markdown(`desc-${issue.id}`, d.description).selectable(true))))
      .when(notes.length > 0, (el) => el.child(v_flex().gap(6)
        .child(div().text_size(11).text_color(t.dim).child("Notes"))
        .children(notes.map((n) => v_flex().gap(1).pl(8).border_l(2).border_color(n.to_agent ? t.accent : t.border)
          .child(div().text_size(10.5).text_color(t.dim).child(`${short(n.actor)} · ${ago(n.at, now)} ago${n.to_agent ? " · to agent" : ""}`))
          .child(div().text_size(12).text_color(t.muted).child(TextView.markdown(`note-${issue.id}-${n.seq}`, n.body).selectable(true)))))))
      .when(events.length > 0, (el) => el.child(v_flex().gap(2)
        .child(div().text_size(11).text_color(t.dim).child("Timeline"))
        .children(events.map((e) => h_flex().gap(6).text_size(11)
          .child(div().w(32).flex_none().text_color(t.dim).child(ago(e.at, now)))
          .child(div().flex_none().max_w(90).truncate().text_color(t.dim).child(short(e.actor)))
          .child(div().flex_1().min_w(0).truncate().text_color(t.muted).child(eventText(e)))))));
  }

  header(t, title) {
    const c = this.data?.counts;
    const runner = this.data?.check_runner;
    const reapable = this.data?.issues.some((i) => i.attention.some((a) => a.startsWith("session ended")));
    return h_flex().h(40).flex_none().px(12).gap(8).items_center().border_b(1).border_color(t.border)
      .child(div().flex_1().min_w(0).truncate().font_semibold().text_color(t.foreground).child(title))
      .when(Boolean(runner), (el) => el.child(div().flex_none().text_size(10.5).text_color(runner?.busy ? t.working : t.dim).tooltip(`trk check --watch, pid ${runner?.pid}`).child(runner?.busy ? "checking" : "checks on")))
      .when(Boolean(c) && !runner && c.waiting > 0, (el) => el.child(div().flex_none().text_size(10.5).text_color(t.done).tooltip("Waiting issues have checks, and no `trk check --watch` is running").child("no check runner")))
      .when(Boolean(c), (el) => el.child(div().flex_none().text_size(10.5).text_color(t.dim).child(`${c.closed} closed`)))
      .when(reapable, (el) => el.child(this.button(t, "reap", "Reap", (cx) => { if (this.confirmed("reap", cx)) this.act(cx, "reap", ["reap"]); }, { tip: "Release the claims of ended sessions" })))
      .child(this.button(t, "new", "New", (cx) => { this.ask({ kind: "create" }); cx.notify(); }, { tip: "New issue (c)" }));
  }

  openDashboard() {
    const space = spaces().find((s) => s.active);
    if (!space) return;
    const tabs = new Set(space.tabs.map((tab) => tab.id));
    const panel = panes().find((p) => p.mod === "trk" && tabs.has(p.tab));
    if (panel) {
      run("tab.select", { id: panel.tab });
      run("pane.focus", { pane: panel.pane });
    } else {
      run("tab.new", { name: "trk dashboard", space: space.id, mod: "trk" });
    }
  }

  launcher(t) {
    const collapsed = (layout()?.children?.find((n) => n.id === "sidebar")?.size?.px ?? 268) < 160;
    return v_flex().size_full().bg(t.chrome).px(8).pt(4).text_size(13)
      .when(collapsed, (el) => el.px(0).items_center())
      .child(h_flex().id("trk-dashboard").h(28).px(8).gap(9).items_center().rounded(8)
        .when(collapsed, (el) => el.w(32).px(0).justify_center())
        .tooltip("Open trk dashboard").text_color(t.muted)
        .hover((s) => s.bg(t.hover).text_color(t.foreground)).cursor_pointer()
        .on_click(() => this.openDashboard())
        .child(svg("mods/trk/dashboard.svg").size(14))
        .when(!collapsed, (el) => el.child("trk dashboard")));
  }

  render() {
    const t = theme();
    if (props()?.launcher) return this.launcher(t);
    this.want = this.location();
    this.agentsBy = new Map(agents().map((a) => [a.address, a]));
    this.panesBy = new Map(panes().filter((p) => p.terminal).map((p) => [p.terminal, p]));
    this.liveTerminals = new Set(terminals().filter((x) => x.state === "live").map((x) => x.address));
    this.wide = (this.size?.width ?? 0) >= WIDE;
    const now = Date.now();
    const root = v_flex().id("trk").track_focus(this.handle).on_key_down((e, cx) => this.key(e, cx))
      .on_size_change((size, cx) => { this.size = size; cx.notify(); })
      .size_full().min_h(0).bg(t.chrome).text_color(t.foreground).text_size(12.8);
    const name = (this.context?.root ?? this.context?.cwd ?? "").split("/").filter(Boolean).pop() ?? "";

    if (!this.data) {
      const text = this.error ?? "Reading…";
      const missing = /no \.trk|not found|trk install/i.test(text);
      return root.child(this.header(t, name)).child(v_flex().p(14).gap(8)
        .child(div().text_size(12).text_color(this.error ? t.muted : t.dim).child(text))
        .when(Boolean(this.error) && missing, (el) => el.child(div().text_size(12).text_color(t.dim).child("Run `trk install` in the project's root to start tracking work here."))));
    }

    const issues = this.data.issues;
    const groups = GROUPS.map((g) => ({ ...g, members: issues.filter((i) => this.groupOf(i) === g.id) })).filter((g) => g.members.length);
    this.order = groups.flatMap((g) => (this.folded.has(g.id) ? [] : g.members.map((i) => i.id)));
    if (props()?.focus === "agent") setAgentOrder(groups.flatMap((g) => g.members).map((i) => i.session?.terminal).filter(Boolean));
    const selected = issues.find((i) => i.id === this.selected);
    const inline = !this.wide && this.open;

    const rows = groups.flatMap((g) => {
      const open = !this.folded.has(g.id);
      const head = h_flex().id(`group-${g.id}`).h(28).pt(6).px(8).gap(6).flex_none().items_center().text_size(11).font_medium().cursor_pointer()
        .hover((s) => s.text_color(t.foreground))
        .on_click((_e, cx) => { open ? this.folded.add(g.id) : this.folded.delete(g.id); cx.notify(); })
        .child(div().text_color(t[g.color] ?? t.dim).child(g.label))
        .child(div().text_color(t.dim).child(String(g.members.length)))
        .child(div().flex_1())
        .when(!open, (el) => el.child(div().text_color(t.dim).child("›")));
      if (!open) return [head];
      return [head, ...g.members.flatMap((i) => (inline && i.id === this.selected ? [this.row(t, i, now), this.detailView(t, i, now)] : [this.row(t, i, now)]))];
    });

    const list = v_flex().id("trk-list").flex_1().min_w(0).min_h(0).px(6).pb(8).gap(1).overflow_y_scrollbar()
      .when(this.prompt?.kind === "create", (el) => el.child(div().pt(8).child(this.promptBox(t))))
      .when(Boolean(this.error), (el) => el.child(div().p(10).text_size(11.5).text_color(t.blocked).child(this.error)))
      .when(issues.length === 0, (el) => el.child(div().p(10).text_size(12).text_color(t.dim).child("Nothing open. New files an issue; `trk run` starts an agent on it.")))
      .children(rows);

    const footer = this.busy || this.status
      ? h_flex().h(26).flex_none().px(12).gap(8).items_center().border_t(1).border_color(t.border).text_size(11.5)
        .when(Boolean(this.busy), (el) => el.child(svg("mods/trk/spinner.svg").size(11).flex_none().text_color(t.working).spin(900)))
        .child(div().flex_1().min_w(0).truncate().text_color(this.status?.level === "error" ? t.blocked : t.muted).child(this.busy ? `${this.busy}…` : this.status.text))
      : null;

    const body = this.wide && this.open && selected
      ? h_flex().flex_1().min_h(0).items_start()
        .child(v_flex().w(380).flex_none().h_full().min_h(0).border_r(1).border_color(t.border).child(list))
        .child(v_flex().id("trk-detail").flex_1().min_w(0).h_full().min_h(0).p(10).overflow_y_scrollbar()
          .child(this.detailView(t, selected, now)))
      : list;

    return root.child(this.header(t, name)).child(body).when(Boolean(footer), (el) => el.child(footer));
  }
}
