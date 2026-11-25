import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { SourceTextModule, SyntheticModule } from "node:vm";

const source = await readFile(new URL("./main.js", import.meta.url), "utf8");

// Retain rendered content and conditional children; native checks cover styling.
class Element {
  constructor(kind) {
    this.kind = kind;
    this.nodes = [];
    return new Proxy(this, {
      get(target, key, receiver) {
        return Reflect.get(target, key, receiver) ?? (() => receiver);
      },
    });
  }
  child(node) { this.nodes.push(node); return this; }
  children(nodes) { this.nodes.push(...nodes); return this; }
  when(condition, apply) { return condition ? apply(this) : this; }
  id(value) { this.elementId = value; return this; }
  on_click(action) { this.click = action; return this; }
}

function content(node) {
  if (typeof node === "string") return [node];
  return node.kind === "spinner" ? ["spinner"] : node.nodes.flatMap(content);
}

function emptyContainers(node) {
  if (typeof node === "string" || node.kind === "spinner") return 0;
  return Number(node.nodes.length === 0) + node.nodes.reduce((n, child) => n + emptyContainers(child), 0);
}

async function dashboard(state) {
  const commands = [];
  const executions = [];
  const modules = {
    "gpui-kit": { View: class {}, div: () => new Element("div"), svg: () => new Element("spinner") },
    "gpui-base": {
      v_flex: () => new Element("column"), h_flex: () => new Element("row"),
      h_resizable: () => new Element("split"), resizable_panel: () => new Element("panel"),
      InputState: { new: () => ({ on() {} }) }, Input() {}, TextView: {},
    },
    herdr: {
      spaces: () => state.spaces,
      panes: () => state.panes,
      focused: () => state.focused,
      props: () => state.props,
      run: (name, args) => {
        commands.push({ name, args });
        // Both opening and selecting a mod tab move focus away from the terminal.
        if (name === "tab.new") {
          const space = state.spaces.find((s) => s.id === args.space);
          space.tabs.push({ id: "dashboard" });
          state.panes.push({ pane: "dashboard-pane", tab: "dashboard", mod: args.mod, terminal: null, cwd: null });
        }
        state.focused = { pane: "dashboard-pane" };
      },
      exec: async (command, args, options) => {
        executions.push({ command, args, options });
        return state.output ?? { code: 0, stdout: JSON.stringify({ ok: true, data: {} }), stderr: "" };
      },
      agents: () => [], terminals: () => [], theme: () => ({}), settings: () => ({}),
      notify() {}, checking: () => true, shared() {},
      setShared() {}, setBadge() {}, setAgentOrder: (order) => { state.agentOrder = order; },
      nextCommand() {}, watchFolder() {}, layout() {},
    },
  };
  const mod = new SourceTextModule(source);
  await mod.link((name) => {
    const exports = modules[name];
    return new SyntheticModule(Object.keys(exports), function () {
      for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
    });
  });
  await mod.evaluate();
  const view = new mod.namespace.default();
  view.init({}, { focus_handle: () => ({ focus() {} }) });
  return { view, commands, executions };
}

function spaceState() {
  return {
    spaces: [
      { id: "other", active: false, folder: "/other", tabs: [{ id: "other-tab" }] },
      { id: "current", active: true, folder: "/original", machine: null, tabs: [{ id: "first" }, { id: "second" }] },
    ],
    // Deliberately not in space/tab order.
    panes: [
      { pane: "other-pane", tab: "other-tab", terminal: "t1", cwd: "/other/live" },
      { pane: "second-pane", tab: "second", terminal: "t2", cwd: "/second-project" },
      { pane: "first-pane", tab: "first", terminal: "t3", cwd: "/current-project" },
    ],
    focused: { pane: "first-pane" },
  };
}

test("sidebar dashboard keeps the space's live project when its mod tab takes focus", async () => {
  const state = spaceState();
  const { view, commands, executions } = await dashboard(state);
  view.openDashboard();
  assert.deepEqual(commands, [{ name: "tab.new", args: { name: "trk dashboard", space: "current", mod: "trk" } }]);
  await view.trk(["dashboard"]);
  assert.equal(executions[0].options.cwd, "/current-project");

  // Reopening selects the existing panel; focus on another project must not leak in.
  state.focused = { pane: "other-pane" };
  view.openDashboard();
  assert.deepEqual(commands.slice(1), [
    { name: "tab.select", args: { id: "dashboard" } },
    { name: "pane.focus", args: { pane: "dashboard-pane" } },
  ]);
  await view.trk(["dashboard"]);
  assert.equal(executions[1].options.cwd, "/current-project");
});

test("unpinned dashboard follows space switches and remote live directories", async () => {
  const state = spaceState();
  const { view, executions } = await dashboard(state);
  state.spaces[1].active = false;
  state.spaces[0].active = true;
  state.spaces[0].machine = "vps";
  await view.trk(["dashboard"]);
  assert.equal(executions[0].options.cwd, "/other/live");
  assert.equal(executions[0].options.on, "vps");
});

test("explicit project stays local and overrides the selected space", async () => {
  const state = spaceState();
  state.spaces[1].machine = "vps";
  state.props = { folder: "/pinned" };
  const { view, executions } = await dashboard(state);
  await view.trk(["dashboard"]);
  assert.equal(executions[0].options.cwd, "/pinned");
  assert.equal(executions[0].options.on, null);
});

test("a space without a terminal cwd uses its own folder, never another space's", async () => {
  const state = spaceState();
  state.panes = state.panes.filter((p) => p.tab === "other-tab");
  state.focused = { pane: "other-pane" };
  const { view, executions } = await dashboard(state);
  await view.trk(["dashboard"]);
  assert.equal(executions[0].options.cwd, "/original");
  state.spaces[1].folder = "";
  assert.equal(view.location().cwd, null);
});

test("working rows prefix the title once and omit empty metadata without losing links", async () => {
  const { view } = await dashboard(spaceState());
  view.agentsBy = new Map([["t3", { status: "working" }]]);
  view.panesBy = new Map();
  view.liveTerminals = new Set(["t3"]);
  const issue = {
    id: "test-working", title: "A long title that can wrap", priority: "P2", type: "code-change",
    state: "claimed", group: "running", attention: [], links: [], session: { terminal: "t3" },
  };
  const columns = { id: 88, type: 112 };
  const row = view.row({}, issue, Date.now(), columns);
  assert.deepEqual(content(row), ["P2", issue.id, issue.type, "spinner", issue.title]);
  assert.equal(emptyContainers(row), 0, "a spinner-only issue must not reserve an empty metadata row");

  issue.links = [{ url: "https://example.com/pr/1", label: "Review" }, { url: "https://example.com" }];
  assert.deepEqual(content(view.row({}, issue, Date.now(), columns)),
    ["P2", issue.id, issue.type, "spinner", issue.title, "↗", "Review", "+1"]);

  // Attention still takes precedence; other session statuses stay below the title.
  issue.links = [];
  issue.attention = ["review required"];
  assert.deepEqual(content(view.row({}, issue, Date.now(), columns)),
    ["P2", issue.id, issue.type, issue.title, "review required"]);
  issue.attention = [];
  for (const [status, label] of [["idle", "omp idle"], ["done", "finished"]]) {
    view.agentsBy.set("t3", { status });
    issue.session.agent = "omp";
    assert.deepEqual(content(view.row({}, issue, Date.now(), columns)),
      ["P2", issue.id, issue.type, issue.title, label]);
  }
});

test("unclaim failures show the CLI recovery hint in the dashboard status", async () => {
  const state = spaceState();
  const message = "orch-123 is claimed by agent:omp:s1; only the claimant may unclaim it";
  const hint = "run `trk unclaim orch-123 --force` to override (recorded)";
  state.output = {
    code: 5, stderr: "",
    stdout: JSON.stringify({ ok: false, error: { code: "not_claimant", message, hint } }),
  };
  const { view } = await dashboard(state);
  let pending;
  const cx = { notify() {}, spawn: (run) => { pending = run(cx); } };
  view.act(cx, "unclaim orch-123", ["unclaim", "orch-123"]);
  await pending;
  assert.equal(view.status.level, "error");
  assert.equal(view.busy, null);
  view.data = { issues: [] };
  view.folded = new Set();
  assert.ok(content(view.render()).includes(`unclaim orch-123: ${message}\n${hint}`));
});

test("CLI errors without hints retain their message and fallback diagnostics", async () => {
  const state = spaceState();
  const { view } = await dashboard(state);
  for (const [stdout, stderr, expected] of [
    [JSON.stringify({ ok: false, error: { message: "not claimed" } }), "", "not claimed"],
    [JSON.stringify({ ok: false, error: "legacy error" }), "", "legacy error"],
    ["", "permission denied\n", "permission denied"],
    ["invalid output\n", "", "invalid output"],
    ["", "", "trk exited 5"],
  ]) {
    state.output = { code: 5, stdout, stderr };
    await assert.rejects(view.trk(["unclaim", "orch-123"]), { message: expected });
  }
});

function findElement(node, id) {
  if (typeof node === "string") return null;
  return node.elementId === id ? node : node.nodes.map((child) => findElement(child, id)).find(Boolean);
}

function sortIssues() {
  return [
    ["a", "P1", 3, 1, "research"],
    ["b", "P0", 4, 2, "bug"],
    ["c", "P1", 1, 4, "ops"],
    ["d", "P2", 2, 3, "change"],
  ].map(([id, priority, created, updated, type]) => ({
    id, priority, type, title: id, group: "ready", state: "open", attention: [],
    created_at: `2026-11-0${created}T00:00:00Z`, updated_at: `2026-11-0${updated}T00:00:00Z`,
    links: [], blocked_by: [], session: { terminal: `t-${id}` },
  }));
}

const sortContext = { notify() {}, stop_propagation() {} };

test("default priority order matches the dashboard's priority then oldest-first order", async () => {
  const { view } = await dashboard(spaceState());
  const issues = sortIssues();
  view.data = { issues };
  view.render();
  assert.deepEqual(view.order, ["b", "c", "a", "d"]);
  assert.deepEqual(view.data.issues.map((i) => i.id), ["a", "b", "c", "d"], "rendering must not reorder the snapshot");
});

for (const [key, expected, reversed] of [
  ["priority", ["b", "c", "a", "d"], ["d", "c", "a", "b"]],
  ["updated", ["c", "d", "b", "a"], ["a", "b", "d", "c"]],
  ["created", ["c", "d", "a", "b"], ["b", "a", "d", "c"]],
  ["type", ["b", "d", "c", "a"], ["a", "c", "d", "b"]],
]) {
  test(`${key} prop and button use natural direction, then flip only that key`, async () => {
    for (const reverse of [false, true]) {
      const state = { ...spaceState(), props: { sort: `${reverse ? "-" : ""}${key}` } };
      const { view } = await dashboard(state);
      view.data = { issues: sortIssues() };
      view.render();
      assert.deepEqual(view.order, reverse ? reversed : expected);
      view.sortBy(key, sortContext);
      view.render();
      assert.deepEqual(view.order, reverse ? expected : reversed);
      // Returning from a different key resets to natural, not its last direction.
      view.sortBy(key === "type" ? "created" : "type", sortContext);
      view.sortBy(key, sortContext);
      view.render();
      assert.deepEqual(view.order, expected);
    }
  });

  test(`${key} ties retain ascending priority, creation time and id in both directions`, async () => {
    const { view } = await dashboard({ ...spaceState(), props: { sort: key } });
    const issues = [
      ["z", "P1", 3], ["b", "P1", 2], ["a", "P1", 2], ["urgent", "P0", 4],
    ].map(([id, priority, day]) => ({
      ...sortIssues()[0], id, priority, created_at: `2026-11-0${day}T00:00:00Z`,
    }));
    // Force equal primary values; the remaining fields exercise the fixed tie-breaks.
    if (key === "priority") issues.forEach((i) => { i.priority = "P1"; });
    if (key === "created") issues.forEach((i) => { i.created_at = "2026-11-01T00:00:00Z"; });
    const expected = key === "priority" ? ["a", "b", "z", "urgent"] : ["urgent", "a", "b", "z"];
    view.data = { issues };
    view.render();
    assert.deepEqual(view.order, expected);
    view.sortBy(key, sortContext);
    view.data = { issues: [...issues].reverse() };
    view.render();
    assert.deepEqual(view.order, expected);
  });
}

test("invalid sort props fall back to priority without throwing", async () => {
  for (const sort of [undefined, null, "", "-", "Updated", "--type", "unknown", "toString", "__proto__", 1, {}]) {
    const { view } = await dashboard({ ...spaceState(), props: { sort } });
    view.data = { issues: sortIssues() };
    view.render();
    assert.deepEqual(view.order, ["b", "c", "a", "d"]);
    assert.deepEqual(view.sort, { key: "priority", desc: false });
  }
});

test("sort row buttons and keyboard cycle, reverse and respect prompts and modifiers", async () => {
  const { view } = await dashboard(spaceState());
  view.data = { issues: sortIssues() };
  let tree = view.render();
  assert.equal(tree.nodes[1].elementId, "sort", "the sort row sits directly below the header");
  assert.deepEqual(content(tree.nodes[1]), ["Sort", "Priority ↑", "Updated", "Created", "Type"]);
  findElement(tree, "sort-updated").click({}, sortContext);
  tree = view.render();
  assert.deepEqual(view.order, ["c", "d", "b", "a"]);
  assert.deepEqual(content(tree.nodes[1]), ["Sort", "Priority", "Updated ↓", "Created", "Type"]);
  findElement(tree, "sort-updated").click({}, sortContext);
  view.render();
  assert.deepEqual(view.order, ["a", "b", "d", "c"]);

  // With no selection, the shortcuts still work and wrap in registry order.
  for (const [key, desc] of [["created", false], ["type", false], ["priority", false], ["updated", true]]) {
    view.key({ key_char: "o" }, sortContext);
    assert.deepEqual(view.sort, { key, desc });
  }
  view.key({ key_char: "O" }, sortContext);
  view.render();
  assert.deepEqual(view.order, ["a", "b", "d", "c"]);
  for (const modifiers of [{ control: true }, { platform: true }, { alt: true }]) {
    view.key({ key_char: "o", modifiers }, sortContext);
    view.key({ key_char: "O", modifiers }, sortContext);
  }
  view.prompt = { kind: "create" };
  view.key({ key_char: "o" }, sortContext);
  view.key({ key_char: "O" }, sortContext);
  assert.deepEqual(view.sort, { key: "updated", desc: false });
});

test("sort stays within groups and drives navigation and agent order without losing selection on refresh", async () => {
  const state = { ...spaceState(), props: { focus: "agent", sort: "updated" } };
  const { view } = await dashboard(state);
  const issues = sortIssues();
  issues[0].group = "running";
  issues[3].group = "blocked";
  const attention = { ...issues[0], id: "attention", attention: ["review"], session: { terminal: "t-attention" } };
  view.data = { issues: [...issues, attention] };
  view.selected = "b";
  view.render();
  assert.deepEqual(view.order, ["attention", "a", "c", "b"]);
  assert.deepEqual(state.agentOrder, ["t-attention", "t-a", "t-c", "t-b", "t-d"]);
  view.sortBy("updated", sortContext);
  view.render();
  assert.deepEqual(view.order, ["attention", "a", "b", "c"]);
  assert.equal(view.selected, "b");
  for (const [key_char, selected] of [["j", "c"], ["k", "b"], ["g", "attention"], ["G", "c"]]) {
    view.key({ key_char }, sortContext);
    assert.equal(view.selected, selected);
  }
  view.context = { root: "/current-project", cwd: "/current-project" };
  state.output = { code: 0, stderr: "", stdout: JSON.stringify({ ok: true, schema_version: 4, data: { issues: [...issues, attention].reverse() } }) };
  let pending;
  const cx = { ...sortContext, spawn: (run) => { pending = run(cx); } };
  view.load(cx);
  await pending;
  view.render();
  assert.equal(view.selected, "c");
  assert.deepEqual(view.order, ["attention", "a", "b", "c"]);
  view.folded.delete("blocked");
  view.render();
  assert.deepEqual(view.order, ["attention", "a", "b", "c", "d"]);
  const { view: another } = await dashboard(spaceState());
  another.data = { issues: sortIssues() };
  another.render();
  assert.deepEqual(another.order, ["b", "c", "a", "d"], "panels must not share sort state");
});

test("launcher variant omits the sort row", async () => {
  const { view } = await dashboard({ ...spaceState(), props: { launcher: true } });
  assert.equal(findElement(view.render(), "sort"), undefined);
});
