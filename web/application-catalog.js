"use strict";
(() => {
  const $ = id => document.getElementById(id);
  const categories = {web:"网页服务",gds:"GDS / 行业系统",communication:"通信应用",network:"网络服务",custom:"自定义"};
  const transports = ["unspecified","tcp","udp"];
  const protocols = ["unspecified","http","https","tcp","tls","udp_dns","quic","other"];
  const kinds = ["unknown","host","url","wildcard"];
  const toolNames = {ping:"Ping",traceroute:"Traceroute",dns:"系统 DNS",tcp:"TCP",tls:"TLS",http:"HTTP",udp_dns:"UDP DNS",quic:"QUIC"};
  const native = Boolean(window.__TAURI__?.core?.invoke);
  const state = {snapshot:null,selected:"",loading:false,saving:false,unknown:false,locked:true,editor:null,deletion:null,focus:null,expanded:new Set(),read:null};
  const subscribers = new Set();
  const ui = {};
  const clone = value => JSON.parse(JSON.stringify(value));
  const strings = value => Array.isArray(value) && value.every(x => typeof x === "string");
  const nullable = value => value === null || typeof value === "string";
  const pick = (value, keys) => Object.fromEntries(keys.map(key => [key,clone(value[key])]));
  const siteFields = site => pick(site,["id","name","address","port","transport","protocol","purpose","alpn"]);
  const ruleFields = rule => ({...pick(rule,["id","kind","address","transport","protocol","ports","purpose","evidence","notes"]),source_ids:[]});
  const appFields = app => ({...pick(app,["id","name","category","provider","product","aliases","access_type","site_ids"]),reference_rules:app.reference_rules.map(ruleFields)});
  const groupFields = group => pick(group,["id","name","description","application_ids"]);
  function custom(snapshot = state.snapshot) {
    return {sites:snapshot.sites.filter(s => !s.builtin).map(siteFields),applications:snapshot.applications.filter(a => !a.builtin).map(appFields),groups:snapshot.groups.filter(g => !g.builtin).map(groupFields)};
  }
  function valid(snapshot) {
    const base = x => x && typeof x.id === "string" && typeof x.name === "string" && typeof x.builtin === "boolean";
    const rule = r => r && typeof r.id === "string" && kinds.includes(r.kind) && nullable(r.address) && (r.transport === null || transports.includes(r.transport)) && (r.protocol === null || protocols.includes(r.protocol)) && ["ports","purpose","evidence","notes"].every(k => typeof r[k] === "string") && strings(r.source_ids);
    if (!snapshot || snapshot.version !== 2 || !Number.isSafeInteger(snapshot.revision) || snapshot.revision < 0 || !Array.isArray(snapshot.sites) || !Array.isArray(snapshot.applications) || !Array.isArray(snapshot.groups)) return false;
    if (!snapshot.sites.every(s => base(s) && typeof s.address === "string" && (s.port === null || Number.isInteger(s.port) && s.port > 0 && s.port <= 65535) && transports.includes(s.transport) && protocols.includes(s.protocol) && typeof s.purpose === "string" && nullable(s.alpn) && typeof s.source === "string" && typeof s.checked_at === "string" && strings(s.capabilities) && strings(s.unavailable_reasons))) return false;
    if (!snapshot.applications.every(a => base(a) && Object.hasOwn(categories,a.category) && ["provider","product","access_type","summary","status"].every(k => typeof a[k] === "string") && strings(a.aliases) && strings(a.site_ids) && Array.isArray(a.reference_rules) && a.reference_rules.every(rule) && Array.isArray(a.sources) && a.sources.every(s => s && typeof s === "object") && strings(a.missing) && strings(a.limitations) && nullable(a.environment) && nullable(a.version))) return false;
    if (!snapshot.groups.every(g => base(g) && typeof g.description === "string" && strings(g.application_ids))) return false;
    const sites = new Set(snapshot.sites.map(s => s.id)), apps = new Set(snapshot.applications.map(a => a.id));
    const ids = [...snapshot.sites,...snapshot.applications,...snapshot.groups].map(x => x.id);
    return new Set(ids).size === ids.length && snapshot.applications.every(a => a.site_ids.every(id => sites.has(id))) && snapshot.groups.every(g => g.application_ids.every(id => apps.has(id)));
  }
  function sameCustom(a,b) {
    function canonical(value) {
      if (Array.isArray(value)) return value.map(canonical).sort((x,y) => JSON.stringify(x).localeCompare(JSON.stringify(y)));
      if (value && typeof value === "object") return Object.fromEntries(Object.keys(value).sort().map(k => [k,canonical(value[k])]));
      return value;
    }
    return JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));
  }
  function blocked() { return !state.snapshot || state.loading || state.saving || state.unknown || state.locked; }
  function errorText(error) { return String(error?.message || error); }
  function message(node,text) { node.textContent = text; node.hidden = !text; }
  function el(tag,text,className) { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node; }
  function button(text,action,disabled = false,className = "") {
    const node = el("button",text,className); node.type = "button"; node.disabled = disabled; node.addEventListener("click",action); return node;
  }
  function plus(label,action) { const node = button("+",action,true,"ac-plus"); node.title = label; node.setAttribute("aria-label",label); return node; }
  function mark(node,id) { node.id = id; return node; }
  function status(text) { return el("p",text,"ac-meta"); }
  function catalogSnapshot() { return state.snapshot ? clone(state.snapshot) : null; }
  function publish() { for (const notify of subscribers) { try { notify(catalogSnapshot()); } catch (e) { console.error("应用目录订阅失败",e); } } }
  async function transport(body) {
    if (native) return window.__TAURI__.core.invoke(body ? "save_site_catalog" : "site_catalog",body ? {request:body} : undefined);
    const controller = new AbortController(), timeout = setTimeout(() => controller.abort(),8000);
    try {
      let token;
      if (body) {
        const info = await fetch("/api/network-tools/info",{signal:controller.signal,credentials:"same-origin",cache:"no-store"});
        if (!info.ok) throw new Error("无法获取会话令牌。");
        token = (await info.json()).token;
        if (typeof token !== "string" || !token) throw new Error("会话令牌格式无效。");
      }
      const response = await fetch("/api/site-catalog",{method:body ? "PUT" : "GET",headers:body ? {"Content-Type":"application/json","X-AoNet-Token":token} : {Accept:"application/json"},body:body ? JSON.stringify(body) : undefined,signal:controller.signal,credentials:"same-origin",cache:"no-store"});
      const text = await response.text(); let result;
      try { result = JSON.parse(text); } catch { throw new Error(response.ok ? "应用目录响应无法解析。" : text || `请求失败（${response.status}）`); }
      if (!response.ok) throw new Error(result.error || `请求失败（${response.status}）`);
      return result;
    } finally { clearTimeout(timeout); }
  }
  async function load() {
    if (state.read) return state.read;
    if (state.saving) return;
    state.loading = true; render();
    state.read = (async () => {
      try {
        const snapshot = await transport();
        if (!valid(snapshot)) throw new Error("应用目录 v2 格式无效，未使用旧版或不完整数据。");
        state.snapshot = snapshot; state.unknown = false;
        message(ui.error,""); message(ui.notice,"选择端点仅填入草稿，不会自动开始。参考规则不会展开为检测目标。");
      } catch (error) { message(ui.error,errorText(error)); }
      finally { state.loading = false; state.read = null; render(); publish(); }
    })();
    return state.read;
  }
  async function save(data,expectedRevision = state.snapshot?.revision) {
    if (blocked()) throw new Error("请等待检测结束并重新读取应用目录后再保存。");
    if (expectedRevision !== state.snapshot.revision) throw new Error("目录已更新，请取消并重新打开编辑，避免覆盖其他修改。");
    state.saving = true; state.unknown = true; render();
    try {
      const snapshot = await transport({schema_version:2,expected_revision:expectedRevision,...data});
      if (!valid(snapshot) || snapshot.revision !== expectedRevision + 1 || !sameCustom(custom(snapshot),data)) throw new Error("保存响应无法核对。");
      state.snapshot = snapshot; state.unknown = false;
      message(ui.error,""); message(ui.notice,"已保存。共享端点的修改会同步到所有引用它的应用。");
    } catch (error) {
      // A failed/ambiguous write is reconciled with ONE read, never replayed.
      try {
        const latest = await transport();
        if (!valid(latest)) throw new Error("重新读取的目录格式无效。");
        state.snapshot = latest; state.unknown = false;
        if (latest.revision === expectedRevision + 1 && sameCustom(custom(latest),data)) {
          message(ui.error,""); message(ui.notice,"保存结果已通过重新读取核对，未重复写入。"); return;
        }
        throw new Error(`${errorText(error)} 已重新读取最新目录；未重发保存，请取消后重新编辑。`);
      } catch (readError) {
        if (state.unknown) message(ui.error,"保存结果不确定，已锁定修改。请重新进入应用管理核对目录，不会自动重发。");
        throw readError;
      }
    } finally { state.saving = false; render(); publish(); }
  }
  function filteredApps(selection,snapshot = state.snapshot) {
    if (!snapshot) return [];
    if (selection.startsWith("category:")) return snapshot.applications.filter(a => a.category === selection.slice(9));
    if (selection.startsWith("group:")) { const group = snapshot.groups.find(g => g.id === selection.slice(6)); return group ? snapshot.applications.filter(a => group.application_ids.includes(a.id)) : []; }
    return snapshot.applications;
  }
  function options(select,rows,placeholder) {
    if (!select) return;
    const previous = select.value; select.replaceChildren();
    const first = el("option",placeholder); first.value = ""; select.append(first);
    for (const row of rows) { const option = el("option",row.name); option.value = row.id; select.append(option); }
    select.value = rows.some(row => row.id === previous) ? previous : "";
  }
  function renderQuick() {
    const snapshot = state.snapshot, group = $("quick-group"), app = $("quick-app"), site = $("quick-site"), applyButton = $("apply-site");
    if (!group || !app || !site || !applyButton) return;
    options(group,snapshot ? [...Object.entries(categories).map(([id,name]) => ({id:`category:${id}`,name})),...snapshot.groups.map(g => ({id:`group:${g.id}`,name:`组合 · ${g.name}`}))] : [],"全部应用");
    options(app,filteredApps(group.value).map(a => ({id:a.id,name:a.name})),"请选择应用");
    const selectedApp = snapshot?.applications.find(a => a.id === app.value);
    options(site,snapshot && selectedApp ? snapshot.sites.filter(s => selectedApp.site_ids.includes(s.id)).map(s => ({id:s.id,name:`${s.name} · ${s.address}${s.port === null ? "" : ` :${s.port}`}`})) : [],"请选择具体端点");
    // Browsing remains available while detection runs; applying the draft does not.
    for (const node of [group,app,site]) node.disabled = !snapshot || state.loading || state.saving;
    applyButton.disabled = blocked() || !selectedQuickSite()?.capabilities.length;
  }
  function selectedQuickSite() { return state.snapshot?.sites.find(s => s.id === $("quick-site")?.value); }
  function applySite(site,tool) {
    if (blocked() || !site.capabilities.length || tool && !site.capabilities.includes(tool)) return;
    try {
      const draft = window.networkToolDraft;
      if (!draft || tool && typeof draft.applyEndpoint !== "function" || !tool && typeof draft.applySite !== "function") throw new Error("检测草稿接口尚未就绪。");
      const applied = tool ? draft.applyEndpoint(clone(site),tool) : draft.applySite(clone(site));
      if (applied && window.appShell) window.appShell.show("tools");
    } catch (error) { message(ui.error,errorText(error)); }
  }
  function selectedGroup() { return state.selected.startsWith("group:") ? state.snapshot?.groups.find(g => g.id === state.selected.slice(6)) : null; }
  function choose(selection) {
    if (state.loading || state.saving) return;
    if (state.selected !== selection) ui.search.value = "";
    state.selected = selection; render();
  }
  function renderNav() {
    const focused = ui.nav.contains(document.activeElement) ? document.activeElement?.dataset.selection : undefined;
    ui.nav.replaceChildren(); const nodes = [];
    function add(id,name,count) {
      const node = button("",() => choose(id),state.loading || state.saving,"ac-nav-item");
      node.dataset.selection = id; node.setAttribute("aria-current",state.selected === id ? "true" : "false");
      node.append(el("span",name),el("span",String(count),"ac-total")); ui.nav.append(node); nodes.push(node);
    }
    add("","全部应用",state.snapshot.applications.length);
    ui.nav.append(el("p","应用分类","ac-section-label"));
    for (const [id,name] of Object.entries(categories)) add(`category:${id}`,name,filteredApps(`category:${id}`).length);
    for (const [builtin,title] of [[true,"内置组合"],[false,"我的组合"]]) {
      ui.nav.append(el("p",title,"ac-section-label"));
      const groups = state.snapshot.groups.filter(g => g.builtin === builtin);
      for (const g of groups) add(`group:${g.id}`,g.name,g.application_ids.length);
      if (!groups.length) ui.nav.append(status("暂无组合"));
    }
    add("endpoints","共享端点库",state.snapshot.sites.length);
    if (focused !== undefined) (nodes.find(n => n.dataset.selection === focused) || nodes[0]).focus();
  }
  function render() {
    window.appShell?.setLocked("catalog-mutation",state.saving || state.unknown);
    const disabled = blocked();
    ui.newApp.disabled = disabled; ui.newGroup.disabled = disabled; ui.newSite.disabled = disabled;
    ui.search.disabled = state.loading || state.saving;
    ui.fields.disabled = disabled; ui.save.disabled = disabled; ui.cancel.disabled = state.saving;
    ui.deleteConfirm.disabled = disabled; ui.deleteCancel.disabled = state.saving;
    if (!state.snapshot) { ui.count.textContent = state.loading ? "正在读取应用目录…" : "目录尚未读取。重新进入本页可重试。"; renderQuick(); return; }
    if (state.selected.startsWith("group:") && !selectedGroup()) state.selected = "";
    renderNav(); const group = selectedGroup(), endpointView = state.selected === "endpoints";
    ui.heading.textContent = endpointView ? "共享端点库" : group ? group.name : state.selected.startsWith("category:") ? categories[state.selected.slice(9)] : "全部应用";
    ui.description.textContent = endpointView ? "统一维护具体端点。删除应用不会删除这里的共享端点。" : group ? group.description : "应用引用具体端点；参考规则仅供查阅，不会自动展开、补端口或推断协议。";
    ui.newApp.hidden = endpointView; ui.newSite.hidden = !endpointView;
    ui.actions.replaceChildren();
    if (group && !group.builtin) ui.actions.append(button("编辑组合",() => openEditor("group",group),disabled),button("删除组合",() => openDelete("group",group),disabled));
    const query = ui.search.value.trim().toLocaleLowerCase();
    const rows = endpointView ? state.snapshot.sites : filteredApps(state.selected);
    const results = rows.filter(item => {
      const sites = endpointView ? [item] : state.snapshot.sites.filter(s => item.site_ids.includes(s.id));
      return !query || [item.name,item.provider,item.product,...item.aliases || [],...sites.flatMap(s => [s.name,s.address]),...item.reference_rules?.flatMap(r => [r.address,r.purpose]) || []].join(" ").toLocaleLowerCase().includes(query);
    });
    ui.count.textContent = `${results.length} 个${endpointView ? "端点" : "应用"}${state.locked ? " · 检测期间可浏览，暂不可修改或填入草稿" : ""}${state.unknown ? " · 保存结果待核对" : ""}`;
    ui.list.replaceChildren();
    for (const item of results) ui.list.append(endpointView ? endpointRow(item) : applicationRow(item));
    if (!results.length) ui.list.append(el("li",query ? `没有匹配「${ui.search.value.trim()}」的结果。切换分类或组合时搜索会清空。` : "暂无内容。使用 + 新增自定义应用或端点。","ac-empty"));
    renderQuick();
  }
  function endpointRow(site,app = null) {
    const row = el("li",undefined,"ac-endpoint"), head = el("div",undefined,"ac-endpoint-heading"), info = el("div");
    info.append(el("strong",site.name),el("p",site.address,"ac-address"),status(`${site.transport} / ${site.protocol} · ${site.port === null ? "未指定端口" : `端口 ${site.port}`}${site.alpn ? ` · ALPN ${site.alpn}` : ""}`));
    if (site.purpose) info.append(status(site.purpose));
    info.append(status(`${site.builtin ? "内置只读" : "自定义共享端点"} · 能力：${site.capabilities.map(c => toolNames[c] || c).join("、") || "无"}`));
    if (site.source || site.checked_at) info.append(status(`来源：${site.source || "未提供"}${site.checked_at ? ` · 核实 ${site.checked_at}` : ""}`));
    for (const reason of site.unavailable_reasons) info.append(status(reason));
    const actions = el("div",undefined,"ac-actions");
    actions.append(button("填入草稿",() => applySite(site),blocked() || !site.capabilities.length));
    const tool = el("select"); tool.setAttribute("aria-label",`${site.name} 的检测工具`);
    options(tool,site.capabilities.map(id => ({id,name:toolNames[id] || id})),"选择能力"); tool.disabled = blocked() || !site.capabilities.length;
    const applyTool = button("用于所选工具",() => applySite(site,tool.value),true);
    tool.addEventListener("change",() => { applyTool.disabled = blocked() || !tool.value; });
    actions.append(tool,applyTool);
    actions.append(button("复制端点",() => openEditor("site",site,app && !app.builtin ? app.id : null,true),blocked()));
    if (!site.builtin && (!app || !app.builtin)) actions.append(button("编辑端点",() => openEditor("site",site,app?.id),blocked()),button("删除端点",() => openDelete("site",site),blocked()));
    head.append(info,actions); row.append(head); return row;
  }
  function applicationRow(app) {
    const row = el("li",undefined,"ac-application"), details = el("details"), summary = el("summary"), title = el("span",undefined,"ac-app-title");
    details.open = state.expanded.has(app.id); details.addEventListener("toggle",() => { if (details.open) state.expanded.add(app.id); else state.expanded.delete(app.id); });
    title.append(el("strong",app.name),el("span",`${app.builtin ? "内置" : "自定义"} · ${categories[app.category]}`,"ac-badge"));
    summary.append(title,el("span",`${app.site_ids.length} 端点 / ${app.reference_rules.length} 规则`,"ac-total")); details.append(summary);
    const body = el("div",undefined,"ac-app-body");
    body.append(status([app.provider,app.product,app.access_type,...app.aliases].filter(Boolean).join(" · ")));
    if (app.summary) body.append(el("p",app.summary,"ac-description"));
    const metadata = [app.status,app.environment && `环境：${app.environment}`,app.version && `版本：${app.version}`].filter(Boolean);
    if (metadata.length) body.append(status(metadata.join(" · ")));
    for (const [name,values] of [["缺失信息",app.missing],["限制",app.limitations]]) if (values.length) body.append(el("p",`${name}：${values.join("；")}`,"ac-warning"));
    if (app.sources.length) {
      const sources = el("details",undefined,"ac-sources"); sources.append(el("summary","来源与核实信息"));
      for (const source of app.sources) sources.append(status(Object.entries(source).map(([key,value]) => `${key}：${typeof value === "string" ? value : JSON.stringify(value)}`).join(" · ")));
      body.append(sources);
    }
    if (!app.builtin) {
      const actions = el("div",undefined,"ac-actions ac-maintenance");
      const addEndpoint=plus("新增端点",() => openEditor("site",null,app.id)),addRule=plus("新增参考规则",() => openEditor("rule",null,app.id));
      addEndpoint.disabled=blocked();addRule.disabled=blocked();
      actions.append(button("编辑应用",() => openEditor("application",app),blocked()),button("删除应用",() => openDelete("application",app),blocked()),addEndpoint,addRule); body.append(actions);
    }
    body.append(el("h3","具体端点","ac-subheading"));
    const endpoints = el("ul",undefined,"ac-endpoints");
    for (const id of app.site_ids) { const site = state.snapshot.sites.find(s => s.id === id); if (site) endpoints.append(endpointRow(site,app)); }
    if (!app.site_ids.length) endpoints.append(el("li","暂无具体端点；参考规则不作为检测目标。","ac-empty"));
    body.append(endpoints,el("h3","参考规则 · 不参与检测","ac-subheading"));
    const rules = el("ul",undefined,"ac-rules");
    for (const rule of app.reference_rules) {
      const entry = el("li",undefined,"ac-rule");
      entry.append(el("strong",rule.address || "地址未知"),status(`${rule.kind} · ${rule.transport ?? "传输未知"} / ${rule.protocol ?? "协议未知"} · ${rule.ports || "端口未提供"}`),el("p",rule.purpose,"ac-description"));
      if (rule.evidence) entry.append(status(`证据：${rule.evidence}`));
      if (rule.notes) entry.append(status(`备注：${rule.notes}`));
      if (rule.source_ids.length) entry.append(status(`来源标识：${rule.source_ids.join("、")}`));
      if (!app.builtin) { const actions = el("div",undefined,"ac-actions"); actions.append(button("编辑规则",() => openEditor("rule",rule,app.id),blocked()),button("删除规则",() => openDelete("rule",rule,app.id),blocked())); entry.append(actions); }
      rules.append(entry);
    }
    if (!app.reference_rules.length) rules.append(el("li","暂无参考规则。","ac-empty"));
    body.append(rules); details.append(body); row.append(details); return row;
  }
  function field(name,label,value = "",type = "text",limit = 200) {
    const wrapper = el("label",undefined,"ac-field"), caption = el("span",label), input = el(type === "textarea" ? "textarea" : "input");
    input.name = name; if (type !== "textarea") input.type = type; input.value = value ?? ""; input.maxLength = limit;
    wrapper.append(caption,input); ui.fields.append(wrapper); state.editor.fields[name] = input; return input;
  }
  function selectField(name,label,choices,value,empty = null) {
    const wrapper = el("label",undefined,"ac-field"), input = el("select"); input.name = name;
    if (empty !== null) { const option = el("option",empty); option.value = ""; input.append(option); }
    for (const choice of choices) { const id = typeof choice === "string" ? choice : choice.id, option = el("option",typeof choice === "string" ? choice : choice.name); option.value = id; input.append(option); }
    input.value = value ?? ""; wrapper.append(el("span",label),input); ui.fields.append(wrapper); state.editor.fields[name] = input; return input;
  }
  function memberFields(name,title,rows,selected) {
    const fieldset = el("fieldset",undefined,"ac-members"); fieldset.append(el("legend",title)); state.editor.fields[name] = [];
    for (const row of rows) {
      const label = el("label"), input = el("input"); input.type = "checkbox"; input.value = row.id; input.checked = selected.includes(row.id);
      state.editor.fields[name].push(input); label.append(input,el("span",`${row.name}${row.builtin ? " · 内置" : ""}${row.address ? ` · ${row.address}` : ""}`)); fieldset.append(label);
    }
    if (!rows.length) fieldset.append(status("暂无可选项。保存后可在应用内新增端点。")); ui.fields.append(fieldset);
  }
  function openEditor(kind,item = null,parentId = null,copy = false) {
    if (copy && kind !== "site") return;
    if (blocked() || item?.builtin && !copy) return;
    const parent = state.snapshot.applications.find(a => a.id === parentId);
    if (parentId && (!parent || parent.builtin)) return;
    state.focus = document.activeElement; state.editor = {kind,id:copy ? null : item?.id || null,parentId,revision:state.snapshot.revision,fields:{}};
    ui.fields.replaceChildren(); message(ui.editorError,"");
    ui.editorTitle.textContent = `${copy ? "复制" : item ? "编辑" : "新增"}${{application:"应用",site:"具体端点",group:"组合",rule:"参考规则"}[kind]}`;
    ui.editorHint.textContent = kind === "site" ? "共享端点修改影响所有引用应用。端口、传输和协议必须由你明确提供；留空/未知不会推断。" : kind === "group" ? "组合只保存应用引用；删除组合不删除应用或共享端点。" : kind === "rule" ? "参考规则仅作说明，不自动探测；端口可填写范围/动态端口描述。自定义规则无内置来源引用。" : "选择现有共享端点，不会复制端点。保存应用后可独立新增端点和参考规则。";
    if (copy) ui.editorHint.textContent = "复制为独立自定义端点，修改副本不影响原端点或其他应用。若从自定义应用内复制，副本会作为额外引用加入此应用。";
    if (kind !== "rule") field("name","名称",copy ? `${item.name}（副本）` : item?.name).required = true;
    if (kind === "application") {
      selectField("category","分类",Object.entries(categories).map(([id,name]) => ({id,name})),item?.category || (state.selected.startsWith("category:") ? state.selected.slice(9) : "custom"));
      field("provider","厂商",item?.provider); field("product","产品",item?.product); field("aliases","别名（每行一个）",item?.aliases.join("\n"),"textarea",4000); field("access_type","接入方式（可留空，不推断）",item?.access_type,"text",100);
      memberFields("site_ids","引用的共享端点（可多选）",state.snapshot.sites,item?.site_ids || []);
    } else if (kind === "site") {
      field("address","单个主机 / IP，或完整 HTTP(S) URL",item?.address,"text",2048).required = true;
      const port = field("port","端口（可留空）",item?.port,"number",5); port.min = "1"; port.max = "65535"; port.step = "1";
      selectField("transport","传输（未知请保留 unspecified）",transports,item?.transport || "unspecified");
      selectField("protocol","协议（未知请保留 unspecified）",protocols,item?.protocol || "unspecified");
      field("purpose","用途",item?.purpose,"textarea",1000); field("alpn","ALPN（可留空；不自动填写 h3）",item?.alpn,"text",255);
      const memberships = state.snapshot.applications.filter(a => a.site_ids.includes(item?.id));
      if (!copy && memberships.length) ui.fields.append(status(`此共享端点被 ${memberships.length} 个应用引用：${memberships.map(a => a.name).join("、")}`));
    } else if (kind === "group") {
      field("description","说明",item?.description,"textarea",1000); memberFields("application_ids","组合中的应用（可多选）",state.snapshot.applications,item?.application_ids || []);
    } else {
      const kindField = selectField("kind","规则类型",kinds,item?.kind || "unknown");
      const address = field("address","规则地址（unknown 时必须为空）",item?.address,"text",2048);
      kindField.addEventListener("change",() => { address.disabled = kindField.value === "unknown"; }); address.disabled = kindField.value === "unknown";
      selectField("transport","传输",transports,item?.transport,"未提供（null）"); selectField("protocol","协议",protocols,item?.protocol,"未提供（null）");
      field("ports","端口描述 / 范围（仅作说明）",item?.ports); field("purpose","用途",item?.purpose,"textarea",1000).required = true;
      field("evidence","证据",item?.evidence,"textarea",2000); field("notes","备注",item?.notes,"textarea",2000);
    }
    ui.dialog.showModal(); ui.fields.querySelector("input,select,textarea")?.focus();
  }
  function restoreFocus() { if (state.focus?.isConnected) state.focus.focus(); else ui.heading.focus(); }
  function closeEditor() { if (state.saving) return; ui.dialog.close(); state.editor = null; restoreFocus(); }
  function readEditor(editor) {
    const f = editor.fields, text = name => f[name].value.trim(), members = name => f[name].filter(n => n.checked).map(n => n.value);
    const id = editor.id || `custom-${crypto.randomUUID()}`;
    if (editor.kind === "application") return {id,name:text("name"),category:text("category"),provider:text("provider"),product:text("product"),aliases:text("aliases").split(/\r?\n/).map(x => x.trim()).filter(Boolean),access_type:text("access_type"),site_ids:members("site_ids"),reference_rules:editor.id ? state.snapshot.applications.find(a => a.id === editor.id).reference_rules.map(ruleFields) : []};
    if (editor.kind === "group") return {id,name:text("name"),description:text("description"),application_ids:members("application_ids")};
    if (editor.kind === "site") {
      const raw = text("port"); let port = null;
      if (raw) { port = Number(raw); if (!/^\d+$/.test(raw) || !Number.isInteger(port) || port < 1 || port > 65535) throw new Error("端口必须为 1～65535 的整数，或留空。"); }
      return {id,name:text("name"),address:text("address"),port,transport:text("transport"),protocol:text("protocol"),purpose:text("purpose"),alpn:text("alpn") || null};
    }
    if (text("kind") === "unknown" && text("address")) throw new Error("unknown 规则地址必须为空；如已填写地址请明确选择规则类型。");
    return {id,kind:text("kind"),address:text("address") || null,transport:text("transport") || null,protocol:text("protocol") || null,ports:text("ports"),purpose:text("purpose"),evidence:text("evidence"),notes:text("notes"),source_ids:[]};
  }
  async function submit(event) {
    event.preventDefault(); if (blocked() || !state.editor) return;
    try {
      const editor = state.editor;
      if (editor.revision !== state.snapshot.revision) throw new Error("目录已更新，请取消并重新打开编辑，避免覆盖其他修改。");
      const data = custom(), value = readEditor(editor), {kind,id,parentId} = editor;
      if (kind !== "rule" && !value.name) throw new Error("请填写名称。");
      if (kind === "site" && !value.address) throw new Error("请填写具体端点地址。");
      if (kind === "rule") {
        if (!value.purpose) throw new Error("请填写规则用途。");
        const parent = data.applications.find(a => a.id === parentId); if (!parent) throw new Error("应用不存在或为只读。");
        parent.reference_rules = id ? parent.reference_rules.map(r => r.id === id ? value : r) : [...parent.reference_rules,value];
      } else {
        const key = {application:"applications",site:"sites",group:"groups"}[kind];
        data[key] = id ? data[key].map(x => x.id === id ? value : x) : [...data[key],value];
        if (kind === "site" && !id && parentId) {
          const parent = data.applications.find(a => a.id === parentId); if (!parent) throw new Error("应用不存在或为只读。"); parent.site_ids.push(value.id);
        }
      }
      await save(data,editor.revision); closeEditor();
    } catch (error) { message(ui.editorError,errorText(error)); }
  }
  function deleteData(snapshot,kind,id,parentId) {
    const data = custom(snapshot);
    if (kind === "application") {
      const app = snapshot.applications.find(a => a.id === id); if (!app || app.builtin) throw new Error("内置应用只读。");
      if (snapshot.groups.some(g => g.builtin && g.application_ids.includes(id))) throw new Error("内置组合引用此应用，不能删除。");
      data.applications = data.applications.filter(a => a.id !== id); data.groups.forEach(g => { g.application_ids = g.application_ids.filter(x => x !== id); });
    } else if (kind === "site") {
      const site = snapshot.sites.find(s => s.id === id); if (!site || site.builtin) throw new Error("内置端点只读。");
      if (snapshot.applications.some(a => a.builtin && a.site_ids.includes(id))) throw new Error("内置应用引用此端点，不能删除。");
      data.sites = data.sites.filter(s => s.id !== id); data.applications.forEach(a => { a.site_ids = a.site_ids.filter(x => x !== id); });
    } else if (kind === "group") {
      const group = snapshot.groups.find(g => g.id === id); if (!group || group.builtin) throw new Error("内置组合只读。"); data.groups = data.groups.filter(g => g.id !== id);
    } else if (kind === "rule") {
      const parent = data.applications.find(a => a.id === parentId); if (!parent) throw new Error("内置应用规则只读。"); parent.reference_rules = parent.reference_rules.filter(r => r.id !== id);
    } else throw new Error("未知删除类型。");
    return data;
  }
  function openDelete(kind,item,parentId = null) {
    if (blocked() || item.builtin) return;
    state.focus = document.activeElement; state.deletion = {kind,id:item.id,parentId,revision:state.snapshot.revision};
    let impact;
    if (kind === "application") {
      const groups = state.snapshot.groups.filter(g => g.application_ids.includes(item.id));
      impact = `删除应用「${item.name}」及其规则，并从 ${groups.length} 个组合移除引用。不会删除任何共享端点，也不影响其他应用。`;
    } else if (kind === "site") {
      const apps = state.snapshot.applications.filter(a => a.site_ids.includes(item.id));
      impact = `永久删除共享端点「${item.name}」，同时从 ${apps.length} 个应用移除引用${apps.length ? `（${apps.map(a => a.name).join("、")}）` : ""}。不会删除应用或参考规则。`;
    } else impact = kind === "group" ? `删除组合「${item.name}」？仅删除组合与应用引用，不删除任何应用或共享端点。` : `删除参考规则「${item.address || item.purpose}」？不影响具体端点。`;
    ui.deleteImpact.textContent = `${impact} 此操作不可撤销。`; message(ui.deleteError,""); ui.deleteDialog.showModal(); ui.deleteCancel.focus();
  }
  function closeDelete() { if (state.saving) return; ui.deleteDialog.close(); state.deletion = null; restoreFocus(); }
  async function remove() {
    if (blocked() || !state.deletion) return;
    try { const {kind,id,parentId,revision} = state.deletion; if (revision !== state.snapshot.revision) throw new Error("目录已更新，请取消并重新打开删除确认。"); await save(deleteData(state.snapshot,kind,id,parentId),revision); closeDelete(); }
    catch (error) { message(ui.deleteError,errorText(error)); }
  }
  function mount() {
    const page = $("catalog-page"); if (!page) return false;
    const title = $("catalog-title") || mark(el("h1","应用管理"),"catalog-title"); title.textContent = "应用管理"; title.tabIndex = -1;
    page.classList.add("application-catalog");
    ui.error = mark(el("p",undefined,"error"),"ac-error"); ui.error.setAttribute("role","alert"); ui.error.hidden = true;
    ui.notice = mark(status("正在读取目录…"),"ac-notice"); ui.notice.setAttribute("role","status");
    const layout = el("div",undefined,"ac-layout"), sidebar = el("aside",undefined,"ac-sidebar"), navHeading = el("div",undefined,"ac-heading"); sidebar.setAttribute("aria-label","应用分类与组合");
    ui.newGroup = mark(plus("新增组合",() => openEditor("group")),"ac-new-group"); navHeading.append(el("strong","分类与组合"),ui.newGroup);
    ui.nav = el("nav",undefined,"ac-nav"); ui.nav.setAttribute("aria-label","选择应用分类或组合"); sidebar.append(navHeading,ui.nav);
    const content = el("div",undefined,"ac-content"), heading = el("div",undefined,"ac-heading"), headingText = el("div");
    ui.heading = el("h2","全部应用"); ui.heading.tabIndex = -1; ui.description = status(""); headingText.append(ui.heading,ui.description);
    ui.newApp = mark(plus("新增应用",() => openEditor("application")),"ac-new-application"); ui.newSite = mark(plus("新增端点",() => openEditor("site")),"ac-new-site"); ui.newSite.hidden = true;
    const pluses = el("div",undefined,"ac-actions"); pluses.append(ui.newApp,ui.newSite); heading.append(headingText,pluses);
    ui.actions = el("div",undefined,"ac-actions ac-maintenance");
    const searchLabel = el("label",undefined,"ac-search"); ui.search = mark(el("input"),"ac-search"); ui.search.type = "search"; ui.search.maxLength = 200; ui.search.placeholder = "应用、别名或端点地址"; searchLabel.append(el("span","搜索应用 / 端点"),ui.search);
    ui.count = status(""); ui.count.setAttribute("role","status"); ui.list = el("ul",undefined,"ac-list");
    content.append(heading,ui.actions,searchLabel,ui.count,ui.list); layout.append(sidebar,content);
    ui.dialog = mark(el("dialog",undefined,"ac-dialog"),"ac-editor-dialog"); ui.editorTitle = mark(el("h2"),"ac-editor-title"); ui.dialog.setAttribute("aria-labelledby",ui.editorTitle.id);
    ui.editorHint = status(""); ui.editorError = el("p",undefined,"error"); ui.editorError.setAttribute("role","alert"); ui.editorError.hidden = true;
    const form = el("form"); ui.fields = el("fieldset",undefined,"ac-fields"); ui.fields.setAttribute("aria-label","编辑字段");
    const editorActions = el("div",undefined,"ac-dialog-actions"); ui.cancel = button("取消",closeEditor); ui.save = el("button","保存","primary"); ui.save.type = "submit"; editorActions.append(ui.cancel,ui.save);
    form.append(ui.editorTitle,ui.editorHint,ui.editorError,ui.fields,editorActions); ui.dialog.append(form);
    ui.deleteDialog = mark(el("dialog",undefined,"ac-dialog"),"ac-delete-dialog"); const deleteTitle = mark(el("h2","确认删除"),"ac-delete-title"); ui.deleteDialog.setAttribute("aria-labelledby",deleteTitle.id);
    ui.deleteImpact = el("p",undefined,"ac-description"); ui.deleteError = el("p",undefined,"error"); ui.deleteError.setAttribute("role","alert"); ui.deleteError.hidden = true;
    const deleteActions = el("div",undefined,"ac-dialog-actions"); ui.deleteCancel = button("取消",closeDelete); ui.deleteConfirm = button("确认删除",() => void remove()); deleteActions.append(ui.deleteCancel,ui.deleteConfirm);
    ui.deleteDialog.append(deleteTitle,ui.deleteImpact,ui.deleteError,deleteActions);
    page.replaceChildren(title,el("p","应用组织端点，组合组织应用。内置资料只读，自定义内容由你维护。","intro"),ui.error,ui.notice,layout,status("内置资料不代表端点可用性或业务完整性已验证。规则、保存与草稿填入均不会发起网络检测。"),ui.dialog,ui.deleteDialog);
    ui.search.addEventListener("input",render); form.addEventListener("submit",event => void submit(event));
    ui.dialog.addEventListener("cancel",event => { event.preventDefault(); closeEditor(); }); ui.deleteDialog.addEventListener("cancel",event => { event.preventDefault(); closeDelete(); });
    return true;
  }
  if (!mount()) return;
  window.siteCatalog = {snapshot:catalogSnapshot,subscribe(notify) { subscribers.add(notify); notify(catalogSnapshot()); return () => subscribers.delete(notify); },reload:load,setLocked(value) { value = Boolean(value); if (state.locked !== value) { state.locked = value; render(); } }};
  window.appShell?.subscribe(source => { if (source !== "catalog-mutation") window.siteCatalog.setLocked(window.appShell.isLockedExcept("catalog-mutation")); });
  window.addEventListener("aonet-page",event => { if (event.detail === "catalog") void load(); });
  $("quick-group")?.addEventListener("change",() => { if ($("quick-app")) $("quick-app").value = ""; if ($("quick-site")) $("quick-site").value = ""; renderQuick(); });
  $("quick-app")?.addEventListener("change",() => { if ($("quick-site")) $("quick-site").value = ""; renderQuick(); });
  $("quick-site")?.addEventListener("change",renderQuick);
  $("apply-site")?.addEventListener("click",() => { const site = selectedQuickSite(); if (site) applySite(site); });
  state.locked = window.appShell ? window.appShell.isLockedExcept("catalog-mutation") : window.networkToolDraft?.isLocked() ?? true;
  void load();
})();
