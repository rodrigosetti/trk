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
  const notifications = [];
  const modules = {
    "gpui-kit": { View: class {}, div: () => new Element("div"), svg: () => new Element("spinner") },
    "gpui-base": {
      v_flex: () => new Element("column"), h_flex: () => new Element("row"),
      h_resizable: () => new Element("split"), resizable_panel: () => new Element("panel"),
      Tabs: { new: (id) => new Element("tabs").id(id) }, Tab: { new: (id) => new Element("tab").id(id) },
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
        if (state.exec) return state.exec(command, args, options);
        return state.output ?? { code: 0, stdout: JSON.stringify({ ok: true, data: {} }), stderr: "" };
      },
      agents: () => [], terminals: () => [], theme: () => ({}), settings: () => ({}),
      notify: (message) => notifications.push(message), checking: () => true, shared: (key) => state.shared?.get(key),
      setShared: (key, value) => state.shared?.set(key, value), setBadge() {}, setAgentOrder: (order) => { state.agentOrder = order; },
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
  view.init({}, { focus_handle: () => ({ focus() { state.hasFocus = true; }, is_focused: () => state.hasFocus ?? false }) });
  return { view, commands, executions, notifications };
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

test("Run confirmation survives refresh and dispatches once after a stale action completes", async () => {
  const state = { ...spaceState(), props: { fixedTab: true }, shared: new Map() };
  const { view, executions, notifications } = await dashboard(state);
  const pending = [];
  const cx = { ...sortContext, spawn: (run) => { pending.push(run(cx)); } };
  const pin = { space: "current", cwd: "/current-project", on: null, epoch: 1 };
  const enter = (location) => {
    state.shared.set("trk-tabs.project", location);
    const loc = view.location();
    view.context = { ...loc, key: JSON.stringify(loc), root: location.cwd };
    view.enter(location.cwd);
  };
  enter(pin);
  let finish;
  state.exec = () => new Promise((resolve) => { finish = resolve; });
  view.act(cx, "check old", ["check", "old"]);
  enter({ ...pin, cwd: "/other-project", epoch: 2 });
  finish({ code: 5, stderr: "", stdout: JSON.stringify({
    ok: false, error: { message: "check failed", hint: "Inspect its output" },
  }) });
  await pending.shift();
  assert.equal(view.busy, null, "an old project's completion must release the panel's command gate");
  assert.equal(view.status, null, "stale results must not replace the new project's status");
  assert.deepEqual(notifications.map((n) => [n.title, n.body, n.level]),
    [["check old: failed", "check failed\nInspect its output", "error"]],
    "an error after navigation must still reach the person without replacing the new project's footer");

  const issues = sortIssues();
  view.data = { issues };
  view.selected = "b";
  view.open = true;
  view.size = { width: 1000 };
  const clickRun = () => findElement(view.render(), "run-b").click({}, cx);
  clickRun();
  assert.deepEqual(content(findElement(view.render(), "run-b")), ["Run?"]);
  assert.equal(executions.length, 1, "arming must not dispatch");

  // Exercise the real snapshot/detail refresh, not just a repaint.
  state.exec = async (_command, args) => ({
    code: 0, stderr: "", stdout: JSON.stringify({
      ok: true, schema_version: 5,
      data: args.includes("dashboard") ? { issues: [...issues].reverse() } : { id: "b" },
    }),
  });
  view.load(cx);
  await pending.shift();
  assert.deepEqual(content(findElement(view.render(), "run-b")), ["Run?"]);
  state.exec = () => new Promise((resolve) => { finish = resolve; });
  clickRun();
  clickRun();
  clickRun();
  const runs = executions.filter((e) => e.args.includes("run"));
  assert.equal(runs.length, 1);
  assert.deepEqual(runs[0].args, ["--json", "--dir", "/other-project/.trk", "run", "b"]);
  assert.equal(runs[0].options.timeout, 120000);
  assert.equal(view.confirm, null, "pending commands must not arm another launch");

  // Hiding/reopening the fixed tab changes its epoch, even in the same project.
  enter({ ...pin, cwd: "/other-project", epoch: 3 });
  finish({ code: 0, stderr: "", stdout: JSON.stringify({ ok: true, data: {} }) });
  await pending.shift();
  assert.equal(view.busy, null);
  state.exec = async (_command, args) => ({
    code: args.includes("run") ? 5 : 0, stderr: "", stdout: JSON.stringify(
      args.includes("run")
        ? { ok: false, error: { message: "b is claimed, not ready", hint: "Refresh before retrying" } }
        : { ok: true, schema_version: 5, data: args.includes("dashboard") ? { issues } : { id: "b" } }),
  });
  clickRun();
  clickRun();
  await pending.shift();
  await pending.shift();
  assert.equal(view.status.level, "error");
  assert.ok(content(view.render()).includes("run b: b is claimed, not ready\nRefresh before retrying"));
});

test("Run confirmation cancels with selection or details and respects refreshed eligibility", async () => {
  const { view, executions } = await dashboard(spaceState());
  view.data = { issues: sortIssues() };
  view.selected = "b";
  view.open = true;
  view.detail = { id: "b" };
  const clickRun = (id) => findElement(view.render(), `run-${id}`).click({}, sortContext);
  clickRun("b");
  view.select("c", sortContext, false);
  view.select("b", sortContext, false);
  view.open = true;
  clickRun("b");
  assert.deepEqual(content(findElement(view.render(), "run-b")), ["Run?"]);
  view.hideDetail(sortContext);
  view.open = true;
  clickRun("b");
  assert.equal(executions.length, 0, "reopening details needs fresh confirmation");
  for (const state of ["claimed", "waiting", "closed"]) {
    view.data.issues[1].state = state;
    assert.equal(findElement(view.render(), "run-b"), undefined);
  }
  view.data.issues[1].state = "open";
  view.data.issues[1].blocked_by = [{ id: "blocker", state: "open", title: "blocker" }];
  assert.equal(findElement(view.render(), "run-b"), undefined);
  view.data.issues[1].blocked_by[0].state = "closed";
  assert.ok(findElement(view.render(), "run-b"));
});

function sortIssues() {
  return [
    ["a", "P1", 3, 1, "research"],
    ["b", "P0", 4, 2, "bug"],
    ["c", "P1", 1, 4, "ops"],
    ["d", "P2", 2, 3, "change"],
  ].map(([id, priority, created, updated, type]) => ({
    id, priority, type, title: id, group: "ready", state: "open", attention: [],
    created_at: `2026-11-0${created}T00:00:00Z`, updated_at: `2026-11-0${updated}T00:00:00Z`,
    links: [], blocked_by: [], blocks: [], session: { terminal: `t-${id}` },
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
  assert.equal(content(findElement(tree, "sort-priority")).join(""), "Priority ↑");
  findElement(tree, "sort-updated").click({}, sortContext);
  tree = view.render();
  assert.deepEqual(view.order, ["c", "d", "b", "a"]);
  assert.equal(content(findElement(tree, "sort-updated")).join(""), "Updated ↓");
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
  state.output = { code: 0, stderr: "", stdout: JSON.stringify({ ok: true, schema_version: 5, data: { issues: [...issues, attention].reverse() } }) };
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

function epicIssues() {
  const issues = sortIssues();
  for (const [index, epic] of ["Alpha", "Zulu", "Alpha", "Beta"].entries()) issues[index].epic = epic;
  return [...issues, {
    ...issues[1], id: "unassigned", epic: null, type: "aaa",
    created_at: "2026-10-31T00:00:00Z", updated_at: "2026-11-09T00:00:00Z",
    session: { terminal: "t-unassigned" },
  }];
}

function listEntries(tree) {
  return findElement(tree, "trk-list").nodes
    .filter((n) => /^(group-|epic-|row-)/.test(n.elementId))
    .map((n) => ({ id: n.elementId, text: content(n) }));
}

for (const [key, expected, reversed] of [
  ["priority", ["b", "c", "a", "d", "unassigned"], ["d", "c", "a", "b", "unassigned"]],
  ["updated", ["c", "a", "d", "b", "unassigned"], ["a", "c", "b", "d", "unassigned"]],
  ["created", ["c", "a", "d", "b", "unassigned"], ["b", "a", "c", "d", "unassigned"]],
  ["type", ["b", "d", "c", "a", "unassigned"], ["a", "c", "d", "b", "unassigned"]],
]) {
  test(`epic buckets rank by their top ${key} row in both directions, with No epic last`, async () => {
    for (const reverse of [false, true]) {
      const state = { ...spaceState(), props: { group: "epic", sort: `${reverse ? "-" : ""}${key}`, focus: "agent" } };
      const { view } = await dashboard(state);
      const issues = epicIssues();
      view.data = { issues };
      const tree = view.render();
      const order = reverse ? reversed : expected;
      assert.deepEqual(view.order, order);
      assert.deepEqual(state.agentOrder, order.map((id) => `t-${id}`));
      const entries = listEntries(tree);
      assert.deepEqual(entries.filter((n) => n.id.startsWith("row-")).map((n) => n.id), order.map((id) => `row-${id}`));
      const epics = [...new Set(order.map((id) => issues.find((i) => i.id === id).epic))];
      assert.deepEqual(entries.filter((n) => n.id.startsWith("epic-")).map((n) => n.text),
        epics.map((epic) => ["▾", epic ?? "No epic", epic === "Alpha" ? "2" : "1"]));
      assert.deepEqual(issues.map((i) => i.id), ["a", "b", "c", "d", "unassigned"], "leave snapshot order intact");
    }
  });

  test(`${key} bucket ties use epic name while row ties retain the fixed tie-break`, async () => {
    const { view } = await dashboard({ ...spaceState(), props: { group: "epic", sort: key } });
    const base = sortIssues()[0];
    const issues = [
      { ...base, id: "zulu", epic: "Zulu", priority: "P0", created_at: "2026-10-01T00:00:00Z" },
      { ...base, id: "alpha-z", epic: "Alpha" },
      { ...base, id: "alpha-a", epic: "Alpha" },
    ];
    if (key === "priority") issues.forEach((i) => { i.priority = "P1"; });
    if (key === "created") issues.forEach((i) => { i.created_at = base.created_at; });
    view.data = { issues };
    for (const reverse of [false, true]) {
      if (reverse) view.sortBy(key, sortContext);
      view.render();
      assert.deepEqual(view.order, ["alpha-a", "alpha-z", "zulu"]);
    }
  });
}

test("epic disclosures fold independently in every state, including No epic, without folding parents", async () => {
  const { view, commands } = await dashboard({ ...spaceState(), props: { group: "epic" } });
  const groups = ["attention", "running", "waiting", "ready", "blocked"];
  view.data = { issues: groups.flatMap((group) => epicIssues().map((i) => ({
    ...i, id: `${group}-${i.id}`, group,
    state: group === "running" ? "claimed" : group === "waiting" ? "waiting" : "open",
    attention: group === "attention" ? ["review"] : [],
  }))) };
  view.folded.clear();
  let tree = view.render();
  assert.deepEqual(listEntries(tree).filter((n) => n.id.startsWith("group-")).map((n) => n.text),
    ["NEEDS YOU", "IN FLIGHT", "WAITING", "READY", "BLOCKED"].map((label) => [label, "5"]));
  const all = [...view.order];
  for (const group of groups) {
    for (const epic of ["Alpha", "Zulu", "Beta", null]) {
      const header = `epic-${group}-${JSON.stringify(epic)}`;
      let stopped = false;
      findElement(tree, `fold-${header}`).click({}, { ...sortContext, stop_propagation() { stopped = true; } });
      tree = view.render();
      const hidden = view.data.issues.filter((i) => i.group === group && i.epic === epic).map((i) => i.id);
      assert.equal(stopped, true, "disclosure must not bubble to future scope navigation");
      assert.deepEqual(content(findElement(tree, header)), ["▸", epic ?? "No epic", String(hidden.length)]);
      assert.deepEqual(view.order, all.filter((id) => !hidden.includes(id)));
      for (const id of hidden) assert.equal(findElement(tree, `row-${id}`), undefined);
      findElement(tree, `fold-${header}`).click({}, sortContext);
      tree = view.render();
      assert.deepEqual(content(findElement(tree, header)), ["▾", epic ?? "No epic", String(hidden.length)]);
      assert.deepEqual(view.order, all);
    }
    findElement(tree, `group-${group}`).click({}, sortContext);
    tree = view.render();
    assert.equal(listEntries(tree).some((n) => n.id.startsWith(`epic-${group}-`) || n.id.startsWith(`row-${group}-`)), false);
    assert.equal(view.order.some((id) => id.startsWith(`${group}-`)), false);
    findElement(tree, `group-${group}`).click({}, sortContext);
    tree = view.render();
  }
  assert.deepEqual(commands, [], "folding must not navigate to an epic or agent");
});

test("all-unassigned states stay flat, while a single assigned epic still gets a header", async () => {
  const { view } = await dashboard({ ...spaceState(), props: { group: "epic" } });
  view.folded.clear();
  view.data = { issues: ["attention", "running", "waiting", "ready", "blocked"].flatMap((group) =>
    sortIssues().slice(0, 2).map((i, index) => ({
      ...i, id: `${group}-${i.id}`, group, epic: index ? null : undefined,
      attention: group === "attention" ? ["review"] : [],
    }))) };
  let tree = view.render();
  assert.deepEqual(listEntries(tree).filter((n) => n.id.startsWith("epic-")), []);
  assert.equal(content(tree).includes("No epic"), false);
  view.data.issues = [{ ...sortIssues()[0], epic: "Only epic" }];
  tree = view.render();
  assert.deepEqual(listEntries(tree).filter((n) => n.id.startsWith("epic-")).map((n) => n.text), [["▾", "Only epic", "1"]]);
});

test("flat mode shows epic tags and blocked hints identify only cross-epic blockers", async () => {
  const { view } = await dashboard(spaceState());
  const issues = epicIssues();
  const blocked = {
    ...issues[0], group: "blocked", blocked_by: [
      { id: "same", state: "open", epic: "Alpha" },
      { id: "other", state: "claimed", epic: "Beta" },
      { id: "none", state: "open", epic: null },
      { id: "closed", state: "closed", epic: "Gamma" },
    ],
  };
  view.data = { issues: [blocked, issues[4]] };
  view.folded.clear();
  let tree = view.render();
  assert.deepEqual(content(findElement(tree, "epic-row-a")), ["Alpha"]);
  assert.equal(findElement(tree, "epic-row-unassigned"), undefined);
  assert.ok(content(findElement(tree, "row-a")).includes("by same, other (Beta), none (No epic)"));
  view.toggleGrouping(sortContext);
  tree = view.render();
  assert.equal(findElement(tree, "epic-row-a"), undefined);
  assert.ok(content(findElement(tree, "row-a")).includes("by same, other (Beta), none (No epic)"));
  blocked.epic = null;
  tree = view.render();
  assert.ok(content(findElement(tree, "row-a")).includes("by same (Alpha), other (Beta), none"));
});

test("group toggle and sort preserve selection and details, and navigation skips all headers", async () => {
  const { view } = await dashboard(spaceState());
  view.data = { issues: epicIssues() };
  view.size = { width: 1000 };
  view.selected = "a";
  view.open = true;
  const detail = { id: "a" };
  view.detail = detail;
  let tree = view.render();
  assert.deepEqual(view.order, ["unassigned", "b", "c", "a", "d"]);
  assert.deepEqual(content(findElement(tree, "group-epic")), ["Group: Off"]);
  findElement(tree, "group-epic").click({}, sortContext);
  tree = view.render();
  assert.deepEqual(view.order, ["b", "c", "a", "d", "unassigned"]);
  assert.deepEqual(content(findElement(tree, "group-epic")), ["Group: Epic"]);
  assert.equal(view.selected, "a");
  assert.equal(view.detail, detail);
  assert.equal(view.open, true);
  findElement(tree, "sort-updated").click({}, sortContext);
  tree = view.render();
  assert.deepEqual(view.order, ["c", "a", "d", "b", "unassigned"]);
  assert.equal(view.selected, "a");
  assert.equal(view.detail, detail);
  assert.ok(findElement(tree, "detail-a"));
  view.key({ key_char: "e" }, sortContext);
  view.render();
  assert.deepEqual(view.order, ["unassigned", "c", "d", "b", "a"]);
  assert.equal(view.selected, "a");
  assert.equal(view.detail, detail);
  view.key({ key_char: "e" }, sortContext);
  view.render();
  view.hideDetail(sortContext);
  for (const [key_char, selected] of [["j", "d"], ["j", "b"], ["j", "unassigned"], ["k", "b"], ["g", "c"], ["G", "unassigned"]]) {
    view.key({ key_char }, sortContext);
    assert.equal(view.selected, selected);
  }
  for (const modifiers of [{ control: true }, { platform: true }, { alt: true }]) view.key({ key_char: "e", modifiers }, sortContext);
  view.prompt = { kind: "create" };
  view.key({ key_char: "e" }, sortContext);
  assert.equal(view.group, "epic");
  const { view: another } = await dashboard(spaceState());
  another.data = view.data;
  another.render();
  assert.deepEqual(another.order, ["unassigned", "b", "c", "a", "d"]);
});

test("fixed dashboard pins its database and remote machine, and refuses stale actions after a space switch", async () => {
  const state = spaceState();
  state.props = { fixedTab: true };
  state.shared = new Map([["trk-tabs.project", { space: "current", cwd: "/srv/pinned project", on: "vps", epoch: 1 }]]);
  const { view, executions } = await dashboard(state);
  const loc = view.location();
  view.context = { ...loc, key: JSON.stringify(loc) };
  await view.trk(["dashboard"]);
  assert.deepEqual(executions[0].args, ["--json", "--dir", "/srv/pinned project/.trk", "dashboard"]);
  assert.equal(executions[0].options.cwd, "/srv/pinned project");
  assert.equal(executions[0].options.on, "vps");
  state.spaces[1].active = false;
  state.spaces[0].active = true;
  await assert.rejects(view.trk(["close", "old-issue"]), /no longer selected/);
  assert.equal(executions.length, 1, "a stale action must never execute against the old or a discovered project");
  view.data = { issues: [{ id: "old-issue" }] };
  view.detail = { id: "old-issue" };
  view.render();
  assert.equal(view.data, null);
  assert.equal(view.detail, null);
});

test("native focus returning to the same hidden pane reveals ordinary work, but dashboard prompts keep selection", async () => {
  const state = spaceState();
  state.props = { fixedTab: true };
  state.shared = new Map([["trk-tabs.project", { space: "current", cwd: "/pinned", on: null, epoch: 7 }]]);
  const { view } = await dashboard(state);
  const loc = view.location();
  view.context = { ...loc, key: JSON.stringify(loc) };
  view.takeFocus();
  state.hasFocus = false;
  view.inputFocused = true;
  view.follow({});
  assert.equal(state.shared.get("trk-tabs.reveal"), undefined);
  view.inputFocused = false;
  view.takeFocus();
  state.hasFocus = false;
  // No intervening render/model change: this also covers focus immediately after selecting it.
  view.follow({});
  assert.deepEqual(state.shared.get("trk-tabs.reveal"), { space: "current", epoch: 7 });
});

test("same-named roots on different machines never retain another project's pending action", async () => {
  const { view } = await dashboard(spaceState());
  view.context = { on: null };
  view.enter("/srv/project");
  view.selected = "local-1";
  view.prompt = { kind: "close", id: "local-1" };
  view.context = { on: "remote" };
  view.enter("/srv/project");
  assert.equal(view.selected, null);
  assert.equal(view.prompt, null);
});

test("epic folds survive sorting, refresh, parent folding and grouping toggles without sharing panel state", async () => {
  const state = { ...spaceState(), props: { group: "epic", focus: "agent" } };
  const { view } = await dashboard(state);
  view.data = { issues: epicIssues() };
  let tree = view.render();
  const header = 'epic-ready-"Alpha"';
  findElement(tree, `fold-${header}`).click({}, sortContext);
  tree = view.render();
  assert.deepEqual(view.order, ["b", "d", "unassigned"]);
  assert.deepEqual(state.agentOrder, ["t-b", "t-c", "t-a", "t-d", "t-unassigned"], "folding must not change session navigation");
  findElement(tree, "sort-type").click({}, sortContext);
  tree = view.render();
  assert.deepEqual(view.order, ["b", "d", "unassigned"]);
  findElement(tree, "group-ready").click({}, sortContext);
  tree = view.render();
  assert.deepEqual(view.order, []);
  findElement(tree, "group-ready").click({}, sortContext);
  tree = view.render();
  assert.deepEqual(content(findElement(tree, header)), ["▸", "Alpha", "2"]);
  findElement(tree, "group-epic").click({}, sortContext);
  tree = view.render();
  assert.deepEqual(view.order, ["unassigned", "b", "d", "c", "a"]);
  findElement(tree, "group-epic").click({}, sortContext);
  view.context = { root: "/current-project", cwd: "/current-project" };
  const refreshed = [...epicIssues().reverse(), { ...epicIssues()[0], id: "new-alpha" }];
  state.output = { code: 0, stderr: "", stdout: JSON.stringify({ ok: true, schema_version: 5, data: { issues: refreshed } }) };
  let pending;
  const cx = { ...sortContext, spawn: (run) => { pending = run(cx); } };
  view.load(cx);
  await pending;
  tree = view.render();
  assert.deepEqual(view.order, ["b", "d", "unassigned"]);
  assert.deepEqual(content(findElement(tree, header)), ["▸", "Alpha", "3"]);
  findElement(tree, `fold-${header}`).click({}, sortContext);
  view.render();
  assert.deepEqual(view.order, ["b", "d", "c", "a", "new-alpha", "unassigned"]);
  const { view: another } = await dashboard({ ...spaceState(), props: { group: "epic" } });
  another.data = { issues: epicIssues() };
  another.render();
  assert.deepEqual(another.order, ["b", "c", "a", "d", "unassigned"]);
});

for (const width of [500, 1000]) {
  test(`collapsing a selected epic clears selection and details at width ${width}; keyboard skips hidden rows`, async () => {
    const { view } = await dashboard({ ...spaceState(), props: { group: "epic" } });
    view.data = { issues: epicIssues() };
    view.size = { width };
    view.selected = "a";
    view.detail = { id: "a" };
    view.open = true;
    let tree = view.render();
    assert.ok(findElement(tree, "detail-a"));
    findElement(tree, 'fold-epic-ready-"Alpha"').click({}, sortContext);
    tree = view.render();
    assert.equal(view.selected, null);
    assert.equal(view.detail, null);
    assert.equal(view.open, false);
    assert.equal(findElement(tree, "detail-a"), undefined);
    for (const [event, expected] of [
      [{ key_char: "j" }, "b"], [{ key: "down" }, "d"], [{ key_char: "G" }, "unassigned"],
      [{ key: "up" }, "d"], [{ key_char: "g" }, "b"], [{ key_char: "k" }, "b"],
    ]) {
      view.key(event, sortContext);
      assert.equal(view.selected, expected);
      assert.equal(view.open, false);
    }
    findElement(tree, 'fold-epic-ready-"Alpha"').click({}, sortContext);
    view.render();
    assert.equal(view.selected, "b", "expanding another subgroup preserves selection");
    view.key({ key_char: "j" }, sortContext);
    assert.equal(view.selected, "c");
  });
}

test("grouping and refresh cannot leave a selected issue inside a folded epic; all-unassigned remains flat", async () => {
  const state = { ...spaceState(), props: { group: "epic" } };
  const { view } = await dashboard(state);
  view.data = { issues: epicIssues() };
  let tree = view.render();
  findElement(tree, "fold-epic-ready-null").click({}, sortContext);
  view.toggleGrouping(sortContext);
  view.render();
  view.select("unassigned", sortContext, false);
  view.toggleGrouping(sortContext);
  view.render();
  assert.equal(view.selected, null);
  assert.deepEqual(view.order, ["b", "c", "a", "d"]);
  view.selected = "b";
  view.context = { root: "/current-project", cwd: "/current-project" };
  const issues = epicIssues().map((i) => i.id === "b" ? { ...i, epic: null } : i);
  state.output = { code: 0, stderr: "", stdout: JSON.stringify({ ok: true, schema_version: 5, data: { issues } }) };
  let pending;
  const cx = { ...sortContext, spawn: (run) => { pending = run(cx); } };
  view.load(cx);
  await pending;
  tree = view.render();
  assert.equal(view.selected, null);
  assert.deepEqual(view.order, ["c", "a", "d"]);
  assert.deepEqual(content(findElement(tree, "epic-ready-null")), ["▸", "No epic", "2"]);
  view.data = { issues: issues.filter((i) => i.epic === null) };
  tree = view.render();
  assert.deepEqual(view.order, ["unassigned", "b"], "a remembered No epic fold must not hide a flat unassigned state");
  assert.equal(findElement(tree, "epic-ready-null"), undefined);
});

function checkRuns() {
  return [
    { seq: 8, issue_id: "a", result: "failed", exit: 2, output: "failed\n<raw output>", output_truncated: true },
    { seq: 7, issue_id: "b", result: "woken", exit: 1, output: "repair needed" },
    { seq: 6, issue_id: "c", result: "woken", exit: 0, output: "follow-up ready" },
    { seq: 5, issue_id: "d", result: "retry", exit: 75, output: "pending" },
    { seq: 4, issue_id: "closed", result: "passed", exit: 0, output: "passed output" },
    { seq: 3, issue_id: "a", result: "skipped", exit: 0, output: "" },
  ].map((r) => ({ at: "2026-11-25T12:30:00Z", duration_ms: 123, timed_out: false, ...r }));
}

test("Checks shows recorded results, expands literal output and filters failed wakes", async () => {
  const { view } = await dashboard(spaceState());
  view.data = { issues: sortIssues(), check_runs: checkRuns() };
  let tree = view.render();
  findElement(tree, "view-checks").click({}, sortContext);
  tree = view.render();
  const text = content(tree);
  for (const label of ["failed", "woken (failed)", "woken", "retry", "passed", "skipped", "2026-11-25 12:30:00 UTC", "123 ms"]) {
    assert.ok(text.includes(label), label);
  }
  assert.ok(!text.includes("failed\n<raw output>"), "output starts collapsed");
  findElement(tree, "check-output-8").click({}, sortContext);
  tree = view.render();
  assert.ok(content(tree).includes("failed\n<raw output>"), "output is literal, not markdown/HTML");
  assert.ok(content(tree).includes("Output truncated · showing tail"));
  findElement(tree, "checks-failed").click({}, sortContext);
  tree = view.render();
  assert.ok(findElement(tree, "check-8"));
  assert.ok(findElement(tree, "check-7"));
  for (const seq of [6, 5, 4, 3]) assert.equal(findElement(tree, `check-${seq}`), undefined);
  findElement(tree, "view-issues").click({}, sortContext);
  assert.ok(findElement(view.render(), "row-a"));
  findElement(view.render(), "view-checks").click({}, sortContext);
  assert.ok(content(view.render()).includes("failed\n<raw output>"), "switching views retains expansion and filter");
});

test("check IDs copy without selecting and row clicks open closed issues exactly once", async () => {
  for (const width of [400, 1000]) {
    const state = spaceState();
    const { view, executions } = await dashboard(state);
    view.view = "checks";
    view.size = { width };
    view.data = { issues: sortIssues(), check_runs: [...checkRuns(), { ...checkRuns()[4], seq: 2 }] };
    state.output = { code: 0, stderr: "", stdout: JSON.stringify({
      ok: true, data: {
        id: "closed", title: "Completed issue", type: "change", priority: "P2", state: "closed",
        links: [], deps: { blocked_by: [], blocks: [] }, notes: [], events: [],
      },
    }) };
    let copied, stopped = false, pending;
    const cx = {
      ...sortContext, write_to_clipboard: (id) => { copied = id; },
      stop_propagation: () => { stopped = true; }, spawn: (run) => { pending = run(cx); },
    };
    let tree = view.render();
    findElement(tree, "copy-check-4-closed").click({}, cx);
    assert.equal(copied, "closed");
    assert.equal(stopped, true);
    assert.equal(view.selected, null);
    findElement(tree, "check-row-4").click({}, cx);
    await pending;
    tree = view.render();
    assert.equal(view.selected, "closed");
    assert.equal(view.open, true);
    assert.ok(content(tree).includes("Completed issue"));
    assert.equal(content(tree).filter((s) => s === "Completed issue").length, 1, "repeated runs must not duplicate native detail IDs");
    assert.deepEqual(executions[0].args, ["--json", "show", "closed", "--with", "notes,events,deps"]);
    findElement(tree, "hide-detail").click({}, cx);
    assert.equal(findElement(view.render(), "detail-closed"), undefined);
  }
});

test("normal refresh supplies Checks in tab, split and fixed layout without a watch process", async () => {
  for (const props of [{ folder: "/pinned" }, {}, { fixedTab: true }]) {
    const state = { ...spaceState(), props, shared: new Map() };
    state.shared.set("trk-tabs.project", { space: "current", cwd: "/current-project", on: null, epoch: 1 });
    const { view, executions } = await dashboard(state);
    const loc = view.location();
    view.context = { ...loc, key: JSON.stringify(loc), root: loc.cwd };
    view.enter(loc.cwd);
    view.view = "checks";
    view.render();
    let pending;
    const cx = { notify() {}, spawn: (run) => { pending = run(cx); } };
    const refresh = async (runs) => {
      state.output = { code: 0, stderr: "", stdout: JSON.stringify({
        ok: true, schema_version: 5, data: { issues: [], check_runs: runs },
      }) };
      view.load(cx);
      await pending;
    };
    await refresh([checkRuns()[0]]);
    findElement(view.render(), "check-output-8").click({}, sortContext);
    await refresh([checkRuns()[1], checkRuns()[0]]);
    let tree = view.render();
    assert.ok(findElement(tree, "check-7"), "another terminal's new run appears on refresh");
    assert.ok(content(tree).includes("failed\n<raw output>"), "refresh preserves expanded output");
    await refresh([]);
    tree = view.render();
    assert.ok(content(tree).includes("No recorded check runs yet."));
    assert.equal(view.expandedChecks.has(8), false, "evicted runs release expansion state");
    assert.ok(executions.every((e) => e.args.at(-1) === "dashboard"), "no buffered --watch command");
    assert.equal(executions[0].options.cwd, loc.cwd);
    if (props.fixedTab) assert.deepEqual(executions[0].args.slice(1, 3), ["--dir", `${loc.cwd}/.trk`]);
  }
});

test("Checks has empty states and cannot dispatch hidden issue keyboard actions", async () => {
  const { view } = await dashboard({ ...spaceState(), props: { view: "checks" } });
  view.data = { issues: sortIssues(), check_runs: [] };
  view.selected = "a";
  view.open = true;
  view.order = ["a", "b"];
  assert.ok(content(view.render()).includes("No recorded check runs yet."));
  for (const key_char of ["j", "k", "o", "O", "e", "x"]) view.key({ key_char }, sortContext);
  assert.equal(view.selected, "a");
  assert.equal(view.prompt, null);
  assert.deepEqual(view.sort, { key: "priority", desc: false });
  findElement(view.render(), "checks-failed").click({}, sortContext);
  assert.ok(content(view.render()).includes("No failed check runs."));
});
