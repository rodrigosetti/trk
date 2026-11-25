import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { SourceTextModule, SyntheticModule } from "node:vm";

const source = await readFile(new URL("./main.js", import.meta.url), "utf8");
class Element {
  constructor() {
    this.nodes = [];
    this.handlers = {};
    return new Proxy(this, { get: (target, key, receiver) => Reflect.get(target, key, receiver) ?? (() => receiver) });
  }
  child(node) { this.nodes.push(node); return this; }
  children(nodes) { this.nodes.push(...nodes); return this; }
  when(condition, apply) { return condition ? apply(this) : this; }
  id(value) { this.elementId = value; return this; }
  on_click(fn) { this.handlers.click = fn; return this; }
  on_mouse_up(button, fn) { this.handlers[`up-${button}`] = fn; return this; }
  on_mouse_down(button, fn) { this.handlers[`down-${button}`] = fn; return this; }
}
function find(node, id) {
  if (node?.elementId === id) return node;
  for (const child of node?.nodes ?? []) { const found = find(child, id); if (found) return found; }
}

async function setup(probe = async (_exec, loc) => ({ ...loc })) {
  const state = {
    spaces: [
      { id: "a", active: true, folder: "/a", machine: null, tabs: [{ id: "a1", active: true }, { id: "a2", active: false }] },
      { id: "b", active: false, folder: "/b", machine: "remote", tabs: [{ id: "b1", active: true }] },
    ],
    panes: [{ pane: "pa1", terminal: "ta1", tab: "a1", cwd: "/a", focused: true }, { pane: "pa2", terminal: "ta2", tab: "a2", cwd: "/a" }, { pane: "pb1", terminal: "tb1", tab: "b1", cwd: "/b" }],
    focus: { pane: "pa1", terminal: "ta1" },
    layout: { id: "trk-content", active: "work-body" },
    shared: new Map(), commands: [], executions: [],
  };
  const modules = {
    "gpui-kit": { View: class {}, div: () => new Element(), svg: () => new Element(), image: () => new Element() },
    "gpui-base": { h_flex: () => new Element(), ScrollHandle: { new: () => ({ scroll_to_item() {} }) } },
    "./project.js": {
      projectLocation: (space, all) => ({ cwd: space.tabs.map((t) => all.find((p) => p.tab === t.id && p.cwd)?.cwd).find(Boolean) ?? space.folder, on: space.machine }),
      probeProject: probe,
    },
    herdr: {
      spaces: () => state.spaces, panes: () => state.panes, focused: () => state.focus,
      layout: () => state.layout, settings: () => ({ zoom: 1 }), theme: () => ({}),
      shared: (key) => state.shared.get(key), setShared: (key, value) => state.shared.set(key, value),
      window: () => ({ visible: true }), checking: () => true, nextCommand() {}, watchFolder() {}, openOverlay() {},
      exec: (...args) => state.executions.push(args),
      run: (command, args) => {
        state.commands.push({ command, args });
        if (command === "layout.select") state.layout.active = args.child;
        if (command === "tab.select") {
          for (const space of state.spaces) if (space.tabs.some((t) => t.id === args.id)) {
            for (const tab of space.tabs) tab.active = tab.id === args.id;
          }
        }
        if (command === "pane.focus") {
          const pane = state.panes.find((p) => p.pane === args.pane);
          state.focus = { pane: pane.pane, terminal: pane.terminal };
        }
      },
    },
  };
  const mod = new SourceTextModule(source);
  await mod.link((name) => new SyntheticModule(Object.keys(modules[name]), function () {
    for (const [key, value] of Object.entries(modules[name])) this.setExport(key, value);
  }));
  await mod.evaluate();
  const view = new mod.namespace.default();
  view.init({}, { focus_handle: () => ({ focus() {} }) });
  const pending = [];
  const cx = { notify() {}, stop_propagation() {}, spawn: (work) => { const promise = work(cx); pending.push(promise); } };
  const qualify = async () => { view.pump(cx); await Promise.all(pending.splice(0)); };
  const switchSpace = (id) => {
    for (const space of state.spaces) space.active = space.id === id;
    const tab = state.spaces.find((s) => s.active).tabs.find((t) => t.active);
    const pane = state.panes.find((p) => p.tab === tab.id);
    state.focus = { pane: pane.pane, terminal: pane.terminal };
    view.sync();
  };
  return { view, state, cx, qualify, switchSpace, pending };
}

test("discovery does not steal selection or create native tabs; fixed entry is first and has no close/drag handlers", async () => {
  const { view, state, qualify, cx } = await setup();
  view.render();
  assert.equal(find(view.render(), "trk-fixed-tab"), undefined);
  await qualify();
  const tree = view.render();
  assert.equal(state.layout.active, "work-body");
  const strip = find(tree, "tab-scroll");
  assert.equal(strip.nodes[0].elementId, "trk-fixed-tab");
  assert.equal(strip.nodes.filter((node) => node.elementId === "trk-fixed-tab").length, 1);
  const fixed = strip.nodes[0];
  assert.equal(fixed.handlers["up-left"], undefined);
  assert.equal(fixed.handlers["down-right"], undefined);
  assert.equal(find(fixed, "close-dashboard"), undefined);
  fixed.handlers.click({}, cx);
  assert.equal(state.layout.active, "trk-dashboard");
  assert.equal(state.shared.get("trk-tabs.project").cwd, "/a");
  assert.equal(state.spaces[0].tabs[0].active, true);
  assert.ok(!state.commands.some(({ command }) => ["tab.new", "tab.move", "tab.close", "pane.close"].includes(command)));
});

test("virtual next/previous/index selection includes the fixed tab without changing native ordering", async () => {
  const { view, state, qualify } = await setup();
  await qualify();
  view.dispatch("tab.select", { index: 1 });
  assert.equal(state.layout.active, "trk-dashboard");
  view.dispatch("tab.next");
  assert.equal(state.spaces[0].tabs.find((t) => t.active).id, "a1");
  assert.equal(state.layout.active, "work-body");
  view.dispatch("tab.prev");
  assert.equal(state.layout.active, "trk-dashboard");
  view.dispatch("tab.prev");
  assert.equal(state.spaces[0].tabs.find((t) => t.active).id, "a2");
  view.dispatch("tab.next");
  assert.equal(state.layout.active, "trk-dashboard");
  view.dispatch("tab.select", { index: "2" });
  assert.equal(state.spaces[0].tabs.find((t) => t.active).id, "a1");
  assert.deepEqual(state.spaces[0].tabs.map((t) => t.id), ["a1", "a2"]);
});

test("dashboard selection is per space, pinned to its machine, and invalidated by project changes", async () => {
  const { view, state, qualify, switchSpace } = await setup();
  await qualify();
  view.dashboard();
  switchSpace("b");
  assert.equal(state.shared.get("trk-tabs.project"), null);
  await qualify();
  assert.equal(state.layout.active, "work-body");
  view.dashboard();
  assert.equal(state.shared.get("trk-tabs.project").on, "remote");
  switchSpace("a");
  assert.equal(state.layout.active, "trk-dashboard");
  assert.equal(state.shared.get("trk-tabs.project").cwd, "/a");
  state.panes[0].cwd = "/different";
  view.sync();
  assert.equal(state.layout.active, "work-body");
  assert.equal(state.shared.get("trk-tabs.project"), null);
});

test("native focus, same-pane reveal and mouse selection reveal ordinary work; background creation does not", async () => {
  const { view, state, qualify, cx } = await setup();
  await qualify();
  view.dashboard();
  view.dispatch("tab.new", { focus: false });
  assert.equal(state.layout.active, "trk-dashboard");
  state.focus = { pane: "pa2", terminal: "ta2" };
  view.sync();
  assert.equal(state.layout.active, "work-body");
  view.dashboard();
  const pin = state.shared.get("trk-tabs.project");
  state.shared.set("trk-tabs.reveal", { space: pin.space, epoch: pin.epoch });
  view.sync();
  assert.equal(state.layout.active, "work-body");
  view.dashboard();
  assert.equal(state.layout.active, "trk-dashboard", "an old focus event cannot cancel a new selection");
  find(view.render(), "tab-a1").handlers["up-left"]({}, cx);
  assert.equal(state.layout.active, "work-body");
  view.dashboard();
  view.dispatch("agent.next");
  assert.equal(state.layout.active, "work-body");
  assert.equal(state.commands.at(-1).command, "agent.next");
});

test("all layout close paths are harmless on dashboard, and forward normally on native work", async () => {
  const { view, state, qualify } = await setup();
  await qualify();
  view.dashboard();
  const before = state.commands.length;
  for (const command of ["tab.close", "pane.close", "space.close"]) view.dispatch(command);
  assert.equal(state.commands.length, before);
  assert.equal(state.layout.active, "trk-dashboard");
  view.ordinary("a1");
  for (const command of ["tab.close", "pane.close", "space.close"]) view.dispatch(command);
  assert.deepEqual(state.commands.slice(-3).map((c) => c.command), ["tab.close", "pane.close", "space.close"]);
});

test("an obsolete asynchronous probe cannot pin another shown space; failed eligibility returns to work", async () => {
  let answer;
  const { view, state, cx, pending, switchSpace } = await setup(() => new Promise((resolve) => { answer = resolve; }));
  view.pump(cx);
  switchSpace("b");
  answer({ cwd: "/a", on: null });
  await Promise.all(pending.splice(0));
  assert.equal(state.shared.get("trk-tabs.project")?.cwd, undefined);
  assert.equal(state.layout.active, "work-body");
  switchSpace("a");
  view.dashboard();
  const entry = view.current().entry;
  entry.project = null;
  view.sync();
  assert.equal(state.shared.get("trk-tabs.project"), null);
  assert.equal(state.layout.active, "work-body");
});
