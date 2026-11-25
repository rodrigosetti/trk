// Personal copy of Herdr 0.1.18's tabs mod. Native tabs remain native;
// the first entry and the dashboard stack child belong only to the trk layout.
import { View, div, svg, image } from "gpui-kit";
import { h_flex, ScrollHandle } from "gpui-base";
import {
  spaces, panes, focused, run, openOverlay, theme, window, shared, setShared,
  layout, settings, exec, checking, nextCommand, watchFolder,
} from "herdr";
import { projectLocation, probeProject } from "./project.js";

function find(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = find(child, id);
    if (found) return found;
  }
  return null;
}

const clean = (s) => (s ?? "").replace(/^[^\p{L}\p{N}\p{Ps}\p{Pi}]+/u, "").replace(/^\[ [!.] \] /, "").trim();
function label(tab, lead) {
  const prog = lead?.program?.name;
  const base = tab.named ? tab.name : prog ?? "Terminal";
  if (!lead?.program?.agent) return base;
  const live = clean(lead.title);
  const path = /^[~/]|^[A-Za-z]:\\/.test((lead.title ?? "").replace(/^[^\p{L}\p{N}~/]+/u, ""));
  const telling = live && live !== lead.terminal && live !== prog && !path && !live.includes("@") && live.toLowerCase() !== lead.program.key;
  return (tab.name === prog || !tab.named) && telling ? live : base;
}

export default class Tabs extends View {
  init(_props, cx) {
    this.pressed = null;
    this.scroll = ScrollHandle.new();
    this.handle = cx.focus_handle();
    try { this.panels = JSON.parse(localStorage.getItem("panels") ?? "{}"); } catch { this.panels = {}; }
    if (!this.panels || typeof this.panels !== "object" || Array.isArray(this.panels)) this.panels = {};
    this.swipe = { acc: 0, steps: 0, at: 0, last: 0 };
    this.projects = new Map();
    this.selections = new Map();
    this.epoch = 0;
    this.closed = false;
    if (checking()) return;
    // Render only records demand. Work uses a task/timer context, never a render context.
    // A timer tick does no I/O or redraw unless a probe is due.
    cx.timer.every(1000, (cx) => this.pump(cx));
    cx.spawn(async (cx) => {
      for (;;) {
        const command = await nextCommand();
        if (command.startsWith("dispatch:")) {
          try { const action = JSON.parse(command.slice(9)); this.dispatch(action.command, action.args ?? {}); }
          catch { /* malformed external mod command: never forward a destructive default */ }
        }
        cx.notify();
      }
    });
  }

  close() {
    this.closed = true;
    for (const entry of this.projects.values()) entry.watch?.cancel();
    setShared("trk-tabs.project", null);
  }

  current() {
    const space = spaces().find((s) => s.active);
    if (!space) return { space: null, state: null, entry: null };
    const loc = projectLocation(space, panes());
    const key = JSON.stringify(loc);
    let entry = this.projects.get(key);
    if (!entry) {
      entry = { loc, key, project: null, at: 0, pending: false, dirty: true };
      this.projects.set(key, entry);
    }
    let state = this.selections.get(space.id);
    if (!state || state.key !== key) {
      state = { key, dashboard: false, epoch: 0, native: null };
      this.selections.set(space.id, state);
    }
    if (!entry.project) state.dashboard = false;
    return { space, state, entry };
  }

  pump(cx) {
    if (this.closed || !window()?.visible) return;
    const { entry } = this.current();
    if (!entry || entry.pending || !entry.loc.cwd) return;
    if (!entry.watch && !entry.loc.on && entry.project) this.watch(entry, cx);
    const age = Date.now() - entry.at;
    if ((!entry.dirty || age < 5000) && age < 60000) return;
    entry.pending = true;
    entry.dirty = false;
    cx.spawn(async (cx) => {
      entry.project = await probeProject(exec, entry.loc);
      entry.at = Date.now();
      entry.pending = false;
      if (this.closed) return;
      if (!entry.watch && !entry.loc.on && entry.project) this.watch(entry, cx);
      this.sync();
      cx.notify();
    });
  }

  watch(entry, cx) {
    cx.spawn(async (cx) => {
      try {
        while (!this.closed && this.current().entry === entry) {
          entry.watch = watchFolder(entry.project.cwd);
          const paths = await entry.watch;
          entry.watch = null;
          const root = entry.project?.cwd;
          if (!root) return;
          if (paths.some((p) => [root, `${root}/.git`, `${root}/.trk`, `${root}/.trk/trk.db`].includes(p))) entry.dirty = true;
        }
      } catch { /* bounded periodic fallback, including remote and unavailable watches */ }
    });
  }

  sync() {
    const { space, state, entry } = this.current();
    const focus = focused();
    const native = JSON.stringify([space?.tabs.find((t) => t.active)?.id, focus?.pane, focus?.terminal, focus?.shown, focus?.agent]);
    const restoring = state?.dashboard && this.shownSpace !== space?.id;
    if (restoring) state.epoch = ++this.epoch;
    if (state) {
      // Space switches restore that space's choice. Native changes within it reveal work.
      if (this.shownSpace === space.id && state.native !== null && native !== state.native) state.dashboard = false;
      const reveal = shared("trk-tabs.reveal");
      if (reveal?.space === space.id && reveal.epoch === state.epoch) state.dashboard = false;
      state.native = native;
    }
    for (const cached of this.projects.values()) {
      if (cached !== entry) cached.watch?.cancel();
    }
    this.shownSpace = space?.id;
    const value = state?.dashboard && entry?.project
      ? { ...entry.project, space: space.id, epoch: state.epoch }
      : null;
    const previous = shared("trk-tabs.project");
    if (Boolean(previous) !== Boolean(value) || (value && ["cwd", "on", "space", "epoch"].some((key) => previous?.[key] !== value[key]))) {
      setShared("trk-tabs.project", value);
    }
    const child = value ? "trk-dashboard" : "work-body";
    if (find(layout(), "trk-content")?.active !== child) run("layout.select", { stack: "trk-content", child });
    if (restoring) run("trk.focus");
    return { space, state, entry };
  }

  dashboard() {
    const { state, entry } = this.current();
    if (!state || !entry.project) return;
    state.dashboard = true;
    this.handle.focus();
    state.epoch = ++this.epoch;
    this.sync();
    run("trk.focus");
  }

  ordinary(id, focus = true) {
    const { state } = this.current();
    if (state) state.dashboard = false;
    this.sync();
    if (id) run("tab.select", { id });
    // Selecting the already-selected native tab must still hand it the keyboard.
    if (focus) {
      const tab = id ?? spaces().find((s) => s.active)?.tabs.find((t) => t.active)?.id;
      const pane = panes().find((p) => p.tab === tab && p.focused) ?? panes().find((p) => p.tab === tab);
      if (pane) run("pane.focus", { pane: pane.pane });
    }
  }

  dispatch(command, args = {}) {
    const { space, state, entry } = this.sync();
    if (entry?.project && space) {
      const tabs = space.tabs;
      if (command === "tab.next" || command === "tab.prev") {
        const at = state.dashboard ? 0 : tabs.findIndex((t) => t.active) + 1;
        const index = (at + (command === "tab.next" ? 1 : tabs.length) + tabs.length + 1) % (tabs.length + 1);
        if (index === 0) this.dashboard();
        else this.ordinary(tabs[index - 1].id);
        return;
      }
      if (command === "tab.select" && args.index !== undefined) {
        const index = Number(args.index);
        if (index === 1) this.dashboard();
        else if (tabs[index - 2]) this.ordinary(tabs[index - 2].id);
        return;
      }
      if (state.dashboard && ["tab.close", "pane.close", "space.close"].includes(command)) return;
    }
    if (args.focus !== false) this.ordinary(null, false);
    run(command, args);
  }

  wheel(e) {
    if (e.delta_lines) {
      if (e.delta_lines.y) this.dispatch(e.delta_lines.y > 0 ? "tab.prev" : "tab.next");
      return;
    }
    const s = this.swipe, now = Date.now();
    if (e.touch_phase === "started" || now - s.at > 250 || e.delta.y * s.acc < 0) Object.assign(s, { acc: 0, steps: 0 });
    s.at = now;
    s.acc += e.delta.y;
    const need = s.steps ? 60 : 20;
    if (Math.abs(s.acc) < need || now - s.last < 100) return;
    s.acc -= Math.sign(s.acc) * need;
    s.steps++;
    s.last = now;
    this.dispatch(e.delta.y > 0 ? "tab.prev" : "tab.next");
  }

  syncPanel(space, panel, body, enabled) {
    if (!space || !panel) return;
    const now = panel.hidden ? null : body?.active ?? null;
    if (space.id !== this.space) {
      const first = this.space === undefined;
      this.space = space.id; this.pending = null;
      if (!(first && !(space.id in this.panels))) {
        const stored = this.panels[space.id] ?? null, want = enabled.includes(stored) ? stored : null;
        if (want !== now) {
          this.pending = { want, until: Date.now() + 1000 };
          if (want) { run("layout.select", { stack: "panel-body", child: want }); run("layout.show", { id: "panel" }); }
          else run("layout.hide", { id: "panel" });
          return;
        }
      }
    }
    if (this.pending) {
      if (this.pending.want !== now && Date.now() < this.pending.until) return;
      this.pending = null;
    }
    if (this.panels[space.id] === now) return;
    this.panels[space.id] = now;
    localStorage.setItem("panels", JSON.stringify(this.panels));
  }

  render() {
    const { space, state, entry } = this.sync();
    const t = theme();
    const shown = space?.tabs ?? [];
    const pinned = Boolean(entry?.project);
    const dashboard = Boolean(state?.dashboard);
    const active = dashboard ? 0 : shown.findIndex((tab) => tab.active) + Number(pinned);
    const selected = dashboard ? "dashboard" : shown.find((tab) => tab.active)?.id;
    if (selected !== this.selected || active !== this.selectedIndex) {
      this.selected = selected;
      this.selectedIndex = active;
      if (active >= 0) this.scroll.scroll_to_item(active);
    }
    const stop = (_e, cx) => cx.stop_propagation();
    const button = (id, label, action) => div().id(id).size(26).flex_none().flex().items_center().justify_center().rounded(6).text_color(t.dim).hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_mouse_down("left", stop).on_click(action).child(label);
    const item = (c, i) => {
      const lead = panes().find((p) => p.tab === c.id && p.program?.agent) ?? panes().find((p) => p.tab === c.id);
      const program = lead?.program;
      const name = label(c, lead);
      const on = c.active && !dashboard;
      return h_flex().id(`tab-${c.id}`).h(29).px(10).gap(7).rounded(7).items_center().flex_none().text_size(12.8)
        .when(i + Number(pinned) <= active, (el) => el.on_size_change((_size, cx) => { this.scroll.scroll_to_item(active); cx.notify(); }))
        .border(1).border_color(on ? t.border : t.chrome).bg(on ? t.raised : t.chrome).text_color(on ? t.foreground : t.muted)
        .hover((s) => s.bg(t.hover))
        .on_mouse_down("left", (e, cx) => { cx.stop_propagation(); this.pressed = i; if (e.click_count === 2) { this.ordinary(c.id); run("tab.rename", { id: c.id }); } })
        .on_mouse_up("left", () => {
          const from = this.pressed; this.pressed = null;
          if (from !== null && from !== i) run("tab.move", { id: shown[from].id, index: space.tabs.findIndex((x) => x.id === c.id) + 1 });
          else this.ordinary(c.id);
        })
        .on_mouse_down("right", (_e, cx) => { cx.stop_propagation(); this.ordinary(c.id); openOverlay({ mod: "menu", kind: "popup", width: 200, props: { tab: "tab", target: { id: c.id, space: space.id } } }); })
        .when(Boolean(program?.icon), (el) => el.child(program.tint ? svg(program.icon).size(14).text_color(t.muted) : image(program.icon).size(14)))
        .child(div().max_w(170).truncate().child(c.zoomed ? `${name} Z` : name))
        .when(c.status === "working", (el) => el.child(svg("mods/workspace-chrome/spinner.svg").size(12).flex_none().text_color(t.working).spin(900)))
        .when(c.status === "blocked" || c.status === "done", (el) => el.child(div().size(6).rounded(3).bg(t[c.status])))
        .when(on, (el) => el.child(div().id(`close-${c.id}`).text_color(t.dim).on_mouse_down("left", stop).on_click((_e, cx) => { cx.stop_propagation(); run("tab.close", { id: c.id }); }).child(svg("mods/pane/close.svg").size(12))));
    };
    const root = layout(), sidebar = find(root, "sidebar"), w = window();
    const lightsPadding = w?.platform === "macos" && w.titlebar === "none" && !w.fullscreen && root?.traffic_lights
      ? Math.max(10, (root.traffic_lights[0] + 64) / settings().zoom - (sidebar?.hidden ? 0 : sidebar?.size?.px ?? 0)) : 10;
    const panel = find(root, "panel"), body = find(root, "panel-body");
    const enabled = shared("workspace.panels") ?? ["files", "changes"];
    this.syncPanel(space, panel, body, enabled);
    const panelButton = (id, name, icon) => {
      const on = panel && !panel.hidden && body?.active === id;
      return h_flex().id(`panel-${id}`).h(26).px(8).gap(5).flex_none().items_center().rounded(6)
        .text_size(12).bg(on ? t.selected : t.chrome).text_color(on ? t.foreground : t.muted)
        .hover((s) => s.bg(t.hover)).on_mouse_down("left", stop)
        .on_click(() => {
          this.ordinary();
          if (on) run("layout.hide", { id: "panel" });
          else { run("layout.select", { stack: "panel-body", child: id }); run("layout.show", { id: "panel" }); }
        }).child(svg(icon).size(14)).child(name);
    };
    const fixed = () => h_flex().id("trk-fixed-tab").h(29).px(10).gap(7).rounded(7).items_center().flex_none().text_size(12.8)
      .border(1).border_color(dashboard ? t.border : t.chrome).bg(dashboard ? t.raised : t.chrome).text_color(dashboard ? t.foreground : t.muted)
      .tooltip("trk dashboard — fixed first tab").hover((s) => s.bg(t.hover))
      .on_mouse_down("left", (_e, cx) => { this.pressed = null; cx.stop_propagation(); })
      .on_click((_e, cx) => { cx.stop_propagation(); this.dashboard(); })
      .child(svg("mods/trk/dashboard.svg").size(14)).child("trk dashboard");
    return h_flex().track_focus(this.handle).size_full().min_w(0).px(10).pl(lightsPadding).gap(3).items_center().bg(t.chrome).border_b(1).border_color(t.border)
      .on_mouse_up("left", () => { this.pressed = null; })
      .on_scroll_wheel((e) => this.wheel(e))
      .child(h_flex().id("tab-scroll").flex_1().min_w(0).h_full().items_center().gap(3).overflow_x_scrollbar().track_scroll(this.scroll)
        .on_size_change((_size, cx) => { if (active >= 0) { this.scroll.scroll_to_item(active); cx.notify(); } })
        .when(pinned, (el) => el.child(fixed())).children(shown.map(item)))
      .child(button("tab-new", "+", () => { this.ordinary(null, false); openOverlay({ mod: "new-menu", kind: "popup", width: 250, height: 512 }); }))
      .when(enabled.includes("files"), (el) => el.child(panelButton("files", "Files", "mods/files/file.svg")))
      .when(enabled.includes("changes"), (el) => el.child(panelButton("changes", "Changes", "mods/changes/changes.svg")))
      .when(w?.buttons, (el) => el.child(button("minimize", w.platform === "windows" ? svg("mods/tabs/win-min.svg").size(16) : "−", () => run("window.minimize")))
        .child(button("maximize", w.platform === "windows" ? svg(`mods/tabs/${w.maximized ? "win-restore" : "win-max"}.svg`).size(16) : "□", () => run("window.maximize")))
        .child(button("close-window", w.platform === "windows" ? svg("mods/tabs/win-close.svg").size(16) : svg("mods/pane/close.svg").size(13), () => run("window.close"))));
  }
}
