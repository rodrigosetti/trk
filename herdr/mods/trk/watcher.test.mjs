import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createContext, SourceTextModule, SyntheticModule } from "node:vm";

const source = await readFile(new URL("./watcher.js", import.meta.url), "utf8");
const project = (dir = "/projects/one/.trk", space = "one") => ({ root: dir.replace(/\/\.trk$/, ""), dir, on: null, space });
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};

function harness() {
  const h = {
    now: 1_000_000, shared: new Map(), terminals: [], runners: new Map(),
    spaces: [{ id: "one" }, { id: "two" }], launches: [], commands: [], executions: [],
    failures: 0, checking: false, nextPid: 100, launchGate: null, killGate: null,
  };
  h.advance = (ms = 300_001) => { h.now += ms; };
  h.remove = (address) => {
    const terminal = h.terminals.find((t) => t.address === address);
    h.terminals = h.terminals.filter((t) => t.address !== address);
    if (terminal) for (const [dir, runner] of h.runners) {
      if (runner?.pid === terminal.pid || runner?.pid === terminal.foreground?.pid) h.runners.set(dir, null);
    }
  };
  h.manual = (context, { host = "local-host", foreground = false, address = "manual" } = {}) => {
    const pid = ++h.nextPid;
    const runner = { pid, host, busy: false };
    h.runners.set(context.dir, runner);
    h.terminals.push({ address, pid: foreground ? ++h.nextPid : pid,
      foreground: foreground ? { pid, argv: ["trk", "check", "--watch"] } : null,
      argv: ["trk", "check", "--watch"], cwd: context.root, labels: {}, state: "live" });
    return runner;
  };
  h.copy = async () => {
    class Clock extends Date {
      constructor(...args) { super(...(args.length ? args : [h.now])); }
      static now() { return h.now; }
    }
    const context = createContext({ Date: Clock, console, setTimeout, clearTimeout });
    const api = {
      shared: (key) => h.shared.get(key), setShared: (key, value) => h.shared.set(key, value),
      checking: () => h.checking, spaces: () => h.spaces, terminals: () => h.terminals,
      hc: (args) => api.exec("hc", args),
      run: (name, args) => {
        h.commands.push({ name, args });
        if (name !== "terminal.kill") throw new Error(`unexpected run ${name}`);
        const address = typeof args === "string" ? args : args.terminal ?? args.address;
        const kill = () => h.remove(address);
        if (h.killGate) return h.killGate.promise.then(kill);
        kill();
      },
      exec: async (command, args = [], options = {}) => {
        h.executions.push({ command, args, options });
        const ok = (stdout = "") => ({ code: 0, stdout, stderr: "" });
        if (command === "hostname") return ok("local-host\n");
        if (command === "trk") {
          assert.ok(args.includes("dashboard"), "supervision queries fresh dashboard liveness");
          const index = args.indexOf("--dir");
          const dir = index < 0 ? options.cwd : args[index + 1];
          return ok(JSON.stringify({ ok: true, data: h.missingLiveness ? {} : { check_runner: h.runners.get(dir) ?? null } }));
        }
        assert.equal(command, "hc");
        assert.equal(args[0], "--json");
        assert.equal(args[1], "run");
        h.launches.push({ args: [...args], options });
        if (h.launchGate) await h.launchGate.promise;
        if (h.failures > 0) { h.failures--; return { code: 1, stdout: "", stderr: "launch failed" }; }
        const value = (flag) => args[args.indexOf(flag) + 1];
        const name = value("--name");
        if (h.terminals.some((t) => t.name === name)) return { code: 1, stdout: "", stderr: "name_taken" };
        const labels = {};
        args.forEach((arg, index) => {
          if (arg === "--label") {
            const label = args[index + 1];
            const split = label.indexOf("=");
            labels[label.slice(0, split)] = label.slice(split + 1);
          }
        });
        const dir = args[args.lastIndexOf("--dir") + 1];
        const pid = ++h.nextPid;
        const terminal = { address: `term-${pid}`, name, pid, foreground: null, state: "live",
          argv: args.slice(args.indexOf("--") + 1), cwd: value("--cwd"), labels };
        h.terminals.push(terminal);
        h.runners.set(dir, { pid, host: "local-host", busy: false });
        return ok(JSON.stringify(terminal));
      },
    };
    const mod = new SourceTextModule(source, { context });
    await mod.link((name) => {
      assert.equal(name, "herdr");
      return new SyntheticModule(Object.keys(api), function () {
        for (const [key, value] of Object.entries(api)) this.setExport(key, value);
      }, { context });
    });
    await mod.evaluate();
    return new mod.namespace.WatcherSupervisor();
  };
  return h;
}

function assertManagedLaunch(h, context) {
  const { args } = h.launches.at(-1);
  assert.ok(args.includes("--bg"));
  assert.ok(args.includes("--no-parent"));
  assert.deepEqual(args.slice(args.indexOf("--") + 1), ["trk", "--dir", context.dir, "check", "--watch"]);
  const terminal = h.terminals.at(-1);
  assert.equal(terminal.labels["trk.check.dir"], context.dir);
  assert.equal(terminal.labels["trk.check.space"], context.space);
  assert.ok(!Object.keys(terminal.labels).some((key) => key.startsWith("herdr.")));
}

test("watching is opt-in, local, and explicitly bound to the observed project", async () => {
  const h = harness();
  const supervisor = await h.copy();
  await supervisor.tick();
  assert.equal(h.launches.length, 0);
  supervisor.observe({ ...project(), on: "remote-host" });
  supervisor.observe({ ...project(), space: null });
  await supervisor.tick();
  assert.equal(h.launches.length, 0);
  const context = project();
  supervisor.observe(context);
  await supervisor.tick();
  await supervisor.tick();
  assert.equal(h.launches.length, 1);
  assertManagedLaunch(h, context);
  assert.equal(supervisor.status(context, h.runners.get(context.dir)).label, "running");
  h.runners.get(context.dir).busy = true;
  assert.equal(supervisor.status(context, h.runners.get(context.dir)).label, "checking");
});

test("independent copies and coalesced same-copy ticks start one watcher", async () => {
  const h = harness();
  const a = await h.copy();
  const b = await h.copy();
  a.observe(project());
  b.observe(project());
  await Promise.all([a.tick(), a.tick(), b.tick()]);
  assert.equal(h.launches.length, 1);
  assert.equal(h.terminals.length, 1);
});

test("external watcher is displayed without control and adopted until it disappears", async () => {
  const h = harness();
  const supervisor = await h.copy();
  const context = project();
  const runner = { pid: 1234, host: "local-host", busy: false };
  h.runners.set(context.dir, runner);
  supervisor.observe(context);
  await supervisor.tick();
  assert.equal(h.launches.length, 0);
  const status = supervisor.status(context, runner);
  assert.equal(status.external, true);
  assert.equal(status.terminal, null);
  assert.match(status.detail, /1234/);
  assert.match(status.detail, /local-host/);
  await assert.rejects(supervisor.restart(context, runner), /outside Herdr/);
  assert.equal(h.commands.length, 0);
  assert.equal(h.launches.length, 0);
  h.runners.set(context.dir, null);
  h.advance();
  await supervisor.tick();
  assert.equal(h.launches.length, 1);
});

for (const foreground of [false, true]) {
  test(`manual local watcher matches ${foreground ? "foreground" : "terminal"} PID and can stop/restart`, async () => {
    const h = harness();
    const supervisor = await h.copy();
    const context = project();
    const runner = h.manual(context, { foreground });
    supervisor.observe(context);
    await supervisor.tick();
    assert.equal(supervisor.status(context, runner).terminal, "manual");
    assert.equal(supervisor.status(context, runner).external, false);
    await supervisor.stop(context, runner);
    assert.equal(h.terminals.length, 0);
    await supervisor.tick();
    assert.equal(h.launches.length, 0);
    assert.equal(supervisor.status(context, null).stopped, true);
    h.advance();
    await supervisor.restart(context, null);
    await supervisor.tick();
    assert.equal(h.launches.length, 1);
  });
}

test("same PID on another host or a remote terminal is never controlled", async () => {
  for (const options of [{ host: "other-host" }, { address: "remote/terminal" }]) {
    const h = harness();
    const supervisor = await h.copy();
    const context = project();
    const runner = h.manual(context, options);
    supervisor.observe(context);
    await supervisor.tick();
    assert.equal(supervisor.status(context, runner).external, true);
    await assert.rejects(supervisor.restart(context, runner), /outside Herdr/);
    await supervisor.stop(context, runner);
    assert.equal(h.commands.length, 0);
    assert.equal(h.terminals.length, 1);
    assert.equal(h.launches.length, 0);
  }
});

test("Stop persists across another copy's observation and an in-flight launch", async () => {
  const h = harness();
  const a = await h.copy();
  const b = await h.copy();
  const context = project();
  a.observe(context);
  h.launchGate = deferred();
  const tick = a.tick();
  // Let the dashboard and hostname awaits settle without advancing fake time.
  for (let i = 0; i < 20 && h.launches.length === 0; i++) await new Promise(setImmediate);
  assert.equal(h.launches.length, 1);
  await b.stop(context, null);
  h.launchGate.resolve();
  await tick;
  b.observe(context);
  h.advance();
  await b.tick();
  await a.tick();
  assert.equal(h.launches.length, 1);
  assert.equal(h.terminals.length, 0);
  assert.equal(b.status(context, null).stopped, true);
});

test("Restart waits for terminal death rather than creating a duplicate", async () => {
  const h = harness();
  const a = await h.copy();
  const b = await h.copy();
  const context = project();
  a.observe(context);
  await a.tick();
  b.observe(context);
  h.killGate = deferred();
  const restart = a.restart(context, h.runners.get(context.dir));
  for (let i = 0; i < 10; i++) await new Promise(setImmediate);
  await b.tick();
  assert.equal(h.launches.length, 1);
  h.killGate.resolve();
  await restart;
  h.advance();
  await a.tick();
  assert.equal(h.terminals.length, 1);
  assert.equal(h.launches.length, 2);
});

test("terminal disappearance and repeated quick exits share retry backoff", async () => {
  const h = harness();
  const a = await h.copy();
  const b = await h.copy();
  const context = project();
  a.observe(context);
  b.observe(context);
  await a.tick();
  h.remove(h.terminals[0].address);
  await b.tick();
  assert.equal(h.launches.length, 1);
  h.advance();
  await b.tick();
  assert.equal(h.launches.length, 2);
  h.remove(h.terminals[0].address);
  await a.tick();
  await b.tick();
  assert.equal(h.launches.length, 2);
  h.advance();
  await a.tick();
  assert.equal(h.launches.length, 3);
});

test("launch failures back off across copies", async () => {
  const h = harness();
  h.failures = 3;
  const a = await h.copy();
  const b = await h.copy();
  a.observe(project());
  b.observe(project());
  await a.tick();
  await b.tick();
  assert.equal(h.launches.length, 1);
  h.advance();
  await b.tick();
  await a.tick();
  assert.equal(h.launches.length, 2);
  h.advance();
  await a.tick();
  assert.equal(h.launches.length, 3);
  h.advance();
  await b.tick();
  assert.equal(h.terminals.length, 1);
});

test("context switches retain prior projects until their owning space disappears", async () => {
  const h = harness();
  const supervisor = await h.copy();
  const first = project();
  const second = project("/projects/two/.trk", "two");
  supervisor.observe(first);
  await supervisor.tick();
  supervisor.observe(second);
  await supervisor.tick();
  assert.equal(h.terminals.length, 2);
  h.remove(h.terminals.find((t) => t.labels["trk.check.dir"] === first.dir).address);
  h.advance();
  await supervisor.tick();
  assert.equal(h.terminals.length, 2);
  h.spaces = [{ id: "two" }];
  await supervisor.tick();
  assert.equal(h.terminals.length, 1);
  assert.equal(h.terminals[0].labels["trk.check.dir"], second.dir);
  h.advance();
  await supervisor.tick();
  assert.equal(h.terminals.length, 1);
});

test("an unobserving copy reaps orphan ownership labels", async () => {
  const h = harness();
  const a = await h.copy();
  a.observe(project());
  await a.tick();
  h.spaces = [];
  const b = await h.copy();
  await b.tick();
  assert.equal(h.terminals.length, 0);
  h.advance();
  await b.tick();
  assert.equal(h.launches.length, 1);
});

test("checking guard prevents polling side effects", async () => {
  const h = harness();
  const supervisor = await h.copy();
  supervisor.observe(project());
  h.checking = true;
  await supervisor.tick();
  assert.equal(h.launches.length, 0);
  h.checking = false;
  await supervisor.tick();
  assert.equal(h.launches.length, 1);
});

test("default-off copies do not revive persisted registrations", async () => {
  const h = harness();
  const enabled = await h.copy();
  enabled.observe(project());
  await enabled.tick();
  h.remove(h.terminals[0].address);
  h.advance();
  const disabled = await h.copy();
  await disabled.tick();
  assert.equal(h.launches.length, 1);
  assert.equal(h.terminals.length, 0);
});

test("close kills only terminals launched by that instance and prevents new work", async () => {
  const h = harness();
  const a = await h.copy();
  const b = await h.copy();
  a.observe(project());
  await a.tick();
  b.observe(project("/projects/two/.trk", "two"));
  await b.tick();
  const own = h.terminals.find((t) => t.labels["trk.check.dir"] === project().dir).address;
  a.close();
  assert.ok(!h.terminals.some((t) => t.address === own));
  assert.equal(h.terminals.length, 1);
  h.advance();
  await a.tick();
  assert.equal(h.launches.length, 2);
});

test("close during startup cleans the late terminal", async () => {
  const h = harness();
  const supervisor = await h.copy();
  supervisor.observe(project());
  h.launchGate = deferred();
  const tick = supervisor.tick();
  for (let i = 0; i < 20 && h.launches.length === 0; i++) await new Promise(setImmediate);
  assert.equal(h.launches.length, 1);
  supervisor.close();
  h.launchGate.resolve();
  await tick;
  assert.equal(h.terminals.length, 0);
});

test("a deterministic-name collision never adopts or kills a differently owned terminal", async () => {
  const h = harness();
  const supervisor = await h.copy();
  const context = project();
  supervisor.observe(context);
  await supervisor.tick();
  const terminal = h.terminals[0];
  h.runners.set(context.dir, null);
  terminal.labels = { "trk.check.dir": "/unrelated/.trk", "trk.check.space": "two" };
  h.advance();
  await supervisor.tick();
  await supervisor.stop(context, null);
  assert.ok(h.terminals.includes(terminal));
  assert.equal(h.commands.length, 0);
});

test("stale displayed runner cannot authorize killing a now-external watcher", async () => {
  const h = harness();
  const supervisor = await h.copy();
  const context = project();
  const stale = h.manual(context);
  supervisor.observe(context);
  await supervisor.tick();
  h.runners.set(context.dir, { pid: 9999, host: "local-host", busy: false });
  await assert.rejects(supervisor.restart(context, stale), /outside Herdr/);
  await supervisor.stop(context, stale);
  assert.equal(h.commands.length, 0);
  assert.equal(h.terminals.length, 1);
  assert.equal(h.launches.length, 0);
});

test("a local folder prop in a remote owning space never launches locally", async () => {
  const h = harness();
  h.spaces[0].machine = "remote-host";
  const supervisor = await h.copy();
  supervisor.observe(project());
  await supervisor.tick();
  assert.equal(h.launches.length, 0);
});

test("an observing copy cleans adopted managed watchers on close", async () => {
  const h = harness();
  const a = await h.copy();
  const b = await h.copy();
  a.observe(project());
  await a.tick();
  b.observe(project());
  await b.tick();
  b.close();
  assert.equal(h.terminals.length, 0);
});

test("missing liveness support fails closed instead of starting a second watcher", async () => {
  const h = harness();
  h.missingLiveness = true;
  const supervisor = await h.copy();
  supervisor.observe(project());
  await supervisor.tick();
  assert.equal(h.launches.length, 0);
});

test("close never kills a manual PID-matched watcher", async () => {
  const h = harness();
  const context = project();
  const manual = h.manual(context);
  const observer = await h.copy();
  observer.observe(context);
  h.advance();
  await observer.tick();
  assert.equal(observer.status(context, manual).terminal, "manual");
  observer.close();
  assert.equal(h.terminals.length, 1);
  assert.equal(h.terminals[0].address, "manual");
});

