"use strict";

(() => {
  const $ = (id) => document.getElementById(id);
  const transport = window.AoNetTransport;
  const SVG_NS = "http://www.w3.org/2000/svg";
  const modeNames = { probe: "轻量探测", http: "HTTP 请求", tcp: "TCP 连接" };
  const statusNames = { running: "运行中", stopping: "停止中", completed: "已完成", stopped: "已停止", failed: "失败", interrupted: "已中断" };
  const modeInputs = {
    probe: { concurrency: "4", rps: "2", duration: "30", timeout: "10", ramp: "0", fresh: false },
    http: { concurrency: "10", rps: "10", duration: "30", timeout: "10", ramp: "10", fresh: false },
    tcp: { concurrency: "10", rps: "10", duration: "30", timeout: "10", ramp: "10", fresh: false }
  };
  const state = {
    monitor: null, monitorKnown: false, monitorBusy: false, monitorUncertain: false, monitorOwner: null, monitorTimer: null, monitorPending: null, monitorEpoch: 0,
    batch: null, batchKnown: false, batchBusy: false, batchUncertain: null, batchOwner: null, batchTimer: null, batchPending: null, batchGeneration: 0,
    diagnostic: null, diagnosticKnown: false, diagnosticBusy: false, diagnosticStopping: false, diagnosticUncertain: null,
    diagnosticGeneration: 0, diagnosticTimer: null, diagnosticPending: null, diagnosticLive: false, diagnosticPaused: false,
    diagnosticSource: null, diagnosticChoices: [],
    bootstrap: null, run: null, mode: "probe", selectedLists: new Set(), selectedTargets: new Set(), selectedAuthorized: new Set(),
    ready: false, stateKnown: false, streamLive: false, busy: false, saving: false, stopping: false,
    source: null, version: 0, refreshId: 0, stateRequest: 0, reconnectTimer: null,
    pollTimer: null, pollGeneration: 0, pollPending: null, nativeDialog: false,
    pendingRun: null, inventoryAction: null, editing: null, cooldownUntil: 0, cooldownRun: null,
    page: "monitor", profiles: [], settings: {version: 1, page: "console", dock_width: 300, sort: "stable", column_widths: {}}, authDraft: null, authEditing: null, history: [], historyDetail: null, workspaceAction: null, resultHighlight: null, resultRows: new Map(), resultRunId: null, workspaceLoaded: false,
    uncertainWrite: false, chartData: [], charts: [], chartIndex: null, restoreFocus: new WeakMap()
  };

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = String(text);
    return node;
  }
  function svgEl(tag, attrs = {}, text) {
    const node = document.createElementNS(SVG_NS, tag);
    for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, String(value));
    if (text !== undefined) node.textContent = String(text);
    return node;
  }
  function text(id, value) { $(id).textContent = value; }
  function isNumber(value) { return typeof value === "number" && Number.isFinite(value); }
  function count(value) { return isNumber(value) && value >= 0 ? value.toLocaleString("zh-CN", { maximumFractionDigits: 0 }) : "—"; }
  function decimal(value, digits = 1) { return isNumber(value) && value >= 0 ? value.toLocaleString("zh-CN", { maximumFractionDigits: digits }) : "—"; }
  function ms(value) { return isNumber(value) && value >= 0 ? `${decimal(value)} ms` : "—"; }
  function bytes(value) {
    if (!isNumber(value) || value < 0) return "—";
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    const index = value === 0 ? 0 : Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
    return `${decimal(value / 1024 ** index)} ${units[index]}`;
  }
  function date(value) {
    if (!value) return "未记录";
    const parsed = new Date(value);
    return Number.isNaN(parsed.getTime()) ? "时间不可用" : parsed.toLocaleString("zh-CN", { hour12: false });
  }
  function activeBatch() { return Boolean(state.batch && ["running", "stopping"].includes(state.batch.status)); }
  function activeRun() { return Boolean(state.run && ["running", "stopping"].includes(state.run.status)); }
  function activeDiagnostic() { return Boolean(state.diagnostic && ["running", "stopping", "draining"].includes(state.diagnostic.status)); }
  function diagnosticAvailable() { return state.bootstrap?.diagnostics?.enabled === true; }
  function diagnosticUnchecked() { return diagnosticAvailable() && !state.diagnosticKnown; }
  function locked() { return monitorLocked() || activeRun() || activeBatch() || state.batchKnown === false || state.batchBusy || Boolean(state.batchUncertain) || activeDiagnostic() || diagnosticUnchecked() || state.diagnosticBusy || Boolean(state.diagnosticUncertain) || state.busy || state.saving || state.nativeDialog || state.uncertainWrite; }
  function key(value) { return String(value); }
  function uid(prefix) {
    const suffix = globalThis.crypto?.randomUUID ? globalThis.crypto.randomUUID() : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;
    return `${prefix}-${suffix}`;
  }
  function notify(message) {
    text("banner-text", message);
    $("banner").hidden = false;
  }
  function clearError(id) { $(id).hidden = true; $(id).textContent = ""; }
  function showError(id, message) { text(id, message); $(id).hidden = false; }
  function readableError(error) { return error instanceof Error ? error.message : "操作失败，请稍后重试。"; }

  async function api(path, options = {}) {
    if (transport.isDesktop) return transport.request(path, options);
    const controller = new AbortController();
    const writing = Boolean(options.method && options.method !== "GET");
    const headers = { Accept: "application/json" };
    if (writing) {
      if (!state.bootstrap?.token) throw new Error("缺少后台令牌，请重新连接后再试。");
      headers["Content-Type"] = "application/json";
      headers["X-AoNet-Token"] = state.bootstrap.token;
    }
    const timer = setTimeout(() => controller.abort(), 15000);
    try {
      const response = await fetch(path, { ...options, headers, signal: controller.signal, credentials: "same-origin", cache: "no-store" });
      const raw = await response.text();
      let data = null;
      if (raw) { try { data = JSON.parse(raw); } catch { /* Report a non-JSON response without inserting HTML. */ } }
      if (!response.ok) {
        const detail = typeof data?.error === "string" ? data.error : typeof data?.message === "string" ? data.message : typeof data?.error?.message === "string" ? data.error.message : "";
        const fallback = { 401: "令牌已失效，请重新连接。", 403: "后台拒绝操作，请检查目标授权与权限。", 409: "后台状态冲突，可能已有测试运行或仍在冷却。", 429: "请求受限或公共探测仍在冷却，请稍后重试。" };
        throw new Error(`请求失败（${response.status}）：${detail || fallback[response.status] || "后台未能完成操作。"}`);
      }
      if (raw && data === null && raw.trim() !== "null") {
        const error = new Error("后台返回了无法解析的 JSON，请检查 API 响应。");
        error.uncertain = writing;
        throw error;
      }
      return data;
    } catch (error) {
      if (error.name === "AbortError" || error instanceof TypeError) {
        const wrapped = new Error(writing ? "未收到写操作结果，后台可能已执行。请先重新连接核对状态，不要重复提交。" : "无法连接 AoNet 后台，请确认 Rust 服务已运行并从同源页面访问。");
        wrapped.uncertain = writing;
        throw wrapped;
      }
      throw error;
    } finally { clearTimeout(timer); }
  }

  function validateBootstrap(data) {
    if (!data || typeof data.token !== "string" || !data.token || !Array.isArray(data.lists) || !Array.isArray(data.authorized)) {
      throw new Error("初始化数据不完整：需要 token、lists 和 authorized。");
    }
    const limits = data.limits;
    if (!limits || ![limits.public_concurrency, limits.public_rps].every((n) => isNumber(n) && n > 0) || !isNumber(limits.public_cooldown) || limits.public_cooldown < 0) {
      throw new Error("后台未提供有效公共限额；为避免越界，已禁用启动。");
    }
    const ids = new Set();
    for (const list of data.lists) {
      if (!list || list.id == null || typeof list.name !== "string" || !Array.isArray(list.targets) || ids.has(key(list.id))) throw new Error("后台站点列表格式错误或编号重复。");
      ids.add(key(list.id));
    }
    const authorizedIds = new Set();
    for (const target of data.authorized) {
      if (!target || target.id == null || typeof target.name !== "string" || typeof target.url !== "string" || !Array.isArray(target.modes) || authorizedIds.has(key(target.id))) throw new Error("后台授权目标格式错误或编号重复。");
      authorizedIds.add(key(target.id));
    }
    validateConnectionPresets(data.connection_presets);
    if (data.diagnostics !== undefined) validateDiagnosticCapability(data.diagnostics);
    return data;
  }
  function validRun(run) {
    return run === null || Boolean(run && typeof run === "object" && run.id != null && Object.hasOwn(statusNames, run.status) && Object.hasOwn(modeNames, run.mode) && run.config && Array.isArray(run.targets) && Array.isArray(run.series));
  }
  function applyBootstrap(data) {
    const previousAuthError=state.bootstrap?.authorization_error;
    state.bootstrap = validateBootstrap(data);
    const version = typeof data.version === "string" && /^\d+\.\d+\.\d+$/.test(data.version) ? `v${data.version}` : "版本未知";
    text("app-version", version);
    if(previousAuthError&&!data.authorization_error&&$("banner-text").textContent===`授权文件需要修复：${previousAuthError}`)$("banner").hidden=true;
    const availableLists = new Set(data.lists.map((list) => key(list.id)));
    const availableTargets = new Set(data.lists.flatMap((list) => list.targets.map((target) => `${list.id}/${target.id}`)));
    state.selectedTargets = new Set([...state.selectedTargets].filter((id) => availableTargets.has(id)));
    state.selectedLists = new Set([...state.selectedLists].filter((id) => availableLists.has(id)));
    const availableAuthorized = new Set(data.authorized.map((target) => key(target.id)));
    state.selectedAuthorized = new Set([...state.selectedAuthorized].filter((id) => availableAuthorized.has(id)));
    state.ready = true;
    text("public-limit-text", `并发 ≤ ${count(data.limits.public_concurrency)} · 速率 ≤ ${decimal(data.limits.public_rps)} 次/秒 · 冷却 ${count(data.limits.public_cooldown)} 秒`);
    renderPicker();
    updateConstraints(true);
    renderInventory();
    renderConnectionCatalog();
    renderDiagnosticChoices();
    if (typeof renderDiagnosticCapability === "function") renderDiagnosticCapability();
    if (typeof renderAuthorized === "function") renderAuthorized();
    updateControls();
  }
  function setConnection() {
    const node = $("connection");
    node.className = "connection";
    if (state.streamLive && state.ready && state.stateKnown) {
      node.classList.add("is-live");
      node.textContent = "● 实时快照已连接";
    } else if (state.ready || state.run) {
      node.classList.add("is-stale");
      node.textContent = "○ 实时已断开 · 读数非实时";
    } else {
      node.classList.add("is-stale");
      node.textContent = "○ 后台未连接";
    }
    text("hero-note", !state.run ? "等待真实采样" : state.streamLive ? "后台实时快照 · 非配置目标值" : "最近快照 · 当前非实时");
  }
  async function fetchState() {
    const request = ++state.stateRequest;
    const version = state.version;
    const run = await api("/api/state");
    if (!validRun(run)) throw new Error("后台状态格式不正确，未覆盖最近有效读数。");
    if (request !== state.stateRequest || version !== state.version) return;
    state.stateKnown = true;
    applyRun(run);
  }
  function disconnectEvents() {
    state.source?.close();
    state.source = null;
    state.pollGeneration += 1;
    state.stateRequest += 1;
    state.version += 1;
    clearTimeout(state.pollTimer);
    state.pollTimer = null;
    clearTimeout(state.reconnectTimer);
    state.reconnectTimer = null;
  }
  function connectEvents() {
    disconnectEvents();
    if (transport.isDesktop) {
      const generation = state.pollGeneration;
      const poll = async () => {
        // An in-flight invoke cannot be cancelled. Wait for it, but never apply
        // a response belonging to an earlier refresh/page lifecycle.
        if (state.pollPending) await state.pollPending.catch(() => {});
        if (generation !== state.pollGeneration) return;
        const version = state.version;
        const pending = api("/api/state");
        state.pollPending = pending;
        try {
          const run = await pending;
          if (generation !== state.pollGeneration || version !== state.version) return;
          if (!validRun(run)) throw new Error("后台状态格式不正确，未覆盖最近有效读数。");
          state.version += 1;
          state.stateKnown = true;
          state.streamLive = true;
          applyRun(run);
        } catch (error) {
          if (generation !== state.pollGeneration || version !== state.version) return;
          state.streamLive = false;
          state.stateKnown = false;
          notify(readableError(error));
          setConnection();
          updateControls();
        } finally {
          if (state.pollPending === pending) state.pollPending = null;
          if (generation === state.pollGeneration) state.pollTimer = setTimeout(poll, 1000);
        }
      };
      void poll();
      return;
    }
    if (!globalThis.EventSource) {
      notify("此浏览器不支持 SSE 实时快照，请使用现代浏览器。仍可通过重新连接读取状态。");
      return;
    }
    const source = new EventSource("/api/events");
    state.source = source;
    source.addEventListener("snapshot", (event) => {
      if (state.source !== source) return;
      try {
        const run = JSON.parse(event.data);
        if (!validRun(run)) throw new Error("快照数据格式不正确");
        state.version += 1;
        state.streamLive = true;
        state.stateKnown = true;
        clearTimeout(state.reconnectTimer);
        state.reconnectTimer = null;
        applyRun(run);
        setConnection();
      } catch {
        state.streamLive = false;
        setConnection();
        notify("收到无法识别的实时快照，已保留最近有效读数。正在重新查询状态。");
        scheduleStateSync();
      }
    });
    source.addEventListener("open", () => {
      if (state.source !== source) return;
      // An open socket is not yet a valid snapshot; fetch state before trusting it.
      fetchState().catch((error) => notify(readableError(error)));
    });
    source.addEventListener("error", () => {
      if (state.source !== source) return;
      state.streamLive = false;
      state.stateKnown = false;
      setConnection();
      updateControls();
      if (activeRun()) notify("实时连接中断，当前为最近快照；后台测试可能仍在运行。正在重新查询状态，SSE 将自动重连。");
      scheduleStateSync();
    });
  }
  function scheduleStateSync() {
    if (state.reconnectTimer) return;
    state.reconnectTimer = setTimeout(async () => {
      state.reconnectTimer = null;
      try { await fetchState(); } catch (error) { notify(readableError(error)); }
      setConnection();
      updateControls();
    }, 800);
  }
  async function initialize() {
    const id = ++state.refreshId;
    state.diagnosticPaused = false;
    stopBatchPolling();
    state.batchKnown = false;
    if (typeof stopDiagnosticPolling === "function") stopDiagnosticPolling();
    state.diagnosticKnown = false;
    state.ready = false;
    state.stateKnown = false;
    state.streamLive = false;
    disconnectEvents();
    $("refresh").disabled = true;
    updateControls();
    text("connection", "○ 正在连接后台");
    try {
      const [bootstrap, run] = await Promise.all([api("/api/bootstrap"), api("/api/state")]);
      if (id !== state.refreshId) return;
      if (!validRun(run)) throw new Error("后台状态格式不正确。");
      applyBootstrap(bootstrap);
      state.version += 1;
      state.stateKnown = true;
      state.uncertainWrite = false;
      applyRun(run);
      $("banner").hidden = true;
      await fetchBatchState(state.batchGeneration);
      if (id !== state.refreshId) return;
      startBatchPolling();
      if (typeof fetchMonitorState === "function") { startMonitorPolling(); void fetchMonitorState().catch(() => {}); }
      if (diagnosticAvailable()) await fetchDiagnosticState(state.diagnosticGeneration,true);
      if (id !== state.refreshId) return;
      if (typeof startDiagnosticPolling === "function") startDiagnosticPolling();
      connectEvents();
      if (typeof loadWorkspace === "function") await loadWorkspace();
      if(state.bootstrap?.authorization_error)notify(`授权文件需要修复：${state.bootstrap.authorization_error}`);
      if(state.page==="history"&&typeof loadHistory==="function")void loadHistory();
    } catch (error) {
      if (id !== state.refreshId) return;
      state.ready = false;
      notify(readableError(error));
      if (!state.bootstrap) {
        $("target-picker").replaceChildren(el("p", "empty-inline", "尚未连接后台。连接后将显示真实列表与授权目标。"));
        $("inventory").replaceChildren(el("p", "empty-inline", "后台尚未连接，暂无可读取的站点列表。"));
        $("connection-catalog").replaceChildren(el("p", "empty-inline", "后台尚未连接，暂无可读取的连接资料。"));
        text("connection-catalog-count", "等待后台连接");
        text("inventory-note", "等待后台连接");
        text("public-limit-text", "后台限额尚不可用 · 已禁止启动");
      }
    } finally {
      if (id === state.refreshId) {
        $("refresh").disabled = false;
        setConnection();
        updateControls();
      }
    }
  }

  const diagnosticStatusNames = {running:"诊断中",stopping:"停止中",draining:"等待系统解析收尾",completed:"执行完成 · 不等于业务成功",cancelled:"已取消",failed:"执行失败"};
  const diagnosticStageNames = {pending:"待执行",running:"执行中",success:"成功",error:"错误",timeout:"超时",blocked:"策略阻止",skipped:"未执行 / 不适用",unsupported:"不支持",cancelled:"已取消"};
  const diagnosticUUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  const record = value => value !== null && typeof value === "object" && !Array.isArray(value);
  const stringList = value => Array.isArray(value) && value.every(item => typeof item === "string");
  function validDiagnosticConfig(config) {
    return record(config) && typeof config.request_id === "string" && diagnosticUUID.test(config.request_id) && typeof config.target === "string" && config.target.length > 0 && Number.isInteger(config.port) && config.port > 0 && config.port <= 65535 && typeof config.tls === "boolean" && typeof config.allow_lan === "boolean";
  }
  function validDiagnostic(snapshot) {
    if (snapshot === null) return true;
    const timestamp = value => typeof value === "string" && !Number.isNaN(Date.parse(value));
    if (!record(snapshot) || !diagnosticUUID.test(snapshot.id) || !diagnosticUUID.test(snapshot.request_id) || !Object.hasOwn(diagnosticStatusNames,snapshot.status) || !validDiagnosticConfig(snapshot.config) || snapshot.request_id !== snapshot.config.request_id || !record(snapshot.stages) || !stringList(snapshot.warnings) || !stringList(snapshot.limitations) || !(snapshot.selected_ip === null || typeof snapshot.selected_ip === "string") || !timestamp(snapshot.started_at) || !timestamp(snapshot.updated_at) || !(snapshot.finished_at === null || timestamp(snapshot.finished_at))) return false;
    if (["running","stopping","draining"].includes(snapshot.status) ? snapshot.finished_at !== null : snapshot.finished_at === null) return false;
    for (const name of ["dns","route","tcp","tls"]) {
      const stage = snapshot.stages[name];
      if (!record(stage) || !Object.hasOwn(diagnosticStageNames,stage.status) || !(stage.elapsed_ms === null || Number.isSafeInteger(stage.elapsed_ms) && stage.elapsed_ms >= 0) || !(stage.error === null || typeof stage.error === "string") || !record(stage.observations)) return false;
      const obs = stage.observations;
      for (const field of ["method","raw_stdout","raw_stderr","outcome","remote_addr","trust_source","server_name"]) if (Object.hasOwn(obs,field) && typeof obs[field] !== "string") return false;
      for (const field of ["protocol","cipher"]) if (Object.hasOwn(obs,field) && !(obs[field] === null || typeof obs[field] === "string")) return false;
      for (const field of ["addresses","untested_addresses"]) if (Object.hasOwn(obs,field) && !stringList(obs[field])) return false;
      if (Object.hasOwn(obs,"selected_ip") && !(obs.selected_ip === null || typeof obs.selected_ip === "string")) return false;
      if (name === "route") {
        if (Object.hasOwn(obs,"outcome") && !["reached","incomplete","truncated","timeout","permission_denied","unsupported","process_error","cancelled"].includes(obs.outcome)) return false;
        for (const field of ["parse_incomplete","reached","truncated"]) if (Object.hasOwn(obs,field) && typeof obs[field] !== "boolean") return false;
        if (Object.hasOwn(obs,"exit_code") && !(obs.exit_code === null || Number.isInteger(obs.exit_code))) return false;
        if (Object.hasOwn(obs,"hops") && (!Array.isArray(obs.hops) || obs.hops.length > 256 || obs.hops.some(hop => !record(hop) || !Number.isInteger(hop.ttl) || hop.ttl < 1 || !stringList(hop.addresses) || !Array.isArray(hop.rtt_ms) || hop.rtt_ms.some(rtt => !isNumber(rtt) || rtt < 0) || !Number.isInteger(hop.stars) || hop.stars < 0 || typeof hop.raw !== "string"))) return false;
      }
    }
    return true;
  }
  function validateDiagnosticCapability(value) {
    if (value === undefined) return; // Old servers do not support diagnostics.
    if (!record(value) || typeof value.enabled !== "boolean") throw new Error("诊断能力格式错误。");
    if (!value.enabled) return;
    const fields = ["dns_timeout_secs","route_timeout_secs","tcp_timeout_secs","tls_timeout_secs","max_hops","max_output_bytes","start_interval_secs","host_interval_secs"];
    if (!record(value.limits) || fields.some(field => !Number.isSafeInteger(value.limits[field]) || value.limits[field] <= 0) || typeof value.trust_source !== "string" || !value.trust_source || !Array.isArray(value.self_test_targets) || value.self_test_targets.some(target => !record(target) || typeof target.name !== "string" || !["127.0.0.1","::1"].includes(target.target) || !Number.isInteger(target.port) || target.port < 1 || target.port > 65535 || target.tls !== false)) throw new Error("诊断限额或自检目标格式错误。");
  }
  function normalizeProxy(value) {
    if (!value || typeof value.url !== "string" || Object.keys(value).some(key=>key!=="url")) throw new Error("代理配置无效，仅支持 url。");
    const raw=value.url.trim();
    if(!/^(?:http|socks5):\/\/(?:\[[0-9a-f:]+\]|[^\s/:@?#]+):[0-9]+\/?$/i.test(raw))throw new Error("代理需为 http:// 或 socks5://主机:端口，不得含账号、密码或路径。");
    const url=new URL(raw);const match=raw.match(/:(\d+)\/?$/);const port=Number(match[1]);
    if(port<1||port>65535||url.username||url.password||url.search||url.hash)throw new Error("代理端口或地址无效。");
    return {url:`${url.protocol}//${url.hostname.toLowerCase()}:${port}`};
  }
  function selectedProxy(prefix) {
    if($(prefix+"-route")?.value!=="proxy")return null;
    if(state.bootstrap?.proxy?.enabled!==true)throw new Error("后台尚未支持代理；不会回退直连。");
    return normalizeProxy({url:$(prefix+"-proxy").value});
  }
  function routeLabel(proxy) { return proxy ? `${proxy.url.startsWith("socks5:")?"SOCKS5":"HTTP"} 代理 ${proxy.url} · 数字地址 CONNECT · 本机 DNS` : "直连"; }
  function routeError(prefix) {try{selectedProxy(prefix);return "";}catch(error){return readableError(error);}}
  function updateRouteControls() {
    for(const prefix of ["monitor","run","batch","diagnostic"]){
      const select=$(prefix+"-route"),input=$(prefix+"-proxy"),label=$(prefix+"-proxy-label"),note=$(prefix+"-route-note");if(!select||!input||!label||!note)continue;
      label.hidden=select.value!=="proxy";select.disabled=input.disabled=locked()||!state.ready;
      note.textContent=select.value==="proxy" ? routeError(prefix)||"仅通过所选代理；代理失败不会直连。本机 DNS，代理后的路径未知；HTTP 使用独立隧道，不复用连接池。" : "直连目标，不使用环境或系统代理。";
    }
  }
  function setRouteDraft(prefix, proxy) {
    const normalized=proxy==null?null:normalizeProxy(proxy);
    if(normalized&&state.bootstrap?.proxy?.enabled!==true)throw new Error("此方案需要代理，但后台未提供支持，未修改草稿。");
    if($(prefix+"-route"))$(prefix+"-route").value=normalized?"proxy":"direct";
    if($(prefix+"-proxy"))$(prefix+"-proxy").value=normalized?.url||state.settings.http_proxy?.url||"";
  }
  function diagnosticDraft() {
    const draft={target:$("diagnostic-target").value.trim(),port:Number($("diagnostic-port").value),tls:$("diagnostic-tls").value === "true",allow_lan:$("diagnostic-lan").checked};
    try {const proxy=selectedProxy("diagnostic");if(proxy)draft.proxy=proxy;}catch{}
    return draft;
  }
  function diagnosticDraftError(draft = diagnosticDraft()) {
    if(routeError("diagnostic"))return routeError("diagnostic");
    if (!draft.target || draft.target.length > 4096 || /[\s\x00-\x1f\x7f*?#@%]/.test(draft.target) || draft.target.startsWith("-")) return "请填写一个精确主机 / IP 或无凭据、查询、片段的 HTTP(S) 地址。";
    if (/^https?:\/\//i.test(draft.target)) {
      try { const url = new URL(draft.target); if (!url.hostname || url.username || url.password || url.search || url.hash) throw new Error(); }
      catch { return "HTTP(S) 地址无效。"; }
    } else if (draft.target.includes("/") || draft.target.includes(":") && !/^(?:\[[0-9a-f:]+\]|[0-9a-f:]+)$/i.test(draft.target)) return "主机与端口须分开填写，不接受网段、范围或其他协议。";
    if (!Number.isInteger(draft.port) || draft.port < 1 || draft.port > 65535) return "请明确填写一个 1～65535 的整数端口。";
    if (!["true","false"].includes($("diagnostic-tls").value)) return "请明确选择 TLS 意图，不根据端口推断。";
    return "";
  }
  function diagnosticEndpointDraft(endpoint) {
    if (!endpoint || !["url","host"].includes(endpoint.kind) || typeof endpoint.address !== "string" || !endpoint.address || /[\s*\x00-\x1f\x7f?#@%]/.test(endpoint.address)) return null;
    if (endpoint.kind === "url") {
      try {
        const url = new URL(endpoint.address);
        if (!["http:","https:"].includes(url.protocol) || !url.hostname || url.username || url.password || url.search || url.hash) return null;
        return {target:endpoint.address,port:endpoint.port ?? (url.port ? Number(url.port) : url.protocol === "https:" ? 443 : 80),tls:""};
      } catch { return null; }
    }
    if (!Number.isInteger(endpoint.port) || endpoint.port < 1 || endpoint.port > 65535 || endpoint.address.includes("/") || endpoint.address.startsWith("-")) return null;
    return {target:endpoint.address,port:endpoint.port,tls:""};
  }
  function buildDiagnosticChoices(bootstrap, query = "") {
    const options = [];
    for (const list of bootstrap?.lists || []) {
      for (const target of list.targets || []) {
        if (target.probe_only === false) continue;
        const draft = diagnosticEndpointDraft({kind:"url", address:target.url});
        const aliases = {"air-china":"国航 CA", "china-eastern":"东航 MU", "china-southern":"南航 CZ"}[target.id] || "";
        const type = list.builtin ? "官网入口" : "自定义网站";
        if (draft) options.push({group:`网站入口 · ${list.name}`, label:`${target.name} · ${type}`, detail:target.url, aliases, draft});
      }
    }
    for (const card of bootstrap?.connection_presets || []) {
      let available = 0;
      for (const endpoint of card.endpoints || []) {
        const draft = diagnosticEndpointDraft(endpoint);
        if (!draft) continue;
        available++;
        options.push({group:`业务连接资料 · ${card.provider} / ${card.product}`, label:`${endpoint.purpose} · ${card.environment || "环境见资料"}`, detail:endpoint.address, aliases:(card.aliases || []).join(" "), draft});
      }
      if (!available) options.push({group:`业务连接资料 · ${card.provider} / ${card.product}`, label:`${card.product} · 地址待提供`, detail:"请向服务商确认后手动输入", aliases:(card.aliases || []).join(" "), draft:null});
    }
    const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
    return options.filter(option => terms.every(term => `${option.group} ${option.label} ${option.detail} ${option.aliases || ""}`.toLocaleLowerCase().includes(term)));
  }
  function renderDiagnosticChoices() {
    const select = $("diagnostic-options");
    if (!select?.replaceChildren) return;
    const options = buildDiagnosticChoices(state.bootstrap, $("diagnostic-search")?.value || "");
    state.diagnosticChoices = options;
    select.replaceChildren();
    const placeholder = el("option", "", options.length ? "请选择，或在下方手动输入" : "没有匹配项，请手动输入");
    placeholder.value = ""; select.append(placeholder);
    let groupName = "", group;
    options.forEach((choice, index) => {
      if (choice.group !== groupName) { groupName = choice.group; group = el("optgroup"); group.label = groupName; select.append(group); }
      const option = el("option", "", `${choice.label} — ${choice.detail}`);
      option.value = String(index); option.disabled = !choice.draft; group.append(option);
    });
    const unavailable = options.filter(option => !option.draft);
    const message = $("diagnostic-unavailable");
    if (message) {
      message.hidden = unavailable.length === 0 || !($("diagnostic-search")?.value || "").trim();
      message.textContent = unavailable.length ? `以下产品尚缺少已确认的连接地址，请向供应商获取后手动输入：${unavailable.map(option => option.label.replace(" · 地址待提供", "")).join("、")}。` : "";
    }
    select.disabled = !state.ready || locked();
  }
  function fillDiagnosticDraft(draft, source = null) {
    if (locked()) { notify("活动尚未结束或状态尚未核对，暂不能替换诊断草稿。"); return; }
    $("diagnostic-target").value = draft.target;
    $("diagnostic-port").value = String(draft.port);
    $("diagnostic-tls").value = draft.tls;
    $("diagnostic-lan").checked = false;
    state.diagnosticSource = source;
    if ($("diagnostic-source")) text("diagnostic-source", source ? `已选择：${source}` : "手动输入或从连接资料填入，请核对地址和用途");
    closeDialog("connection-dialog");
    location.hash = "diagnostics";
    updateDiagnosticControls();
    $("diagnostic-target").focus();
    notify("仅填入诊断草稿，未发送目标流量；请核对端口、TLS 与范围后另行明确开始。");
  }
  function renderDiagnosticCapability() {
    if (!$("diagnostic-form")?.querySelectorAll) return;
    const capability = state.bootstrap?.diagnostics;
    text("diagnostic-capability",diagnosticAvailable() ? "后台支持分层诊断 · 默认不运行" : "当前后台不支持分层诊断；已有测试功能不受影响。");
    if (diagnosticAvailable()) {
      const limits = capability.limits;
      text("diagnostic-budget",`DNS ${limits.dns_timeout_secs}s / 路由 ${limits.route_timeout_secs}s / TCP ${limits.tcp_timeout_secs}s / TLS ${limits.tls_timeout_secs}s；最多 ${limits.max_hops} 跳，输出 ≤ ${bytes(limits.max_output_bytes)}。全局间隔 ${limits.start_interval_secs}s / 同主机 ${limits.host_interval_secs}s；系统 DNS 收尾可能超出预算。`);
      text("diagnostic-trust",`TLS 信任：${capability.trust_source}。不关闭证书或身份验证。`);
      const select = $("diagnostic-selftest");
      const signature = JSON.stringify(capability.self_test_targets);
      if (select.dataset.signature !== signature) {
        select.dataset.signature = signature;
        select.replaceChildren(el("option","","选择后仅填入草稿")); select.children[0].value = "";
        capability.self_test_targets.forEach((target,index) => { const option=el("option","",`${target.name} · ${target.target}:${target.port} · 无 TLS`); option.value=String(index); select.append(option); });
      }
      $("diagnostic-selftest-label").hidden = capability.self_test_targets.length === 0;
    } else { $("diagnostic-selftest-label").hidden=true; text("diagnostic-budget","诊断限额尚不可用，禁止启动。"); text("diagnostic-trust","诊断信任来源尚不可用。"); }
    renderDiagnostic();
  }
  function updateDiagnosticControls() {
    if (!$("diagnostic-form")?.querySelectorAll) return;
    const disabled = !state.ready || !state.stateKnown || !diagnosticAvailable() || !state.diagnosticKnown || locked();
    for (const id of ["diagnostic-target","diagnostic-port","diagnostic-tls","diagnostic-lan","diagnostic-selftest","diagnostic-suggest-tls","diagnostic-search","diagnostic-options"]) $(id).disabled=disabled;
    const error = diagnosticDraftError();
    $("diagnostic-start").disabled=disabled || Boolean(error);
    text("diagnostic-start",state.diagnosticBusy ? "正在提交…" : state.diagnosticUncertain ? "请重新连接核对" : activeDiagnostic() ? "诊断执行 / 收尾中" : "开始排查");
    const https = /^https:\/\//i.test($("diagnostic-target").value.trim());
    $("diagnostic-suggest-tls").hidden = !https || $("diagnostic-tls").value === "true";
    text("diagnostic-tls-hint",https ? "HTTPS 地址建议启用 TLS；仍需明确选择，端口不会自动改变。" : "端口 443 不代表已确认使用 TLS。");
    const draft=diagnosticDraft();
    const blockedReason = !state.ready || !state.stateKnown ? "后台尚未连接，请先重新连接。" : !diagnosticAvailable() ? "当前后台不支持分层诊断。" : state.diagnosticUncertain || !state.diagnosticKnown ? "诊断状态尚未核对，请重新连接；不要重复提交。" : activeRun() ? "其他测试正在运行，请等待结束。" : activeDiagnostic() ? "当前诊断仍在执行或收尾，请等待结束。" : state.busy || state.saving || state.nativeDialog || state.uncertainWrite ? "其他操作尚未结束，请稍候。" : "";
    text("diagnostic-draft",blockedReason || error || `待排查：${draft.target} · 端口 ${draft.port} · ${draft.tls ? "验证 TLS" : "仅 TCP"} · ${draft.allow_lan ? "允许局域网" : "仅公网"}。点击后才开始。`);
    $("diagnostic-stop").disabled=!activeDiagnostic() || !state.diagnosticKnown || state.diagnosticStopping || state.diagnosticBusy || state.diagnostic?.status !== "running";
    text("diagnostic-stop",state.diagnosticStopping || ["stopping","draining"].includes(state.diagnostic?.status) ? "停止 / 收尾中…" : "停止诊断");
    const exportEnabled=Boolean(state.diagnostic) && !state.nativeDialog;
    const link=$("diagnostic-export");link.setAttribute("aria-disabled",String(!exportEnabled));link.tabIndex=exportEnabled?0:-1;
    if (state.diagnostic) {link.href=`/api/diagnostics/report?id=${encodeURIComponent(state.diagnostic.id)}`;link.download=`aonet-diagnostic-${state.diagnostic.id}.json`;}
  }
  function diagnosticNextStep(name,stage) {
    if (stage.status === "pending" || stage.status === "running") return "等待真实观测，不提前归因。";
    if (stage.status === "cancelled") return "本阶段已取消，不能据此判断目标可用性。";
    const advice = {dns:"核对目标拼写、hosts / 系统 DNS 与本机网络；其他解析地址未自动测试。",route:"必要时与网络管理员交叉核对；星号、最后响应跳点均不构成故障边界。",tcp:"结合实际服务监听、ACL / 防火墙与固定 IP 核验；TCP 可达不代表应用成功。",tls:"核对原主机身份、证书链和信任来源；企业 CA 与随包根可能不同，不以关闭校验绕过。"};
    return advice[name];
  }
  function renderDiagnosticRoute(holder,obs) {
    if (Array.isArray(obs.hops)) {
      const scroll=el("div","table-scroll");scroll.tabIndex=0;scroll.setAttribute("role","region");scroll.setAttribute("aria-label","路由跳点证据");
      const table=el("table"),head=el("thead"),row=el("tr");for (const label of ["TTL","数值响应地址","RTT / ms","星号"]) {const th=el("th","",label);th.scope="col";row.append(th);}head.append(row);table.append(head);
      const body=el("tbody");for(const hop of obs.hops){const tr=el("tr");for(const value of [hop.ttl,hop.addresses.join(" / ") || "未收到地址",hop.rtt_ms.map(n=>decimal(n,3)).join(" / ") || "—",hop.stars])tr.append(el("td","",value));body.append(tr);}table.append(body);scroll.append(table);holder.append(scroll);
    }
    if(obs.subject) holder.append(el("p","field-help",`追踪对象：${obs.subject}。代理之后的目标路径不可观测。`));
    const outcomes={reached:"已观察到追踪终点响应",incomplete:"未完整到达 / 不能确定边界",truncated:"输出被截断",timeout:"路由阶段超时",permission_denied:"系统权限不足",unsupported:"本平台不支持",process_error:"系统工具执行错误",cancelled:"路由追踪已取消"};
    if (typeof obs.outcome === "string") holder.append(el("p","field-help",`路由结论：${outcomes[obs.outcome] || obs.outcome}；${obs.parse_incomplete === true ? "解析不完整，以原文为准" : "跳点仅作路径参考"}。`));
    if (typeof obs.raw_stdout === "string" || typeof obs.raw_stderr === "string") {const details=el("details"),pre=el("pre","diagnostic-raw",`stdout\n${obs.raw_stdout ?? "未提供"}\n\nstderr\n${obs.raw_stderr ?? "未提供"}`);details.append(el("summary","","系统路由原始输出"),pre);holder.append(details);}
  }
  function diagnosticConclusion(snapshot, known, live, uncertain) {
    if (uncertain) return {label:"状态待核对", title:"上一次操作的结果尚不确定", detail:"请重新连接并核对当前诊断，不要重复开始。", next:"重新连接后按请求编号确认。", tone:"warning"};
    if (!snapshot && !known) return {label:"等待连接", title:"正在连接诊断后台", detail:"尚未发起排查，也没有可用的结果。", next:"连接完成后选择目标；如果连接失败，请点击重新连接。", tone:"idle"};
    if (!known || !live) return {label:"状态待核对", title:"当前状态尚未确认", detail:snapshot ? "以下是最近保存的快照，可能不是实时结果。" : "尚未读取到后台状态。", next:"重新连接后台后再判断。", tone:"warning"};
    if (!snapshot) return {label:"等待开始", title:"选择目标后开始排查", detail:"工具会依次检查名称解析、路由、TCP 和所选的 TLS。", next:"先选择一个系统，或输入地址。", tone:"idle"};
    if (["running", "stopping", "draining"].includes(snapshot.status)) {
      const running = Object.entries(snapshot.stages).find(([, stage]) => stage.status === "running");
      const names = {dns:"系统 DNS",route:"路由追踪",tcp:"TCP 连接",tls:"TLS 验证"};
      const title = snapshot.status === "running" ? `正在排查${running ? `：${names[running[0]]}` : ""}` : snapshot.status === "draining" ? "正在等待系统解析收尾" : "正在停止并释放资源";
      return {label:"进行中", title, detail:"已得到的阶段证据会继续显示，当前不能据此作最终判断。", next:"请等待结束；停止请求不等于清理已经完成。", tone:"running"};
    }
    if (snapshot.status === "cancelled") return {label:"已取消", title:"排查已取消", detail:"未执行的阶段无法判断，已完成的证据仍可查看。", next:"需要完整结果时，请检查目标后重新开始。", tone:"warning"};
    const {dns, route, tcp, tls} = snapshot.stages;
    if (snapshot.config.proxy && !["blocked","error","timeout"].includes(dns.status)) {
      if (["error","timeout","blocked"].includes(tcp.status)) return {label:"代理通路未建立",title:"代理连接或 CONNECT 隧道未成功",detail:`请求路径：${snapshot.config.proxy.url}。尚不能判断目标 TCP 是否可达，未回退直连。`,next:"查看代理 TCP 与 CONNECT 证据，核对代理监听、认证要求和端口放行策略。",tone:"warning"};
      if (tcp.status === "success") {
        const verified=tls.status === "success";
        return {label:verified?"代理通路验证完成":"代理隧道已建立",title:verified?"CONNECT 成功 · 目标 TLS 验证通过":snapshot.config.tls?"CONNECT 成功 · TLS 未通过或未完成":"CONNECT 成功 · TLS 未测试",detail:"客户端观测到代理接受隧道请求；无法独立确认代理实际连接的远端 IP。代理之后的路由不可观测，业务登录与交易未测试。",next:verified?"如业务仍异常，请核对业务配置和服务商状态。":"展开证据核对目标协议和证书信任；不要关闭 TLS 验证。",tone:verified||!snapshot.config.tls?"success":"warning"};
      }
    }
    if (dns.status === "blocked") return {label:"安全策略阻止", title:"目标地址未通过安全范围检查", detail:"本次没有继续连接；策略阻止不等于目标离线。", next:"核对目标地址、解析结果与是否允许局域网。", tone:"warning"};
    if (["error","timeout"].includes(dns.status)) return {label:"名称解析未完成", title:"未获得可用于后续检查的地址", detail:dns.status === "timeout" ? "系统名称解析超时。" : "系统名称解析返回错误。", next:"核对地址拼写、系统 DNS 和 hosts 配置。", tone:"warning"};
    if (["error","timeout","blocked"].includes(tcp.status)) return {label:"TCP 连接未成功", title:"未能建立本次 TCP 连接", detail:`固定目标 ${snapshot.selected_ip || "未选定"}:${snapshot.config.port} 的单次连接${tcp.status === "timeout" ? "超时" : "未成功"}；原因尚未确定。`, next:"核对服务监听、地址、端口及网络访问策略。", tone:"warning"};
    if (tcp.status === "success" && ["error","timeout","blocked"].includes(tls.status)) return {label:"TLS 验证未成功", title:"TCP 已连接，TLS 未通过", detail:"基础连接已建立，但安全握手或身份验证未成功。", next:"核对服务协议、主机名、证书链和信任来源；不要关闭证书校验。", tone:"warning"};
    if (tcp.status === "success") {
      const tlsText = tls.status === "success" ? "TLS 身份验证通过" : ["skipped", "unsupported"].includes(tls.status) ? "TLS 未测试" : "TLS 状态未确认";
      const routeNote = route.status === "skipped" || route.status === "unsupported" ? "路由未测试，请另看详细证据。" : route.observations?.outcome !== "reached" ? "路由未完整确认，请另看详细证据。" : "";
      const local = ["127.0.0.1", "::1"].includes(snapshot.selected_ip);
      return {label:"连接检查完成", title:`TCP 已连接 · ${tlsText}`, detail:local ? "本次仅验证本机自检服务，不经过办公网络出口，也不代表业务系统可用。" : `${routeNote}本次仅检查固定 IP 的连接，不代表登录、查询或出票成功。`, next:local ? "选择实际业务目标后，才能观察当前网络出口的表现。" : "如业务仍异常，请核对账号、业务配置和服务商状态。", tone:"success"};
    }
    return {label:"证据不足", title:"尚不能判断连接是否正常", detail:"部分阶段未执行、被跳过或没有足够观测。", next:"展开详细证据，确认未执行原因后再决定如何复测。", tone:"warning"};
  }
  function renderDiagnostic() {
    if (!$("diagnostic-form")?.querySelectorAll) return;
    const snapshot=state.diagnostic;
    const conclusion = diagnosticConclusion(snapshot, state.diagnosticKnown, state.diagnosticLive, state.diagnosticUncertain);
    const conclusionNode = $("diagnostic-conclusion");
    if (conclusionNode) {
      const signature = JSON.stringify(conclusion);
      if (conclusionNode.dataset.signature !== signature) {
        conclusionNode.dataset.signature = signature;
        conclusionNode.dataset.tone = conclusion.tone;
        conclusionNode.replaceChildren(el("p","conclusion-label",conclusion.label), el("h3","",conclusion.title), el("p","conclusion-detail",conclusion.detail), el("p","conclusion-next",`下一步：${conclusion.next}`));
      }
    }
    text("diagnostic-status",snapshot ? diagnosticStatusNames[snapshot.status] : "尚未诊断");
    text("diagnostic-freshness",state.diagnosticUncertain ? "启动 / 停止结果不确定，禁止重发；请重新连接核对。" : !state.diagnosticKnown || !state.diagnosticLive ? "最近快照 · 尚未获得实时诊断状态" : `独立状态已核对${snapshot ? ` · ${date(snapshot.updated_at)}` : " · 无当前诊断"}`);
    text("diagnostic-meta",snapshot ? `#${snapshot.id} · 后台目标 ${snapshot.config.target}:${snapshot.config.port} · ${snapshot.config.tls ? "TLS 开" : "TLS 关"} · ${snapshot.config.allow_lan ? "允许 LAN" : "仅公网"} · ${routeLabel(snapshot.config.proxy)} · 固定 IP ${snapshot.selected_ip ?? "未选定"} · 开始 ${date(snapshot.started_at)}${snapshot.finished_at ? ` · 结束 ${date(snapshot.finished_at)}` : ""}` : "等待明确开始；未产生目标流量。");
    const holder=$("diagnostic-stages");
    const signature=JSON.stringify(snapshot?.stages ?? null);
    if(holder.dataset.signature!==signature){
      holder.dataset.signature=signature;holder.replaceChildren();
      for(const [index,name,title] of [["01","dns","系统 DNS"],["02","route","路由追踪"],["03","tcp","TCP 建连"],["04","tls","TLS 握手 / 身份验证"]]){
        const stage=snapshot?.stages[name],article=el("article","diagnostic-stage");article.dataset.status=stage?.status || "pending";
        const heading=el("div","diagnostic-stage-heading");heading.append(el("h3","",`${index} / ${name==='tcp'&&snapshot?.config.proxy?'代理 TCP / CONNECT':name==='route'&&snapshot?.config.proxy?'到代理的路由':title}`),el("span","status-pill",stage ? `${diagnosticStageNames[stage.status]} · ${ms(stage.elapsed_ms)}` : "尚无观测"));article.append(heading);
        if(stage){if(stage.error)article.append(el("p","inline-error",stage.error));if(typeof stage.observations.method==="string")article.append(el("p","field-help",`真实方法：${stage.observations.method}`));if(name==="route")renderDiagnosticRoute(article,stage.observations);const details=el("details");details.append(el("summary","","查看本层全部原始证据"),el("pre","diagnostic-raw",JSON.stringify(stage.observations,null,2)));article.append(details,el("p","field-help",`下一步：${diagnosticNextStep(name,stage)}`));}
        else article.append(el("p","field-help","等待后台返回真实证据；不填零，不推导成功。"));holder.append(article);
      }
    }
    $("diagnostic-notes").replaceChildren(...(snapshot ? [...snapshot.warnings.map(item=>el("p","diagnostic-warning",`提示：${item}`)),...snapshot.limitations.map(item=>el("p","field-help",`限制：${item}`))] : []));
    updateDiagnosticControls();
  }
  function stopDiagnosticPolling() {
    state.diagnosticGeneration+=1;clearTimeout(state.diagnosticTimer);state.diagnosticTimer=null;state.diagnosticLive=false;
  }
  async function fetchDiagnosticState(generation=state.diagnosticGeneration,reconcile=false) {
    if(!diagnosticAvailable())return;
    while(state.diagnosticPending){await state.diagnosticPending.catch(()=>{});if(generation!==state.diagnosticGeneration)return;}
    if(generation!==state.diagnosticGeneration)return;
    const expected=state.diagnostic?.id;
    const pending=api("/api/diagnostics/state");state.diagnosticPending=pending;
    try{
      const snapshot=await pending;
      if(generation!==state.diagnosticGeneration || expected!==state.diagnostic?.id)return;
      if(!validDiagnostic(snapshot))throw new Error("诊断状态格式无效，保留最近有效证据。缺失状态不等于无诊断。");
      state.diagnostic=snapshot;state.diagnosticKnown=true;state.diagnosticLive=true;
      if(reconcile && state.diagnosticUncertain){
        const uncertain=state.diagnosticUncertain;
        if(snapshot && snapshot.request_id===uncertain.request_id){state.diagnosticUncertain=null;notify("已按请求 UUID 核对诊断结果，未重复开始。");}
        else notify("当前快照未匹配上一次不确定请求；可能已重启或被后续请求替换，不能据此认定未执行，禁止自动重试。");
      }
      renderDiagnostic();updateControls();
    }catch(error){if(generation===state.diagnosticGeneration){state.diagnosticKnown=false;state.diagnosticLive=false;renderDiagnostic();updateControls();}throw error;}
    finally{if(state.diagnosticPending===pending)state.diagnosticPending=null;}
  }
  function startDiagnosticPolling() {
    stopDiagnosticPolling();if(!diagnosticAvailable() || state.diagnosticPaused)return;
    const generation=state.diagnosticGeneration;
    const poll=async()=>{try{await fetchDiagnosticState(generation);}catch(error){if(generation===state.diagnosticGeneration)notify(readableError(error));}finally{if(generation===state.diagnosticGeneration)state.diagnosticTimer=setTimeout(poll,1000);}};
    state.diagnosticTimer=setTimeout(poll,1000);
  }
  async function startDiagnostic(event) {
    event.preventDefault();clearError("diagnostic-error");
    if(!state.ready || !state.stateKnown || !state.diagnosticKnown || !diagnosticAvailable() || locked())return;
    const draft=diagnosticDraft(),error=diagnosticDraftError(draft);if(error){showError("diagnostic-error",error);return;}
    if(!globalThis.crypto?.randomUUID){showError("diagnostic-error","当前环境不能安全生成请求 UUID，已禁止启动。");return;}
    const config={request_id:globalThis.crypto.randomUUID(),...draft};
    state.diagnosticBusy=true;stopDiagnosticPolling();clearTimeout(settingsTimer);updateControls();
    const generation=state.diagnosticGeneration;
    try{
      const snapshot=await api("/api/diagnostics",{method:"POST",body:JSON.stringify(config)});
      if(generation!==state.diagnosticGeneration || state.diagnosticPaused)return;
      if(!snapshot || !validDiagnostic(snapshot) || snapshot.request_id!==config.request_id){const invalid=new Error("诊断可能已启动，但返回结果无效。请重新连接按 UUID 核对，不要重复开始。");invalid.uncertain=true;throw invalid;}
      state.diagnostic=snapshot;state.diagnosticKnown=true;state.diagnosticLive=true;renderDiagnostic();
    }catch(error){if(error.uncertain){state.diagnosticUncertain={request_id:config.request_id,operation:"start"};state.diagnosticKnown=false;}showError("diagnostic-error",readableError(error));}
    finally{state.diagnosticBusy=false;renderDiagnostic();updateControls();startDiagnosticPolling();}
  }
  async function stopDiagnostic() {
    if(!activeDiagnostic() || state.diagnosticStopping || state.diagnosticBusy || state.diagnostic.status!=="running")return;
    const id=state.diagnostic.id,request_id=state.diagnostic.request_id;
    state.diagnosticStopping=true;stopDiagnosticPolling();updateControls();
    const generation=state.diagnosticGeneration;
    try{
      const snapshot=await api("/api/diagnostics/stop",{method:"POST",body:JSON.stringify({id})});
      if(generation!==state.diagnosticGeneration || state.diagnosticPaused)return;
      if(!snapshot || !validDiagnostic(snapshot) || snapshot.id!==id){const invalid=new Error("停止结果不完整，请重新连接核对；停止请求不代表已结束。");invalid.uncertain=true;throw invalid;}
      state.diagnostic=snapshot;state.diagnosticKnown=true;state.diagnosticLive=true;renderDiagnostic();
    }catch(error){if(error.uncertain){state.diagnosticUncertain={request_id,operation:"stop"};state.diagnosticKnown=false;}notify(readableError(error));}
    finally{state.diagnosticStopping=false;updateControls();startDiagnosticPolling();}
  }
  // Batch state is global, but observations and stop authority belong only to this page's request.
  const batchStatuses = {running:"运行中",stopping:"停止中",stopped:"已停止",timed_out:"已超时",completed:"已完成",failed:"失败"};
  function validBatch(value) {
    const validItem = item => item && item.target && typeof item.target.id === "string" && typeof item.target.url === "string" && Number.isInteger(item.round) && item.round > 0 && typeof item.attempted === "boolean" && (typeof item.status === "string" || (item.status && typeof item.status === "object" && !Array.isArray(item.status)));
    return value === null || Boolean(value && diagnosticUUID.test(value.request_id) && Object.hasOwn(batchStatuses,value.status) && Array.isArray(value.items) && value.items.every(validItem) && (value.report === null || (value.report && typeof value.report.complete === "boolean" && Array.isArray(value.report.items) && value.report.items.every(validItem))) && (value.error === null || typeof value.error === "string"));
  }
  function batchOwned() { return Boolean(state.batchOwner && state.batch?.request_id === state.batchOwner); }
  function stopBatchPolling() { state.batchGeneration++; clearTimeout(state.batchTimer); state.batchTimer=null; }
  function applyBatch(value) {
    if (!validBatch(value)) throw new Error("批量状态格式无效，已保留最近状态。");
    state.batch=value; state.batchKnown=true;
    if (state.batchOwner && value && value.request_id !== state.batchOwner) state.batchOwner=null;
    if (state.batchUncertain && value?.request_id === state.batchUncertain.request_id) {
      if (state.batchUncertain.operation === "start" || value.status !== "running") state.batchUncertain=null;
    }
    renderBatch(); updateControls();
  }
  async function fetchBatchState(generation) {
    while (state.batchPending) { await state.batchPending.catch(()=>{}); if (generation !== state.batchGeneration) return; }
    if (generation !== state.batchGeneration) return;
    const pending=api("/api/batch/state"); state.batchPending=pending;
    try { const value=await pending; if (generation===state.batchGeneration) applyBatch(value); }
    catch(error) { if(generation===state.batchGeneration){state.batchKnown=false;renderBatch();updateControls();} throw error; }
    finally { if(state.batchPending===pending)state.batchPending=null; }
  }
  function startBatchPolling() {
    clearTimeout(state.batchTimer); const generation=state.batchGeneration;
    const poll=async()=>{try{await fetchBatchState(generation);}catch{}finally{if(generation===state.batchGeneration)state.batchTimer=setTimeout(poll,2000);}};
    state.batchTimer=setTimeout(poll,2000);
  }
  function renderBatch() {
    const exportLink=$("batch-export");
    if(exportLink?.setAttribute){const enabled=Boolean(state.batch)&&batchOwned()&&!state.nativeDialog;exportLink.setAttribute("aria-disabled",String(!enabled));exportLink.tabIndex=enabled?0:-1;if(enabled){exportLink.href=`/api/batch/report?id=${encodeURIComponent(state.batch.request_id)}`;exportLink.download=`aonet-batch-${state.batch.request_id}.json`;}}
    const items=state.mode === "probe" ? selectedItems() : [];
    const total=items.reduce((n,list)=>n+list.targets.length,0);
    text("batch-selection",state.mode === "probe" ? `已选 ${items.length} 个列表 / ${total} 个站点；不会隐式选择全部。` : "批量巡检仅使用轻量探测的站点选择，请切换模式并勾选站点。");
    const holder=$("batch-items"); holder.replaceChildren();
    if (state.batchKnown === false) { text("batch-status","批量状态未核对，请重新连接。"); return; }
    if (state.batchUncertain) { text("batch-status","批量请求结果不确定，请重新连接核对；不要重复提交。"); return; }
    if (!state.batch) { text("batch-status","尚无批量运行。"); return; }
    if (!batchOwned()) { text("batch-status",activeBatch()?"另一个会话正在批量巡检；无法查看明细或停止。":"另一个会话的批量记录不可查看。"); return; }
    const batch=state.batch, rows=batch.report?.items || batch.items;
    const attempted=rows.filter(item=>item.attempted).length;
    text("batch-status",`${batchStatuses[batch.status]} · ${routeLabel(batch.config?.proxy)} · 已尝试 ${attempted} / ${rows.length} · ${batch.report ? batch.report.complete ? "报告完整（全部已尝试，不代表全部成功）" : "报告部分完成" : "报告尚未生成（实时部分结果）"}${batch.error ? ` · ${batch.error}` : ""}`);
    for (const item of rows) {
      const status=typeof item.status === "string" ? item.status : Object.entries(item.status).map(([name,detail])=>`${name}: ${detail}`).join(", ");
      holder.append(el("div","batch-item",`第 ${item.round} 轮 · ${item.target.id} · ${item.target.url} · ${item.attempted ? "已尝试" : "未尝试"} · ${status}`));
    }
  }
  async function startBatch(event) {
    event.preventDefault(); if(locked() || !state.ready || !state.stateKnown || !state.monitorKnown) return;
    clearError("batch-error");
    const lists=state.mode === "probe" ? selectedItems() : [];
    const target_ids=lists.flatMap(list=>list.targets.map(target=>`${list.id}/${target.id}`));
    if (!target_ids.length) {showError("batch-error","请先在轻量探测中明确勾选站点。");return;}
    const rounds=Number($("batch-rounds").value), concurrency=Number($("batch-concurrency").value), timeout_secs=Number($("batch-timeout").value);
    if (!Number.isInteger(rounds)||rounds<1||rounds>3||!Number.isInteger(concurrency)||concurrency<1||concurrency>4||!Number.isInteger(timeout_secs)||timeout_secs<1||timeout_secs>120) {showError("batch-error","轮次须为 1–3，并发为 1–4，最长时间为 1–120 秒。");return;}
    if (!globalThis.crypto?.randomUUID) {showError("batch-error","无法生成安全请求 ID，禁止启动。");return;}
    let proxy;try{proxy=selectedProxy("batch");}catch(error){showError("batch-error",readableError(error));return;}
    const request={request_id:crypto.randomUUID(),list_ids:lists.map(list=>list.id),target_ids,concurrency,rounds,timeout_secs,route:proxy?"proxy":"direct",...(proxy?{proxy}:{})};
    state.batchBusy=true; state.batchOwner=request.request_id; stopBatchPolling(); const generation=state.batchGeneration; updateControls();
    try {const value=await api("/api/batch",{method:"POST",body:JSON.stringify(request)}); if(generation!==state.batchGeneration)return; if(!validBatch(value)||!value||value.request_id!==request.request_id){const error=new Error("启动结果无效，请重新连接核对，不要重复提交。");error.uncertain=true;throw error;} applyBatch(value);}
    catch(error){if(generation!==state.batchGeneration)return; if(error.uncertain){state.batchUncertain={request_id:request.request_id,operation:"start"};state.batchKnown=false;}else state.batchOwner=null;showError("batch-error",readableError(error));}
    finally{state.batchBusy=false;renderBatch();updateControls();if(generation===state.batchGeneration)startBatchPolling();}
  }
  async function stopBatch() {
    if(!batchOwned() || state.batchKnown === false || state.batchUncertain || state.batchBusy || state.batch?.status!=="running")return;
    const id=state.batchOwner; state.batchBusy=true; stopBatchPolling();const generation=state.batchGeneration; updateControls();
    try {const value=await api("/api/batch/stop",{method:"POST",body:JSON.stringify({id})});if(generation!==state.batchGeneration)return;if(!validBatch(value)||!value||value.request_id!==id){const error=new Error("停止结果无效，请重新连接核对，不要重复提交。");error.uncertain=true;throw error;}applyBatch(value);}
    catch(error){if(generation!==state.batchGeneration)return;if(error.uncertain){state.batchUncertain={request_id:id,operation:"stop"};state.batchKnown=false;}notify(readableError(error));}
    finally{state.batchBusy=false;renderBatch();updateControls();if(generation===state.batchGeneration)startBatchPolling();}
  }
  function selectedItems() {
    if (!state.bootstrap) return [];
    return state.mode === "probe"
      ? state.bootstrap.lists.map((list) => ({...list, targets: list.targets.filter((target) => state.selectedTargets.has(`${list.id}/${target.id}`))})).filter((list) => list.targets.length)
      : state.bootstrap.authorized.filter((target) => state.selectedAuthorized.has(key(target.id)) && target.modes.includes(state.mode) && !target.expired);
  }
  function currentCaps() {
    if (!state.bootstrap) return null;
    if (state.mode === "probe") return { concurrency: Math.min(4,Math.floor(state.bootstrap.limits.public_concurrency)), rps: Math.min(2,Math.floor(state.bootstrap.limits.public_rps)), duration: Math.min(120, state.bootstrap.limits.public_duration || 120) };
    const targets = selectedItems();
    if (!targets.length) return null;
    const values = ["max_concurrency", "max_rps", "max_duration"];
    if (targets.some((target) => values.some((field) => !isNumber(target[field]) || target[field] <= 0))) return null;
    return {
      concurrency: Math.min(1000, Math.floor(Math.min(...targets.map((target) => target.max_concurrency)))),
      rps: Math.min(1000, Math.floor(Math.min(...targets.map((target) => target.max_rps)))),
      duration: Math.min(300, Math.floor(Math.min(...targets.map((target) => target.max_duration))))
    };
  }
  function renderPicker() {
    const holder = $("target-picker");
    holder.replaceChildren();
    const probe = state.mode === "probe";
    text("target-label", probe ? "勾选探测目标" : "勾选授权目标");
    $("manage-lists").hidden = !probe;
    $("public-limits").hidden = !probe;
    text("mode-help", probe ? "仅低频探测；未勾选目标禁止运行。" : "仅限自有 / 明确授权 / 本机目标，过期许可不可选。");
    const add = (target, id, selection, supported) => {
      const label = el("label", "picker-option");
      const input = el("input"); input.type = "checkbox"; input.value = id;
      input.dataset.target = supported ? "available" : "unsupported";
      input.checked = selection.has(id) && supported;
      input.disabled = !supported || !state.ready || locked();
      const content = el("span", "picker-content");
      const address=el("span", "picker-detail", target.url);address.title=target.url;address.tabIndex=0;
      content.append(el("span", "picker-title", target.name), address);
      if (!probe) content.append(el("span", "picker-detail", `${target.expired ? "已过期 · " : ""}${target.modes.join(" / ")} · 并发 ${count(target.max_concurrency)} / ${count(target.max_rps)} RPS / ${count(target.max_duration)}s`));
      label.append(input, content); holder.append(label);
      input.addEventListener("change", () => {
        if (locked()) return;
        if (input.checked) selection.add(id); else selection.delete(id);
        state.selectedLists = new Set(selectedItems().map((list) => key(list.id)));
        updateConstraints(false); updateControls();
      });
    };
    if (probe) {
      for (const list of state.bootstrap?.lists || []) {
        holder.append(el("div", "picker-list-name", `${list.name} · ${list.targets.length}`));
        for (const target of list.targets) add(target, `${list.id}/${target.id}`, state.selectedTargets, true);
      }
    } else for (const target of state.bootstrap?.authorized || []) add(target, key(target.id), state.selectedAuthorized, target.modes.includes(state.mode) && !target.expired);
    if (!holder.childElementCount) holder.append(el("p", "empty-inline", probe ? "暂无目标，请添加站点列表。" : "暂无授权目标，请在授权目标页面添加。"));
  }
  function switchMode(mode) {
    if (mode === state.mode || !Object.hasOwn(modeInputs, mode)) return;
    // Store raw input strings too: switching modes must not erase in-progress edits.
    for (const id of ["concurrency", "rps", "duration", "timeout", "ramp"]) modeInputs[state.mode][id] = $(id).value;
    modeInputs[state.mode].fresh = $("fresh").checked;
    state.mode = mode;
    for (const id of ["concurrency", "rps", "duration", "timeout", "ramp"]) $(id).value = modeInputs[mode][id];
    $("fresh").checked = modeInputs[mode].fresh;
    renderPicker();
    // Keep a remembered draft intact; configError still rejects values above caps.
    updateConstraints(false);
    updateControls();
  }
  function updateConstraints(clamp = false) {
    const caps = currentCaps();
    const isProbe = state.mode === "probe";
    const isTcp = state.mode === "tcp";
    for (const [id, cap] of [["concurrency", caps?.concurrency], ["rps", caps?.rps], ["duration", caps?.duration]]) {
      const input = $(id);
      if (isNumber(cap)) {
        input.max = String(cap);
        if (clamp && isNumber(input.valueAsNumber) && input.valueAsNumber > cap) input.value = String(cap);
      } else input.removeAttribute("max");
    }
    $("rps").min = "1";
    $("rps").step = "1";
    $("timeout").max = "30";
    if (isProbe) { $("ramp").value = "0"; $("fresh").checked = false; }
    if (isTcp) $("fresh").checked = false;
    $("ramp").max = $("duration").value || "0";
    text("rps-label", isTcp ? "建连速率上限" : "速率上限");
    text("rps-unit", isTcp ? "次建连/秒" : "次/秒");
    const items = selectedItems();
    const targetCount = isProbe ? items.reduce((sum, list) => sum + list.targets.length, 0) : items.length;
    text("selection-note", isProbe ? `已选 ${items.length} 个列表 · ${targetCount} 个站点条目（重复项由后台处理）` : `已选 ${items.length} 个 ${state.mode.toUpperCase()} 授权目标；不使用站点列表。`);
    text("parameter-help", isProbe
      ? "公共限额适用于本次全部目标；探测不使用渐增或强制新连接。"
      : !items.length ? "请先选择目标，再按后台授权上限配置参数。"
        : !caps ? "所选目标缺少有效授权上限，已禁止启动，请检查后台配置。"
          : `按最严格目标约束整次测试：并发 ≤ ${count(caps.concurrency)}、速率 ≤ ${decimal(caps.rps)} 次/秒、时长 ≤ ${count(caps.duration)} 秒。${isTcp ? "TCP 不使用 HTTP 连接复用选项。" : ""}`);
    updateSummary();
  }
  function getConfig() {
    const items = selectedItems();
    return {
      mode: state.mode,
      list_ids: state.mode === "probe" ? items.map((list) => list.id) : [],
      target_ids: state.mode === "probe" ? items.flatMap((list) => list.targets.map((target) => `${list.id}/${target.id}`)) : [],
      authorized_ids: state.mode === "probe" ? [] : items.map((target) => target.id),
      concurrency: $("concurrency").valueAsNumber,
      rps: $("rps").valueAsNumber,
      duration_secs: $("duration").valueAsNumber,
      timeout_secs: $("timeout").valueAsNumber,
      ramp_secs: state.mode === "probe" ? 0 : $("ramp").valueAsNumber,
      fresh_connections: state.mode === "http" && $("fresh").checked,
      ...(!routeError("run") && selectedProxy("run") ? {proxy:selectedProxy("run")} : {})
    };
  }
  function configError(config) {
    if(routeError("run"))return routeError("run");
    if (!state.ready || !state.stateKnown) return "尚未读取完整后台状态，请重新连接。";
    if (state.uncertainWrite) return "上一次写操作结果尚未核对，请先重新连接。";
    if (state.bootstrap.authorization_error) return `授权配置需要修复：${state.bootstrap.authorization_error}`;
    if (config.mode === "probe" && !config.target_ids.length) return "请至少勾选一个探测目标。";
    if (activeRun() || activeDiagnostic() || diagnosticUnchecked() || state.diagnosticBusy || state.diagnosticUncertain) return "已有测试 / 诊断执行、收尾中或状态未核对，请等待结束并核对。";
    const items = selectedItems();
    if (!items.length) return state.mode === "probe" ? "请至少选择一个站点列表。" : "请至少选择一个具备对应模式权限的后台授权目标。";
    if (state.mode === "probe" && !items.some((list) => list.targets.length)) return "所选列表没有站点，请先添加。";
    if (state.mode === "probe" && cooldownRemaining() > 0) return `公共探测仍在冷却，请再等待 ${cooldownRemaining()} 秒。`;
    const caps = currentCaps();
    if (!caps || caps.concurrency < 1 || caps.rps <= 0 || (caps.duration !== null && caps.duration < 1)) return "授权上限不完整或不可用，请检查后台配置。";
    for (const [field, name] of [["concurrency", "并发"], ["duration_secs", "持续时间"], ["timeout_secs", "超时"]]) {
      if (!Number.isSafeInteger(config[field]) || config[field] < 1) return `${name}必须是大于零的整数。`;
    }
    if (!Number.isSafeInteger(config.rps) || config.rps < 1) return "速率必须是大于零的整数。";
    if (!Number.isSafeInteger(config.ramp_secs) || config.ramp_secs < 0 || config.ramp_secs > config.duration_secs) return "渐增时间必须为非负整数，且不超过持续时间。";
    if (config.concurrency > caps.concurrency || config.rps > caps.rps || (caps.duration !== null && config.duration_secs > caps.duration)) return "配置超出公共限额或所选目标的最严格授权上限。";
    if (config.timeout_secs > Math.min(30,state.bootstrap.limits.timeout||30)) return "单次超时请设置在 1～30 秒之间。";
    return "";
  }
  function updateSummary() {
    const config = getConfig();
    const items = selectedItems();
    const size = state.mode === "probe" ? `${items.length} 个列表 / ${items.reduce((sum, item) => sum + item.targets.length, 0)} 条站点` : `${items.length} 个授权目标`;
    text("preflight-summary", `${modeNames[state.mode]} · ${size}。并发 ${count(config.concurrency)}，${decimal(config.rps)} 次/秒，持续 ${count(config.duration_secs)} 秒。${routeLabel(config.proxy)}。${state.mode === "tcp" ? config.proxy ? "计数为代理隧道，非独立观测的目标连接数。" : "观测本机持有 socket。" : config.proxy ? "每次独立HTTP/1.1隧道，不复用连接池。" : ""}`);
  }
  function cooldownRemaining() { return Math.max(0, Math.ceil((state.cooldownUntil - Date.now()) / 1000)); }
  function updateControls() {
    renderMonitor();
    renderBatch();
    const lock = locked();
    for (const id of ["batch-rounds", "batch-concurrency", "batch-timeout"]) $(id).disabled = !state.ready || lock;
    $("batch-start").disabled = !state.ready || !state.stateKnown || !state.monitorKnown || lock;
    $("batch-stop").disabled = !state.ready || state.batchKnown === false || !batchOwned() || state.batchBusy || Boolean(state.batchUncertain) || state.batch?.status !== "running";
    document.querySelectorAll('input[name="mode"]').forEach((input) => { input.disabled = lock; });
    $("target-picker").querySelectorAll("input").forEach((input) => { input.disabled = !state.ready || lock || input.dataset.target === "unsupported"; });
    for (const id of ["concurrency", "rps", "duration", "timeout"]) $(id).disabled = !state.ready || lock;
    $("ramp").disabled = !state.ready || lock || state.mode === "probe";
    $("fresh").disabled = !state.ready || lock || state.mode !== "http" || $("run-route")?.value === "proxy";
    const remaining = cooldownRemaining();
    $("review-run").disabled = !state.ready || !state.stateKnown || lock || !selectedItems().length || (state.mode === "probe" && remaining > 0);
    $("review-run").firstElementChild.textContent = state.busy ? "正在提交…" : activeRun() ? "测试进行中" : state.uncertainWrite ? "请重新连接核对状态" : "检查并开始";
    $("stop-run").disabled = !state.ready || !state.stateKnown || state.uncertainWrite || !activeRun() || state.run?.status === "stopping" || state.stopping || state.busy;
    text("stop-run", state.stopping || state.run?.status === "stopping" ? "停止中…" : "停止测试");
    text("cooldown-text", remaining > 0 ? `本地保守冷却：剩余 ${remaining} 秒，后台仍会再次校验。` : "冷却由后台强制校验。");
    const inventoryDisabled = !state.ready || state.saving || state.busy || state.nativeDialog || state.uncertainWrite || activeBatch() || state.batchKnown === false || state.batchBusy || Boolean(state.batchUncertain);
    for (const id of ["new-list", "import-lists"]) $(id).disabled = inventoryDisabled || activeRun() || activeDiagnostic() || diagnosticUnchecked() || Boolean(state.diagnosticUncertain) || state.diagnosticBusy;
    $("export-lists").disabled = !state.bootstrap || state.nativeDialog;
    $("inventory").querySelectorAll("button").forEach((button) => { button.disabled = inventoryDisabled || activeRun() || activeDiagnostic() || diagnosticUnchecked() || Boolean(state.diagnosticUncertain) || state.diagnosticBusy; });
    $("save-list").disabled = inventoryDisabled || activeRun() || activeDiagnostic() || diagnosticUnchecked() || Boolean(state.diagnosticUncertain) || state.diagnosticBusy;
    $("inventory-confirm-save").disabled = inventoryDisabled || activeRun() || activeDiagnostic() || diagnosticUnchecked() || Boolean(state.diagnosticUncertain) || state.diagnosticBusy;
    $("authorize-check").disabled = state.busy;
    $("confirm-start").disabled = state.busy || (state.mode !== "probe" && !$("authorize-check").checked);
    text("confirm-start", state.busy ? "正在提交…" : "开始测试 ↗");
    for (const id of ["export-json", "export-csv"]) {
      const enabled = Boolean(state.run) && !state.nativeDialog;
      $(id).setAttribute("aria-disabled", String(!enabled));
      $(id).tabIndex = enabled ? 0 : -1;
    }
    updateWorkspaceControls();
    updateDiagnosticControls();
    setConnection();
  }

  function openDialog(id) {
    const dialog = $(id);
    state.restoreFocus.set(dialog, document.activeElement);
    dialog.showModal();
  }
  function closeDialog(id) { if ($(id).open) $(id).close(); }
  function addSummaryRow(label, value) { $("confirm-summary").append(el("dt", "", label), el("dd", "", value)); }
  function reviewRun(event) {
    event.preventDefault();
    const config = getConfig();
    const error = configError(config);
    if (error) { notify(error); return; }
    state.pendingRun = config;
    clearError("confirm-error");
    $("confirm-summary").replaceChildren();
    addSummaryRow("测试模式", modeNames[config.mode]);
    addSummaryRow("实际路径", routeLabel(config.proxy));
    addSummaryRow("并发 / 速率", `${count(config.concurrency)} 路 / ${decimal(config.rps)} ${config.mode === "tcp" ? "次建连" : "次"}/秒（整次测试）`);
    addSummaryRow("持续 / 超时", `${count(config.duration_secs)} 秒 / ${count(config.timeout_secs)} 秒`);
    addSummaryRow("渐增时间", config.mode === "probe" ? "不适用" : `${count(config.ramp_secs)} 秒`);
    addSummaryRow("连接策略", config.proxy ? config.mode === "tcp" ? "持有代理 CONNECT 隧道；目标TCP不可独立观测" : "每次独立 HTTP/1.1 代理隧道，不复用连接池" : config.mode === "http" ? config.fresh_connections ? "每次新建连接" : "允许 HTTP 连接复用" : config.mode === "tcp" ? "本机持有真实 socket；无 HTTP 2xx/3xx 比例" : "低频探测；由后台控制连接");
    const targets = $("confirm-targets");
    targets.replaceChildren();
    if (config.mode === "probe") {
      for (const list of selectedItems()) {
        targets.append(el("p", "", `${list.name} · ${list.targets.length} 个站点`));
        for (const target of list.targets) targets.append(el("p", "result-url", `${target.name || "未命名"} — ${target.url}`));
      }
    } else {
      for (const target of selectedItems()) targets.append(el("p", "", `${target.name} — ${target.url} · ${config.mode.toUpperCase()} 许可`));
    }
    const caps = currentCaps();
    text("confirm-limit", config.mode === "probe" ? `公共保护：并发 ≤ ${count(caps.concurrency)}、速率 ≤ ${decimal(caps.rps)} 次/秒，冷却 ${count(state.bootstrap.limits.public_cooldown)} 秒。` : `仅限自有 / 已授权 / 本机目标。按最严格授权：并发 ≤ ${count(caps.concurrency)}、速率 ≤ ${decimal(caps.rps)} 次/秒、时长 ≤ ${count(caps.duration)} 秒。`);
    $("authorization-row").hidden = config.mode === "probe";
    $("authorize-check").checked = false;
    updateControls();
    openDialog("confirm-dialog");
  }
  async function startRun() {
    if (state.busy || !state.pendingRun) return;
    if (state.mode !== "probe" && !$("authorize-check").checked) return;
    const config = state.pendingRun;
    const error = configError(config);
    if (error) { showError("confirm-error", error); return; }
    state.busy = true;
    clearTimeout(settingsTimer);
    clearError("confirm-error");
    updateControls();
    try {
      const run = await api("/api/runs", { method: "POST", body: JSON.stringify(config) });
      if (!run || !validRun(run)) { const invalid = new Error("创建测试响应不完整。后台可能已启动，请重新连接核对，不要重复开始。"); invalid.uncertain = true; throw invalid; }
      state.version += 1;
      state.stateKnown = true;
      applyRun(run);
      state.pendingRun = null;
      closeDialog("confirm-dialog");
      $("banner").hidden = true;
    } catch (error) {
      if (error.uncertain) {
        state.uncertainWrite = true;
        closeDialog("confirm-dialog");
        notify(readableError(error));
      } else showError("confirm-error", readableError(error));
      fetchState().catch((syncError) => notify(readableError(syncError)));
    } finally { state.busy = false; updateControls(); }
  }
  async function stopRun() {
    if (state.stopping || state.uncertainWrite || !activeRun()) return;
    state.stopping = true;
    updateControls();
    try {
      const result = await api("/api/stop", { method: "POST", body: "{}" });
      if (result && validRun(result)) { state.version += 1; applyRun(result); }
      else await fetchState();
      if (activeRun() && state.run.status !== "stopping") notify("已提交停止请求，正在等待后台确认；请以运行状态为准。");
    } catch (error) {
      if (error.uncertain) state.uncertainWrite = true;
      notify(readableError(error));
      fetchState().catch((syncError) => notify(readableError(syncError)));
    } finally { state.stopping = false; updateControls(); }
  }

  function applyRun(run) {
    const previous = state.run;
    const changedRun = previous?.id !== run?.id;
    if (previous && ["running", "stopping"].includes(previous.status) && run && !["running", "stopping"].includes(run.status) && state.page === "history") void loadHistory();
    state.run = run;
    // No invented server timestamp: start a conservative local cooldown when a
    // terminal probe is first observed. Backend remains authoritative across tabs.
    if (run?.mode === "probe" && !activeRun() && state.cooldownRun !== key(run.id)) {
      state.cooldownRun = key(run.id);
      state.cooldownUntil = Date.now() + (state.bootstrap?.limits.public_cooldown || 0) * 1000;
    }
    if (changedRun) { state.chartIndex = null; state.charts = []; state.resultRows.clear(); $("results-body").replaceChildren(); }
    const tcp = run?.mode === "tcp";
    text("run-status", run ? `${run.status === "running" ? "● " : run.status === "stopping" ? "◌ " : ""}${statusNames[run.status]}` : "尚未运行");
    $("run-status").dataset.status = run?.status || "idle";
    text("run-meta", run ? `${modeNames[run.mode]} · ${date(run.started_at)} · #${run.id}` : "等待第一次测试");
    text("hero-label", tcp ? "连接生命周期完成速率" : "实际速率");
    text("metric-rps", decimal(run?.rps));
    text("metric-p95", decimal(run?.p95_ms));
    text("active-label", tcp ? run?.config.proxy ? "已建立代理隧道" : "本机已连接" : "请求在途");
    text("metric-active", count(tcp ? run?.connected : run?.active));
    text("active-note", tcp ? `${run?.config.proxy ? "CONNECT 隧道，非目标连接数" : "真实 socket"} / 峰值 ${count(run?.peak_connected)}` : `当前 / 峰值 ${count(run?.peak_active)}`);
    text("success-label", tcp ? "HTTP 2xx/3xx 比例 · 不适用" : "HTTP 2xx/3xx 比例");
    const success = !tcp && isNumber(run?.responses) && run.responses > 0 && isNumber(run?.good_status) && run.good_status >= 0 && run.good_status <= run.responses ? `${decimal(run.good_status / run.responses * 100)}%` : "—";
    text("metric-success", success);
    text("success-note", tcp ? "TCP 不产生 HTTP 状态" : `2xx/3xx ${count(run?.good_status)} / 响应 ${count(run?.responses)}`);
    text("elapsed", `已用 ${decimal(run?.elapsed_secs, 0)} / ${count(run?.config?.duration_secs)} 秒`);
    const duration = run?.config?.duration_secs;
    $("progress").value = isNumber(duration) && duration > 0 && isNumber(run?.elapsed_secs) ? Math.min(100, Math.max(0, run.elapsed_secs / duration * 100)) : 0;
    text("progress-note", run ? !state.streamLive ? "最近状态 · 非实时" : statusNames[run.status] : "未开始");
    $("run-reason").hidden = !run?.reason;
    text("run-reason", run?.reason || "");
    $("run-config").hidden = !run;
    text("run-config", run ? `本次配置：${routeLabel(run.config.proxy)} · 并发 ${count(run.config.concurrency)} · ${decimal(run.config.rps)} 次/秒 · ${count(run.config.duration_secs)} 秒 · 超时 ${count(run.config.timeout_secs)} 秒${run.mode !== "probe" ? ` · 渐增 ${count(run.config.ramp_secs)} 秒` : ""}${run.mode === "http" ? ` · ${run.config.proxy ? "独立HTTP代理隧道，不复用" : run.config.fresh_connections ? "每次新连接" : "允许连接复用"}` : ""}` : "");
    for (const field of ["attempted", "completed", "errors", "cancelled"]) text(`count-${field}`, count(run?.[field]));
    text("count-p50", ms(run?.p50_ms));
    text("count-bytes", bytes(run?.bytes));
    text("p95-chart-title", tcp ? "P95 建连延迟" : "P95 延迟");
    text("rps-chart-title", tcp ? "连接生命周期完成速率" : "实际速率");
    text("series-rate-heading", tcp ? "生命周期完成 / 次每秒" : "速率 / 次每秒");
    text("chart-instruction", `${tcp ? "TCP 速率为连接生命周期结束速率，不是新建连接速率。" : ""}悬停读取采样；键盘聚焦图表后，用 ← → / Home / End 逐点查看。`);
    text("results-caption", run ? tcp ? "完成 = 已结束建连尝试；已连接 = 本机仍持有的真实 socket，不代表远端全局连接数。" : "完成 = 已结束尝试，不等于 HTTP 成功；错误和取消按后台分别列出。" : "所有数字来自后台快照，不使用演示数据。");
    text("target-active-heading", tcp ? "已连接" : "在途");
    renderResults();
    state.chartData = (run?.series || []).filter((point) => point && isNumber(point.elapsed_secs) && point.elapsed_secs >= 0).slice().sort((a, b) => a.elapsed_secs - b.elapsed_secs);
    renderCharts();
    if ($("series-details").open) renderSeriesTable();
    else if (!state.chartData.length) renderSeriesTable();
    updateControls();
  }
  function renderResults() {
    const holder = $("results-body");
    const scroll=holder.closest(".table-scroll"), scrollTop=scroll?.scrollTop, scrollLeft=scroll?.scrollLeft;
    const focused=holder.contains(document.activeElement)?document.activeElement:null;
    const targets = state.run?.targets || [];
    const query = (state.resultSearch || "").trim().toLocaleLowerCase();
    let visible = targets.map((target,index) => ({target,index,id: key(target.id ?? `${target.url}/${index}`)})).filter(({target}) => !query || [target.name,target.url,target.last_error].some(v => String(v || "").toLocaleLowerCase().includes(query)));
    const sort = state.settings.sort;
    if (sort && sort !== "stable") visible.sort((a,b) => {
      const cmp = sort === "name" ? String(a.target.name||"").localeCompare(String(b.target.name||""), "zh-CN") : (b.target[sort]||0)-(a.target[sort]||0);
      return cmp || a.index-b.index;
    });
    text("result-count", `${visible.length} / ${targets.length}`);
    if (state.resultRunId !== state.run?.id) {state.resultRows.clear(); holder.replaceChildren(); state.resultRunId=state.run?.id; state.resultHighlight=null;}
    const validIds=new Set(targets.map((t,i)=>key(t.id??`${t.url}/${i}`)));
    for(const [id,row]of state.resultRows) if(!validIds.has(id)){row.remove();state.resultRows.delete(id);}
    holder.querySelectorAll(".result-empty").forEach(row=>row.remove());
    for(const {target,id}of visible) {
      let row=state.resultRows.get(id);
      if(!row) {
        row=el("tr");row.tabIndex=0;row.dataset.resultId=id;
        for(let i=0;i<8;i++)row.append(el("td"));
        row.cells[0].append(el("span","result-name"),el("span","result-url"));
        row.addEventListener("click",()=>highlightResult(id));
        row.addEventListener("keydown",event=>{if(["Enter"," "].includes(event.key)){event.preventDefault();highlightResult(id);}});
        state.resultRows.set(id,row);
      }
      row.cells[0].firstChild.textContent=target.name||"未命名目标";row.cells[0].lastChild.textContent=target.url||"";row.cells[0].lastChild.title=target.url||"";row.cells[0].lastChild.tabIndex=0;row.cells[0].title=target.url||"";
      const values=[count(target.attempted),count(target.completed),`${count(target.errors)} / ${count(target.cancelled)}`,count(state.run.mode==="tcp"?target.connected:target.active),ms(target.last_latency_ms),`${state.run.mode==="tcp"?"TCP":target.last_status!=null?`HTTP ${target.last_status}`:"无响应"}${target.last_error?` · ${target.last_error}`:""}`,bytes(target.bytes)];
      values.forEach((value,index)=>{if(row.cells[index+1].textContent!==value)row.cells[index+1].textContent=value;});
      row.classList.toggle("is-highlighted",state.resultHighlight===id);
    }
    // Move only rows whose order actually changed; live numeric updates leave
    // existing keyed row nodes, focused controls and scroll positions in place.
    const shown=new Set(visible.map(v=>v.id));
    for(const [id,row]of state.resultRows) if(!shown.has(id))row.hidden=true;
    let cursor=holder.firstElementChild;
    for(const {id}of visible){const row=state.resultRows.get(id);row.hidden=false;if(row!==cursor)holder.insertBefore(row,cursor);cursor=row.nextElementSibling;}
    if(!visible.length){const row=el("tr","result-empty"),cell=el("td","table-empty",targets.length?"没有匹配的目标":"暂无测试结果");cell.colSpan=8;row.append(cell);holder.append(row);}
    if(state.resultHighlight) highlightResult(state.resultHighlight);
    else if($("result-detail"))$("result-detail").hidden=true;
    if(focused?.isConnected&&!focused.hidden&&document.activeElement!==focused)focused.focus({preventScroll:true});
    if(scroll){scroll.scrollTop=scrollTop;scroll.scrollLeft=scrollLeft;}
  }
  function highlightResult(id) {
    state.resultHighlight=id;
    for(const [rowId,row]of state.resultRows)row.classList.toggle("is-highlighted",rowId===id);
    const target=(state.run?.targets||[]).find((t,i)=>key(t.id??`${t.url}/${i}`)===id);
    if($("result-detail")){$("result-detail").hidden=!target;$("result-detail").textContent=target?JSON.stringify(target,null,2):"";}
  }

  function niceMax(value) {
    if (!isNumber(value) || value <= 0) return 1;
    const step = value / 4;
    const magnitude = 10 ** Math.floor(Math.log10(step));
    const normalized = step / magnitude;
    const snapped = [1, 2, 2.5, 5, 10].find((candidate) => candidate >= normalized) || 10;
    return snapped * magnitude * 4;
  }
  function axisNumber(value) {
    if (value >= 1e6) return `${decimal(value / 1e6)}M`;
    if (value >= 1e3) return `${decimal(value / 1e3)}k`;
    return decimal(value, value < 1 ? 2 : 1);
  }
  function renderCharts() {
    const specs = [
      { holder: "rps-chart", readout: "rps-readout", field: "rps", title: state.run?.mode === "tcp" ? "连接生命周期完成速率" : "实际速率", unit: "次/秒", color: "blue" },
      { holder: "p95-chart", readout: "p95-readout", field: "p95_ms", title: state.run?.mode === "tcp" ? "P95 建连延迟" : "P95 延迟", unit: "毫秒", color: "orange" }
    ];
    const focused = state.charts.find((chart) => chart.svg === document.activeElement)?.spec.holder;
    state.charts = [];
    for (const spec of specs) {
      const holder = $(spec.holder);
      holder.replaceChildren();
      const data = state.chartData;
      const valid = data.filter((point) => isNumber(point[spec.field]) && point[spec.field] >= 0);
      if (!valid.length) {
        const empty = el("div", "chart-empty");
        empty.append(el("strong", "", data.length ? "尚无有效读数" : "还没有采样"), el("span", "", "测试开始后呈现真实变化"));
        holder.append(empty);
        text(spec.readout, data.length ? "该指标暂无有效采样，不补零" : "暂无采样数据");
        continue;
      }
      // Match the viewBox to its actual CSS width so narrow plots do not shrink
      // axis labels into unreadable text. No inline styles are needed under CSP.
      const width = Math.max(240, holder.clientWidth || 420), height = 196;
      const plot = { left: 46, right: width - 20, top: 16, bottom: 164 };
      const xmax = Math.max(1, data[data.length - 1].elapsed_secs);
      let maxValue = 0;
      for (const point of valid) maxValue = Math.max(maxValue, point[spec.field]);
      const ymax = niceMax(maxValue);
      const x = (value) => plot.left + value / xmax * (plot.right - plot.left);
      const y = (value) => plot.bottom - value / ymax * (plot.bottom - plot.top);
      const svg = svgEl("svg", { viewBox: `0 0 ${width} ${height}`, tabindex: 0, role: "img", "aria-label": `${spec.title}趋势，单位${spec.unit}。左右方向键逐点读取，Home / End 跳转首末点。`, "aria-describedby": `chart-instruction ${spec.readout}` });
      svg.append(svgEl("title", {}, `${spec.title}随测试经过时间的变化`));
      for (let i = 0; i <= 4; i += 1) {
        const value = ymax * i / 4;
        const yy = y(value);
        svg.append(svgEl("line", { x1: plot.left, y1: yy, x2: plot.right, y2: yy, class: "chart-grid" }));
        svg.append(svgEl("text", { x: plot.left - 9, y: yy + 3, "text-anchor": "end", class: "chart-axis-label" }, axisNumber(value)));
      }
      for (let i = 0; i <= 2; i += 1) {
        const value = xmax * i / 2;
        svg.append(svgEl("text", { x: x(value), y: height - 11, "text-anchor": i === 0 ? "start" : i === 2 ? "end" : "middle", class: "chart-axis-label" }, `${decimal(value, 0)}s`));
      }
      let path = "", inSegment = false;
      for (const point of data) {
        if (!isNumber(point[spec.field]) || point[spec.field] < 0) { inSegment = false; continue; }
        path += `${inSegment ? "L" : "M"}${x(point.elapsed_secs).toFixed(2)},${y(point[spec.field]).toFixed(2)} `;
        inSegment = true;
      }
      svg.append(svgEl("path", { d: path, class: `chart-line ${spec.color}` }));
      const last = valid[valid.length - 1];
      svg.append(svgEl("circle", { cx: x(last.elapsed_secs), cy: y(last[spec.field]), r: 4, class: `chart-marker ${spec.color}` }));
      svg.append(svgEl("text", { x: Math.min(plot.right - 2, Math.max(plot.left + 30, x(last.elapsed_secs) - 7)), y: Math.max(11, y(last[spec.field]) - 10), "text-anchor": "end", class: "chart-end-label" }, axisNumber(last[spec.field])));
      const crosshair = svgEl("line", { x1: plot.left, x2: plot.left, y1: plot.top, y2: plot.bottom, class: "chart-crosshair", visibility: "hidden" });
      const marker = svgEl("circle", { r: 4, class: `chart-marker ${spec.color}`, visibility: "hidden" });
      const hit = svgEl("rect", { x: plot.left, y: plot.top - 6, width: plot.right - plot.left, height: plot.bottom - plot.top + 12, class: "chart-hit" });
      svg.append(crosshair, marker, hit);
      holder.append(svg);
      const chart = { spec, svg, plot, width, xmax, x, y, crosshair, marker };
      state.charts.push(chart);
      hit.addEventListener("pointermove", (event) => {
        const rect = svg.getBoundingClientRect();
        const local = (event.clientX - rect.left) / rect.width * width;
        const elapsed = Math.min(xmax, Math.max(0, (local - plot.left) / (plot.right - plot.left) * xmax));
        setChartIndex(nearestIndex(elapsed));
      });
      svg.addEventListener("pointerleave", () => {
        if (document.activeElement !== svg) { state.chartIndex = null; updateChartReadouts(); }
      });
      svg.addEventListener("focus", () => { if (state.chartIndex === null) setChartIndex(data.length - 1); });
      svg.addEventListener("blur", () => { state.chartIndex = null; updateChartReadouts(); });
      svg.addEventListener("keydown", (event) => {
        if (!["ArrowLeft", "ArrowRight", "Home", "End", "Escape"].includes(event.key)) return;
        event.preventDefault();
        if (event.key === "Escape") { state.chartIndex = null; updateChartReadouts(); return; }
        const current = state.chartIndex ?? data.length - 1;
        setChartIndex(event.key === "Home" ? 0 : event.key === "End" ? data.length - 1 : current + (event.key === "ArrowLeft" ? -1 : 1));
      });
    }
    updateChartReadouts();
    if (focused) state.charts.find((chart) => chart.spec.holder === focused)?.svg.focus({ preventScroll: true });
  }
  function nearestIndex(elapsed) {
    const data = state.chartData;
    let left = 0, right = data.length - 1;
    while (left < right) { const mid = Math.floor((left + right) / 2); if (data[mid].elapsed_secs < elapsed) left = mid + 1; else right = mid; }
    return left > 0 && Math.abs(data[left - 1].elapsed_secs - elapsed) <= Math.abs(data[left].elapsed_secs - elapsed) ? left - 1 : left;
  }
  function setChartIndex(index) {
    state.chartIndex = Math.min(state.chartData.length - 1, Math.max(0, index));
    updateChartReadouts();
  }
  function updateChartReadouts() {
    for (const chart of state.charts) {
      const index = state.chartIndex === null ? state.chartData.length - 1 : Math.min(state.chartIndex, state.chartData.length - 1);
      const point = state.chartData[index];
      if (!point) continue;
      const value = point[chart.spec.field];
      text(chart.spec.readout, `${state.chartIndex === null ? "最近采样" : "采样读数"} ${decimal(point.elapsed_secs)}s · ${chart.spec.title} ${decimal(value)}${isNumber(value) && value >= 0 ? ` ${chart.spec.unit}` : "（无数据）"}`);
      const inspecting = state.chartIndex !== null;
      const xx = chart.x(point.elapsed_secs);
      chart.crosshair.setAttribute("x1", xx);
      chart.crosshair.setAttribute("x2", xx);
      chart.crosshair.setAttribute("visibility", inspecting ? "visible" : "hidden");
      const valid = isNumber(value) && value >= 0;
      chart.marker.setAttribute("visibility", inspecting && valid ? "visible" : "hidden");
      if (valid) { chart.marker.setAttribute("cx", xx); chart.marker.setAttribute("cy", chart.y(value)); }
    }
  }
  function renderSeriesTable() {
    const body = $("series-body");
    body.replaceChildren();
    if (!state.chartData.length) {
      const row = el("tr"); const cell = el("td", "table-empty", "暂无采样数据"); cell.colSpan = 5; row.append(cell); body.append(row); return;
    }
    const fragment = document.createDocumentFragment();
    for (const point of state.chartData) {
      const row = el("tr");
      for (const value of [decimal(point.elapsed_secs), decimal(point.rps), decimal(point.p95_ms), count(point.active), count(point.connected)]) row.append(el("td", "", value));
      fragment.append(row);
    }
    body.append(fragment);
  }

  function safeReferenceUrl(value) {
    try {
      const parsed = new URL(value);
      return parsed.protocol === "https:" && parsed.hostname && !parsed.username && !parsed.password ? parsed.href : null;
    } catch { return null; }
  }
  function validateConnectionPresets(records) {
    if (records === undefined) return; // Older backends have no reference catalog.
    if (!Array.isArray(records) || records.length > 64) throw new Error("连接资料目录格式错误。");
    const ids = new Set();
    for (const card of records) {
      const strings = ["id", "provider", "product", "access_type", "status", "summary"];
      if (!card || strings.some((field) => typeof card[field] !== "string") || !card.id || ids.has(card.id)) throw new Error("连接资料格式错误或编号重复。");
      ids.add(card.id);
      for (const field of ["aliases", "limitations", "missing"]) {
        if (!Array.isArray(card[field]) || card[field].some((item) => typeof item !== "string")) throw new Error("连接资料说明格式错误。");
      }
      if (!Array.isArray(card.sources) || !Array.isArray(card.endpoints) || card.sources.length > 64 || card.endpoints.length > 128) throw new Error("连接资料配置格式错误。");
      const sources = new Set();
      for (const source of card.sources) {
        if (!source || typeof source.id !== "string" || !source.id || sources.has(source.id) || !safeReferenceUrl(source.url) || ["title", "checked_at", "scope"].some((field) => typeof source[field] !== "string")) throw new Error("连接资料来源无效。");
        sources.add(source.id);
      }
      for (const endpoint of card.endpoints) {
        if (!endpoint || !["url", "host", "wildcard"].includes(endpoint.kind) || ["purpose", "evidence", "notes"].some((field) => typeof endpoint[field] !== "string") || !Array.isArray(endpoint.source_ids) || endpoint.source_ids.some((id) => !sources.has(id))) throw new Error("连接资料端点或来源引用无效。");
        if (endpoint.port != null && (!Number.isInteger(endpoint.port) || endpoint.port < 1 || endpoint.port > 65535)) throw new Error("连接资料端口无效。");
        if ([endpoint.address, endpoint.protocol].some((value) => value != null && typeof value !== "string")) throw new Error("连接资料地址或协议无效。");
      }
    }
  }
  function connectionAccess(card) {
    return { browser: "浏览器工作台", desktop: "桌面客户端", api: "API 接口", desktop_browser: "桌面 / 浏览器", unconfirmed: "接入方式待确认" }[card.access_type] || card.access_type;
  }
  function connectionStatus(card) {
    return card.status === "unconfirmed" || card.id === "travelsky-ibe-plus" ? "产品接入待确认" : "部分配置 · 查看适用范围";
  }
  function renderConnectionCatalog() {
    const holder = $("connection-catalog");
    holder.replaceChildren();
    const cards = state.bootstrap?.connection_presets || [];
    text("connection-catalog-count", `${cards.length} 项 · 只读参考`);
    if (!cards.length) {
      holder.append(el("p", "empty-inline", "当前资源未提供业务连接资料目录。不会自动生成或探测未知端点。"));
      return;
    }
    for (const card of cards) {
      const article = el("article", "connection-card");
      const identity = el("div", "connection-card-identity");
      identity.append(el("p", "catalog-kicker", card.provider), el("h3", "", card.product), el("span", "tiny-tag", connectionAccess(card)));
      const description = el("div", "connection-card-description");
      description.append(el("p", "", card.summary), el("small", "", connectionStatus(card)));
      const action = el("button", "button secondary small", "查看连接配置");
      action.type = "button";
      action.dataset.connectionId = card.id;
      action.setAttribute("aria-label", `查看 ${card.product} 连接配置`);
      action.addEventListener("click", () => showConnectionPreset(card.id));
      article.append(identity, description, action);
      holder.append(article);
    }
  }
  function showConnectionPreset(id) {
    const card = state.bootstrap?.connection_presets?.find((item) => item.id === id);
    if (!card) return;
    text("connection-dialog-provider", card.provider);
    text("connection-dialog-title", card.product);
    const detail = $("connection-detail");
    detail.replaceChildren(el("p", "connection-summary", card.summary));
    const metadata = el("dl", "connection-metadata");
    for (const [label, value] of [["接入方式", connectionAccess(card)], ["环境", card.environment || "未限定 / 见逐项说明"], ["版本", card.version || "官方资料未限定"], ["资料状态", connectionStatus(card)], ["别名", card.aliases.join(" / ") || "—"]]) {
      metadata.append(el("dt", "", label), el("dd", "", value));
    }
    detail.append(metadata);
    const addNotes = (title, items) => {
      if (!items.length) return;
      const section = el("section", "connection-notes");
      const list = el("ul");
      for (const item of items) list.append(el("li", "", item));
      section.append(el("h3", "", title), list);
      detail.append(section);
    };
    addNotes("待补充 / 待确认", card.missing);
    addNotes("适用范围与限制", card.limitations);
    const endpoints = el("section", "connection-endpoints");
    endpoints.append(el("h3", "", "连接地址与放行规则"));
    if (!card.endpoints.length) endpoints.append(el("p", "field-help", "官方公开资料未确认可用端点，主机、协议和端口保持未知。"));
    const kindNames = { url: "URL", host: "精确主机", wildcard: "通配放行规则 · 非可连接主机" };
    const evidenceNames = { documented: "官方文档明确", derived: "标准推导（见说明）", observed: "页面身份观察", unknown: "未公开 / 未确认" };
    for (const endpoint of card.endpoints) {
      const row = el("article", "connection-endpoint");
      row.append(el("h4", "", endpoint.purpose), el("code", "connection-address", endpoint.address || "地址未公开 / 待确认"));
      row.append(el("p", "field-help", `${kindNames[endpoint.kind]} · 协议 ${endpoint.protocol || "未明确"} · 端口 ${endpoint.port ?? "未明确"}`));
      row.append(el("p", "field-help", `依据：${evidenceNames[endpoint.evidence] || endpoint.evidence} · 来源：${endpoint.source_ids.join("、") || "未提供"}`));
      if (endpoint.notes) row.append(el("p", "", endpoint.notes));
      const draft=diagnosticEndpointDraft(endpoint);
      if(draft){const fill=el("button","button secondary small","填入诊断草稿 · 不运行");fill.type="button";fill.disabled=!diagnosticAvailable() || locked();fill.addEventListener("click",()=>fillDiagnosticDraft(draft));row.append(fill);}
      endpoints.append(row);
    }
    detail.append(endpoints);
    const sources = el("section", "connection-sources");
    sources.append(el("h3", "", "官方资料与核对记录"));
    for (const source of card.sources) {
      const item = el("article", "connection-source");
      item.append(el("h4", "", `${source.id} · ${source.title}`));
      const url = safeReferenceUrl(source.url);
      // Desktop sources remain selectable text: no new external-navigation IPC capability.
      if (url && !transport.isDesktop) {
        const link = el("a", "connection-address", url);
        link.href = url; link.target = "_blank"; link.rel = "noopener noreferrer";
        item.append(link);
      } else item.append(el("span", "connection-address", url || "来源地址无效"));
      item.append(el("p", "field-help", `资料核对：${source.checked_at || "未记录"} · 发布：${source.published_at || "未标明"}`), el("p", "", source.scope));
      sources.append(item);
    }
    detail.append(sources);
    openDialog("connection-dialog");
  }

  function customLists() {
    return (state.bootstrap?.lists || []).filter((list) => !list.builtin).map((list) => ({
      id: list.id, name: list.name, description: typeof list.description === "string" ? list.description : "",
      targets: list.targets.map((target) => ({ id: target.id, name: target.name, url: target.url }))
    }));
  }
  function renderInventory() {
    const holder = $("inventory");
    holder.replaceChildren();
    const lists = state.bootstrap?.lists || [];
    text("inventory-count", `${lists.length} 个站点列表`);
    text("inventory-note", `内置 ${lists.filter((list) => list.builtin).length} · 自定义 ${lists.filter((list) => !list.builtin).length}`);
    if (!lists.length) { holder.append(el("p", "empty-inline", "还没有站点列表。新建自定义列表，或导入 JSON 开始整理。")); return; }
    for (const list of lists) {
      const card = el("article", "list-card");
      const header = el("div", "list-card-header");
      const heading = el("div");
      const title = el("div", "list-card-title");
      title.append(el("h2", "", list.name), el("span", "tiny-tag", list.builtin ? "内置 · 只读" : "自定义"), el("span", "tiny-tag", `${list.targets.length} 站点`));
      heading.append(title, el("p", "list-card-description", list.description || "暂无说明"));
      const actions = el("div", "list-card-actions");
      const copy = el("button", "text-button", "复制为自定义 ↗"); copy.type = "button";
      copy.addEventListener("click", () => editList(list, true)); actions.append(copy);
      if (!list.builtin) {
        const edit = el("button", "text-button", "编辑"); edit.type = "button";
        edit.addEventListener("click", () => editList(list));
        const remove = el("button", "text-button", "删除"); remove.type = "button";
        remove.addEventListener("click", () => confirmInventory({ type: "delete", id: list.id, name: list.name }));
        actions.append(edit, remove);
      }
      header.append(heading, actions); card.append(header);
      const scroll = el("div", "table-scroll"); scroll.tabIndex = 0; scroll.setAttribute("role", "region"); scroll.setAttribute("aria-label", `${list.name}站点`);
      const table = el("table", "list-table");
      table.append(el("caption", "sr-only", `${list.name}站点明细`));
      const head = el("thead"); const headRow = el("tr");
      for (const label of ["站点 / 地址", "地区", "来源 / 核验时间", "用途"]) { const cell = el("th", "", label); cell.scope = "col"; headRow.append(cell); }
      head.append(headRow); table.append(head);
      const body = el("tbody");
      for (const target of list.targets) {
        const row = el("tr");
        const name = el("td"); const address=el("div", "site-url", target.url);address.title=target.url;address.tabIndex=0;name.append(el("span", "site-name", target.name || "未命名站点"), address);
        const source = el("td", "site-source"); source.append(el("div", "", target.source || "用户自定义"), el("small", "", `核验：${date(target.checked_at)}`));
        const permission = el("td"); permission.append(el("span", "permission-tag", "仅轻量探测"));
        row.append(name, el("td", "", target.region || "—"), source, permission); body.append(row);
      }
      if (!list.targets.length) { const row = el("tr"); const cell = el("td", "table-empty", "此列表暂无站点"); cell.colSpan = 4; row.append(cell); body.append(row); }
      table.append(body); scroll.append(table); card.append(scroll); holder.append(card);
    }
    updateControls();
  }
  function editList(list = null, copy = false) {
    if (!state.ready || locked()) return;
    const isCopy = Boolean(copy && list);
    state.editing = { id: list && !isCopy ? list.id : null, original: list && !isCopy ? list : null };
    text("list-dialog-title", isCopy ? "复制为自定义列表" : list ? "编辑站点列表" : "新建站点列表");
    $("list-name").value = list ? `${list.name}${isCopy ? " · 副本" : ""}` : "";
    $("list-description").value = list?.description || "";
    $("list-targets").value = (list?.targets || []).map((target) => `${target.name || ""} ${target.url}`.trim()).join("\n");
    clearError("list-error");
    openDialog("list-dialog");
  }
  function validateUrl(raw, context) {
    let parsed;
    try { parsed = new URL(raw); } catch { throw new Error(`${context}：URL 无效，请使用完整 http:// 或 https:// 地址。`); }
    if (!["http:", "https:"].includes(parsed.protocol) || !parsed.hostname) throw new Error(`${context}：仅接受 HTTP / HTTPS 地址。`);
    if (parsed.username || parsed.password) throw new Error(`${context}：URL 不得携带账号或密码。`);
    if (raw.length > 4096) throw new Error(`${context}：URL 过长。`);
    return parsed.href;
  }
  function parseTargetLines(value, original) {
    const lines = value.split(/\r?\n/).map((line, index) => ({ text: line.trim(), line: index + 1 })).filter((line) => line.text);
    if (!lines.length) throw new Error("请至少填写一个站点。");
    if (lines.length > 1000) throw new Error("单个列表最多 1000 个站点，请分批整理。");
    const oldTargets = original?.targets || [];
    const reused = new Set();
    return lines.map((line) => {
      const match = line.text.match(/^(?:(.*?)\s+)?(https?:\/\/\S+)$/i);
      if (!match) throw new Error(`第 ${line.line} 行：请填写“名称 URL”或完整 URL，URL 不可包含空格。`);
      const url = validateUrl(match[2], `第 ${line.line} 行`);
      const name = (match[1] || "").trim() || new URL(url).hostname;
      if (utf8Length(name) > 200) throw new Error(`第 ${line.line} 行：站点名称最多 200 UTF-8 字节。`);
      const existing = oldTargets.find((target) => {
        if (reused.has(key(target.id))) return false;
        try { return new URL(target.url).href === url; } catch { return false; }
      });
      const id = existing ? existing.id : uid("target");
      reused.add(key(id));
      return { id, name, url };
    });
  }
  async function persistLists(lists) {
    // Explicit whitelist: never send builtin, source, probe_only, modes or limits.
    const clean = lists.map((list) => ({ id: list.id, name: list.name, description: list.description || "", targets: list.targets.map((target) => ({ id: target.id, name: target.name, url: target.url })) }));
    if (clean.length > 50) throw new Error("自定义列表总数最多 50 个。");
    if (clean.reduce((sum, list) => sum + list.targets.length, 0) > 1000) throw new Error("自定义站点总数最多 1000 个。");
    const body = JSON.stringify({ lists: clean });
    if (new TextEncoder().encode(body).length > 1024 * 1024) throw new Error("JSON 数据不能超过 1 MiB。");
    state.saving = true;
    updateControls();
    try {
      let result = await api("/api/lists", { method: "PUT", body });
      if (!result || !Array.isArray(result.lists) || typeof result.token !== "string") {
        try { result = await api("/api/bootstrap"); } catch {
          const error = new Error("列表可能已保存，但刷新结果失败。请重新连接核对后再编辑，不要重复保存。"); error.uncertain = true; throw error;
        }
      }
      try { applyBootstrap(result); } catch (cause) {
        const error = new Error(`列表可能已保存，但返回数据无效。请重新连接核对。${readableError(cause)}`); error.uncertain = true; throw error;
      }
    } catch (error) {
      if (error.uncertain) {
        state.uncertainWrite = true;
        closeDialog("list-dialog"); closeDialog("inventory-confirm-dialog");
        notify(readableError(error));
      }
      throw error;
    } finally { state.saving = false; updateControls(); }
  }
  async function saveList(event) {
    event.preventDefault();
    if (state.saving || !state.editing || !state.ready || state.uncertainWrite) return;
    clearError("list-error");
    try {
      if (activeRun() || activeDiagnostic() || diagnosticUnchecked() || state.diagnosticUncertain || state.diagnosticBusy) throw new Error("测试 / 诊断正在进行或状态未核对，请结束后再修改列表。");
      const name = $("list-name").value.trim();
      if (!name || utf8Length(name) > 200) throw new Error("请填写 1～200 UTF-8 字节的列表名称。");
      const description = $("list-description").value.trim();
      if (utf8Length(description) > 1000) throw new Error("说明最多 1000 UTF-8 字节。");
      const edited = { id: state.editing.id ?? uid("list"), name, description, targets: parseTargetLines($("list-targets").value, state.editing.original) };
      let lists = customLists();
      if (state.editing.id !== null) lists = lists.map((list) => key(list.id) === key(edited.id) ? edited : list);
      else lists.push(edited);
      await persistLists(lists);
      closeDialog("list-dialog");
      state.editing = null;
    } catch (error) { if (!error.uncertain) showError("list-error", readableError(error)); }
  }
  function confirmInventory(action) {
    if (!state.ready || locked()) return;
    state.inventoryAction = action;
    clearError("inventory-confirm-error");
    $("inventory-confirm-items").replaceChildren();
    if (action.type === "delete") {
      text("inventory-confirm-title", "删除自定义列表？");
      text("inventory-confirm-message", `将删除“${action.name}”。这会提交剩余全部自定义列表；内置列表和后台授权不会改变。此操作无法自动撤销，建议先导出备份。`);
      text("inventory-confirm-save", "确认删除");
    } else {
      text("inventory-confirm-title", "确认导入自定义列表");
      text("inventory-confirm-message", `将追加 ${action.lists.length} 个列表，不覆盖现有自定义列表。导入将生成新的列表与站点编号，忽略内置标记、授权和其他额外属性。保存时提交全部自定义列表。`);
      for (const list of action.lists) $("inventory-confirm-items").append(el("li", "", `${list.name} · ${list.targets.length} 个站点`));
      text("inventory-confirm-save", "确认导入并保存");
    }
    openDialog("inventory-confirm-dialog");
  }
  async function saveInventoryAction() {
    const action = state.inventoryAction;
    if (!action || state.saving || !state.ready || state.uncertainWrite) return;
    clearError("inventory-confirm-error");
    try {
      if (activeRun() || activeDiagnostic() || diagnosticUnchecked() || state.diagnosticUncertain || state.diagnosticBusy) throw new Error("测试 / 诊断正在进行或状态未核对，请结束后再修改列表。");
      let lists = customLists();
      lists = action.type === "delete" ? lists.filter((list) => key(list.id) !== key(action.id)) : lists.concat(action.lists);
      await persistLists(lists);
      closeDialog("inventory-confirm-dialog");
      state.inventoryAction = null;
    } catch (error) { if (!error.uncertain) showError("inventory-confirm-error", readableError(error)); }
  }
  function importSchema(data) {
    const lists = Array.isArray(data) ? data : data?.lists;
    if (!Array.isArray(lists) || !lists.length) throw new Error("JSON 需要非空 lists 数组，或直接使用列表数组。");
    if (lists.length + customLists().length > 50) throw new Error("自定义列表总数最多 50 个。");
    let total = customLists().reduce((sum, list) => sum + list.targets.length, 0);
    return lists.map((list, index) => {
      const context = `第 ${index + 1} 个列表`;
      if (!list || typeof list.name !== "string" || !list.name.trim() || utf8Length(list.name) > 200) throw new Error(`${context}：名称需为 1～200 UTF-8 字节。`);
      if (list.description != null && (typeof list.description !== "string" || new TextEncoder().encode(list.description).length > 1000)) throw new Error(`${context}：说明需为不超过 1000 UTF-8 字节的文本。`);
      if (!Array.isArray(list.targets) || !list.targets.length || list.targets.length > 1000) throw new Error(`${context}：targets 需要 1～1000 个站点。`);
      total += list.targets.length;
      if (total > 1000) throw new Error("自定义站点总数最多 1000 个。");
      const targets = list.targets.map((target, targetIndex) => {
        const prefix = `${context}第 ${targetIndex + 1} 个站点`;
        if (!target || typeof target.url !== "string") throw new Error(`${prefix}：缺少 URL。`);
        const url = validateUrl(target.url.trim(), prefix);
        if (target.name != null && (typeof target.name !== "string" || utf8Length(target.name) > 200)) throw new Error(`${prefix}：名称需为不超过 200 UTF-8 字节的文本。`);
        return { id: uid("target"), name: target.name?.trim() || new URL(url).hostname, url };
      });
      return { id: uid("list"), name: list.name.trim(), description: list.description?.trim() || "", targets };
    });
  }
  async function importFile(event) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    try {
      if (file.size > 1024 * 1024) throw new Error("JSON 文件不能超过 1 MiB。");
      let data;
      try { data = JSON.parse((await file.text()).replace(/^﻿/, "")); } catch { throw new Error("无法解析 JSON，请检查文件格式。"); }
      const lists = importSchema(data);
      confirmInventory({ type: "import", lists });
    } catch (error) { notify(readableError(error)); }
  }
  async function importLists() {
    if (!state.ready || locked()) return;
    if (!transport.isDesktop) { $("import-file").click(); return; }
    state.nativeDialog = true;
    updateControls();
    try {
      const data = await transport.importLists();
      if (data === null) return; // Cancelling the native picker is not an error.
      const lists = importSchema(data);
      confirmInventory({ type: "import", lists });
    } catch (error) { notify(readableError(error)); }
    finally { state.nativeDialog = false; updateControls(); }
  }
  async function nativeExport(kind, id) {
    if (state.nativeDialog) return;
    state.nativeDialog = true;
    updateControls();
    try {
      const saved = await transport.exportDialog(kind, id);
      if (saved === true) notify("已导出到所选文件。");
    } catch (error) {
      if (error.uncertain) state.uncertainWrite = true;
      notify(readableError(error));
    } finally { state.nativeDialog = false; updateControls(); }
  }
  function exportLists() {
    if (!state.bootstrap) return;
    if (transport.isDesktop) { void nativeExport("lists"); return; }
    const blob = new Blob([JSON.stringify({ lists: customLists() }, null, 2)], { type: "application/json;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const link = el("a");
    link.href = url;
    link.download = `aonet-custom-lists-${new Date().toISOString().slice(0, 10)}.json`;
    document.body.append(link); link.click(); link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 30000);
  }

  function utf8Length(value) { return new TextEncoder().encode(value).length; }
  async function workspace(operation, payload = {}) {
    const mutating = ["authorization.prepare","authorization.confirm","authorization.reload","selftest","profiles.save","settings.save","history.clear"].includes(operation);
    if(mutating && (locked() || !state.ready || !state.stateKnown))throw new Error("测试进行中、状态未核对或其他操作尚未结束，不能修改配置。");
    if(mutating){state.saving=true;updateControls();}
    try {return await api("/api/workspace", {method:"POST", body:JSON.stringify({operation,payload})});}
    catch(error){if(error.uncertain&&mutating){state.uncertainWrite=true;state.authDraft=null;notify(readableError(error));}throw error;}
    finally{if(mutating){state.saving=false;updateControls();}}
  }
  function updateWorkspaceControls() {
    updateRouteControls();
    if($("settings-http-proxy"))$("settings-http-proxy").disabled=locked()||!state.ready;
    const running = locked() || !state.stateKnown;
    if($("global-stop")){const diagnostic=activeDiagnostic(); if(activeBatch()){ $("global-stop").disabled=state.batchKnown === false || !batchOwned() || state.batchBusy || state.batch?.status!=="running"; text("global-stop","停止批量巡检"); } else if(monitorActive()){ $("global-stop").disabled=!monitorOwned() || state.monitorBusy || !state.monitorKnown; text("global-stop", "停止监控"); } else { $("global-stop").disabled=diagnostic ? !state.diagnosticKnown || state.diagnosticStopping || state.diagnosticBusy || state.diagnostic.status!=="running" : $("stop-run").disabled;text("global-stop",diagnostic ? state.diagnosticStopping || state.diagnostic.status!=="running" ? "诊断收尾中…" : "停止诊断" : $("stop-run").textContent);}}
    const sort=$("console-sort"), search=$("result-search");
    if(sort) sort.value=state.settings.sort||"stable";
    if(search && document.activeElement!==search) search.value=state.resultSearch||"";
    const side=$("self-test-enabled"); if(side){side.checked=Boolean(state.bootstrap?.self_test_enabled);side.disabled=running||!state.ready;}
    for(const node of document.querySelectorAll("#auth-form input, #auth-form textarea, #auth-prepare, #auth-confirm, #workspace-dialog-submit, #auth-json-prepare, #auth-json-content"))node.disabled=running||!state.ready;
    for(const id of ["dock-width","result-sort","column-select","column-width-number","column-width-range","reset-column-widths"])$(id).disabled=running||!state.ready;
    for(const id of ["new-auth","edit-auth-json","reload-auth","clear-history","save-settings","save-profile","delete-profile","load-profile","load-profile-console"]) {const n=$(id);if(n)n.disabled=running||!state.ready;}
    for(const node of document.querySelectorAll("#authorized-body button"))node.disabled=running||!state.ready||node.dataset.session==="true";
    const dock=$("dock-width"); if(dock){dock.value=String(state.settings.dock_width||300);text("dock-width-value",`${dock.value} px`);document.documentElement.style.setProperty("--dock-width",`${dock.value}px`);}
    const sorter=$("result-sort");if(sorter)sorter.value=state.settings.sort||"stable";
    for(const select of [$("profile-select"),$("profile-select-console")]) if(select){select.disabled=running||!state.ready;fillProfileSelect(select);}
    if(typeof applyColumnWidths==="function")applyColumnWidths();
    const status=$("statusbar"); if(status) text("statusbar",activeDiagnostic()?`${diagnosticStatusNames[state.diagnostic.status]} · #${state.diagnostic.id}`:activeRun()?`${statusNames[state.run.status]} · #${state.run.id}`:`${state.bootstrap?.authorized?.length||0} 项授权 · ${state.selectedTargets.size} 个目标已选`);
  }
  async function loadWorkspace() {
    try {
      const [settings, profiles]=await Promise.all([workspace("settings.get"),workspace("profiles.list")]);
      if(settings?.version===1) state.settings={...state.settings,...settings,dock_width:Math.min(480,Math.max(240,Number(settings.dock_width)||300))};
      state.profiles=Array.isArray(profiles)?profiles.filter(p=>p&&typeof p.id==="string"&&typeof p.name==="string"):[];
      state.workspaceLoaded=true;
      if($("settings-http-proxy"))$("settings-http-proxy").value=state.settings.http_proxy?.url||"";
      for(const prefix of ["monitor","run","batch","diagnostic"]){const input=$(prefix+"-proxy");if(input&&!input.value)input.value=state.settings.http_proxy?.url||"";}
      updateWorkspaceControls();renderResults();
    } catch(error) { notify(`工作区设置暂不可用：${readableError(error)}`); }
  }
  function fillProfileSelect(select) {
    const signature=JSON.stringify(state.profiles.map(p=>[p.id,p.name]));
    if(select.dataset.signature===signature)return;select.dataset.signature=signature;
    const old=select.value;select.replaceChildren();select.append(new Option("选择预设", ""));
    for(const p of state.profiles) select.append(new Option(p.name,p.id));
    if(state.profiles.some(p=>p.id===old))select.value=old;
  }
  function applyProfile(profile) {
    if(!profile?.config||locked())return;
    const c=profile.config;
    try {setRouteDraft("run",c.proxy);}catch(error){notify(readableError(error));return;}
    if(Object.hasOwn(modeInputs,c.mode)) {state.mode=c.mode;document.querySelectorAll('input[name="mode"]').forEach(i=>i.checked=i.value===c.mode);}
    for(const [source,id] of [["concurrency","concurrency"],["rps","rps"],["duration_secs","duration"],["timeout_secs","timeout"],["ramp_secs","ramp"]]) if(Number.isSafeInteger(c[source])) $(id).value=String(c[source]);
    if(typeof c.fresh_connections==="boolean")$("fresh").checked=c.fresh_connections;
    const validTargets=new Set((state.bootstrap?.lists||[]).flatMap(l=>l.targets.map(t=>`${l.id}/${t.id}`)));
    const validPermits=new Set((state.bootstrap?.authorized||[]).filter(t=>!t.expired&&!String(t.id).startsWith("demo-")).map(t=>key(t.id)));
    state.selectedTargets=new Set((c.target_ids||[]).filter(id=>validTargets.has(id)));
    state.selectedAuthorized=new Set((c.authorized_ids||[]).filter(id=>validPermits.has(id)));
    state.selectedLists=new Set((c.list_ids||[]).filter(id=>state.bootstrap?.lists.some(l=>l.id===id)));
    renderPicker();updateConstraints(false);updateControls();notify("已载入配置草稿；无效或 demo 目标已排除，不会自动启动测试。");
  }
  function profileSelected() {const id=$("profile-select")?.value||$("profile-select-console")?.value;return state.profiles.find(p=>p.id===id);}
  async function saveProfile() {
    if(locked())return;
    const existing=profileSelected();state.workspaceAction={type:"profile-save",id:existing?.id||uid("profile")};
    text("workspace-dialog-title",existing?"更新预设":"保存配置预设");text("workspace-dialog-message","保存运行参数与非 demo 目标选择。载入仅填写草稿，绝不自动运行。");$("workspace-dialog-label").hidden=false;$("workspace-dialog-input").value=existing?.name||"";openDialog("workspace-dialog");
  }
  async function workspaceSubmit(event) {
    event.preventDefault();const action=state.workspaceAction;if(!action||locked())return;clearError("workspace-dialog-error");
    try {
      if(action.type==="profile-save") {const name=$("workspace-dialog-input").value.trim();if(!name||utf8Length(name)>200)throw new Error("预设名称需为 1～200 UTF-8 字节。");const config=getConfig();config.authorized_ids=config.authorized_ids.filter(id=>!String(id).startsWith("demo-"));config.list_ids=config.list_ids.filter(id=>!String(id).startsWith("demo-"));config.target_ids=config.target_ids.filter(id=>!String(id).startsWith("demo-"));if(JSON.stringify(config).includes("demo-"))throw new Error("预设不能包含 demo 授权目标。");const profiles=state.profiles.filter(p=>p.id!==action.id).concat({id:action.id,name,config});state.profiles=await workspace("profiles.save",{profiles});}
      else if(action.type==="profile-delete") state.profiles=await workspace("profiles.save",{profiles:state.profiles.filter(p=>p.id!==action.id)});
      else if(action.type==="history-clear") {await workspace("history.clear",{confirm:true});state.history=[];state.historyDetail=null;$("history-detail").hidden=true;renderHistory();}
      else if(action.type==="auth-delete") await preparePermits(persistentPermits().filter(p=>p.id!==action.id));
      closeDialog("workspace-dialog");state.workspaceAction=null;updateWorkspaceControls();
    }catch(error){showError("workspace-dialog-error",readableError(error));}
  }
  function renderAuthorized() {
    const body=$("authorized-body");if(!body)return;body.replaceChildren();
    if(state.bootstrap?.authorization_error){showError("auth-error",state.bootstrap.authorization_error);}else clearError("auth-error");
    for(const target of state.bootstrap?.authorized||[]){const tr=el("tr");const controls=el("td");const edit=el("button","text-button","编辑");edit.type="button";edit.addEventListener("click",()=>editAuthorization(target));const del=el("button","text-button","删除");del.type="button";del.addEventListener("click",()=>{if(locked())return;state.workspaceAction={type:"auth-delete",id:target.id};text("workspace-dialog-title","删除授权");text("workspace-dialog-message",`将从完整授权配置中删除 ${target.name}。随后需要审核并确认。`);$("workspace-dialog-label").hidden=true;openDialog("workspace-dialog");});edit.dataset.session=del.dataset.session=String(String(target.id).startsWith("demo-"));edit.disabled=del.disabled=locked()||String(target.id).startsWith("demo-");controls.append(edit,del);
      const values=[`${target.id} / ${target.name}`,target.url,(target.modes||[]).join(", "),(target.allowed_ips||[]).join(", "),`${target.authorization} / ${target.expires_at}${target.expired?" · 已过期":""}`,`并发 ${target.max_concurrency} · ${target.max_rps} RPS · ${target.max_duration}s`];for(const [index,v] of values.entries()){const cell=el("td","",v);cell.title=String(v??"");if(index===1)cell.tabIndex=0;tr.append(cell);}tr.append(controls);body.append(tr);}
    if(!body.childElementCount){const tr=el("tr"),td=el("td","table-empty",state.bootstrap?.authorization_error?"授权文件不可读取，可新增完整配置修复。":"暂无授权记录");td.colSpan=7;tr.append(td);body.append(tr);}
  }
  function editAuthorization(target=null) {
    if(locked()||!state.ready||!state.stateKnown||String(target?.id||"").startsWith("demo-"))return;state.authEditing=target?.id||null;const fields={"auth-id":target?.id||uid("permit"),"auth-name":target?.name||"","auth-url":target?.url||"","auth-expiry":target?.expires_at||"","auth-ips":(target?.allowed_ips||[]).join("\n"),"auth-concurrency":target?.max_concurrency??10,"auth-rps":target?.max_rps??10,"auth-duration":target?.max_duration??120,"auth-authorization":target?.authorization||"","auth-proxy-routes":JSON.stringify(target?.proxy_routes||[],null,2)};for(const [id,v]of Object.entries(fields))$(id).value=String(v);$("auth-http").checked=target?.modes?.includes("http")||false;$("auth-tcp").checked=target?.modes?.includes("tcp")||false;clearError("auth-edit-error");openDialog("auth-dialog");
  }
  async function prepareAuthorization(event) {
    event.preventDefault();if(locked())return;clearError("auth-edit-error");
    try {const modes=[];if($("auth-http").checked)modes.push("http");if($("auth-tcp").checked)modes.push("tcp");if(!modes.length)throw new Error("至少选择 HTTP 或 TCP 模式。");const target={id:$("auth-id").value.trim(),name:$("auth-name").value.trim(),url:$("auth-url").value.trim(),modes,allowed_ips:$("auth-ips").value.split(/\r?\n/).map(x=>x.trim()).filter(Boolean),max_concurrency:Number($("auth-concurrency").value),max_rps:Number($("auth-rps").value),max_duration:Number($("auth-duration").value),authorization:$("auth-authorization").value.trim(),expires_at:$("auth-expiry").value.trim()};if(!target.id||utf8Length(target.id)>200||!target.name||utf8Length(target.name)>200||utf8Length(target.authorization)>1000)throw new Error("ID、名称或授权说明无效或超过 UTF-8 长度限制。");for(const f of ["max_concurrency","max_rps","max_duration"])if(!Number.isSafeInteger(target[f])||target[f]<1)throw new Error("并发、整数 RPS 和时长必须为正整数。");if(target.max_duration>300)throw new Error("最大时长不能超过 300 秒。");const routes=JSON.parse($("auth-proxy-routes").value||"[]");if(!Array.isArray(routes))throw new Error("代理路径必须是JSON数组");target.proxy_routes=routes.map(route=>{if(!route||typeof route.authorization!=="string"||!route.authorization.trim()||!Array.isArray(route.allowed_ips)||!route.allowed_ips.length||Object.keys(route).some(k=>!["url","authorization","allowed_ips"].includes(k)))throw new Error("每条代理路径需url、allowed_ips和authorization");return {...route,url:normalizeProxy({url:route.url}).url};});const permits=persistentPermits().filter(x=>x.id!==state.authEditing);if(permits.some(p=>p.id===target.id))throw new Error("授权 ID 已存在。");permits.push(target);await preparePermits(permits);closeDialog("auth-dialog");}
    catch(error){showError("auth-edit-error",readableError(error));}
  }
  function persistentPermits() {
    const fields=["id","name","url","modes","allowed_ips","max_concurrency","max_rps","max_duration","authorization","expires_at"];
    return (state.bootstrap?.authorized||[]).filter(p=>!String(p.id).startsWith("demo-")).map(p=>{
      const record=Object.fromEntries(fields.map(f=>[f,p[f]]));
      if(Object.hasOwn(p,"proxy_routes")) record.proxy_routes=JSON.parse(JSON.stringify(p.proxy_routes));
      return record;
    });
  }
  async function preparePermits(targets) {
    if(targets.some(p=>String(p.id).startsWith("demo-")))throw new Error("demo-* 是会话专用 ID，不能写入授权文件。");
    state.authDraft=null;
    const preview=await workspace("authorization.prepare",{targets});
    if(!preview||typeof preview.draftToken!=="string"||!preview.draftToken||!Array.isArray(preview.targets)||!Number.isSafeInteger(preview.revision)||!isNumber(preview.expiresIn)||preview.expiresIn<=0)throw new Error("授权审核响应格式无效；未允许确认。");
    state.authDraft={...preview,deadline:Date.now()+preview.expiresIn*1000};
    text("auth-preview-meta",`完整配置将替换本机授权文件 · 版本 ${preview.revision} · ${preview.expiresIn} 秒后过期。只保存，不会启动测试。`);
    $("auth-preview-content").textContent=JSON.stringify(preview.targets,null,2);
    $("auth-preview-effects").replaceChildren(...(preview.sideEffects||[]).map(x=>el("li","",x)));clearError("auth-preview-error");openDialog("auth-preview-dialog");
  }
  async function confirmAuthorization() {
    if(!state.authDraft||locked())return;clearError("auth-preview-error");if(Date.now()>=state.authDraft.deadline){showError("auth-preview-error","审核草稿已过期，请返回编辑后重新审核。");return;}$("auth-confirm").disabled=true;
    try{await workspace("authorization.confirm",{draftToken:state.authDraft.draftToken,revision:state.authDraft.revision});state.authDraft=null;closeDialog("auth-preview-dialog");await initialize();}
    catch(error){showError("auth-preview-error",readableError(error));}
    finally{$("auth-confirm").disabled=false;}
  }
  async function loadHistory() {
    try{const list=await workspace("history.list",{});state.history=Array.isArray(list)?list:[];renderHistory();}
    catch(error){notify(`历史记录读取失败：${readableError(error)}`);}
  }
  function renderHistory() {
    const body=$("history-body");if(!body)return;body.replaceChildren();
    for(const row of state.history){const tr=el("tr");for(const v of [statusNames[row.status]||row.status,modeNames[row.mode]||row.mode,date(row.started_at),`${decimal(row.elapsed_secs)} s`,count(row.attempted),count(row.errors)])tr.append(el("td","",v));const td=el("td");const view=el("button","text-button","详情");view.type="button";view.addEventListener("click",()=>void showHistory(row.id));td.append(view);for(const [kind,label] of [["json","JSON"],["csv","CSV"]]){const b=el("button","text-button",label);b.type="button";b.addEventListener("click",()=>void exportHistory(row.id,kind));td.append(b);}tr.append(td);body.append(tr);}
    if(!body.childElementCount){const tr=el("tr"),td=el("td","table-empty","暂无历史运行");td.colSpan=7;tr.append(td);body.append(tr);}
  }
  async function showHistory(id) {state.historyRequestId=id;try{const run=await workspace("history.get",{id});if(state.historyRequestId!==id)return;state.historyDetail=run;const panel=$("history-detail");panel.replaceChildren(el("h2","",`历史详情 · #${run.id} · ${run.status}`));panel.append(el("p","",`${modeNames[run.mode]||run.mode} · ${date(run.started_at)} · 尝试 ${count(run.attempted)} · 错误 ${count(run.errors)}`));const pre=el("pre","",JSON.stringify(run,null,2));panel.append(pre);panel.hidden=false;}catch(error){notify(readableError(error));}}
  async function exportHistory(id,kind) {try{if(transport.isDesktop){await nativeExport(kind,id);return;}const result=await workspace("history.export",{id,kind});const blob=new Blob([result.content],{type:result.mime||"application/octet-stream"});const url=URL.createObjectURL(blob);const a=el("a");a.href=url;a.download=result.filename||`run-${id}.${kind}`;document.body.append(a);a.click();a.remove();setTimeout(()=>URL.revokeObjectURL(url),30000);}catch(error){notify(readableError(error));}}
  async function saveSettings() {if(locked())return;try {const raw=$("settings-http-proxy").value.trim();state.settings.http_proxy=raw?normalizeProxy({url:raw}):null;}catch(error){notify(readableError(error));return;}state.settings={...state.settings,version:1,page:state.page,dock_width:Number($("dock-width").value),sort:$("result-sort").value,column_widths:state.settings.column_widths||{}};try{state.settings=await workspace("settings.save",{settings:state.settings});for(const prefix of ["monitor","run","batch","diagnostic"]){const input=$(prefix+"-proxy");if(input&&!input.value)input.value=state.settings.http_proxy?.url||"";}updateWorkspaceControls();notify("设置已保存；各测试仍需明确选择直连或代理。");}catch(error){notify(readableError(error));}}
  async function refreshAuthorization() {if(locked())return;try{const data=await workspace("authorization.reload",{});if(data?.authorized)applyBootstrap(data);else await initialize();}catch(error){notify(readableError(error));}}

  const resultColumns = [
    {key:"name",label:"目标 / 地址",width:240,sort:"name"},
    {key:"attempted",label:"尝试",width:80,sort:"attempted"},
    {key:"completed",label:"完成",width:80},
    {key:"errors",label:"错误 / 取消",width:100,sort:"errors"},
    {key:"active",label:"在途 / 已连接",width:80},
    {key:"latency",label:"最近延迟",width:100},
    {key:"result",label:"最近结果",width:220},
    {key:"bytes",label:"接收数据",width:100}
  ];
  function columnWidth(column) {
    const saved=state.settings.column_widths?.[column.key];
    return isNumber(saved)?Math.min(800,Math.max(60,Math.round(saved))):column.width;
  }
  function applyColumnWidths() {
    const table=document.querySelector(".results-table");
    if(!table)return;
    let group=table.querySelector("colgroup");
    if(!group){group=el("colgroup");for(const column of resultColumns){const col=el("col");col.dataset.column=column.key;group.append(col);}table.insertBefore(group,table.firstChild);}
    let total=0;for(const [index,column] of resultColumns.entries()){const width=columnWidth(column);group.children[index].style.width=`${width}px`;total+=width;}
    table.style.width=`${total}px`;
    table.querySelectorAll("thead th").forEach((header,index)=>{const column=resultColumns[index];if(column?.sort)header.setAttribute("aria-sort",state.settings.sort===column.sort?(column.sort==="name"?"ascending":"descending"):"none");});
    const selected=resultColumns.find(c=>c.key===$("column-select")?.value)||resultColumns[0];
    for(const id of ["column-width-number","column-width-range"]){const input=$(id);if(input&&document.activeElement!==input)input.value=String(columnWidth(selected));}
  }
  function setColumnWidth(value) {
    if(locked()||!state.ready||!Number.isFinite(value))return;
    const key=$("column-select").value;
    if(!resultColumns.some(c=>c.key===key))return;
    state.settings.column_widths={...state.settings.column_widths,[key]:Math.min(800,Math.max(60,Math.round(value)))};
    $("column-width-number").value=String(state.settings.column_widths[key]);$("column-width-range").value=String(state.settings.column_widths[key]);
    applyColumnWidths();queueSettingsSave();
  }
  function setResultSort(sort) {
    state.settings.sort=sort;$("console-sort").value=sort;$("result-sort").value=sort;
    renderResults();applyColumnWidths();queueSettingsSave();
  }
  function switchResultTab(name,focus=false) {
    const names=["results","curves","samples"];if(!names.includes(name))return;
    state.resultTab=name;
    for(const item of names){const tab=$(`tab-${item}`);tab.setAttribute("aria-selected",String(item===name));tab.tabIndex=item===name?0:-1;$(`view-${item}`).hidden=item!==name;}
    if(focus)$(`tab-${name}`).focus({preventScroll:true});
    if(name==="curves")renderCharts();
    if(name==="samples"){$("series-details").open=true;renderSeriesTable();}
  }
  function initializeResultViews() {
    document.querySelectorAll("[data-result-tab]").forEach(tab=>{
      tab.addEventListener("click",()=>switchResultTab(tab.dataset.resultTab));
      tab.addEventListener("keydown",event=>{const names=["results","curves","samples"],index=names.indexOf(tab.dataset.resultTab);if(!["ArrowLeft","ArrowRight","Home","End"].includes(event.key))return;event.preventDefault();switchResultTab(names[event.key==="Home"?0:event.key==="End"?2:(index+(event.key==="ArrowRight"?1:2))%3],true);});
    });
    document.querySelectorAll(".results-table thead th").forEach((header,index)=>{
      const column=resultColumns[index];if(!column?.sort)return;
      const button=el("button","table-sort-button",column.label);button.type="button";button.title="按此列排序，再次点击恢复后台顺序";
      button.addEventListener("click",()=>setResultSort(state.settings.sort===column.sort?"stable":column.sort));header.replaceChildren(button);header.setAttribute("aria-sort","none");
    });
    $("column-select").addEventListener("change",applyColumnWidths);
    for(const id of ["column-width-number","column-width-range"])$(id).addEventListener("input",()=>setColumnWidth(Number($(id).value)));
    $("reset-column-widths").addEventListener("click",()=>{if(locked())return;state.settings.column_widths={};applyColumnWidths();queueSettingsSave();});
    applyColumnWidths();switchResultTab("results");
  }
  let settingsTimer;
  function queueSettingsSave() {
    clearTimeout(settingsTimer);
    if(!state.workspaceLoaded||locked())return;
    settingsTimer=setTimeout(async()=>{
      if(locked())return;
      try {await workspace("settings.save",{settings:state.settings});}catch(error){notify(readableError(error));}
    },500);
  }
  // Observing a generation never grants ownership of its browser lease.
  function monitorActive() { return Boolean(state.monitor?.active || state.monitor?.draining); }
  function monitorOwned() { return Boolean(state.monitorOwner && state.monitor?.generation === state.monitorOwner); }
  function monitorLocked() { return monitorActive() || state.monitorBusy || state.monitorUncertain || Boolean(state.monitor && !state.monitorKnown); }
  function validMonitor(value) {
    return value && typeof value.active === "boolean" && typeof value.draining === "boolean" && (value.generation === null || typeof value.generation === "string") && value.config && Number.isInteger(value.config.revision) && Array.isArray(value.config.targets) && value.targets && typeof value.targets === "object";
  }
  function applyMonitor(value) {
    if (!validMonitor(value)) throw new Error("监控状态格式无效，无法核对。");
    state.monitor=value; state.monitorKnown=true;
    if (!value.active && !value.draining) state.monitorUncertain=false;
    if (state.monitorOwner && value.generation !== state.monitorOwner) state.monitorOwner=null;
    renderMonitor(); updateControls();
  }
  async function fetchMonitorState() {
    if(state.monitorPending) return state.monitorPending;
    const epoch=state.monitorEpoch;
    const pending=(async()=>{try {const value=await api("/api/monitor/state"); if(epoch===state.monitorEpoch) applyMonitor(value);} catch(error) {if(epoch===state.monitorEpoch){state.monitorKnown=false; renderMonitor(); updateControls();} throw error;}})();
    state.monitorPending=pending;
    try {await pending;} finally {if(state.monitorPending===pending) state.monitorPending=null;}
  }
  function startMonitorPolling() {
    clearTimeout(state.monitorTimer); const epoch=++state.monitorEpoch;
    const poll=async()=>{if(epoch!==state.monitorEpoch)return; try {
      if(!transport.isDesktop && monitorOwned() && state.monitor?.active && !state.monitorUncertain) { try { await api("/api/monitor/heartbeat",{method:"POST",body:JSON.stringify({generation:state.monitorOwner})}); } catch(error) { if(error.uncertain) state.monitorUncertain=true; /* Reconcile even when the lease expired or the backend stopped on 407. */ } }
      await fetchMonitorState();
    } catch(error) {state.monitorKnown=false; renderMonitor(); updateControls();} finally {if(epoch===state.monitorEpoch)state.monitorTimer=setTimeout(poll,5000);}};
    state.monitorTimer=setTimeout(poll,5000);
  }
  async function monitorWrite(path, body) {
    if(state.monitorBusy || state.monitorUncertain) return null;
    state.monitorBusy=true; renderMonitor(); updateControls();
    try {const result=await api(path,{method:"POST",body:JSON.stringify(body)}); if(!validMonitor(result)){const error=new Error("后台返回的监控结果无法核对；不要重复提交。");error.uncertain=true;throw error;} applyMonitor(result);return result;}
    catch(error){if(error.uncertain)state.monitorUncertain=true; showError("monitor-error",readableError(error)); if(!$('monitor-dialog')?.open)notify(readableError(error)); return null;}
    finally {state.monitorBusy=false;renderMonitor();updateControls();}
  }
  function renderMonitor() {
    if(!$('monitor-cards'))return;
    const snapshot=state.monitor, owned=monitorOwned(), busy=state.monitorBusy || state.monitorUncertain;
    const canEdit=state.ready && state.stateKnown && state.monitorKnown && !locked();
    for(const node of $('monitor-form').querySelectorAll('input, button')) node.disabled=!canEdit;
    $('monitor-open').disabled=!canEdit;
    $('monitor-start').disabled=!canEdit || !snapshot?.config.targets.length;
    $('monitor-stop').disabled=!state.monitorKnown || !owned || !snapshot?.active || busy;
    text('monitor-status',state.monitorUncertain ? '操作结果不确定，请勿重复提交；重新连接核对。' : !state.monitorKnown ? '状态未知或后台不支持监控；不会自动开始。' : snapshot.draining ? '正在停止并等待收尾' : snapshot.active ? owned ? '本窗口正在监控' : '监控由其他窗口启动，本窗口不能控制其租约' : '未运行 · 等待手动开始');
    if($('monitor-actual-route'))text('monitor-actual-route',snapshot?.generation?`本次监控路径：${routeLabel(snapshot.proxy)}${snapshot.dns_draining?' · DNS仍在收尾':''}`:'尚未开始监控；路径选择仅为草稿。');
    const cards=$('monitor-cards');
    const query=($('monitor-search')?.value || '').trim().toLocaleLowerCase();
    const signature=JSON.stringify([snapshot,state.monitorKnown,owned,busy,canEdit,query]);
    if(cards.dataset?.signature===signature)return;
    const expanded=new Set([...cards.querySelectorAll('details[open]')].map(node=>node.dataset.target));
    const focused=document.activeElement?.dataset?.monitorAction;
    const focusTarget=document.activeElement?.dataset?.monitorTarget;
    cards.replaceChildren(); if(cards.dataset)cards.dataset.signature=signature;
    const groups=new Map();
    for(const target of snapshot?.config.targets || []) {
      if(query && !`${target.name} ${target.host} ${target.group}`.toLocaleLowerCase().includes(query))continue;
      const group=target.group || '其他';
      if(!groups.has(group))groups.set(group,[]);
      groups.get(group).push(target);
    }
    for(const [group,targets] of groups) {
      const section=el('section','monitor-section'), header=el('div','monitor-section-heading'), grid=el('div','monitor-group-grid');
      header.append(el('h2','',group),el('span','quiet-label',`${targets.length} 个系统`));section.append(header,grid);cards.append(section);
      for(const target of targets) {
      const item=snapshot.targets[target.id] || {}, history=Array.isArray(item.history)?item.history:[], last=history[history.length-1], outcome=last?.outcome;
      const card=el('article','monitor-card'), top=el('div','monitor-card-top'), title=el('div');
      title.append(el('h3','',target.name));
      const label=!state.monitorKnown?'状态未知':!snapshot.active?'监控已停止':item.paused?'已暂停':!last?'等待首次检查':outcome?.success?'最近检查成功':'最近检查失败';
      top.append(title,el('span','status-pill',label));card.append(top,el('p','monitor-address',`${target.host}:${target.port} · ${target.tls?'TCP + TLS':'仅 TCP'} · ${target.interval_secs} 秒`));
      const evidence=el('p','monitor-evidence',last?`最近检查 ${date(last.checked_at_ms)} · TCP ${outcome?.tcp_ok===true?'成功':outcome?.tcp_ok===false?'失败':'未知'} · TLS ${outcome?.tls_ok===true?'成功':outcome?.tls_ok===false?'失败':'未检查'} · ${ms(outcome?.elapsed_ms)} · 路径 ${outcome?.path ?? '未知'}`:'尚无真实观测，不能判断在线。');card.append(evidence);
      const details=el('details','monitor-card-details');details.dataset.target=target.id;details.open=expanded.has(target.id);details.append(el('summary','','检查详情'));
      if(last?.outcome?.detail)details.append(el('p','field-help',last.outcome.detail));
      details.append(el('p','field-help',`连续失败 ${item.consecutive_failures ?? 0} 次${item.history_dropped ? ` · 历史丢弃 ${item.history_dropped}` : ''}${item.last_error ? ` · 最近异常 ${date(item.last_error.checked_at_ms)}` : ''}`));
      const actions=el('div','toolbar-actions');
      const pause=el('button','button secondary small',item.paused?'恢复':'暂停');pause.type='button';pause.disabled=!owned || !snapshot.active || !state.monitorKnown || busy;pause.addEventListener('click',()=>void monitorWrite('/api/monitor/pause',{generation:state.monitorOwner,target_id:target.id,paused:!item.paused}));
      const remove=el('button','button secondary small','删除');remove.type='button';remove.disabled=!canEdit;remove.addEventListener('click',()=>void monitorWrite('/api/monitor/config',{expected_revision:snapshot.config.revision,targets:snapshot.config.targets.filter(t=>t.id!==target.id)}));
      const diagnostic=el('button','text-button','单站排障 ↗');diagnostic.type='button';diagnostic.disabled=!canEdit;diagnostic.addEventListener('click',()=>fillDiagnosticDraft({target:target.host,port:target.port,tls:String(target.tls),allow_lan:target.allow_lan},`监控目标 / ${target.name}`));actions.append(pause,diagnostic);details.append(remove);card.append(actions,details);grid.append(card);
      for(const [action,node] of [['pause',pause],['remove',remove],['diagnostic',diagnostic]]) {node.dataset.monitorAction=action;node.dataset.monitorTarget=target.id;if(action===focused&&target.id===focusTarget&&!node.disabled)node.focus({preventScroll:true});}
      }
    }
    if(!groups.size) cards.append(el('p','empty-inline',!state.monitorKnown?'等待可用的监控状态。':(snapshot?.config.targets || []).length?'没有匹配的系统，请调整搜索条件。':'还没有系统。点击右上角“添加系统”，保存后再开始监控。'));
  }
  async function monitorStop() { if(!monitorOwned() || !state.monitorKnown || !state.monitor?.active)return; await monitorWrite('/api/monitor/stop',{generation:state.monitorOwner}); }
  $('monitor-form').addEventListener('submit',async event=>{event.preventDefault();if(locked() || !state.monitorKnown)return;clearError('monitor-error');
    const host=$('monitor-host').value.trim(), port=Number($('monitor-port').value), interval=Number($('monitor-interval').value);
    if(!host || /[\s/*?#@]/.test(host) || host.includes('://') || !Number.isInteger(port) || port<1 || port>65535 || !Number.isInteger(interval) || interval<60){showError('monitor-error','请输入确切主机、有效端口和不少于 60 秒的间隔。');return;}
    const target={id:uid('monitor'),name:$('monitor-name').value.trim(),group:$('monitor-group').value.trim() || '其他',host,port,tls:$('monitor-tls').checked,allow_lan:$('monitor-lan').checked,interval_secs:interval};
    if(!target.name){showError('monitor-error','请输入名称。');return;}
    const result=await monitorWrite('/api/monitor/config',{expected_revision:state.monitor.config.revision,targets:[...state.monitor.config.targets,target]});if(result){$('monitor-form').reset();$('monitor-interval').value='60';closeDialog('monitor-dialog');}
  });
  $('monitor-start').addEventListener('click',async()=>{if(locked() || !state.monitorKnown || !state.ready || !state.stateKnown)return;clearError('monitor-error');if(!globalThis.crypto?.randomUUID){showError('monitor-error','无法生成安全请求 ID，禁止启动。');return;}let proxy;try{proxy=selectedProxy('monitor');}catch(error){showError('monitor-error',readableError(error));return;}const result=await monitorWrite('/api/monitor/start',{request_id:crypto.randomUUID(),revision:state.monitor.config.revision,proxy});if(result?.active && result.generation){state.monitorOwner=result.generation;renderMonitor();updateControls();}});
  $('monitor-stop').addEventListener('click',()=>void monitorStop());
  $('monitor-open').addEventListener('click',()=>{if(locked()||!state.monitorKnown)return;clearError('monitor-error');openDialog('monitor-dialog');});
  $('monitor-search').addEventListener('input',renderMonitor);
  const pageLabels = {monitor: "系统监控", batch: "批量测试", load: "连接负载测试", console: "高级时长测试", lists: "站点与连接资料", diagnostics: "单站故障排查", authorized: "承载授权", history: "历史记录", settings: "设置", guide: "使用说明"};
  function resolvePage(hash) {
    const desired = String(hash || "").replace(/^#/, "");
    return Object.hasOwn(pageLabels, desired) ? desired : "monitor";
  }
  function navigate() {
    const labels = pageLabels;
    const page = resolvePage(location.hash);
    const changed=state.page!==page;state.page = page; state.settings.page = page;
    if(changed)queueSettingsSave();
    const physicalPage=['batch','load'].includes(page)?'console':page;
    for (const name of Object.keys(labels).filter(name=>!['batch','load'].includes(name))) $(`page-${name}`).hidden = physicalPage !== name;
    $('page-console').dataset.view=page==='batch'?'batch':'run';
    if(physicalPage==='console') {
      text('console-title',labels[page]);
      if(!locked() && ((page==='batch'&&state.mode!=='probe') || (page==='load'&&changed&&state.mode==='probe'))) {
        const mode=page==='batch'?'probe':'tcp';
        switchMode(mode);document.querySelectorAll('input[name="mode"]').forEach(input=>input.checked=input.value===mode);
      }
    }
    document.querySelectorAll("[data-nav]").forEach((link) => {
      if (link.dataset.nav === page) link.setAttribute("aria-current", "page"); else link.removeAttribute("aria-current");
    });
    text("breadcrumb", labels[page]);
    document.title = `${labels[page]} · AoNet 网络测试工作台`;
    if (page === "history" && state.ready) void loadHistory();
  }
  for(const prefix of ["monitor","run","batch","diagnostic"]){
    for(const id of [prefix+"-route",prefix+"-proxy"]) $(id).addEventListener("input",()=>{updateRouteControls();updateControls();});
  }
  $("diagnostic-form").addEventListener("submit",startDiagnostic);
  $("diagnostic-form").addEventListener("input",event=>{
    if (["diagnostic-target","diagnostic-port","diagnostic-tls","diagnostic-lan"].includes(event.target.id) && state.diagnosticSource) {
      state.diagnosticSource=null; text("diagnostic-source","已手动修改，请核对地址和连接方式");
    }
    updateDiagnosticControls();
  });
  $("diagnostic-search").addEventListener("input",renderDiagnosticChoices);
  $("diagnostic-options").addEventListener("change",()=>{
    const value=$("diagnostic-options").value;
    if(!/^\d+$/.test(value))return;
    const choice=state.diagnosticChoices[Number(value)];
    if(choice?.draft)fillDiagnosticDraft(choice.draft,`${choice.group} / ${choice.label}`);
  });
  $("diagnostic-stop").addEventListener("click",()=>void stopDiagnostic());
  $("diagnostic-suggest-tls").addEventListener("click",()=>{if(locked())return;$("diagnostic-tls").value="true";updateDiagnosticControls();});
  $("diagnostic-selftest").addEventListener("change",()=>{const value=$("diagnostic-selftest").value;if(!/^\d+$/.test(value))return;const target=state.bootstrap?.diagnostics?.self_test_targets?.[Number(value)];if(target)fillDiagnosticDraft({...target,tls:"false"},`${target.name} · 本机自检，不代表外网`);});
  $("diagnostic-export").addEventListener("click",event=>{if(!state.diagnostic || state.nativeDialog){event.preventDefault();return;}if(transport.isDesktop){event.preventDefault();void nativeExport("diagnostic",state.diagnostic.id);}});
  $("edit-auth-json").addEventListener("click",()=>{if(locked())return;$("auth-json-content").value=JSON.stringify({version:1,targets:persistentPermits()},null,2);clearError("auth-json-error");openDialog("auth-json-dialog");});
  $("auth-json-form").addEventListener("submit",async event=>{event.preventDefault();if(locked())return;clearError("auth-json-error");try{const value=JSON.parse($("auth-json-content").value);const targets=Array.isArray(value)?value:value.targets;if(!Array.isArray(targets))throw new Error("需要 targets 数组或授权数组。");await preparePermits(targets);closeDialog("auth-json-dialog");}catch(error){showError("auth-json-error",readableError(error));}});
  $("batch-form").addEventListener("submit", startBatch);
  $("batch-stop").addEventListener("click", () => void stopBatch());
  $("batch-export").addEventListener("click", event=>{if(!state.batch||!batchOwned()||state.nativeDialog){event.preventDefault();return;}if(transport.isDesktop){event.preventDefault();void nativeExport("batch",state.batch.request_id);}});
  $("global-stop").addEventListener("click", () => { if(activeBatch()) void stopBatch(); else if(monitorActive()) void monitorStop(); else if(activeDiagnostic()) void stopDiagnostic(); else void stopRun(); });
  $("new-auth").addEventListener("click",()=>editAuthorization());
  $("reload-auth").addEventListener("click",()=>void refreshAuthorization());
  $("auth-form").addEventListener("submit",prepareAuthorization);
  $("auth-confirm").addEventListener("click",()=>void confirmAuthorization());
  $("workspace-dialog-form").addEventListener("submit",workspaceSubmit);
  $("save-profile").addEventListener("click",()=>void saveProfile());
  $("load-profile").addEventListener("click",()=>applyProfile(state.profiles.find(p=>p.id===$("profile-select").value)));
  $("load-profile-console").addEventListener("click",()=>applyProfile(state.profiles.find(p=>p.id===$("profile-select-console").value)));
  $("delete-profile").addEventListener("click",()=>{const profile=profileSelected();if(!profile||locked())return;state.workspaceAction={type:"profile-delete",id:profile.id};text("workspace-dialog-title","删除配置预设");text("workspace-dialog-message",`删除 ${profile.name}？不影响运行记录。`);$("workspace-dialog-label").hidden=true;openDialog("workspace-dialog");});
  $("clear-history").addEventListener("click",()=>{if(locked())return;state.workspaceAction={type:"history-clear"};text("workspace-dialog-title","清空历史记录");text("workspace-dialog-message","删除所有已保存运行记录？此操作不可撤销，不会停止或删除当前实时测试。");$("workspace-dialog-label").hidden=true;openDialog("workspace-dialog");});
  $("save-settings").addEventListener("click",()=>void saveSettings());
  $("dock-width").addEventListener("input",()=>{state.settings.dock_width=Number($("dock-width").value);document.documentElement.style.setProperty("--dock-width",`${state.settings.dock_width}px`);text("dock-width-value",`${state.settings.dock_width} px`);renderCharts();queueSettingsSave();});
  for(const id of ["console-sort","result-sort"])$(id).addEventListener("change",()=>{setResultSort($(id).value);});
  $("result-search").addEventListener("input",()=>{state.resultSearch=$("result-search").value;renderResults();});
  $("self-test-enabled").addEventListener("change",async()=>{const enabled=$("self-test-enabled").checked;try{const bootstrap=await workspace("selftest",{enabled});if(bootstrap?.authorized)applyBootstrap(bootstrap);else await initialize();}catch(error){notify(readableError(error));}finally{updateControls();}});
  document.querySelectorAll("[data-close]").forEach((button) => button.addEventListener("click", () => {
    if ((state.busy || state.saving) && button.dataset.close !== "connection-dialog") return;
    if(button.dataset.close==="auth-preview-dialog")state.authDraft=null;
    closeDialog(button.dataset.close);
  }));
  document.querySelectorAll("dialog").forEach((dialog) => {
    dialog.addEventListener("cancel", (event) => { if ((state.busy || state.saving) && dialog.id !== "connection-dialog") event.preventDefault(); });
    dialog.addEventListener("close", () => {
      if (dialog.id === "confirm-dialog") state.pendingRun = null;
      if (dialog.id === "auth-preview-dialog") state.authDraft = null;
      const previous = state.restoreFocus.get(dialog);
      if (previous?.isConnected && !previous.disabled) previous.focus({ preventScroll: true });
    });
  });
  document.querySelectorAll('input[name="mode"]').forEach((input) => input.addEventListener("change", () => {
    if (!input.checked || locked()) return;
    switchMode(input.value);
  }));
  $("run-form").addEventListener("submit", reviewRun);
  $("run-form").addEventListener("input", (event) => { if (event.target.name !== "mode") updateConstraints(false); });
  $("confirm-start").addEventListener("click", startRun);
  $("authorize-check").addEventListener("change", updateControls);
  $("stop-run").addEventListener("click", stopRun);
  $("refresh").addEventListener("click", () => {
    if (state.busy || state.saving || state.stopping || state.diagnosticBusy || state.diagnosticStopping || state.nativeDialog) { notify("操作尚未结束，请等待结果后再重新连接。"); return; }
    closeDialog("confirm-dialog");
    initialize();
  });
  $("dismiss-banner").addEventListener("click", () => { $("banner").hidden = true; });
  $("new-list").addEventListener("click", () => editList());
  $("list-form").addEventListener("submit", saveList);
  $("inventory-confirm-save").addEventListener("click", saveInventoryAction);
  $("import-lists").addEventListener("click", importLists);
  $("import-file").addEventListener("change", importFile);
  $("export-lists").addEventListener("click", exportLists);
  $("series-details").addEventListener("toggle", () => { if ($("series-details").open) renderSeriesTable(); });
  for (const [id, kind] of [["export-json", "json"], ["export-csv", "csv"]]) $(id).addEventListener("click", (event) => {
    if (transport.isDesktop) {
      event.preventDefault();
      if (state.run && !state.nativeDialog) void nativeExport(kind);
    } else if (!state.run) event.preventDefault();
  });
  window.addEventListener("hashchange", navigate);
  let resizeTimer;
  window.addEventListener("resize", () => {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(renderCharts, 150);
  });
  window.addEventListener("pagehide", () => { state.refreshId += 1; stopBatchPolling(); state.batchKnown=false; state.diagnosticPaused=true; state.monitorEpoch++; clearTimeout(state.monitorTimer); state.monitorOwner=null; disconnectEvents(); stopDiagnosticPolling(); state.diagnosticKnown=false; renderDiagnostic(); });
  window.addEventListener("pageshow", (event) => { if (event.persisted) initialize(); });
  setInterval(() => { if (state.bootstrap && state.cooldownUntil > 0) updateControls(); }, 1000);
  initializeResultViews();
  navigate();
  renderCharts();
  initialize();
})();
