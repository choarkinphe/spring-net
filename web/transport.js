"use strict";

(() => {
  const isDesktop = !!window.__TAURI__?.core?.invoke;
  const MAX_BYTES = 1024 * 1024;
  const encoder = new TextEncoder();

  function checkSize(json) {
    if (encoder.encode(json).length > MAX_BYTES) throw new Error("JSON 数据不能超过 1 MiB。");
  }
  function checkLists(lists) {
    if (!Array.isArray(lists)) throw new Error("需要 lists 数组。");
    if (lists.length > 50) throw new Error("自定义列表总数最多 50 个。");
    let total = 0;
    for (const list of lists) {
      if (!Array.isArray(list?.targets)) throw new Error("列表需要 targets 数组。");
      total += list.targets.length;
      if (total > 1000) throw new Error("自定义站点总数最多 1000 个。");
    }
  }
  function parseBody(body) {
    if (typeof body !== "string") throw new Error("IPC 请求需要 JSON 字符串。");
    checkSize(body);
    return JSON.parse(body);
  }
  const statePending = new Map();
  const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  function validProxy(proxy) {
    if(proxy==null)return true;
    if(typeof proxy!=="object"||Object.keys(proxy).length!==1||typeof proxy.url!=="string")return false;
    return /^http:\/\/(?:\[[0-9a-f:]+\]|[^\s/:@?#]+):[0-9]+\/?$/i.test(proxy.url);
  }
  function diagnosticConfig(value) {
    const fields = ["request_id", "target", "port", "tls", "allow_lan"];
    if (!value || Object.keys(value).some(key=>!fields.includes(key)&&key!=="proxy") || !validProxy(value.proxy) || fields.some(field => !Object.hasOwn(value, field)) || !UUID.test(value.request_id) || typeof value.target !== "string" || !value.target.trim() || !Number.isInteger(value.port) || value.port < 1 || value.port > 65535 || typeof value.tls !== "boolean" || typeof value.allow_lan !== "boolean") throw new Error("分层诊断配置无效。");
    return value;
  }
  function batchRequest(value) {
    const fields = ["request_id", "list_ids", "target_ids", "concurrency", "rounds", "timeout_secs", "route"];
    if (!value || Object.keys(value).some(key=>!fields.includes(key)&&key!=="proxy") || !validProxy(value.proxy) || fields.some(field => !Object.hasOwn(value, field)) || !UUID.test(value.request_id) || !Array.isArray(value.list_ids) || !value.list_ids.length || !value.list_ids.every(id => typeof id === "string" && id) || !Array.isArray(value.target_ids) || !value.target_ids.length || !value.target_ids.every(id => typeof id === "string" && id) || !Number.isInteger(value.concurrency) || value.concurrency < 1 || value.concurrency > 4 || !Number.isInteger(value.rounds) || value.rounds < 1 || value.rounds > 3 || !Number.isInteger(value.timeout_secs) || value.timeout_secs < 1 || value.timeout_secs > 120 || !["direct","proxy"].includes(value.route) || (value.route === "proxy") !== (value.proxy != null)) throw new Error("批量配置无效。");
    return value;
  }
  async function invoke(command, args, writing = false, timeout = 15000) {
    if (!isDesktop) throw new Error("此操作仅适用于桌面版。");
    let timer;
    try {
      let expired = false;
      const pending = (async () => {
        if (command === "state" || command === "diagnostic_state" || command === "monitor_state" || command === "batch_state") {
          // Serialize each read command until its actual IPC settles, even after timeout.
          while (statePending.has(command)) await statePending.get(command).catch(() => {});
          if (expired) return null; // Do not dispatch a queued read after its deadline.
          const actual = Promise.resolve(window.__TAURI__.core.invoke(command, args));
          statePending.set(command, actual);
          const release = () => { if (statePending.get(command) === actual) statePending.delete(command); };
          actual.then(release, release);
          return actual;
        }
        return window.__TAURI__.core.invoke(command, args);
      })();
      if (!timeout) return await pending; // Native dialogs may remain open indefinitely.
      return await Promise.race([pending, new Promise((_, reject) => {
        timer = setTimeout(() => {
          expired = true;
          const error = new Error(writing
            ? "未收到写操作结果，后台可能已执行。请先重新连接核对状态，不要重复提交。"
            : "读取桌面后台超时，请重新连接。");
          error.uncertain = writing;
          reject(error);
        }, timeout);
      })]);
    } catch (cause) {
      const error = cause instanceof Error ? cause : new Error(typeof cause === "string" ? cause : "桌面后台通信失败。");
      // Rust business errors are strings. Bridge failures/timeouts can leave a
      // write's outcome unknown; never retry them or fall back to HTTP.
      if (writing && (cause?.uncertain || typeof cause !== "string")) {
        error.uncertain = true;
      }
      if (error.uncertain && !error.message.includes("不要重复提交")) {
        error.message += " 后台可能已执行，请重新连接核对状态，不要重复提交。";
      }
      throw error;
    } finally { clearTimeout(timer); }
  }
  async function request(path, options = {}) {
    if (!isDesktop) return null; // Browser HTTP/SSE remains in app.js.
    const method = options.method || "GET";
    if (method === "GET" && path === "/api/bootstrap") return invoke("bootstrap");
    if (method === "GET" && path === "/api/state") return invoke("state");
    if (method === "GET" && path === "/api/monitor/state") return invoke("monitor_state");
    if (method === "GET" && path === "/api/batch/state") return invoke("batch_state");
    if (method === "POST" && path === "/api/batch") return invoke("batch_start", { request: batchRequest(parseBody(options.body)) }, true);
    if (method === "POST" && path === "/api/batch/stop") {
      const data = parseBody(options.body);
      if (!data || Object.keys(data).length !== 1 || !UUID.test(data.id)) throw new Error("批量运行 ID 无效。");
      return invoke("batch_stop", { request: { request_id: data.id } }, true);
    }
    const monitorCommands = {"/api/monitor/config":"monitor_save", "/api/monitor/start":"monitor_start", "/api/monitor/heartbeat":"monitor_heartbeat", "/api/monitor/stop":"monitor_stop", "/api/monitor/pause":"monitor_pause"};
    if (method === "POST" && Object.hasOwn(monitorCommands, path)) return invoke(monitorCommands[path], { request: parseBody(options.body) }, true);
    if (method === "GET" && path === "/api/diagnostics/state") return invoke("diagnostic_state");
    if (method === "POST" && path === "/api/diagnostics") return invoke("start_diagnostic", { config: diagnosticConfig(parseBody(options.body)) }, true);
    if (method === "POST" && path === "/api/diagnostics/stop") {
      const data = parseBody(options.body);
      if (!data || Object.keys(data).length !== 1 || !UUID.test(data.id)) throw new Error("诊断 ID 无效。");
      return invoke("stop_diagnostic", { id: data.id }, true);
    }
    if (method === "POST" && path === "/api/runs") {
      return invoke("start_run", { config: parseBody(options.body) }, true);
    }
    if (method === "POST" && path === "/api/stop") return invoke("stop_run", undefined, true);
    if (method === "PUT" && path === "/api/lists") {
      const { lists } = parseBody(options.body);
      checkLists(lists);
      return invoke("save_lists", { lists }, true);
    }
    if (method === "POST" && path === "/api/workspace") {
      const { operation, payload = {} } = parseBody(options.body);
      const operations = ["authorization.prepare", "authorization.confirm", "authorization.reload", "selftest", "profiles.list", "profiles.save", "settings.get", "settings.save", "history.list", "history.get", "history.clear", "history.export"];
      if (!operations.includes(operation)) throw new Error("不支持的工作台操作。");
      const reading = ["profiles.list", "settings.get", "history.list", "history.get", "history.export"].includes(operation);
      return invoke("workspace", { operation, payload }, !reading);
    }
    throw new Error(`不支持的桌面请求：${method} ${path}`);
  }
  async function importLists() {
    const data = await invoke("import_lists_dialog", undefined, false, 0);
    if (data === null) return null;
    checkSize(JSON.stringify(data));
    checkLists(Array.isArray(data) ? data : data?.lists);
    return data;
  }
  async function exportDialog(kind, id) {
    if (!["json", "csv", "lists", "diagnostic", "batch"].includes(kind)) throw new Error("不支持的导出类型。");
    if (["diagnostic","batch"].includes(kind) && (typeof id !== "string" || !UUID.test(id))) throw new Error("诊断 ID 无效。");
    if (id !== undefined && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id)) throw new Error("运行 ID 无效。");
    const result = await invoke("export_dialog", id === undefined ? { kind } : { kind, id }, true, 0);
    if (typeof result !== "boolean") {
      const error = new Error("未收到有效导出结果，文件可能已写入，请先核对，不要重复提交。");
      error.uncertain = true;
      throw error;
    }
    return result === true;
  }

  window.AoNetTransport = Object.freeze({ isDesktop, request, importLists, exportDialog });
})();
