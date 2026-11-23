import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { SourceTextModule, SyntheticModule } from "node:vm";

const source = await readFile(new URL("./main.js", import.meta.url), "utf8");

async function dashboard(state) {
  const commands = [];
  const executions = [];
  const modules = {
    "gpui-kit": { View: class {}, div() {}, svg() {} },
    "gpui-base": { v_flex() {}, h_flex() {}, InputState: {}, Input() {}, TextView: {} },
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
        return { code: 0, stdout: JSON.stringify({ ok: true, data: {} }), stderr: "" };
      },
      agents() {}, terminals() {}, theme() {}, notify() {}, checking() {}, shared() {},
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
  assert.deepEqual(commands, [{ name: "tab.new", args: { name: "trk", space: "current", mod: "trk" } }]);
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
