import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { promisify } from "node:util";

const source = await readFile(new URL("./project.js", import.meta.url), "utf8");
const { projectLocation, probeProject } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
const run = promisify(execFile);
const env = {
  ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_"))),
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_TERMINAL_PROMPT: "0",
};

async function fixture(t) {
  const path = await mkdtemp(join(tmpdir(), "trk-project-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return realpath(path);
}

async function git(cwd, ...args) {
  return run("git", ["-c", "init.defaultBranch=main", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "-c", "commit.gpgsign=false", ...args], { cwd, env });
}

async function repository(parent, name = "primary") {
  const path = join(parent, name);
  await git(parent, "init", "--quiet", path);
  await git(path, "commit", "--quiet", "--allow-empty", "-m", "fixture");
  return path;
}

async function database(path) {
  await mkdir(join(path, ".trk"), { recursive: true });
  await writeFile(join(path, ".trk", "trk.db"), "fixture database");
}

async function execute(command, args, options) {
  try {
    const result = await run(command, args, { cwd: options.cwd, env, timeout: options.timeout });
    return { code: 0, stdout: result.stdout, stderr: result.stderr };
  } catch (error) {
    if (typeof error.code !== "number") throw error;
    return { code: error.code, stdout: error.stdout, stderr: error.stderr };
  }
}

function probe(cwd, on = null) {
  return probeProject(execute, { cwd, on });
}

test("projectLocation follows space tab order and ignores nonterminals and foreign panes", () => {
  const space = Object.freeze({
    folder: "/fallback",
    machine: "remote",
    tabs: Object.freeze([{ id: "dashboard" }, { id: "first" }, { id: "second" }]),
  });
  const panes = Object.freeze([
    { tab: "foreign", terminal: "other", cwd: "/other" },
    { tab: "second", terminal: "second-terminal", cwd: "/second" },
    { tab: "dashboard", terminal: null, cwd: "/not-a-terminal" },
    { tab: "first", terminal: "first-terminal", cwd: "/first" },
  ]);
  assert.deepEqual(projectLocation(space, panes), { cwd: "/first", on: "remote" });
});

test("projectLocation falls back only to the space folder and never needs focus", () => {
  assert.deepEqual(projectLocation({ folder: "/own", tabs: [{ id: "one" }] }, [
    { tab: "one", terminal: "terminal", cwd: null },
    { tab: "foreign", terminal: "other", cwd: "/foreign" },
  ]), { cwd: "/own", on: null });
  assert.deepEqual(projectLocation({ folder: "", tabs: [] }, []), { cwd: null, on: null });
  assert.deepEqual(projectLocation(null, []), { cwd: null, on: null });
});

test("primary checkout and subdirectories qualify at the actual repository root", async (t) => {
  const parent = await fixture(t);
  const root = await repository(parent);
  await database(root);
  const nested = join(root, "src", "nested");
  await mkdir(nested, { recursive: true });
  assert.deepEqual(await probe(root), { cwd: root, on: null });
  assert.deepEqual(await probe(nested), { cwd: root, on: null });
});

test("a primary checkout with separate Git metadata still qualifies", async (t) => {
  const parent = await fixture(t);
  const root = join(parent, "checkout");
  await git(parent, "init", "--quiet", "--separate-git-dir", join(parent, "metadata"), root);
  await database(root);
  assert.deepEqual(await probe(root), { cwd: root, on: null });
});

test("linked worktrees are excluded with shared or their own tracker database", async (t) => {
  const parent = await fixture(t);
  const root = await repository(parent);
  await database(root);
  const linked = join(parent, "ordinary-folder-name");
  await git(root, "worktree", "add", "--quiet", "--detach", linked, "HEAD");
  await symlink(join(root, ".trk"), join(linked, ".trk"));
  assert.equal(await probe(linked), null);
  await rm(join(linked, ".trk"));
  await database(linked);
  const nested = join(linked, "nested");
  await mkdir(nested);
  assert.equal(await probe(linked), null);
  assert.equal(await probe(nested), null);
  assert.deepEqual(await probe(root), { cwd: root, on: null });
});

test("a Git checkout cannot inherit a tracker or use a subdirectory's tracker", async (t) => {
  const parent = await fixture(t);
  await database(parent);
  const root = await repository(parent);
  assert.equal(await probe(root), null);
  const nested = join(root, "nested");
  await mkdir(nested);
  await database(nested);
  assert.equal(await probe(nested), null);
  assert.equal(await probe(root), null);
});

test("non-Git folders require their own database, not an ancestor's", async (t) => {
  const root = await fixture(t);
  await database(root);
  const nested = join(root, "nested");
  await mkdir(nested);
  assert.deepEqual(await probe(root), { cwd: root, on: null });
  assert.equal(await probe(nested), null);
  await database(nested);
  assert.deepEqual(await probe(nested), { cwd: nested, on: null });
  assert.equal(await probe(join(root, "missing")), null);
});

test("Git and non-Git projects reject symlink tracker directories and databases", async (t) => {
  const parent = await fixture(t);
  const backing = join(parent, "backing");
  await mkdir(backing);
  await database(backing);
  const nongit = join(parent, "nongit");
  await mkdir(nongit);
  const checkout = await repository(parent);
  for (const root of [nongit, checkout]) {
    await symlink(join(backing, ".trk"), join(root, ".trk"));
    assert.equal(await probe(root), null);
    await rm(join(root, ".trk"));
    await mkdir(join(root, ".trk"));
    await symlink(join(backing, ".trk", "trk.db"), join(root, ".trk", "trk.db"));
    assert.equal(await probe(root), null);
  }
});

test("paths with spaces and apostrophes travel as argv and resolve physical roots", async (t) => {
  const parent = await fixture(t);
  const root = await repository(parent, "project's folder");
  await database(root);
  const nested = join(root, "nested's folder");
  await mkdir(nested);
  const alias = join(parent, "alias's folder");
  await symlink(nested, alias);
  const calls = [];
  const exec = async (command, args, options) => {
    calls.push({ command, args, options });
    return execute(command, args, options);
  };
  assert.deepEqual(await probeProject(exec, { cwd: alias, on: null }), { cwd: root, on: null });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, "sh");
  assert.equal(calls[0].args.at(-1), alias);
  assert.equal(calls[0].args[1].includes(alias), false);
  assert.deepEqual(calls[0].options, { cwd: alias, on: null, timeout: 5000 });
});

test("remote qualification uses one remote exec and keeps its machine", async (t) => {
  const root = await fixture(t);
  await database(root);
  let calls = 0;
  const exec = async (command, args, options) => {
    calls++;
    assert.deepEqual(options, { cwd: root, on: "remote-machine", timeout: 5000 });
    return execute(command, args, options);
  };
  assert.deepEqual(await probeProject(exec, { cwd: root, on: "remote-machine" }), { cwd: root, on: "remote-machine" });
  assert.equal(calls, 1);
});

test("unreachable remote fails closed without a local retry even if local tracker exists", async (t) => {
  const root = await fixture(t);
  await database(root);
  let calls = 0;
  const exec = async (_command, _args, options) => {
    calls++;
    assert.equal(options.on, "unreachable");
    throw new Error("machine unreachable");
  };
  assert.equal(await probeProject(exec, { cwd: root, on: "unreachable" }), null);
  assert.equal(calls, 1);
});

test("Git errors and missing Git fail closed for folders with a database", async (t) => {
  const root = await fixture(t);
  await database(root);
  await writeFile(join(root, ".git"), "gitdir: missing-metadata\n");
  assert.equal(await probe(root), null);
  await rm(join(root, ".git"));
  let calls = 0;
  const noGit = async (command, args, options) => {
    calls++;
    assert.equal(command, "sh");
    try {
      const result = await run("/bin/sh", args, { cwd: options.cwd, env: { ...env, PATH: "/no-such-program-directory" }, timeout: options.timeout });
      return { code: 0, ...result };
    } catch (error) {
      return { code: error.code, stdout: error.stdout, stderr: error.stderr };
    }
  };
  assert.equal(await probeProject(noGit, { cwd: root, on: null }), null);
  assert.equal(calls, 1);
});

test("missing locations skip execution and unsuccessful or malformed output is rejected", async () => {
  let calls = 0;
  const never = async () => { calls++; throw new Error("must not execute"); };
  assert.equal(await probeProject(never, { cwd: null, on: "remote" }), null);
  assert.equal(await probeProject(never, null), null);
  assert.equal(calls, 0);
  for (const result of [
    { code: 1, stdout: "/project\0" },
    { code: 0, stdout: "/project\n" },
    { code: 0, stdout: "relative\0" },
    { code: 0, stdout: "/one\0/two\0" },
    { code: 0, stdout: null },
    null,
  ]) {
    assert.equal(await probeProject(async () => result, { cwd: "/project", on: null }), null);
  }
});
