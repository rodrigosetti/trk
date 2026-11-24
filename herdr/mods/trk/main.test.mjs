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
      h_resizable() {}, resizable_panel() {}, InputState: {}, Input() {}, TextView: {},
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
      notify() {}, checking() {}, shared() {},
      setShared() {}, setBadge() {}, setAgentOrder() {}, nextCommand() {}, watchFolder() {}, layout() {},
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
  return { view: new mod.namespace.default(), commands, executions };
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
