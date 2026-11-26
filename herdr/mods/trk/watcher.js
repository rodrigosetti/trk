import { exec, hc, spaces, terminals, shared, setShared, run, checking } from "herdr";

const KEY = "trk.check.watchers";
const BASE_RETRY_MS = 10000;
const MAX_RETRY_MS = 300000;
const STABLE_MS = 60000;
const PENDING_MS = 120000;
const DIR_LABEL = "trk.check.dir";
const SPACE_LABEL = "trk.check.space";

function records() {
  const value = shared(KEY);
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function record(dir) {
  return records()[dir] ?? null;
}

function save(dir, value) {
  setShared(KEY, { ...records(), [dir]: value });
}

function localTerminal(terminal) {
  return terminal.state === "live" && typeof terminal.address === "string" && !terminal.address.includes("/");
}

function owned(terminal, dir) {
  return localTerminal(terminal) && terminal.labels?.[DIR_LABEL] === dir &&
    typeof terminal.labels?.[SPACE_LABEL] === "string" && Boolean(terminal.labels[SPACE_LABEL]);
}

function localContext(context) {
  return Boolean(context?.root && context?.dir && !context.on &&
    (!context.space || (typeof context.space === "string" && !context.space.includes("/") &&
      !spaces().find((space) => space.id === context.space)?.machine)));
}

function spaceExists(id) {
  return typeof id === "string" && !id.includes("/") &&
    spaces().some((space) => space.id === id && !space.machine);
}

function eligible(context) {
  return localContext(context) && spaceExists(context.space);
}

// The name is an atomic second barrier in core, not evidence of ownership.
// Check the full directory label before adopting a terminal with this name.
function terminalName(dir) {
  let a = 2166136261;
  let b = 5381;
  for (let i = 0; i < dir.length; i++) {
    a = Math.imul(a ^ dir.charCodeAt(i), 16777619);
    b = Math.imul(b, 33) ^ dir.charCodeAt(i);
  }
  return `trk-check-${(a >>> 0).toString(16).padStart(8, "0")}${(b >>> 0).toString(16).padStart(8, "0")}`;
}

function delay(failures) {
  return Math.min(MAX_RETRY_MS, BASE_RETRY_MS * 2 ** Math.min(Math.max(0, failures - 1), 5));
}

function message(error) {
  return error?.message ?? String(error);
}

function initial(context) {
  return {
    generation: 0, stopped: false, failures: 0, retryAt: 0, pending: null,
    detail: "", terminal: null, launchAt: 0, space: context.space,
  };
}

function runnerDetail(runner) {
  return `trk check --watch, pid ${runner.pid} on ${runner.host}`;
}

export class WatcherSupervisor {
  constructor() {
    this.projects = new Map();
    this.managed = new Map();
    this.id = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
    this.sequence = 0;
    this.hostname = null;
    this.hostnameRequest = null;
    this.work = null;
    this.closed = false;
  }

  observe(context) {
    if (this.closed || checking() || !eligible(context) || this.projects.has(context.dir)) return;
    const old = record(context.dir);
    const terminal = terminals().find((terminal) => owned(terminal, context.dir));
    const owner = terminal?.labels[SPACE_LABEL] ?? (spaceExists(old?.space) ? old.space : context.space);
    const project = { root: context.root, dir: context.dir, space: owner };
    this.projects.set(context.dir, project);
    if (terminal) this.managed.set(terminal.address, { dir: context.dir, space: owner });
    if (!old) save(context.dir, initial(project));
    else if (old.space !== owner && !old.pending) save(context.dir, { ...old, space: owner });
  }

  async host() {
    if (this.hostname || this.closed || checking()) return this.hostname;
    if (!this.hostnameRequest) {
      this.hostnameRequest = (async () => {
        try {
          const result = await exec("hostname", [], { timeout: 5000 });
          const name = result.stdout.trim();
          if (result.code === 0 && /^[A-Za-z0-9_.-]+$/.test(name)) this.hostname = name;
        } catch { /* Without a verified host, manual watchers remain external. */ }
        finally { this.hostnameRequest = null; }
        return this.hostname;
      })();
    }
    return this.hostnameRequest;
  }

  matching(runner) {
    if (!runner || !this.hostname || runner.host !== this.hostname || !Number.isInteger(runner.pid) || runner.pid <= 0) return null;
    return terminals().find((terminal) => localTerminal(terminal) &&
      (terminal.pid === runner.pid || terminal.foreground?.pid === runner.pid)) ?? null;
  }

  async dashboard(context) {
    const result = await exec("trk", ["--json", "--dir", context.dir, "dashboard"], { cwd: context.root, timeout: 30000 });
    let output;
    try { output = JSON.parse(result.stdout); } catch { /* Report the CLI error below. */ }
    if (result.code !== 0 || !output?.ok || !output.data ||
      !Object.prototype.hasOwnProperty.call(output.data, "check_runner") ||
      (output.data.check_runner !== null && (!output.data.check_runner || typeof output.data.check_runner !== "object"))) {
      throw new Error(output?.error?.message || result.stderr?.trim() || "Cannot read watcher liveness from trk dashboard.");
    }
    return output.data.check_runner;
  }

  kill(address) {
    run("terminal.kill", { terminal: address });
    this.managed.delete(address);
  }

  stopOwner(dir, space) {
    const old = record(dir);
    if (!old || old.space !== space || old.stopped) return;
    save(dir, { ...old, generation: old.generation + 1, stopped: true, pending: null, terminal: null, detail: "Owner space closed." });
  }

  reap() {
    for (const project of this.projects.values()) {
      if (!spaceExists(project.space)) this.stopOwner(project.dir, project.space);
    }
    for (const terminal of terminals()) {
      const dir = terminal.labels?.[DIR_LABEL];
      if (typeof dir !== "string" || !owned(terminal, dir)) continue;
      const space = terminal.labels[SPACE_LABEL];
      if (!spaceExists(space)) {
        this.stopOwner(dir, space);
        this.kill(terminal.address);
      } else if (record(dir)?.stopped) {
        // Also catches a late launch from another copy after a persisted Stop.
        this.kill(terminal.address);
      }
    }
  }

  async tick() {
    if (this.closed || checking()) return;
    if (this.work) return this.work;
    this.work = this.reconcile().catch((error) => {
      if (this.closed || checking()) return;
      for (const project of this.projects.values()) {
        const value = record(project.dir);
        if (!value || value.stopped) continue;
        const failures = value.failures + 1;
        save(project.dir, { ...value, pending: null, failures, retryAt: Date.now() + delay(failures), detail: message(error) });
      }
    });
    try { await this.work; }
    finally { this.work = null; }
  }

  async reconcile() {
    this.reap();
    await this.host();
    if (this.closed || checking()) return;
    for (const project of this.projects.values()) {
      if (this.closed || checking()) return;
      await this.supervise(project);
    }
  }

  current(project, token, generation) {
    const value = record(project.dir);
    return !this.closed && !checking() && spaceExists(project.space) && value &&
      !value.stopped && value.generation === generation && value.pending?.token === token;
  }

  release(dir, token) {
    const value = record(dir);
    if (value?.pending?.token === token) save(dir, { ...value, pending: null });
  }

  failure(project, token, generation, error, attempted) {
    if (!this.current(project, token, generation)) return;
    const value = record(project.dir);
    const failures = attempted || value.retryAt > Date.now() ? value.failures : value.failures + 1;
    save(project.dir, {
      ...value, pending: null, failures,
      retryAt: attempted || value.retryAt <= Date.now() ? Date.now() + delay(failures) : value.retryAt,
      detail: message(error), terminal: null,
    });
  }

  async supervise(project) {
    let value = record(project.dir);
    if (!value || value.stopped || !spaceExists(project.space)) return;
    if (spaceExists(value.space) && value.space !== project.space) {
      project.space = value.space;
    }
    const now = Date.now();
    if (value.pending && value.pending.until > now) return;
    const token = `${this.id}:${++this.sequence}`;
    const generation = value.generation;
    save(project.dir, { ...value, pending: { token, generation, owner: this.id, until: now + PENDING_MS } });
    let attempted = false;
    try {
      const runner = await this.dashboard(project);
      if (!this.current(project, token, generation)) return;
      value = record(project.dir);
      if (runner) {
        const terminal = this.matching(runner);
        if (terminal && owned(terminal, project.dir)) {
          this.managed.set(terminal.address, { dir: project.dir, space: terminal.labels[SPACE_LABEL] });
        }
        const stable = !value.launchAt || Date.now() - value.launchAt >= STABLE_MS;
        save(project.dir, {
          ...value, pending: null, terminal: terminal?.address ?? null,
          detail: `${runnerDetail(runner)}${terminal ? "" : " (outside Herdr)"}`,
          failures: stable ? 0 : value.failures, retryAt: stable ? 0 : value.retryAt,
        });
        return;
      }
      const name = terminalName(project.dir);
      const all = terminals();
      const existing = all.find((terminal) => owned(terminal, project.dir));
      if (existing) {
        const space = existing.labels[SPACE_LABEL];
        if (!spaceExists(space)) {
          this.stopOwner(project.dir, value.space);
          this.kill(existing.address);
          return;
        }
        project.space = space;
        this.managed.set(existing.address, { dir: project.dir, space });
        save(project.dir, { ...value, space, pending: null, terminal: existing.address, detail: "Starting watcher…" });
        return;
      }
      if (value.retryAt > Date.now()) {
        this.release(project.dir, token);
        return;
      }
      if (all.some((terminal) => localTerminal(terminal) && terminal.name === name)) {
        throw new Error("Watcher terminal name is already in use by an unowned terminal.");
      }
      attempted = true;
      const failures = value.failures + 1;
      save(project.dir, {
        ...value, failures, retryAt: Date.now() + delay(failures),
        space: project.space, detail: "Starting watcher…", terminal: null,
      });
      // No herdr.space/tab labels: core starts it hidden without creating a pane.
      const result = await hc([
        "--json", "run", "--bg", "--no-parent", "--name", name, "--cwd", project.root,
        "--label", `${DIR_LABEL}=${project.dir}`, "--label", `${SPACE_LABEL}=${project.space}`,
        "--", "trk", "--dir", project.dir, "check", "--watch",
      ]);
      let output;
      try { output = JSON.parse(result.stdout); } catch { /* Report the core error below. */ }
      if (result.code !== 0) {
        if (output?.error?.kind === "name_taken" && this.current(project, token, generation)) {
          const own = terminals().find((terminal) => terminal.name === name && owned(terminal, project.dir));
          if (own && spaceExists(own.labels[SPACE_LABEL])) {
            project.space = own.labels[SPACE_LABEL];
            this.managed.set(own.address, { dir: project.dir, space: project.space });
            save(project.dir, { ...record(project.dir), pending: null, space: project.space, terminal: own.address, detail: "Starting watcher…" });
            return;
          }
        }
        throw new Error(output?.error?.message || result.stderr?.trim() || `Cannot start watcher (hc exited ${result.code}).`);
      }
      const address = output?.address;
      if (typeof address !== "string" || !address || address.includes("/")) throw new Error("hc did not return a local watcher address.");
      this.managed.set(address, { dir: project.dir, space: project.space });
      if (!this.current(project, token, generation)) {
        this.kill(address);
        if (!spaceExists(project.space)) this.stopOwner(project.dir, project.space);
        return;
      }
      value = record(project.dir);
      save(project.dir, { ...value, pending: null, terminal: address, launchAt: Date.now(), retryAt: Date.now() + delay(value.failures), detail: "Starting watcher…" });
    } catch (error) {
      this.failure(project, token, generation, error, attempted);
    } finally {
      this.release(project.dir, token);
    }
  }

  status(context, runner) {
    const value = context?.dir ? record(context.dir) : null;
    const enabled = !this.closed && this.projects.has(context?.dir) && eligible(context);
    const stopped = Boolean(value?.stopped);
    const remote = !localContext(context) || Boolean(context?.space && context.space.includes("/"));
    if (runner) {
      const terminal = remote ? null : this.matching(runner);
      return {
        label: runner.busy ? "checking" : "running", terminal: terminal?.address ?? null,
        external: !terminal, detail: `${runnerDetail(runner)}${terminal ? "" : " (outside Herdr)"}`,
        enabled, stopped,
      };
    }
    const terminal = remote ? null : terminals().find((terminal) => owned(terminal, context.dir));
    let detail = remote ? "Watcher supervision is local-only." : stopped ? value.detail || "Stopped" : value?.detail || "No check watcher is running.";
    if (!stopped && value?.retryAt > Date.now() && !terminal && !remote) {
      detail += ` Retry in ${Math.ceil((value.retryAt - Date.now()) / 1000)}s.`;
    }
    return { label: "stopped", terminal: terminal?.address ?? null, external: false, detail, enabled, stopped };
  }

  async stop(context, _runner) {
    if (this.closed || checking() || !localContext(context)) return;
    const old = record(context.dir) ?? initial(context);
    const generation = old.generation + 1;
    save(context.dir, { ...old, generation, stopped: true, pending: null, retryAt: 0, terminal: null, detail: "Stopped" });
    for (const terminal of terminals()) {
      if (owned(terminal, context.dir)) this.kill(terminal.address);
    }
    try {
      await this.host();
      if (this.closed || checking() || record(context.dir)?.generation !== generation) return;
      const runner = await this.dashboard(context);
      if (this.closed || checking() || record(context.dir)?.generation !== generation) return;
      const terminal = this.matching(runner);
      if (terminal) this.kill(terminal.address);
    } catch (error) {
      const value = record(context.dir);
      if (value?.generation === generation) save(context.dir, { ...value, detail: `Stopped; ${message(error)}` });
      throw error;
    }
  }

  async restart(context, _runner) {
    if (this.closed || checking() || !eligible(context)) return;
    const old = record(context.dir) ?? initial(context);
    const generation = old.generation + 1;
    save(context.dir, { ...old, generation, stopped: true, pending: null, detail: "Verifying watcher before restart…" });
    try {
      await this.host();
      if (this.closed || checking() || record(context.dir)?.generation !== generation) return;
      const runner = await this.dashboard(context);
      if (this.closed || checking() || record(context.dir)?.generation !== generation) return;
      const terminal = this.matching(runner);
      if (runner && !terminal) throw new Error(`Cannot restart watcher outside Herdr: ${runnerDetail(runner)}.`);
      if (terminal) this.kill(terminal.address);
      for (const own of terminals()) {
        if (owned(own, context.dir) && own.address !== terminal?.address) this.kill(own.address);
      }
      const project = { root: context.root, dir: context.dir, space: context.space };
      this.projects.set(context.dir, project);
      save(context.dir, {
        ...record(context.dir), stopped: false, pending: null, failures: 0, retryAt: 0,
        terminal: null, launchAt: 0, space: context.space, detail: "Restarting watcher…",
      });
      // An older in-flight tick may have read liveness before this restart.
      if (this.work) await this.work;
      if (!this.closed && record(context.dir)?.generation === generation && !record(context.dir)?.stopped) await this.tick();
    } catch (error) {
      const value = record(context.dir);
      if (value?.generation === generation) save(context.dir, { ...value, stopped: old.stopped, detail: message(error) });
      throw error;
    }
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    if (checking()) return;
    for (const address of [...this.managed.keys()]) this.kill(address);
    for (const project of this.projects.values()) {
      const value = record(project.dir);
      if (value?.pending?.owner === this.id) save(project.dir, { ...value, pending: null });
    }
  }
}
