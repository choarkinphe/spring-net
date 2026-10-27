"use strict";

// Offline only: node --test web/transport.test.cjs (no npm dependencies).
const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const transportSource = fs.readFileSync(path.join(__dirname, "transport.js"), "utf8");
const appSource = fs.readFileSync(path.join(__dirname, "app.js"), "utf8");
const flush = () => new Promise((resolve) => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function harness(invoke) {
  let id = 0;
  const timers = new Map();
  const window = invoke ? { __TAURI__: { core: { invoke } } } : {};
  const context = vm.createContext({
    window, TextEncoder, Error, TypeError, URL, AbortController,
    setTimeout: (fn, delay) => { timers.set(++id, { fn, delay }); return id; },
    clearTimeout: (key) => timers.delete(key)
  });
  vm.runInContext(transportSource, context);
  return { context, transport: window.AoNetTransport, timers };
}
const plain = (value) => value === undefined ? undefined : JSON.parse(JSON.stringify(value));

test("fixed IPC mappings preserve bodies and never pass tokens or paths", async () => {
  const calls = [];
  const h = harness(async (command, args) => { calls.push([command, plain(args)]); return { ok: true }; });
  const config = { mode: "probe", list_ids: ["a"], concurrency: 1 };
  await h.transport.request("/api/bootstrap");
  await h.transport.request("/api/state", { method: "GET" });
  await h.transport.request("/api/runs", { method: "POST", body: JSON.stringify(config) });
  await h.transport.request("/api/stop", { method: "POST", body: "{}" });
  await h.transport.request("/api/lists", { method: "PUT", body: '{"lists":[]}' });
  assert.deepEqual(calls, [
    ["bootstrap", undefined], ["state", undefined], ["start_run", { config }],
    ["stop_run", undefined], ["save_lists", { lists: [] }]
  ]);
  await assert.rejects(h.transport.request("/api/export.json"), /不支持/);
  await assert.rejects(h.transport.request("/api/runs", { method: "POST", body: "{" }));
  assert.equal(calls.length, 5);
  assert.equal(h.timers.size, 0);
});

test("workspace IPC accepts only named operations and historical UUID exports", async () => {
  const calls = [];
  const h = harness(async (command, args) => { calls.push([command, plain(args)]); return true; });
  await h.transport.request("/api/workspace", { method: "POST", body: JSON.stringify({ operation: "history.list", payload: {} }) });
  await h.transport.request("/api/workspace", { method: "POST", body: JSON.stringify({ operation: "authorization.prepare", payload: { targets: [] } }) });
  await assert.rejects(h.transport.request("/api/workspace", { method: "POST", body: JSON.stringify({ operation: "write_file", payload: { path: "/tmp/x" } }) }), /不支持/);
  const id = "00000000-0000-4000-8000-000000000001";
  await h.transport.exportDialog("json", id);
  await assert.rejects(h.transport.exportDialog("json", "../../path"), /无效/);
  assert.deepEqual(calls, [["workspace", { operation: "history.list", payload: {} }], ["workspace", { operation: "authorization.prepare", payload: { targets: [] } }], ["export_dialog", {kind:"json",id}]]);
});

test("browser transport is inert; app keeps HTTP and SSE", async () => {
  const h = harness();
  assert.equal(h.transport.isDesktop, false);
  assert.equal(await h.transport.request("/api/bootstrap"), null);
  assert.match(appSource, /if \(transport\.isDesktop\) return transport\.request\(path, options\)/);
  assert.match(appSource, /await fetch\(path/);
  assert.match(appSource, /new EventSource\("\/api\/events"\)/);
  const html = fs.readFileSync(path.join(__dirname, "legacy-workbench.html"), "utf8");
  assert.ok(html.indexOf('src="transport.js"') < html.indexOf('src="app.js"'));
  assert.doesNotMatch(html, /<script(?![^>]*\bsrc=)[^>]*>/i);
});

test("native picker cancellation and export booleans; only kind crosses export IPC", async () => {
  const calls = [];
  let result = null;
  const h = harness(async (command, args) => { calls.push([command, plain(args)]); return result; });
  assert.equal(await h.transport.importLists(), null);
  result = { lists: [{ name: "test", targets: [{ url: "https://example.com" }] }] };
  assert.deepEqual(plain(await h.transport.importLists()), result);
  result = false;
  assert.equal(await h.transport.exportDialog("json"), false);
  result = true;
  assert.equal(await h.transport.exportDialog("csv"), true);
  assert.equal(await h.transport.exportDialog("lists"), true);
  assert.deepEqual(calls.slice(2), [
    ["export_dialog", { kind: "json" }], ["export_dialog", { kind: "csv" }],
    ["export_dialog", { kind: "lists" }]
  ]);
  await assert.rejects(h.transport.exportDialog("/tmp/file"), /不支持/);
  result = "true";
  await assert.rejects(h.transport.exportDialog("json"), (error) => error.uncertain === true);
  assert.equal(h.timers.size, 0, "native dialogs have no timeout");
});

test("string errors become Error; unknown writes and timeouts are uncertain, never retried", async () => {
  let calls = 0;
  let failure = "后台拒绝操作";
  const h = harness(async () => { calls++; throw failure; });
  await assert.rejects(h.transport.request("/api/stop", { method: "POST" }), (error) => {
    assert.ok(error instanceof Error);
    assert.equal(error.message, failure);
    assert.notEqual(error.uncertain, true);
    return true;
  });
  failure = new TypeError("IPC disconnected");
  await assert.rejects(h.transport.request("/api/stop", { method: "POST" }), (error) => error.uncertain === true);
  assert.equal(calls, 2);
  const pending = deferred();
  const slow = harness(() => { calls++; return pending.promise; });
  const request = slow.transport.request("/api/stop", { method: "POST" });
  const rejected = assert.rejects(request, (error) => error.uncertain === true && /不要重复提交/.test(error.message));
  [...slow.timers.values()][0].fn();
  await rejected;
  pending.resolve(null);
  await flush();
  assert.equal(calls, 3);
  assert.equal(slow.timers.size, 0);
});

test("1 MiB UTF-8, 50 lists and 1000 total targets rejected before saving", async () => {
  let calls = 0;
  const h = harness(async () => { calls++; return null; });
  for (const lists of [Array.from({ length: 51 }, () => ({ targets: [] })), [{ targets: Array(1001).fill({}) }]]) {
    await assert.rejects(h.transport.request("/api/lists", { method: "PUT", body: JSON.stringify({ lists }) }), /最多/);
  }
  await assert.rejects(h.transport.request("/api/runs", { method: "POST", body: JSON.stringify({ name: "中".repeat(350000) }) }), /1 MiB/);
  assert.equal(calls, 0);
  const imported = harness(async () => ({ lists: Array.from({ length: 51 }, () => ({ targets: [] })) }));
  await assert.rejects(imported.transport.importLists(), /最多/);
});

test("state IPC remains serialized even after timeout", async () => {
  const pending = deferred();
  let calls = 0;
  const h = harness(() => { calls++; return calls === 1 ? pending.promise : null; });
  const first = h.transport.request("/api/state");
  const rejected = assert.rejects(first, /超时/);
  [...h.timers.values()][0].fn();
  await rejected;
  const second = h.transport.request("/api/state");
  await flush();
  assert.equal(calls, 1);
  pending.resolve(null);
  assert.equal(await second, null);
  assert.equal(calls, 2);
});

test("queued state reads time out even when the first IPC never settles", async () => {
  const pending = deferred();
  let calls = 0;
  const h = harness(() => { calls++; return pending.promise; });
  const first = assert.rejects(h.transport.request("/api/state"), /超时/);
  [...h.timers.values()][0].fn();
  await first;
  const second = assert.rejects(h.transport.request("/api/state"), /超时/);
  await flush();
  assert.equal(h.timers.size, 1);
  [...h.timers.values()][0].fn();
  await second;
  assert.equal(calls, 1);
  pending.resolve(null);
  await flush();
  assert.equal(calls, 1, "expired queued read must never dispatch later");
});

function appHarness() {
  const pendingStates = [];
  const commands = [];
  const bootstrap = { token: "compatibility", lists: [], authorized: [], limits: { public_concurrency: 4, public_rps: 2, public_cooldown: 30 } };
  const h = harness((command) => {
    commands.push(command);
    if (command === "bootstrap") return Promise.resolve(bootstrap);
    if (command === "batch_state") return Promise.resolve(null);
    assert.equal(command, "state", "initialization and polling must not start a run");
    const pending = deferred(); pendingStates.push(pending); return pending.promise;
  });
  const nodes = new Map();
  h.context.document = { getElementById: (id) => {
    if (!nodes.has(id)) nodes.set(id, { textContent: "", classList: { add() {} }, replaceChildren() {} });
    return nodes.get(id);
  } };
  h.context.applied = [];
  h.context.EventSource = function () { throw new Error("Desktop must not open SSE"); };
  // Execute the actual connection/bootstrap functions without rendering the UI.
  const prefix = appSource.slice(0, appSource.indexOf("  function selectedItems()"));
  vm.runInContext(prefix + `
    function updateControls() {}
    function renderBatch() {}
    function renderPicker() {}
    function updateConstraints() {}
    function renderInventory() {}
    function validateConnectionPresets() {}
    function renderConnectionCatalog() {}
    function renderDiagnosticChoices() {}
    function applyRun(run) { state.run = run; applied.push(run); }
    window.connectionTest = { state, connectEvents, disconnectEvents, initialize };
  })();`, h.context);
  return { ...h, ...h.context.window.connectionTest, pendingStates, commands, applied: h.context.applied };
}
const run = (id) => ({ id, status: "completed", mode: "probe", config: {}, targets: [], series: [] });

test("polling is non-overlapping, uses valid snapshots and rejects old generations", async () => {
  const h = appHarness();
  h.connectEvents();
  assert.equal(h.pendingStates.length, 1);
  assert.equal([...h.timers.values()].filter((t) => t.delay === 1000).length, 0);
  h.connectEvents();
  h.pendingStates[0].resolve(run("old"));
  await flush();
  assert.equal(h.applied.length, 0);
  assert.equal(h.pendingStates.length, 2);
  h.pendingStates[1].resolve(run("fresh"));
  await flush();
  assert.equal(h.applied[0].id, "fresh");
  assert.equal(h.state.stateKnown, true);
  assert.equal(h.state.streamLive, true);
  const [timerId, tick] = [...h.timers.entries()].find(([, t]) => t.delay === 1000);
  h.timers.delete(timerId); tick.fn();
  h.pendingStates[2].resolve({ invalid: true });
  await flush();
  assert.equal(h.applied.length, 1);
  assert.equal(h.state.stateKnown, false);
  assert.equal(h.state.streamLive, false);
  h.disconnectEvents();
  assert.equal(h.timers.size, 0);
});

test("refresh invalidates old reads and never starts a task", async () => {
  const h = appHarness();
  h.connectEvents();
  const refreshing = h.initialize();
  h.pendingStates[0].resolve(run("obsolete"));
  await flush();
  h.pendingStates[1].resolve(null);
  await refreshing;
  assert.deepEqual(plain(h.applied), [null]);
  assert.equal(h.state.stateKnown, true);
  h.disconnectEvents();
  h.pendingStates[2].resolve(run("hidden"));
  await flush();
  assert.deepEqual(plain(h.applied), [null]);
  assert.ok(h.commands.includes("batch_state"));
  assert.ok(h.commands.every((command) => ["bootstrap", "state", "batch_state"].includes(command)));
});
