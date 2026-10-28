"use strict";
(() => {
  const C=window.catalogController;
  if (!C) { console.error("catalog-controller.js must load before application-catalog.js"); return; }
  const $=id=>document.getElementById(id);
  const categories={web:"HTTP(S)",communication:"即时通讯",gds:"GDS",network:"网络服务",custom:"其他"};
  const valueLabels={unspecified:"未指定",tcp:"TCP",udp:"UDP",http:"HTTP",https:"HTTPS",tls:"TLS",udp_dns:"UDP DNS",quic:"QUIC",other:"其他协议",unknown:"地址待补",host:"主机 / IP",url:"完整网址",wildcard:"通配规则"};
  const transports=["unspecified","tcp","udp"],protocols=["unspecified","http","https","tcp","tls","udp_dns","quic","other"],kinds=["unknown","host","url","wildcard"];
  const toolNames={ping:"Ping",traceroute:"Traceroute",dns:"系统 DNS",tcp:"TCP",tls:"TLS",http:"HTTP",udp_dns:"UDP DNS",quic:"QUIC"};
  const {clone,custom,ruleFields,blocked,save}=C;
  const state=C.state,views=[];
  const quick=Object.fromEntries(["group","app","site"].map(key=>[key,{value:"",query:""}]));
  let choiceId=0,quickKind="website";
  const errorText=error=>String(error?.message||error);
  function message(node,text){node.textContent=text;node.hidden=!text;}
  function el(tag,text,className){const node=document.createElement(tag);if(text!==undefined)node.textContent=text;if(className)node.className=className;return node;}
  function button(text,action,disabled=false,className=""){const node=el("button",text,className);node.type="button";node.disabled=disabled;node.addEventListener("click",action);return node;}
  function plus(label,action){const node=button("+",action,true,"ac-plus");node.title=label;node.setAttribute("aria-label",label);return node;}
  function mark(node,id){node.id=id;return node;}
  function status(text){return el("p",text,"ac-meta");}
  function filteredApps(selection,snapshot=state.snapshot,kind=null){
    if(!snapshot)return [];
    let apps=snapshot.applications.filter(a=>!kind||a.kind===kind);
    if(selection==="untagged")return apps.filter(a=>!a.tag_ids.length);
    if(selection.startsWith("tag:"))return apps.filter(a=>a.tag_ids.includes(selection.slice(4)));
    if(selection.startsWith("group:")){const group=snapshot.groups.find(g=>g.id===selection.slice(6));return group?apps.filter(a=>group.application_ids.includes(a.id)):[];}
    return apps;
  }
  function personalParent(data,id){
    const app=data.applications.find(a=>a.id===id);if(app)return app;
    if(!state.snapshot.applications.some(a=>a.id===id&&a.builtin))throw new Error("资源不存在。");
    let supplement=data.resource_supplements.find(s=>s.resource_id===id);
    if(!supplement){supplement={resource_id:id,site_ids:[],reference_rules:[]};data.resource_supplements.push(supplement);}
    return supplement;
  }
  function setTags(data,id,tagIds){data.tag_assignments=data.tag_assignments.filter(a=>a.resource_id!==id);if(tagIds.length)data.tag_assignments.push({resource_id:id,tag_ids:tagIds});}
  function websiteError(app,sites){
    if(app.kind!=="website")return;
    for(const id of app.site_ids){const site=sites.find(s=>s.id===id);let url;try{url=new URL(site?.address);}catch{}
      if(!site||!url||!["http:","https:"].includes(url.protocol)||!url.hostname||url.username||url.password||site.transport!=="tcp"||site.protocol!==url.protocol.slice(0,-1))throw new Error(`网站连接「${site?.name||id}」须为完整 HTTP(S) URL、对应 HTTP/HTTPS 协议和 TCP 传输。请明确修正或解除该连接，不会自动删除或猜测。`);
    }
  }
  function validateWebsites(data){
    const sites=[...state.snapshot.sites.filter(s=>s.builtin),...data.sites];
    for(const app of data.applications)websiteError(app,sites);
    for(const app of state.snapshot.applications.filter(a=>a.builtin&&a.kind==="website")){
      const previous=app.supplement?.site_ids||[];const next=data.resource_supplements.find(s=>s.resource_id===app.id)?.site_ids||[];
      websiteError({...app,site_ids:[...app.site_ids.filter(id=>!previous.includes(id)),...next]},sites);
    }
  }
  function radioChoices(title,rows,value,onChange) {
    const node = el("fieldset",undefined,"ac-choices"), inputs = [], name = `ac-choice-${++choiceId}`;
    node.append(el("legend",title));
    for (const row of rows) {
      const label = el("label",undefined,"tool-choice"), input = el("input");
      input.type = "radio"; input.name = name; input.value = row.id; input.checked = row.id === value;
      input.addEventListener("change",() => {
        if (!input.checked) return;
        control.setValue(input.value); onChange?.(input.value);
      });
      label.append(input,el("span",row.name)); node.append(label); inputs.push(input);
    }
    const control = {node,inputs,getValue:() => inputs.find(input => input.checked)?.value ?? "",setValue(value) { for (const input of inputs) input.checked = input.value === value; }};
    return control;
  }
  function clearQuick(key) { quick[key].value = ""; quick[key].query = ""; if (quick[key].search) quick[key].search.value = ""; }
  function quickControl(key,title) {
    const model = quick[key], container = $(`quick-${key}`);
    if (!container) return null;
    if (model.container === container) return model;
    container.classList.add("ac-quick");
    const searchLabel = el("label",undefined,"ac-quick-search"), search = el("input"), summary = el("p",undefined,"ac-choice-summary");
    search.type = "search"; search.maxLength = 200; search.value = model.query; search.placeholder = `搜索${title}`;
    searchLabel.append(el("span",title),search); summary.setAttribute("role","status");
    const clear = button("清除选择",() => { setQuick(key,""); }), body = el("div",undefined,"ac-quick-list");
    search.addEventListener("input",() => { model.query = search.value; filterQuick(model); });
    container.replaceChildren(searchLabel,summary,clear,body);
    if (key === "group") {
      const range = radioChoices("资源范围",[{id:"website",name:"网站"},{id:"application",name:"应用"}],quickKind,value=>{quickKind=value;for(const key of ["group","app","site"])clearQuick(key);renderQuick();});
      container.replaceChildren(range.node,searchLabel,summary,clear,body);model.range=range;
    }
    Object.assign(model,{container,search,summary,clear,body,signature:null,rows:[]}); return model;
  }
  function filterQuick(model) {
    const query = model.query.trim().toLocaleLowerCase(); let count = 0;
    for (const {row,label} of model.rows) {
      label.hidden = Boolean(query && ![row.name,row.search || ""].join(" ").toLocaleLowerCase().includes(query));
      if (!label.hidden) count++;
    }
    model.empty.hidden = count > 0;
    model.summary.textContent = `当前：${model.rows.find(entry => entry.row.id === model.value)?.row.name || "未选择"}`;
    model.clear.disabled = model.disabled || !model.value;
  }
  function setQuick(key,value) {
    if (quick[key].disabled || quick[key].value === value) return;
    quick[key].value = value;
    if (key === "group") { clearQuick("app"); clearQuick("site"); }
    if (key === "app") clearQuick("site");
    renderQuick();
  }
  function updateQuick(key,title,rows) {
    const model = quickControl(key,title); if (!model) return;
    if (model.value && !rows.some(row => row.id === model.value)) {
      clearQuick(key);
      if (key === "group") { clearQuick("app"); clearQuick("site"); }
      if (key === "app") clearQuick("site");
    }
    const signature = JSON.stringify([title,rows]);
    if (model.signature !== signature) {
      const choices = radioChoices(title,rows,model.value,value => setQuick(key,value));
      choices.node.classList.add("ac-direct-choices"); model.choices = choices;
      model.rows = rows.map((row,index) => ({row,label:choices.inputs[index].parentElement}));
      model.empty = el("p","没有匹配项。","ac-choice-empty");
      model.body.replaceChildren(choices.node,model.empty); model.signature = signature;
    }
    model.disabled = !state.snapshot || state.loading || state.saving;
    model.search.disabled = model.disabled; model.choices.node.disabled = model.disabled;if(model.range)model.range.node.disabled=model.disabled;
    model.choices.setValue(model.value); filterQuick(model);
  }
  function renderQuick() {
    const snapshot = state.snapshot, applyButton = $("apply-site");
    if (!$("quick-group") || !$("quick-app") || !$("quick-site") || !applyButton) return;
    updateQuick("group","标签与分组",snapshot ? [{id:"all",name:"全部"},{id:"untagged",name:"未打标签"},...snapshot.tags.map(t=>({id:`tag:${t.id}`,name:`标签 · ${t.name}`})),...snapshot.groups.map(g => ({id:`group:${g.id}`,name:`分组 · ${g.name}`}))] : []);
    updateQuick("app",quickKind === "website" ? "网站" : "应用",filteredApps(quick.group.value,state.snapshot,quickKind).map(a => ({id:a.id,name:a.name,search:[a.provider,a.product,...a.aliases].join(" ")})));
    const app = snapshot?.applications.find(a => a.id === quick.app.value);
    updateQuick("site","连接",snapshot && app ? snapshot.sites.filter(s => app.site_ids.includes(s.id)).map(s => ({id:s.id,name:`${s.name} · ${s.address}${s.port === null ? "" : ` :${s.port}`}`})) : []);
    // Browsing remains available while detection runs; applying the draft does not.
    applyButton.disabled = blocked() || !selectedQuickSite()?.capabilities.length;
  }
  function selectedQuickSite() {
    const app = filteredApps(quick.group.value,state.snapshot,quickKind).find(a => a.id === quick.app.value);
    return app?.site_ids.includes(quick.site.value) ? state.snapshot?.sites.find(s => s.id === quick.site.value) : undefined;
  }
  function applySite(site,tool) {
    if (blocked() || !site.capabilities.length || tool && !site.capabilities.includes(tool)) return;
    try {
      const draft = window.networkToolDraft;
      if (!draft || tool && typeof draft.applyEndpoint !== "function" || !tool && typeof draft.applySite !== "function") throw new Error("检测草稿接口尚未就绪。");
      const applied = tool ? draft.applyEndpoint(clone(site),tool) : draft.applySite(clone(site));
      if (applied && window.appShell) window.appShell.show("tools");
    } catch (error) { C.state.error=errorText(error); for(const view of views)view.render(); }
  }
  function createView(pageId,titleId,resourceKind,prefix){
    const noun=resourceKind==="website"?"网站":"应用",connectionNoun=resourceKind==="website"?"页面与接口":"功能连接";
    const own={selected:"",editor:null,deletion:null,focus:null,detailId:null,returnFocus:null};
    const state=new Proxy(own,{get(target,key){return key in target?target[key]:C.state[key];},set(target,key,value){if(key in target)target[key]=value;else C.state[key]=value;return true;}});
    const ui={},toolSelections=new Map();
    const id=value=>`${prefix}${value}`;
    const localApps=selection=>filteredApps(selection,state.snapshot,resourceKind);
  function selectedGroup() { return state.selected.startsWith("group:") ? state.snapshot?.groups.find(g => g.id === state.selected.slice(6)) : null; }
  function choose(selection) {
    if (state.loading || state.saving) return;
    if (state.selected !== selection) ui.search.value = "";
    state.detailId = null; state.selected = selection; render();
  }
  function openDetails(id) {
    state.returnFocus = id; state.detailId = id; render(); ui.heading.focus();
  }
  function backToList() {
    state.detailId = null; render();
    const target = [...ui.list.querySelectorAll("button[data-site-id]")].find(n => n.dataset.siteId === state.returnFocus);
    (target || ui.heading).focus();
  }
  function renderNav() {
    const focused = ui.nav.contains(document.activeElement) ? document.activeElement?.dataset.selection : undefined;
    ui.nav.replaceChildren(); const nodes = [];
    function add(id,name,count) {
      const node = button("",() => choose(id),state.loading || state.saving,"ac-nav-item");
      node.dataset.selection = id; node.setAttribute("aria-current",state.selected === id ? "true" : "false");
      node.append(el("span",name),el("span",String(count),"ac-total")); ui.nav.append(node); nodes.push(node);
    }
    add("",`全部${noun}`,localApps("").length);
    add("untagged","未打标签",localApps("untagged").length);
    ui.nav.append(el("p","标签","ac-section-label"));
    for(const tag of state.snapshot.tags)add(`tag:${tag.id}`,`${tag.name}${tag.builtin?" · 预设":""}`,localApps(`tag:${tag.id}`).length);
    ui.nav.append(el("p","分组","ac-section-label"));
    for (const g of state.snapshot.groups) add(`group:${g.id}`,`${g.name}${g.builtin ? " · 预设" : ""}`,localApps(`group:${g.id}`).length);
    if (!state.snapshot.groups.length) ui.nav.append(status("暂无分组"));
    if (focused !== undefined) (nodes.find(n => n.dataset.selection === focused) || nodes[0]).focus();
  }
  function render() {
    message(ui.error,C.state.error); message(ui.notice,C.state.notice);
    const disabled = blocked();
    ui.newApp.disabled = disabled; ui.newGroup.disabled = disabled; ui.newTag.disabled=disabled;ui.newSite.disabled = disabled;
    ui.search.disabled = state.loading || state.saving;
    ui.fields.disabled = disabled; ui.save.disabled = disabled; ui.cancel.disabled = state.saving;
    ui.deleteConfirm.disabled = disabled; ui.deleteCancel.disabled = state.saving;
    if (!state.snapshot) { ui.count.textContent = state.loading ? `正在读取${noun}…` : `${noun}尚未读取，重新进入本页可重试。`; renderQuick(); return; }
    if (state.selected.startsWith("group:") && !selectedGroup() || state.selected.startsWith("tag:") && !state.snapshot.tags.some(t=>t.id===state.selected.slice(4))) state.selected = "";
    const app = state.snapshot.applications.find(a => a.id === state.detailId && a.kind===resourceKind);
    if (!app) state.detailId = null;
    renderNav(); const group = selectedGroup();
    ui.back.hidden = !app; ui.searchLabel.hidden = Boolean(app); ui.newApp.hidden = Boolean(app);
    ui.newSite.hidden = !app;
    ui.heading.textContent = app ? app.name : group ? group.name : state.selected.startsWith("tag:") ? state.snapshot.tags.find(t=>t.id===state.selected.slice(4))?.name || "标签" : state.selected==="untagged"?"未打标签":`全部${noun}`;
    ui.description.textContent = app ? `${categories[app.category]} · ${app.builtin ? "预设资料 + 我的配置" : `自定义${noun}`} · ${app.site_ids.length} 条${connectionNoun}` : group ? group.description : `${resourceKind==="website"?"维护首页、子页面与 HTTP(S) 接口。":"维护软件与服务的功能连接，支持多种协议。"}标签与分组跨网站和应用共享。`;
    ui.actions.replaceChildren();
    if(app){
      ui.actions.append(button("管理标签",()=>openEditor("assignment",app),disabled));
      if(!app.builtin)ui.actions.append(button(`编辑${noun}`,()=>openEditor("application",app),disabled),button(`删除${noun}`,()=>openDelete("application",app),disabled));
      else ui.actions.append(button("我的连接与规则",()=>openEditor("supplement",app),disabled));
    }
    else if (!app && group && !group.builtin) ui.actions.append(button("编辑分组",() => openEditor("group",group),disabled),button("删除分组",() => openDelete("group",group),disabled));
    if(!app&&state.selected.startsWith("tag:")){const tag=state.snapshot.tags.find(t=>t.id===state.selected.slice(4));if(tag&&!tag.builtin)ui.actions.append(button("编辑标签",()=>openEditor("tag",tag),disabled),button("删除标签",()=>openDelete("tag",tag),disabled));}
    ui.list.replaceChildren();
    if (app) {
      ui.count.textContent = state.locked ? "检测期间可浏览，暂不可维护或填入草稿。" : "配置不代表连通；选择工具填入草稿后仍需手动开始。";
      ui.list.append(applicationDetail(app));
    } else {
      const query = ui.search.value.trim().toLocaleLowerCase();
      const results = localApps(state.selected).filter(item => {
        const sites = state.snapshot.sites.filter(s => item.site_ids.includes(s.id));
        return !query || [item.name,item.provider,item.product,...item.aliases,...sites.flatMap(s => [s.name,s.address])].join(" ").toLocaleLowerCase().includes(query);
      });
      ui.count.textContent = `${results.length} 个${noun}${state.unknown ? " · 保存结果待核对" : ""}`;
      const heading = el("li",undefined,"ac-table-heading"); heading.setAttribute("aria-hidden","true");
      heading.append(el("span","名称"),el("span","资料分类"),el("span","配置"),el("span","")); ui.list.append(heading);
      for (const item of results) ui.list.append(applicationRow(item));
      if (!results.length) ui.list.append(el("li",query ? `没有匹配「${ui.search.value.trim()}」的${noun}。` : `暂无${noun}，使用 + 新增。`,"ac-empty"));
    }
    renderQuick();
  }
  function endpointRow(site,app) {
    const row = el("li",undefined,"ac-endpoint"), info = el("div",undefined,"ac-connection-info");
    info.append(el("strong",site.name),el("p",site.address,"ac-address"),status(`${valueLabels[site.protocol]} · ${valueLabels[site.transport]}${site.port === null ? " · 端口未指定" : ` · 端口 ${site.port}`}${site.alpn ? ` · ALPN ${site.alpn}` : ""}`));
    if (site.purpose) info.append(status(site.purpose));
    if (!site.capabilities.some(c => !["ping","traceroute","dns"].includes(c))) info.append(status("目前仅支持主机层检查；连接协议或参数仍待补充。"));
    for (const reason of site.unavailable_reasons) info.append(status(reason));
    row.append(info);
    const selected = site.capabilities.includes(toolSelections.get(site.id)) ? toolSelections.get(site.id) : "";
    if (!selected) toolSelections.delete(site.id);
    const apply = button("填入草稿",() => {const tool=toolSelections.get(site.id);if(tool)applySite(site,tool);},blocked() || !selected);
    const choices = radioChoices("检测工具",site.capabilities.map(id => ({id,name:toolNames[id] || id})),selected,value => {toolSelections.set(site.id,value);apply.disabled = blocked();});
    choices.node.disabled = blocked();
    const detection = el("div",undefined,"ac-detection"); detection.append(choices.node,apply); row.append(detection);
    const maintenance = el("div",undefined,"ac-actions ac-connection-maintenance");
    maintenance.append(button("复制连接",() => openEditor("site",site,app.id,true),blocked()));
    if (!site.builtin && (!app.builtin || app.supplement?.site_ids.includes(site.id))) maintenance.append(button("编辑",() => openEditor("site",site,app.id),blocked()),button("永久删除",() => openDelete("site",site),blocked()));
    row.append(maintenance); return row;
  }
  function applicationRow(app) {
    const row = el("li",undefined,"ac-site-row"), name = button(app.name,() => openDetails(app.id),false,"ac-site-name");
    name.dataset.siteId = app.id;
    const identity=el("div",undefined,"ac-site-name-wrap"),chips=el("div",undefined,"ac-chips");
    for(const id of app.tag_ids){const tag=state.snapshot.tags.find(t=>t.id===id);if(tag)chips.append(el("span",tag.name,"ac-chip"));}
    identity.append(name,chips);
    row.append(identity,el("span",categories[app.category],"ac-site-type"),el("span",app.site_ids.length ? `${app.site_ids.length} 条连接` : "待补配置","ac-site-state"),button("配置",() => openDetails(app.id),false,"ac-configure"));
    return row;
  }
  function applicationDetail(app) {
    const body = el("li",undefined,"ac-site-detail");
    const chips=el("div",undefined,"ac-chips");for(const id of app.tag_ids){const tag=state.snapshot.tags.find(t=>t.id===id);if(tag)chips.append(el("span",tag.name,"ac-chip"));}body.append(chips);
    body.append(el("h3",app.builtin?`预设资料 · ${connectionNoun}`:connectionNoun));
    if (!app.site_ids.length) body.append(el("p","尚未配置具体连接；资料规则不作为检测目标。","ac-warning"));
    const endpoints = el("ul",undefined,"ac-endpoints");
    for (const id of app.site_ids) { const site = state.snapshot.sites.find(s => s.id === id); if (site && (!app.builtin || !app.supplement?.site_ids.includes(id))) endpoints.append(endpointRow(site,app)); }
    body.append(endpoints);
    let personalSection=null;
    if(app.builtin){const personal=el("section",undefined,"ac-personal");personal.append(el("h3","我的配置"),status("个人标签、连接与规则不会修改预设资料或官方来源。"));const personalSites=el("ul",undefined,"ac-endpoints");for(const id of app.supplement?.site_ids||[]){const site=state.snapshot.sites.find(s=>s.id===id);if(site)personalSites.append(endpointRow(site,app));}personal.append(personalSites);body.append(personal);personalSection=personal;}
    const reference = el("details",undefined,"ac-reference"), summary = el("summary","配置说明与参考资料"); reference.append(summary);
    if (app.summary) reference.append(el("p",app.summary,"ac-description"));
    for (const [title,values] of [["待补信息",app.missing],["限制",app.limitations]]) if (values.length) reference.append(status(`${title}：${values.join("；")}`));
    const identity = [app.provider,app.product,...app.aliases,app.environment,app.version].filter(Boolean).join(" · ");
    if (identity) reference.append(status(identity));
    for (const source of app.sources) reference.append(status(`${source.title || "来源"}：${source.url || ""}${source.checked_at ? ` · 核实 ${source.checked_at}` : ""}${source.scope ? ` · ${source.scope}` : ""}`));
    { const addRule = plus("新增参考规则",() => openEditor("rule",null,app.id)); addRule.disabled=blocked();(personalSection||reference).append(addRule); }
    for (const rule of app.reference_rules) {
      const entry = el("div",undefined,"ac-rule");
      const personal=!app.builtin||app.supplement?.reference_rules.some(r=>r.id===rule.id);
      entry.append(status(personal?"我的规则":"预设规则 · 只读"));
      entry.append(el("strong",rule.address || "地址待补"),status(`${valueLabels[rule.kind]} · ${valueLabels[rule.protocol] || "协议未提供"} · ${rule.ports || "端口未提供"}`),el("p",rule.purpose,"ac-description"));
      if (rule.evidence) entry.append(status(`依据：${rule.evidence}`));
      if (rule.notes) entry.append(status(rule.notes));
      if (personal) { const actions = el("div",undefined,"ac-actions");actions.append(button("编辑规则",() => openEditor("rule",rule,app.id),blocked()),button("删除规则",() => openDelete("rule",rule,app.id),blocked()));entry.append(actions); }
      (personal&&personalSection?personalSection:reference).append(entry);
    }
    reference.append(status("参考规则不参与检测，不会展开通配地址、猜测协议或端口。"));
    body.append(reference); return body;
  }
  function field(name,label,value = "",type = "text",limit = 200) {
    const wrapper = el("label",undefined,"ac-field"), caption = el("span",label), input = el(type === "textarea" ? "textarea" : "input");
    input.name = name; if (type !== "textarea") input.type = type; input.value = value ?? ""; input.maxLength = limit;
    wrapper.append(caption,input); ui.fields.append(wrapper); state.editor.fields[name] = input; return input;
  }
  function selectField(name,label,choices,value,empty = null) {
    const rows=choices.map(c=>typeof c === "string"?{id:c,name:valueLabels[c] || c}:c);
    if(empty!==null) rows.unshift({id:"",name:"未提供"});
    const control=radioChoices(label,rows,value ?? "",()=>control.node.dispatchEvent(new Event("change")));
    ui.fields.append(control.node); state.editor.fields[name]=control;
    return control;
  }
  function memberFields(name,title,rows,selected,collapsible = false) {
    const host=collapsible?el("details",undefined,"ac-reuse"):el("div");
    if(collapsible)host.append(el("summary",title));
    const fieldset=el("fieldset",undefined,"ac-members");fieldset.append(el("legend",collapsible?"选择已有连接（可多选）":title));
    const search=el("input");search.type="search";search.placeholder="搜索名称或地址";search.setAttribute("aria-label",title);fieldset.append(search);
    const entries=[]; state.editor.fields[name]=[];
    for(const row of rows){
      const label=el("label"),input=el("input");input.type="checkbox";input.value=row.id;input.checked=selected.includes(row.id);
      state.editor.fields[name].push(input);label.append(input,el("span",`${row.name}${row.builtin?" · 预设":""}${row.address?` · ${row.address}`:""}`));fieldset.append(label);entries.push({row,label});
    }
    search.addEventListener("input",()=>{const query=search.value.trim().toLocaleLowerCase();for(const {row,label} of entries)label.hidden=Boolean(query&&!`${row.name} ${row.address || ""}`.toLocaleLowerCase().includes(query));});
    if(!rows.length)fieldset.append(status("暂无可复用项，保存资源后可新增连接。"));
    host.append(fieldset);ui.fields.append(host);
  }
  function openEditor(kind,item = null,parentId = null,copy = false) {
    if (copy && kind !== "site") return;
    if (blocked() || item?.builtin && !copy && !["assignment","supplement"].includes(kind)) return;
    const parent = state.snapshot.applications.find(a => a.id === parentId);
    if (parentId && !parent) return;
    state.focus = document.activeElement; state.editor = {kind,id:copy ? null : item?.id || null,parentId,revision:state.snapshot.revision,fields:{}};
    ui.fields.replaceChildren(); message(ui.editorError,"");
    ui.editorTitle.textContent = `${copy ? "复制" : item ? "编辑" : "新增"}${{application:noun,site:"连接",group:"分组",rule:"参考规则",tag:"标签",assignment:"个人标签",supplement:"我的连接与规则"}[kind]}`;
    ui.editorHint.textContent = kind === "site" ? `共享连接修改影响所有引用它的应用与网站。${parent?.kind==="website"?"网站仅允许完整 HTTP(S) URL、对应 HTTP/HTTPS 协议与 TCP 传输。":"端口、传输和协议必须由你明确提供；留空/未知不会推断。"}` : kind === "group" ? "分组关联标签，匹配任一标签（OR）即加入。空标签组匹配零资源；不直接维护成员。删除分组不删除资源或标签。" : kind === "rule" ? "参考规则仅作说明，不自动探测；端口可填写范围/动态端口描述。自定义规则无内置来源引用。" : `${noun}的类型不根据协议猜测；网站只允许完整 HTTP(S) URL、对应协议和 TCP。后端会最终校验共享引用。`;
    if (copy) ui.editorHint.textContent = "复制为独立连接，修改副本不影响原连接。保存后仅为当前资源添加个人副本，原连接仍保留。";
    if (!["rule","assignment","supplement"].includes(kind)) field("name","名称",copy ? `${item.name}（副本）` : item?.name).required = true;
    if(kind==="assignment"){
      const selected=state.snapshot.tag_assignments.find(a=>a.resource_id===item.id)?.tag_ids||[];
      memberFields("tag_ids","我的标签（预设标签也可选择）",state.snapshot.tags,selected);
      const personal=new Set(selected);const base=item.tag_ids.filter(id=>!personal.has(id));if(base.length)ui.fields.append(status(`预设基础标签只读：${base.map(id=>state.snapshot.tags.find(t=>t.id===id)?.name).join("、")}`));
    }else if(kind==="supplement"){
      ui.editorHint.textContent="仅管理个人补充；预设名称、类型、官方连接、规则和来源均只读。解除引用不会自动删除孤立连接。";
      memberFields("site_ids","复用个人连接",state.snapshot.sites.filter(s=>!s.builtin),item.supplement?.site_ids||[],true);
    }else if(kind==="tag"){ui.editorHint.textContent="标签共享于网站与应用。重命名不改变稳定 ID；预设标签只读但可用于个人打标。";
    }else if (kind === "application") {
      if(item)selectField("resource_kind","资源类型",[{id:"website",name:"网站"},{id:"application",name:"应用"}],item.kind);
      selectField("category","资料分类",Object.entries(categories).map(([id,name]) => ({id,name})),item?.category || "custom");
      field("provider","厂商",item?.provider); field("product","产品",item?.product); field("aliases","别名（每行一个）",item?.aliases.join("\n"),"textarea",4000); field("access_type","接入方式（可留空，不推断）",item?.access_type,"text",100);
      memberFields("site_ids","复用已有连接",state.snapshot.sites,item?.site_ids || [],true);
      memberFields("tag_ids","我的标签",state.snapshot.tags,state.snapshot.tag_assignments.find(a=>a.resource_id===item?.id)?.tag_ids||[]);
    } else if (kind === "site") {
      field("address","单个主机 / IP，或完整 HTTP(S) URL",item?.address,"text",2048).required = true;
      const port = field("port","端口（可留空）",item?.port,"number",5); port.min = "1"; port.max = "65535"; port.step = "1";
      selectField("transport","传输协议",transports,item?.transport || "unspecified");
      selectField("protocol","连接协议",protocols,item?.protocol || "unspecified");
      field("purpose","用途",item?.purpose,"textarea",1000); field("alpn","ALPN（可留空；不自动填写 h3）",item?.alpn,"text",255);
      const memberships = state.snapshot.applications.filter(a => a.site_ids.includes(item?.id));
      if (!copy && memberships.length) ui.fields.append(status(`此共享连接被 ${memberships.length} 个资源引用：${memberships.map(a => a.name).join("、")}`));
    } else if (kind === "group") {
      field("description","说明",item?.description,"textarea",1000); memberFields("tag_ids","关联标签（任一匹配）",state.snapshot.tags,item?.tag_ids || []);
      const preview=status("");preview.setAttribute("role","status");ui.fields.append(preview);
      const update=()=>{const tags=state.editor.fields.tag_ids.filter(n=>n.checked).map(n=>n.value);const matches=state.snapshot.applications.filter(a=>a.tag_ids.some(id=>tags.includes(id)));preview.textContent=`匹配预览：${matches.filter(a=>a.kind==="website").length} 个网站 · ${matches.filter(a=>a.kind==="application").length} 个应用${tags.length?"":" · 空分组不匹配资源"}`;};
      for(const input of state.editor.fields.tag_ids)input.addEventListener("change",update);update();
    } else {
      const kindField = selectField("kind","规则类型",kinds,item?.kind || "unknown");
      const address = field("address","规则地址（地址待补时留空）",item?.address,"text",2048);
      kindField.node.addEventListener("change",() => { address.disabled = kindField.getValue() === "unknown"; }); address.disabled = kindField.getValue() === "unknown";
      selectField("transport","传输",transports,item?.transport,"未提供"); selectField("protocol","协议",protocols,item?.protocol,"未提供");
      field("ports","端口描述 / 范围（仅作说明）",item?.ports); field("purpose","用途",item?.purpose,"textarea",1000).required = true;
      field("evidence","证据",item?.evidence,"textarea",2000); field("notes","备注",item?.notes,"textarea",2000);
    }
    ui.dialog.showModal(); ui.fields.querySelector("input,textarea")?.focus();
  }
  function restoreFocus() { if (state.focus?.isConnected) state.focus.focus(); else ui.heading.focus(); }
  function closeEditor() { if (state.saving) return; ui.dialog.close(); state.editor = null; restoreFocus(); }
  function readEditor(editor) {
    const f = editor.fields, text = name => String(f[name].getValue ? f[name].getValue() : f[name].value).trim(), members = name => f[name].filter(n => n.checked).map(n => n.value);
    const id = editor.id || `custom-${crypto.randomUUID()}`;
    if (editor.kind === "application") return {id,name:text("name"),kind:editor.id?text("resource_kind"):resourceKind,category:text("category"),provider:text("provider"),product:text("product"),aliases:text("aliases").split(/\r?\n/).map(x => x.trim()).filter(Boolean),access_type:text("access_type"),site_ids:members("site_ids"),reference_rules:editor.id ? state.snapshot.applications.find(a => a.id === editor.id).reference_rules.map(ruleFields) : []};
    if(editor.kind==="assignment")return {resource_id:editor.id,tag_ids:members("tag_ids")};
    if(editor.kind==="supplement")return {resource_id:editor.id,site_ids:members("site_ids"),reference_rules:state.snapshot.applications.find(a=>a.id===editor.id).supplement?.reference_rules.map(ruleFields)||[]};
    if(editor.kind==="tag")return {id,name:text("name")};
    if (editor.kind === "group") return {id,name:text("name"),description:text("description"),tag_ids:members("tag_ids")};
    if (editor.kind === "site") {
      const raw = text("port"); let port = null;
      if (raw) { port = Number(raw); if (!/^\d+$/.test(raw) || !Number.isInteger(port) || port < 1 || port > 65535) throw new Error("端口必须为 1～65535 的整数，或留空。"); }
      return {id,name:text("name"),address:text("address"),port,transport:text("transport"),protocol:text("protocol"),purpose:text("purpose"),alpn:text("alpn") || null};
    }
    if (text("kind") === "unknown" && text("address")) throw new Error("地址待补的规则必须留空；如已填写地址请明确选择规则类型。");
    return {id,kind:text("kind"),address:text("address") || null,transport:text("transport") || null,protocol:text("protocol") || null,ports:text("ports"),purpose:text("purpose"),evidence:text("evidence"),notes:text("notes"),source_ids:[]};
  }
  async function submit(event) {
    event.preventDefault(); if (blocked() || !state.editor) return;
    try {
      const editor = state.editor;
      if (editor.revision !== state.snapshot.revision) throw new Error("目录已更新，请取消并重新打开编辑，避免覆盖其他修改。");
      const data = custom(), value = readEditor(editor), {kind,id,parentId} = editor;
      if (!["rule","assignment","supplement"].includes(kind) && !value.name) throw new Error("请填写名称。");
      if (kind === "site" && !value.address) throw new Error("请填写具体连接地址。");
      if(kind==="assignment"){setTags(data,id,value.tag_ids);
      }else if(kind==="supplement"){data.resource_supplements=data.resource_supplements.filter(s=>s.resource_id!==id);if(value.site_ids.length||value.reference_rules.length)data.resource_supplements.push(value);
      }else if (kind === "rule") {
        if (!value.purpose) throw new Error("请填写规则用途。");
        const parent = personalParent(data,parentId);
        parent.reference_rules = id ? parent.reference_rules.map(r => r.id === id ? value : r) : [...parent.reference_rules,value];
      } else {
        const key = {application:"applications",site:"sites",group:"groups",tag:"tags"}[kind];
        data[key] = id ? data[key].map(x => x.id === id ? value : x) : [...data[key],value];
        if (kind === "site" && !id && parentId) {
          const parent = personalParent(data,parentId); parent.site_ids.push(value.id);
        }
      }
      if(kind==="application")setTags(data,value.id,editor.fields.tag_ids.filter(n=>n.checked).map(n=>n.value));
      validateWebsites(data);
      await save(data,editor.revision);
      if(kind === "application"){state.detailId=value.kind===resourceKind?value.id:null;render();}
      closeEditor();
    } catch (error) { message(ui.editorError,errorText(error)); }
  }
  function deleteData(snapshot,kind,id,parentId){
    const data=custom(snapshot);
    if(kind==="application"){
      if(!snapshot.applications.some(a=>a.id===id&&!a.builtin))throw new Error("预设资源只读。");
      data.applications=data.applications.filter(a=>a.id!==id);data.tag_assignments=data.tag_assignments.filter(a=>a.resource_id!==id);
    }else if(kind==="site"){
      if(!snapshot.sites.some(s=>s.id===id&&!s.builtin))throw new Error("预设连接只读。");
      if(snapshot.applications.some(a=>a.builtin&&a.site_ids.includes(id)&&!a.supplement?.site_ids.includes(id)))throw new Error("官方资料引用此连接，不能删除。");
      data.sites=data.sites.filter(s=>s.id!==id);for(const a of [...data.applications,...data.resource_supplements])a.site_ids=a.site_ids.filter(x=>x!==id);
    }else if(kind==="tag"){
      if(!snapshot.tags.some(t=>t.id===id&&!t.builtin))throw new Error("预设标签只读。");
      if(snapshot.groups.some(g=>g.builtin&&g.tag_ids.includes(id)))throw new Error("预设分组引用此标签，不能删除。");
      data.tags=data.tags.filter(t=>t.id!==id);for(const row of [...data.tag_assignments,...data.groups])row.tag_ids=row.tag_ids.filter(x=>x!==id);
      data.tag_assignments=data.tag_assignments.filter(a=>a.tag_ids.length);
    }else if(kind==="group"){
      if(!snapshot.groups.some(g=>g.id===id&&!g.builtin))throw new Error("预设分组只读。");data.groups=data.groups.filter(g=>g.id!==id);
    }else if(kind==="rule"){
      const parent=personalParent(data,parentId);if(!parent.reference_rules.some(r=>r.id===id))throw new Error("预设规则只读。");parent.reference_rules=parent.reference_rules.filter(r=>r.id!==id);
    }else throw new Error("未知删除类型。");
    data.resource_supplements=data.resource_supplements.filter(s=>s.site_ids.length||s.reference_rules.length);
    return data;
  }
  function openDelete(kind,item,parentId = null) {
    if (blocked() || item.builtin) return;
    state.focus = document.activeElement; state.deletion = {kind,id:item.id,parentId,revision:state.snapshot.revision};
    let impact;
    if(kind==="tag"){
      const apps=state.snapshot.applications.filter(a=>a.tag_ids.includes(item.id)),groups=state.snapshot.groups.filter(g=>g.tag_ids.includes(item.id));
      impact=`删除标签「${item.name}」将从 ${apps.length} 个资源（${apps.map(a=>`${a.name} · ${a.kind==="website"?"网站":"应用"}`).join("、")||"无"}）与 ${groups.length} 个分组（${groups.map(g=>g.name).join("、")||"无"}）移除该标签；最后一个标签删除后分组为空。不会删除资源或分组。`;
    }else if (kind === "application") {
      const groups = state.snapshot.groups.filter(g => g.application_ids.includes(item.id));
      impact = `删除${noun}「${item.name}」及其规则，并从 ${groups.length} 个分组移除引用。不会删除任何共享连接，也不影响其他应用或网站。`;
    } else if (kind === "site") {
      const apps = state.snapshot.applications.filter(a => a.site_ids.includes(item.id));
      impact = `永久删除共享连接「${item.name}」，同时从 ${apps.length} 个资源移除引用${apps.length ? `（${apps.map(a => a.name).join("、")}）` : ""}。不会删除应用、网站或参考规则。`;
    } else impact = kind === "group" ? `删除分组「${item.name}」？仅删除分组，不删除任何应用、网站、标签或共享连接。` : `删除参考规则「${item.address || item.purpose}」？不影响具体连接。`;
    ui.deleteImpact.textContent = `${impact} 此操作不可撤销。`; message(ui.deleteError,""); ui.deleteDialog.showModal(); ui.deleteCancel.focus();
  }
  function closeDelete() { if (state.saving) return; ui.deleteDialog.close(); state.deletion = null; restoreFocus(); }
  async function remove() {
    if (blocked() || !state.deletion) return;
    try { const {kind,id,parentId,revision} = state.deletion; if (revision !== state.snapshot.revision) throw new Error("目录已更新，请取消并重新打开删除确认。"); await save(deleteData(state.snapshot,kind,id,parentId),revision); closeDelete(); }
    catch (error) { message(ui.deleteError,errorText(error)); }
  }
  function mount() {
    const page = $(pageId); if (!page) return false;
    const title = $(titleId) || mark(el("h1",`${noun}管理`),titleId); title.textContent = `${noun}管理`; title.tabIndex = -1;
    page.classList.add("application-catalog");
    ui.error = mark(el("p",undefined,"error"),id("error")); ui.error.setAttribute("role","alert"); ui.error.hidden = true;
    ui.notice = mark(status("正在读取目录…"),id("notice")); ui.notice.setAttribute("role","status");
    const layout = el("div",undefined,"ac-layout"), sidebar = el("aside",undefined,"ac-sidebar"), navHeading = el("div",undefined,"ac-heading"); sidebar.setAttribute("aria-label",`${noun}标签与分组`);
    ui.newGroup = mark(plus("新增分组",() => openEditor("group")),id("new-group"));ui.newTag=mark(plus("新增标签",()=>openEditor("tag")),id("new-tag"));navHeading.append(el("strong","标签与分组"),ui.newTag,ui.newGroup);
    ui.nav = el("nav",undefined,"ac-nav"); ui.nav.setAttribute("aria-label",`选择${noun}标签或分组`); sidebar.append(navHeading,ui.nav);
    const content = el("div",undefined,"ac-content"), heading = el("div",undefined,"ac-heading"), headingText = el("div");
    ui.heading = el("h2",`全部${noun}`); ui.heading.tabIndex = -1; ui.description = status(""); headingText.append(ui.heading,ui.description);
    ui.newApp = mark(plus(`新增${noun}`,() => openEditor("application")),id("new-application")); ui.newSite = mark(plus("新增连接",() => openEditor("site",null,state.detailId)),id("new-site")); ui.newSite.hidden = true;
    const pluses = el("div",undefined,"ac-actions"); pluses.append(ui.newApp,ui.newSite); heading.append(headingText,pluses);
    ui.actions = el("div",undefined,"ac-actions ac-maintenance");
    const searchLabel = el("label",undefined,"ac-search"); ui.search = mark(el("input"),id("search")); ui.search.type = "search"; ui.search.maxLength = 200; ui.search.placeholder = "名称、别名或连接地址"; ui.searchLabel=searchLabel; searchLabel.append(el("span",`搜索${noun}`),ui.search);
    ui.count = status(""); ui.count.setAttribute("role","status"); ui.list = el("ul",undefined,"ac-list");
    ui.back=button("← 返回列表",backToList,false,"ac-back");ui.back.hidden=true;
    content.append(ui.back,heading,ui.actions,searchLabel,ui.count,ui.list); layout.append(sidebar,content);
    ui.dialog = mark(el("dialog",undefined,"ac-dialog"),id("editor-dialog")); ui.editorTitle = mark(el("h2"),id("editor-title")); ui.dialog.setAttribute("aria-labelledby",ui.editorTitle.id);
    ui.editorHint = status(""); ui.editorError = el("p",undefined,"error"); ui.editorError.setAttribute("role","alert"); ui.editorError.hidden = true;
    const form = el("form"); ui.fields = el("fieldset",undefined,"ac-fields"); ui.fields.setAttribute("aria-label","编辑字段");
    const editorActions = el("div",undefined,"ac-dialog-actions"); ui.cancel = button("取消",closeEditor); ui.save = el("button","保存","primary"); ui.save.type = "submit"; editorActions.append(ui.cancel,ui.save);
    form.append(ui.editorTitle,ui.editorHint,ui.editorError,ui.fields,editorActions); ui.dialog.append(form);
    ui.deleteDialog = mark(el("dialog",undefined,"ac-dialog"),id("delete-dialog")); const deleteTitle = mark(el("h2","确认删除"),id("delete-title")); ui.deleteDialog.setAttribute("aria-labelledby",deleteTitle.id);
    ui.deleteImpact = el("p",undefined,"ac-description"); ui.deleteError = el("p",undefined,"error"); ui.deleteError.setAttribute("role","alert"); ui.deleteError.hidden = true;
    const deleteActions = el("div",undefined,"ac-dialog-actions"); ui.deleteCancel = button("取消",closeDelete); ui.deleteConfirm = button("确认删除",() => void remove()); deleteActions.append(ui.deleteCancel,ui.deleteConfirm);
    ui.deleteDialog.append(deleteTitle,ui.deleteImpact,ui.deleteError,deleteActions);
    page.replaceChildren(title,el("p",`按标签与分组管理${noun}，选择具体连接填入检测草稿。`,"intro"),ui.error,ui.notice,layout,status("内置资料不代表端点可用性或业务完整性已验证。规则、保存与草稿填入均不会发起网络检测。"),ui.dialog,ui.deleteDialog);
    ui.search.addEventListener("input",render); form.addEventListener("submit",event => void submit(event));
    ui.dialog.addEventListener("cancel",event => { event.preventDefault(); closeEditor(); }); ui.deleteDialog.addEventListener("cancel",event => { event.preventDefault(); closeDelete(); });
    return true;
  }
    if(!mount())return null;
    return {render};
  }
  for(const args of [["catalog-page","catalog-title","website","ac-"],["applications-page","applications-title","application","app-ac-"]]){const view=createView(...args);if(view)views.push(view);}
  C.listen(()=>{for(const view of views)view.render();renderQuick();});
  window.addEventListener("aonet-page",event=>{if(event.detail==="catalog"||event.detail==="applications")void C.load();});
  $("apply-site")?.addEventListener("click",()=>{const site=selectedQuickSite();if(site)applySite(site);});
  if(views.length)void C.load();
})();
