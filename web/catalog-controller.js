"use strict";
(() => {
  if (window.catalogController) return;
  const clone = value => JSON.parse(JSON.stringify(value));
  const strings = value => Array.isArray(value) && value.every(x => typeof x === "string");
  const nullable = value => value === null || typeof value === "string";
  const pick = (value,keys) => Object.fromEntries(keys.map(key => [key,clone(value[key])]));
  const categories = ["web","communication","gds","network","custom"];
  const transports = ["unspecified","tcp","udp"], protocols = ["unspecified","http","https","tcp","tls","udp_dns","quic","other"];
  const siteFields = site => pick(site,["id","name","address","port","transport","protocol","purpose","alpn"]);
  const ruleFields = rule => ({...pick(rule,["id","kind","address","transport","protocol","ports","purpose","evidence","notes"]),source_ids:[]});
  const appFields = app => ({...pick(app,["id","name","kind","category","provider","product","aliases","access_type","site_ids"]),reference_rules:app.reference_rules.map(ruleFields)});
  const groupFields = group => pick(group,["id","name","description","tag_ids"]);
  const supplementFields = value => ({resource_id:value.resource_id,site_ids:clone(value.site_ids),reference_rules:value.reference_rules.map(ruleFields)});
  const state = {snapshot:null,loading:false,saving:false,unknown:false,locked:true,read:null,error:"",notice:""};
  const listeners = new Set(), subscribers = new Set();
  function custom(snapshot = state.snapshot) {
    return {sites:snapshot.sites.filter(x=>!x.builtin).map(siteFields),applications:snapshot.applications.filter(x=>!x.builtin).map(appFields),groups:snapshot.groups.filter(x=>!x.builtin).map(groupFields),tags:snapshot.tags.filter(x=>!x.builtin).map(x=>pick(x,["id","name"])),tag_assignments:snapshot.tag_assignments.map(x=>pick(x,["resource_id","tag_ids"])),resource_supplements:snapshot.resource_supplements.map(supplementFields)};
  }
  function valid(snapshot) {
    const base = x => x && typeof x.id === "string" && x.id.length > 0 && typeof x.name === "string" && typeof x.builtin === "boolean";
    const rule = r => r && typeof r.id === "string" && ["unknown","host","url","wildcard"].includes(r.kind) && nullable(r.address) && (r.transport===null || transports.includes(r.transport)) && (r.protocol===null || protocols.includes(r.protocol)) && ["ports","purpose","evidence","notes"].every(k=>typeof r[k]==="string") && strings(r.source_ids);
    const supplement = s => s && typeof s.resource_id === "string" && strings(s.site_ids) && Array.isArray(s.reference_rules) && s.reference_rules.every(rule);
    if (!snapshot || snapshot.version!==3 || !Number.isSafeInteger(snapshot.revision) || snapshot.revision<0 || !["sites","applications","groups","tags","tag_assignments","resource_supplements"].every(k=>Array.isArray(snapshot[k]))) return false;
    if (!snapshot.sites.every(s=>base(s) && typeof s.address==="string" && (s.port===null || Number.isInteger(s.port)&&s.port>0&&s.port<=65535) && transports.includes(s.transport) && protocols.includes(s.protocol) && typeof s.purpose==="string" && nullable(s.alpn) && typeof s.source==="string" && typeof s.checked_at==="string" && strings(s.capabilities) && strings(s.unavailable_reasons))) return false;
    if (!snapshot.applications.every(a=>base(a) && ["website","application"].includes(a.kind) && categories.includes(a.category) && ["provider","product","access_type","summary","status"].every(k=>typeof a[k]==="string") && strings(a.aliases) && strings(a.site_ids) && strings(a.tag_ids) && Array.isArray(a.reference_rules) && a.reference_rules.every(rule) && Array.isArray(a.sources) && a.sources.every(s=>s&&typeof s==="object") && strings(a.missing) && strings(a.limitations) && nullable(a.environment) && nullable(a.version) && (a.supplement===null || supplement(a.supplement)))) return false;
    if (!snapshot.groups.every(g=>base(g)&&typeof g.description==="string"&&strings(g.application_ids)&&strings(g.tag_ids)) || !snapshot.tags.every(base)) return false;
    const sites = new Map(snapshot.sites.map(s=>[s.id,s])), apps = new Map(snapshot.applications.map(a=>[a.id,a])), tags = new Set(snapshot.tags.map(t=>t.id));
    const ids = [...snapshot.sites,...snapshot.applications,...snapshot.groups,...snapshot.tags].map(x=>x.id);
    if (new Set(ids).size!==ids.length || !snapshot.applications.every(a=>a.site_ids.every(id=>sites.has(id))&&a.tag_ids.every(id=>tags.has(id))) || !snapshot.groups.every(g=>g.application_ids.every(id=>apps.has(id))&&g.tag_ids.every(id=>tags.has(id)))) return false;
    if (!snapshot.tag_assignments.every(a=>a&&typeof a.resource_id==="string"&&apps.has(a.resource_id)&&strings(a.tag_ids)&&a.tag_ids.every(id=>tags.has(id))) || new Set(snapshot.tag_assignments.map(a=>a.resource_id)).size!==snapshot.tag_assignments.length) return false;
    return snapshot.resource_supplements.every(s=>supplement(s)&&apps.get(s.resource_id)?.builtin&&s.site_ids.every(id=>sites.has(id)&&!sites.get(id).builtin)) && new Set(snapshot.resource_supplements.map(s=>s.resource_id)).size===snapshot.resource_supplements.length;
  }
  function sameCustom(a,b) {
    function canonical(value) {
      if (Array.isArray(value)) return value.map(canonical).sort((x,y)=>JSON.stringify(x).localeCompare(JSON.stringify(y)));
      if (value&&typeof value==="object") return Object.fromEntries(Object.keys(value).sort().map(k=>[k,canonical(value[k])]));
      return value;
    }
    return JSON.stringify(canonical(a))===JSON.stringify(canonical(b));
  }
  const snapshot = () => state.snapshot ? clone(state.snapshot) : null;
  const blocked = () => !state.snapshot || state.loading || state.saving || state.unknown || state.locked;
  const errorText = error => String(error?.message || error);
  function changed(publish=false) {
    window.appShell?.setLocked("catalog-mutation",state.saving||state.unknown);
    for (const notify of listeners) { try { notify(); } catch(e) { console.error("目录视图更新失败",e); } }
    if (publish) for (const notify of subscribers) { try { notify(snapshot()); } catch(e) { console.error("目录订阅失败",e); } }
  }
  async function transport(body) {
    if (window.__TAURI__?.core?.invoke) return window.__TAURI__.core.invoke(body ? "save_site_catalog" : "site_catalog",body ? {request:body} : undefined);
    const controller = new AbortController(), timeout = setTimeout(()=>controller.abort(),8000);
    try {
      let token;
      if (body) {
        const info = await fetch("/api/network-tools/info",{signal:controller.signal,credentials:"same-origin",cache:"no-store"});
        if (!info.ok) throw new Error("无法获取会话令牌。");
        token = (await info.json()).token;
        if (typeof token!=="string" || !token) throw new Error("会话令牌格式无效。");
      }
      const response = await fetch("/api/site-catalog",{method:body ? "PUT" : "GET",headers:body ? {"Content-Type":"application/json","X-AoNet-Token":token} : {Accept:"application/json"},body:body ? JSON.stringify(body) : undefined,signal:controller.signal,credentials:"same-origin",cache:"no-store"});
      const text = await response.text(); let result;
      try { result = JSON.parse(text); } catch { throw new Error(response.ok ? "目录响应无法解析。" : text || `请求失败（${response.status}）`); }
      if (!response.ok) throw new Error(result.error || `请求失败（${response.status}）`);
      return result;
    } finally { clearTimeout(timeout); }
  }
  function load() {
    if (state.read) return state.read;
    if (state.saving) return Promise.resolve();
    state.loading=true; changed();
    state.read = Promise.resolve().then(async()=>{
      try {
        const latest = await transport();
        if (!valid(latest)) throw new Error("目录 v3 格式无效，未使用旧版或不完整数据。");
        state.snapshot=latest; state.unknown=false; state.error=""; state.notice="";
      } catch(error) { state.error=errorText(error); }
      finally { state.loading=false; state.read=null; changed(true); }
    });
    return state.read;
  }
  async function save(data,expectedRevision=state.snapshot?.revision) {
    if (blocked()) throw new Error("请等待检测结束并重新读取目录后再保存。");
    if (expectedRevision!==state.snapshot.revision) throw new Error("目录已更新，请取消并重新打开编辑，避免覆盖其他修改。");
    const keys=["sites","applications","groups","tags","tag_assignments","resource_supplements"];
    if (!keys.every(k=>Array.isArray(data[k])) || Object.keys(data).some(k=>!keys.includes(k))) throw new Error("必须提供完整 v3 可写目录。");
    // Copy at the boundary: callers cannot mutate an in-flight request or its reconciliation target.
    data=clone(data); state.saving=true; state.unknown=true; changed();
    try {
      const latest = await transport({schema_version:3,expected_revision:expectedRevision,...data});
      if (!valid(latest) || latest.revision!==expectedRevision+1 || !sameCustom(custom(latest),data)) throw new Error("保存响应无法核对。");
      state.snapshot=latest; state.unknown=false; state.error=""; state.notice="已保存。共享连接的修改会同步到所有引用它的应用与网站。";
    } catch(error) {
      try {
        const latest=await transport();
        if (!valid(latest)) throw new Error("重新读取的目录格式无效。");
        state.snapshot=latest; state.unknown=false;
        if (latest.revision===expectedRevision+1 && sameCustom(custom(latest),data)) {state.error="";state.notice="保存结果已通过重新读取核对，未重复写入。";return;}
        throw new Error(`${errorText(error)} 已重新读取最新目录；未重发保存，请取消后重新编辑。`);
      } catch(readError) {
        if(state.unknown)state.error="保存结果不确定，已锁定修改。请重新进入任一目录页核对，不会自动重发。";
        throw readError;
      }
    } finally {state.saving=false;changed(true);}
  }
  function setLocked(value) {value=Boolean(value);if(state.locked!==value){state.locked=value;changed();}}
  window.catalogController={state,clone,custom,valid,sameCustom,siteFields,ruleFields,appFields,groupFields,blocked,save,load,listen(fn){listeners.add(fn);return()=>listeners.delete(fn);}};
  window.siteCatalog={snapshot,reload:load,setLocked,subscribe(fn){subscribers.add(fn);fn(snapshot());return()=>subscribers.delete(fn);}};
  window.appShell?.subscribe(source=>{if(source!=="catalog-mutation")setLocked(window.appShell.isLockedExcept("catalog-mutation"));});
  state.locked=window.appShell ? window.appShell.isLockedExcept("catalog-mutation") : window.networkToolDraft?.isLocked() ?? true;
})();
