"use strict";
(() => {
  const $=id=>document.getElementById(id),native=Boolean(window.__TAURI__?.core?.invoke);
  const activeStatuses=new Set(["preparing","running","stopping","draining"]);
  const statuses={preparing:"准备中",running:"运行中",stopping:"停止并回收连接…",draining:"等待系统 DNS 收尾…",completed:"执行结束",cancelled:"已停止",timed_out:"达到批次总时限",failed:"批次失败"};
  const phases={queued:"排队",waiting_admission:"等待准入",resolving:"解析中",connecting:"建连中",http:"等待响应头",holding:"保持连接",finished:"结束"};
  const outcomes={http_response:"收到 HTTP 响应",tcp_connected:"已完成连接保持",early_closed:"远端提前关闭",network_error:"网络错误",timed_out:"超时",cancelled:"已取消",not_executed:"未执行"};
  const modes={public_http:"公共连通性检查",authorized_http:"授权负载 · HTTP",tcp:"授权负载 · TCP"};
  function modeLimits(){
    const publicMode=mode()==="public_http";
    return state.info?.limits?.[publicMode?"public_http":"authorized"]||{max_concurrency:publicMode?4:1000,max_connections:publicMode?4:1000,max_attempts_per_site:publicMode?3:100,max_start_rate:publicMode?2:1000,max_batch_timeout_secs:publicMode?120:300};
  }
  const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  const state={info:null,catalog:null,groups:new Set(),excludedApplications:new Set(),bindings:new Map(),snapshot:null,known:false,pending:false,uncertain:null,owned:null,externalBusy:false,externalSources:[],read:null,epoch:0,timer:null,preview:null,permitDraft:null,permitBusy:false,permitUnknown:false,nativeWrites:0,targetPage:0,resultPage:0,resultSignature:null};
  function message(id,value){$(id).textContent=value;$(id).hidden=!value;}
  function errorText(error){return String(error?.message||error||"未知错误");}
  function mode(){return document.querySelector('input[name="batch-mode"]:checked')?.value||"public_http";}
  function active(){return Boolean(state.snapshot&&activeStatuses.has(state.snapshot.status));}
  function ownLocked(){return !state.known||state.pending||Boolean(state.uncertain)||state.nativeWrites>0||state.permitBusy||state.permitUnknown||state.externalBusy||active();}
  function locked(){return ownLocked()||Boolean(window.appShell?.isLockedExcept("batch"));}
  function publishLock(){window.appShell?.setLocked("batch",ownLocked()||Boolean(state.preview));}
  function el(tag,text,className){const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(className)n.className=className;return n;}
  function button(text,fn,disabled=false){const n=el("button",text);n.type="button";n.disabled=disabled;n.addEventListener("click",fn);return n;}
  function groupApplications(group){return (group.application_ids||[]).map(id=>state.catalog.applications.find(a=>a.id===id)).filter(Boolean);}
  function selectedApplications(){
    if(!state.catalog)return [];
    return state.catalog.groups.filter(g=>state.groups.has(g.id)).flatMap(g=>groupApplications(g).filter(a=>!state.excludedApplications.has(`${g.id}/${a.id}`)));
  }
  function selectedSites(){
    if(!state.catalog)return [];
    const ids=new Set(selectedApplications().flatMap(a=>a.site_ids));
    return state.catalog.sites.filter(s=>ids.has(s.id));
  }
  function endpointIssue(site){
    const tcp=mode()==="tcp",allowed=tcp?["http","https","tcp","tls","other"]:["http","https"];
    if(site.transport!=="tcp"||!allowed.includes(site.protocol)||!site.capabilities?.includes(tcp?"tcp":"http"))return tcp?"此端点不支持 TCP 批量：需要明确的 TCP 传输与可建连协议；UDP、QUIC、未知协议和规则不可用。":"HTTP 批量只支持 TCP 上的明确 HTTP/HTTPS 端点。";
    try{if(tcp)hostPort(site);else if(!/^https?:\/\//i.test(site.address))return "HTTP 需要完整 HTTP(S) 网址。";}catch(e){return e.message;}
    return "";
  }
  function applicationIssue(app){
    if(!app)return "组合引用的应用不存在，请重新读取目录。";
    if(!app.site_ids?.length)return "此应用没有明确端点；参考规则不会展开或探测。";
    for(const id of app.site_ids){const site=state.catalog.sites.find(s=>s.id===id);if(!site)return `应用引用的端点不存在：${id}`;const issue=endpointIssue(site);if(issue)return `${site.name}：${issue}`;}
    return "";
  }
  function groupIssue(group){
    if(!group.application_ids?.length)return "组合中没有应用。";
    for(const id of group.application_ids){const issue=applicationIssue(state.catalog.applications.find(a=>a.id===id));if(issue)return issue;}
    return "";
  }
  function selectionIssue(){
    if(state.catalog?.version!==2||!Array.isArray(state.catalog.applications))return "需要 v2 应用目录，请重新读取。";
    for(const id of state.groups){const group=state.catalog.groups.find(g=>g.id===id);if(!group)return "所选组合不存在，请重新读取。";if(!group.application_ids.length)return "组合中没有应用。";for(const appId of group.application_ids){const app=state.catalog.applications.find(a=>a.id===appId);if(!app)return "组合引用的应用不存在。";if(app.site_ids.some(id=>!state.catalog.sites.some(s=>s.id===id)))return "应用引用的端点不存在。";if(!state.excludedApplications.has(`${id}/${appId}`)){const issue=applicationIssue(app);if(issue)return `${app.name}：${issue}`;}}}
    return "";
  }
  function hostPort(site){
    if(/^https?:\/\//i.test(site.address)){const u=new URL(site.address);return {host:u.hostname.toLowerCase(),port:site.port??Number(u.port||(u.protocol==="https:"?443:80))};}
    if(!site.port)throw new Error("裸主机必须保存默认端口。");
    return {host:site.address.replace(/^\[|\]$/g,"").toLowerCase(),port:site.port};
  }
  function sameEndpoint(site,permit,testMode=mode()){
    try{if(testMode!=="tcp")return /^https?:\/\//i.test(site.address)&&new URL(site.address).href===new URL(permit.url).href;
      const s=hostPort(site),p=new URL(permit.url);return s.host.replace(/^\[|\]$/g,"")===p.hostname.replace(/^\[|\]$/g,"").toLowerCase()&&s.port===Number(p.port||(p.protocol==="https:"?443:80));
    }catch{return false;}
  }
  function matchingPermits(site){return (state.info?.authorized||[]).filter(p=>!p.expired&&Date.parse(p.expires_at)>Date.now()&&p.modes.includes(mode()==="tcp"?"tcp":"http")&&sameEndpoint(site,p));}
  function siteIssue(site){
    const unsupported=endpointIssue(site);if(unsupported)return unsupported;
    if(mode()!=="public_http"&&!matchingPermits(site).some(p=>p.id===state.bindings.get(site.id)))return "请选择精确匹配的有效授权；未配置时整批不会开始。";
    return "";
  }
  function number(id,min,max,label){const raw=$(id).value;const value=Number(raw);if(!/^\d+$/.test(raw)||!Number.isSafeInteger(value)||value<min||value>max)throw new Error(`${label}必须为 ${min}～${max} 的整数。`);return value;}
  function buildRequest(requestId,confirmed=false){
    const sites=selectedSites(),testMode=mode(),publicMode=testMode==="public_http",limits=modeLimits();
    if(!state.catalog||!state.groups.size||!sites.length)throw new Error("请选择至少一个有明确端点的组合。");
    const invalid=selectionIssue();if(invalid)throw new Error(invalid);
    if(sites.length>1000)throw new Error("每批最多 1000 个站点。");
    const issues=sites.map(s=>[s,siteIssue(s)]).filter(([,reason])=>reason);if(issues.length)throw new Error(issues.slice(0,5).map(([s,r])=>`${s.name}：${r}`).join("\n")+(issues.length>5?`\n另有 ${issues.length-5} 项，请检查站点预览。`:""));
    if(!publicMode&&state.info?.authorization_error)throw new Error(`授权配置需修复：${state.info.authorization_error}`);
    const body={request_id:requestId,catalog_revision:state.catalog.revision,group_ids:[...state.groups].sort(),site_ids:sites.map(s=>s.id).sort(),mode:testMode,
      permit_bindings:publicMode?[]:sites.map(s=>({site_id:s.id,permit_id:state.bindings.get(s.id)})),
      concurrency:number("batch-concurrency",1,limits.max_concurrency,"执行并发"),connection_limit:number("batch-connections",1,limits.max_connections,"连接上限"),queue_capacity:number("batch-queue",1,256,"队列容量"),
      attempts_per_site:number("batch-attempts",1,limits.max_attempts_per_site,"每站次数"),start_rate:number("batch-rate",1,limits.max_start_rate,"起发速率"),request_timeout_secs:number("batch-timeout",1,30,"单次超时"),batch_timeout_secs:number("batch-duration",1,limits.max_batch_timeout_secs,"批次总时限"),confirmed_authorized:!publicMode&&confirmed};
    if(sites.length*body.attempts_per_site>10000)throw new Error("计划总尝试次数不能超过 10000。");
    if(testMode==="tcp"){body.per_site_connections=number("batch-per-site",1,body.connection_limit,"每站连接上限");body.tcp_hold_secs=number("batch-hold",1,30,"保持时长");}
    return body;
  }
  function nullableNumber(value){return value===null||(typeof value==="number"&&Number.isFinite(value)&&value>=0);}
  function valid(snapshot){
    if(snapshot===null)return true;
    const integers=["planned","pending_generation","queued","waiting_admission","active","connecting","connected","peak_connected","connection_permits","peak_connection_permits","terminal","unexecuted"];
    return Boolean(snapshot&&uuid.test(snapshot.request_id)&&snapshot.config&&snapshot.config.request_id===snapshot.request_id&&Object.hasOwn(modes,snapshot.config.mode)&&Object.hasOwn(statuses,snapshot.status)&&typeof snapshot.details_retained==="boolean"&&Array.isArray(snapshot.sites)&&snapshot.sites.length<=1000&&Array.isArray(snapshot.attempts)&&snapshot.attempts.length<=10000&&snapshot.counts&&integers.every(k=>Number.isSafeInteger(snapshot.counts[k])&&snapshot.counts[k]>=0)&&snapshot.sites.every(s=>typeof s.site_id==="string"&&typeof s.name==="string"&&typeof s.address==="string"&&nullableNumber(s.last_latency_ms)&&nullableNumber(s.average_latency_ms)&&(s.last_error===null||typeof s.last_error==="string")&&(s.last_status===null||Number.isInteger(s.last_status))&&["planned","attempted","responses","good_status","bad_status","network_errors","timed_out","cancelled","tcp_connected","tcp_failed","connected","peak_connected","early_closed"].every(k=>Number.isSafeInteger(s[k])&&s[k]>=0))&&snapshot.attempts.every(a=>typeof a.site_id==="string"&&Number.isSafeInteger(a.sequence)&&Number.isSafeInteger(a.attempt)&&nullableNumber(a.latency_ms)&&nullableNumber(a.hold_ms)&&(a.error===null||typeof a.error==="string")&&Object.hasOwn(phases,a.phase)&&(a.outcome===null||Object.hasOwn(outcomes,a.outcome))));
  }
  async function request(kind,body=null,id=null){
    const write=body!==null,epoch=state.epoch;
    if(native){
      const args=kind==="state"?{request:{request_id:id}}:body?{request:body}:undefined;
      const command={info:"catalog_batch_info",state:"catalog_batch_state",start:"catalog_batch_start",stop:"catalog_batch_stop",workspace:"workspace"}[kind];
      if(write)state.nativeWrites++;
      let timer;
      const op=Promise.resolve().then(()=>window.__TAURI__.core.invoke(command,kind==="workspace"?body:args));
      if(write)void op.finally(()=>{state.nativeWrites--;if(epoch===state.epoch){publishLock();renderControls();}}).catch(()=>{});
      try{return await Promise.race([op,new Promise((_,reject)=>{timer=setTimeout(()=>{const e=new Error("原生操作等待超时，请重新连接核对。");e.uncertain=write;reject(e);},8000);})]);}
      catch(error){if(typeof error==="string"){const e=new Error(error);e.rejected=true;throw e;}throw error;}finally{clearTimeout(timer);}
    }
    const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),8000);
    try{
      const path=kind==="workspace"?"/api/workspace":`/api/catalog-batch/${kind}${kind==="state"&&id?`?request_id=${encodeURIComponent(id)}`:""}`;
      const response=await fetch(path,{method:write?"POST":"GET",headers:write?{"Content-Type":"application/json","X-AoNet-Token":state.info?.token||""}:{Accept:"application/json"},body:write?JSON.stringify(body):undefined,credentials:"same-origin",cache:"no-store",signal:controller.signal});
      let data;try{data=await response.json();}catch{const e=new Error("后台响应无法解析，请重新连接核对。");e.uncertain=write;throw e;}
      if(!response.ok){const e=new Error(data.error||`请求失败（${response.status}）`);e.rejected=true;throw e;}return data;
    }catch(e){if(e.name==="AbortError"||e instanceof TypeError)e.uncertain=write;throw e;}finally{clearTimeout(timer);}
  }
  function acceptState(reply,queriedId=null){
    if(!reply||typeof reply.accepted!=="boolean"||typeof reply.external_busy!=="boolean"||!Array.isArray(reply.external_busy_sources)||!reply.external_busy_sources.every(s=>typeof s==="string")||!Object.hasOwn(reply,"snapshot")||!valid(reply.snapshot)||(reply.accepted&&!reply.snapshot)||(!reply.accepted&&reply.snapshot!==null)||(queriedId&&reply.request_id!==queriedId)||(reply.snapshot&&reply.snapshot.request_id!==reply.request_id))throw new Error("批次状态格式无效，保留最近结果。");
    return reply.snapshot;
  }
  async function readState(){
    if(state.read)return state.read;const epoch=state.epoch;
    const op=(async()=>{
      if(state.uncertain){const id=state.uncertain,reply=await request("state",null,id);const exact=acceptState(reply,id);if(epoch!==state.epoch)return;
        if(exact){state.snapshot=exact;state.owned=id;state.uncertain=null;}
        else message("batch-error","后台尚未记录该请求，仍保留启动结果待核对锁。请重新连接；不会自动重发或据此断言原请求已结束。");
      }
      if(state.pending)return;
      const reply=await request("state");const current=acceptState(reply);if(epoch!==state.epoch||state.pending)return;
      state.externalBusy=reply.external_busy===true;state.externalSources=reply.external_busy_sources||[];
      if(current)state.snapshot=current;else if(active()&&!state.uncertain&&!state.pending&&!state.nativeWrites)state.snapshot=null;state.known=true;render();
    })().catch(e=>{if(epoch===state.epoch){state.known=false;message("batch-error",errorText(e));renderControls();}});
    state.read=op;try{await op;}finally{if(state.read===op)state.read=null;}
  }
  function poll(){clearTimeout(state.timer);const epoch=state.epoch;state.timer=setTimeout(async()=>{await readState();if(epoch===state.epoch)poll();},1000);}
  async function info(){const result=await request("info");if(!result||typeof result.version!=="string"||!Array.isArray(result.authorized)||!result.authorized.every(p=>typeof p.id==="string"&&typeof p.name==="string"&&typeof p.url==="string"&&Array.isArray(p.modes)&&Array.isArray(p.allowed_ips)&&typeof p.expires_at==="string")||!Number.isSafeInteger(result.auth_revision)||(!native&&typeof result.token!=="string"))throw new Error("批量初始化信息无效。");state.info=result;}
  async function connect(){
    state.known=false;renderControls();
    try{await info();await readState();renderGroups();renderTargets();renderPermits();poll();}catch(e){message("batch-error",errorText(e));renderControls();}
  }
  function renderGroups(){
    const root=$("batch-groups"),focus=document.activeElement?.dataset.batchGroup;root.replaceChildren();
    for(const group of state.catalog?.groups||[]){const label=el("label"),check=el("input"),apps=groupApplications(group),siteCount=new Set(apps.flatMap(a=>a.site_ids)).size,issue=groupIssue(group);const text=el("span",`${group.name} · ${apps.length} 个应用 / ${siteCount} 个端点${group.builtin?" · 内置":""}${issue?` · 不可选：${issue}`:""}`);check.type="checkbox";check.dataset.batchGroup=group.id;check.checked=state.groups.has(group.id);check.disabled=locked()||Boolean(issue&&!check.checked);check.dataset.batchUnavailable=check.checked?"":issue;check.addEventListener("change",()=>{if(locked())return;if(check.checked)state.groups.add(group.id);else state.groups.delete(group.id);state.preview=null;state.targetPage=0;renderGroups();renderControls();renderTargets();});label.append(check,text);root.append(label);if(check.checked)for(const app of apps){const appLabel=el("label",undefined,"batch-application-option"),appCheck=el("input"),appText=el("span",`${app.name} · ${(app.site_ids||[]).length} 个端点${applicationIssue(app)?` · 不可用：${applicationIssue(app)}`:""}`);appCheck.type="checkbox";appCheck.checked=!state.excludedApplications.has(`${group.id}/${app.id}`);appCheck.disabled=locked()||Boolean(applicationIssue(app));appCheck.dataset.batchUnavailable=applicationIssue(app);appCheck.addEventListener("change",()=>{if(appCheck.checked)state.excludedApplications.delete(`${group.id}/${app.id}`);else state.excludedApplications.add(`${group.id}/${app.id}`);state.preview=null;state.targetPage=0;renderControls();renderGroups();renderTargets();});appLabel.append(appCheck,appText);root.append(appLabel);}if(focus===group.id)check.focus();}
    if(!state.catalog)root.append(el("p","站点库尚未读取。","hint"));
  }
  function pager(id,total,page,onPage){const root=$(id);root.replaceChildren();const pages=Math.ceil(total/25);if(pages<=1)return;root.append(button("上一页",()=>onPage(page-1),page===0),el("span",`${page+1} / ${pages}`),button("下一页",()=>onPage(page+1),page>=pages-1));}
  function renderTargets(){
    const sites=selectedSites(),root=$("batch-targets");root.replaceChildren();state.targetPage=Math.min(state.targetPage,Math.max(0,Math.ceil(sites.length/25)-1));
    $("batch-preview-title").textContent=`所选站点预览 · ${sites.length} 个`;
    for(const site of sites.slice(state.targetPage*25,state.targetPage*25+25)){
      const li=el("li"),names=state.catalog.groups.filter(g=>state.groups.has(g.id)&&groupApplications(g).some(a=>!state.excludedApplications.has(`${g.id}/${a.id}`)&&a.site_ids.includes(site.id))).map(g=>g.name),apps=[...new Map(selectedApplications().filter(a=>a.site_ids.includes(site.id)).map(a=>[a.id,a])).values()];
      li.append(el("strong",site.name),el("p",site.address+(site.port?` · 默认端口 ${site.port}`:"")+` · ${site.transport} / ${site.protocol}`,"hint"),el("p",`${names.join("、")} · 应用：${apps.map(a=>a.name).join("、")}`,"hint"));
      if(mode()!=="public_http"){
        const label=el("label","绑定授权端点"),select=el("select");select.setAttribute("aria-label",`${site.name} 的授权端点`);const placeholder=el("option","请选择匹配授权");placeholder.value="";select.append(placeholder);
        for(const p of matchingPermits(site)){const option=el("option",`${p.name} · ${p.url}`);option.value=p.id;select.append(option);}select.value=state.bindings.get(site.id)||"";select.disabled=locked();select.addEventListener("change",()=>{state.bindings.set(site.id,select.value);state.preview=null;renderControls();renderTargets();});label.append(select);li.append(label);
      }
      const issue=siteIssue(site);if(issue)li.append(el("p",issue,"batch-target-error"));root.append(li);
    }
    if(!sites.length)root.append(el("li","尚未选择站点，或所选组合为空。"));
    pager("batch-target-pager",sites.length,state.targetPage,page=>{state.targetPage=page;renderTargets();});
  }
  function renderControls(){
    publishLock();const disabled=locked()||Boolean(state.preview),tcp=mode()==="tcp",publicMode=mode()==="public_http",sites=selectedSites(),limits=modeLimits();
    $("batch-route").textContent=window.proxySettings?.label()||"代理设置尚未核对";
    $("batch-selection").textContent=state.groups.size?`已选 ${state.groups.size} 个组合 · ${sites.length} 个唯一站点 · 计划 ${sites.length*(Number($("batch-attempts").value)||0)} 次尝试`:"请选择至少一个组合。";
    for(const id of ["batch-concurrency","batch-connections","batch-queue","batch-attempts","batch-rate","batch-timeout","batch-duration"])$(id).disabled=disabled;
    $("batch-concurrency").max=String(limits.max_concurrency);$("batch-connections").max=String(limits.max_connections);$("batch-attempts").max=String(limits.max_attempts_per_site);$("batch-rate").max=String(limits.max_start_rate);$("batch-duration").max=String(limits.max_batch_timeout_secs);
    for(const node of document.querySelectorAll('input[name="batch-mode"],#batch-groups input,#batch-targets select'))node.disabled=disabled||Boolean(node.dataset.batchUnavailable);
    for(const node of document.querySelectorAll(".batch-tcp-field"))node.hidden=!tcp;
    for(const id of ["batch-per-site","batch-hold"])$(id).disabled=disabled||!tcp;
    $("batch-attempts-label").firstChild.textContent=tcp?"每站建连次数":"每站请求次数";$("batch-timeout-label").firstChild.textContent=tcp?"建连超时 / 秒":"单次超时 / 秒";
    $("batch-budget").textContent=publicMode?"这是公共连通性检查，不是负载性能测试。仅 HEAD / HTTP1 短连接；全局最多 2 次/秒，同域至少间隔 30 秒。冷却与排队计入总时限，计划次数可能不能全部执行。":tcp?"授权 TCP 连接负载：参数由页面设置，实际受端点授权与系统可用资源约束。成功连接保持后释放，不发送应用数据、失败不自动补满。执行并发包括建连与保持。":"授权 HTTP 请求负载：参数由页面设置，实际受端点授权与系统可用资源约束。固定 HEAD / HTTP1 独立短连接，不重试、不跳转、不读取正文。连接上限不是连接复用池。";
    $("batch-start").disabled=disabled||!sites.length||Boolean(selectionIssue())||sites.some(s=>siteIssue(s));$("batch-stop").disabled=!state.known||state.pending||!active()||state.owned!==state.snapshot?.request_id||["stopping","draining"].includes(state.snapshot?.status);
    $("batch-refresh-catalog").disabled=disabled;$("batch-authorizations").disabled=disabled||!state.info;$("batch-confirm-start").disabled=locked()||!state.preview;
  }
  function renderResults(){
    const s=state.snapshot;
    $("batch-status").textContent=state.externalBusy?`其他网络任务仍在运行或收尾：${state.externalSources.join("、")}`:state.uncertain?"启动结果待核对，不会自动重发":!state.known?"后台状态尚未连接":s?`${modes[s.config.mode]} · ${statuses[s.status]}${s.route?` · ${window.proxySettings?.label(null,s.route)||""}`:""}${state.owned!==s.request_id?"（其他窗口的批次，仅查看）":""}`:"等待开始";
    if(!s){$("batch-progress").textContent="未发起任何请求。";return;}
    const c=s.counts;$("batch-progress").textContent=`计划 ${c.planned} · 待生成 ${c.pending_generation} · 排队 ${c.queued} · 等待准入 ${c.waiting_admission} · 执行 ${c.active} · 已终结 ${c.terminal} · 未执行 ${c.unexecuted}；连接额度 ${c.connection_permits}（峰值 ${c.peak_connection_permits}）${s.config.mode==="tcp"?` · 已建立 ${c.connected}（峰值 ${c.peak_connected}）`:""}${s.error?`；${s.error}`:""}`;
    const open=[...$("batch-results-list").querySelectorAll("details[open]")].map(n=>n.dataset.siteId);
    const signature=JSON.stringify([s.request_id,s.status,s.sites,s.attempts,state.resultPage,s.details_retained]);if(signature===state.resultSignature)return;state.resultSignature=signature;
    const root=$("batch-results-list");root.replaceChildren();
    if(!s.details_retained){root.append(el("p","此历史请求仅保留幂等摘要，完整明细已释放。","hint"));return;}
    state.resultPage=Math.min(state.resultPage,Math.max(0,Math.ceil(s.sites.length/25)-1));
    for(const site of s.sites.slice(state.resultPage*25,state.resultPage*25+25)){
      const attemptsForSite=s.attempts.filter(a=>a.site_id===site.site_id),inflight=attemptsForSite.find(a=>a.phase!=="finished");
      const row=el("article",undefined,"batch-result"),heading=el("div",undefined,"batch-result-heading");heading.append(el("strong",site.name),el("span",`${inflight?phases[inflight.phase]:activeStatuses.has(s.status)?"等待调度":"已结束"} · ${site.attempted} / ${site.planned} 次已尝试`));row.append(heading,el("p",site.address,"hint"));
      row.append(el("p",s.config.mode==="tcp"?`建连成功 ${site.tcp_connected} · 建连失败 ${site.tcp_failed} · 当前 ${site.connected} · 峰值 ${site.peak_connected} · 提前关闭 ${site.early_closed} · 超时 ${site.timed_out} · 取消 ${site.cancelled}`:`收到响应 ${site.responses} · 2xx/3xx ${site.good_status} · 4xx/5xx ${site.bad_status} · 网络错误 ${site.network_errors} · 超时 ${site.timed_out} · 取消 ${site.cancelled}`,"batch-result-counts"));
      row.append(el("p",`最近 ${site.last_status?`HTTP ${site.last_status}`:"—"} · ${s.config.mode==="tcp"?(s.route?.proxy?"解析＋代理隧道":"解析＋建连"):"响应头"}耗时 ${site.last_latency_ms===null?"—":`${site.last_latency_ms.toFixed(1)} ms`} · 平均 ${site.average_latency_ms===null?"—":`${site.average_latency_ms.toFixed(1)} ms`}${site.last_error?` · ${site.last_error}`:""}`,"hint"));
      const details=el("details"),summary=el("summary","逐次明细"),list=el("ol",undefined,"batch-attempt-list");details.dataset.siteId=site.site_id;details.open=open.includes(site.site_id);
      const attempts=attemptsForSite;
      for(const a of attempts){const text=`#${a.attempt} · ${a.outcome?outcomes[a.outcome]:phases[a.phase]}${a.status?` · HTTP ${a.status}`:""}${a.target_ip?` · 目标 ${a.target_ip}`:a.connected_ip?` · ${a.connected_ip}`:""}${a.proxy_ip?` · 代理 ${a.proxy_ip}（隧道，非目标 TCP RTT）`:""}${a.latency_ms!==null?` · ${a.latency_ms.toFixed(1)} ms`:""}${a.hold_ms!==null?` · 保持 ${a.hold_ms.toFixed(0)} ms`:""}\n${a.started_at||"未开始"}${a.ended_at?` → ${a.ended_at}`:""}${a.error?`\n${a.error}${a.error_truncated?"（已截断）":""}`:""}`;list.append(el("li",text));}
      if(!attempts.length)list.append(el("li","尚未生成本次尝试明细。"));details.append(summary,list);row.append(details);root.append(row);
    }
    pager("batch-result-pager",s.sites.length,state.resultPage,page=>{state.resultPage=page;state.resultSignature=null;renderResults();});
  }
  function render(){renderControls();renderResults();}
  async function execute(body){
    if(locked())return;state.pending=true;state.owned=body.request_id;state.preview=null;message("batch-error","");renderControls();const epoch=state.epoch;
    try{const snapshot=await request("start",body);if(epoch!==state.epoch)return;if(!valid(snapshot)||!snapshot||snapshot.request_id!==body.request_id){const e=new Error("启动响应无法核对。");e.uncertain=true;throw e;}state.snapshot=snapshot;state.known=true;state.resultPage=0;}
    catch(e){if(epoch!==state.epoch)return;if(!e.rejected){state.uncertain=body.request_id;state.known=false;}else state.owned=null;message("batch-error",errorText(e));}
    finally{state.pending=false;if(epoch===state.epoch){render();poll();}}
  }
  function start(event){event.preventDefault();if(locked())return;try{const body=buildRequest(crypto.randomUUID());message("batch-error","");if(body.mode==="public_http"){void execute(body);return;}
      state.preview=body;$("batch-confirm-text").textContent=JSON.stringify({模式:modes[body.mode],组合:[...state.groups].map(id=>state.catalog.groups.find(g=>g.id===id)?.name),站点数:selectedSites().length,计划总次数:selectedSites().length*body.attempts_per_site,参数:body,绑定:selectedSites().map(s=>({站点:s.name,地址:s.address,授权ID:state.bindings.get(s.id)}))},null,2);$("batch-confirm-dialog").showModal();renderControls();
    }catch(e){message("batch-error",errorText(e));}}
  async function stop(){
    if(!state.known||state.pending||!active()||state.owned!==state.snapshot.request_id)return;const id=state.owned,epoch=state.epoch;state.pending=true;renderControls();
    try{const result=await request("stop",{request_id:id});if(epoch!==state.epoch)return;if(!valid(result)||!result||result.request_id!==id)throw new Error("停止响应无法核对。");state.snapshot=result;}
    catch(e){state.known=false;message("batch-error",`${errorText(e)} 将继续只读核对停止结果。`);}finally{state.pending=false;if(epoch===state.epoch){render();poll();}}
  }
  function persistentPermits(){const fields=["id","name","url","modes","allowed_ips","max_concurrency","max_rps","max_duration","authorization","expires_at","proxy_routes"];return (state.info?.authorized||[]).filter(p=>!p.id.startsWith("demo-")).map(p=>Object.fromEntries(fields.filter(k=>Object.hasOwn(p,k)).map(k=>[k,JSON.parse(JSON.stringify(p[k]))])));}
  function renderPermits(){const root=$("permit-existing");root.replaceChildren();for(const p of state.info?.authorized||[])root.append(el("li",`${p.name} · ${p.id}${p.expired?" · 已过期":""}\n${p.url} · ${p.modes.join(" / ")}\nIP: ${p.allowed_ips.join(", ")}\n并发 ${p.max_concurrency} / ${p.max_rps} 次/秒 / ${p.max_duration} 秒 · 到期 ${p.expires_at}`));if(!root.children.length)root.append(el("li","暂无授权端点。"));}
  function permitControls(){const disabled=state.permitBusy||state.permitUnknown||state.nativeWrites>0;for(const id of ["permit-prepare","permit-confirm","permit-discard"])$(id).disabled=disabled;$("permit-close").disabled=state.permitBusy||state.nativeWrites>0;$("permit-reload").disabled=state.permitBusy||state.nativeWrites>0;}
  async function preparePermit(event){
    event.preventDefault();if(locked())return;message("permit-error","");
    try{
      const permitModes=[$("permit-http").checked?"http":null,$("permit-tcp").checked?"tcp":null].filter(Boolean);if(!permitModes.length)throw new Error("至少选择一种授权模式。");
      const record={id:`permit-${crypto.randomUUID()}`,name:$("permit-name").value.trim(),url:$("permit-url").value.trim(),modes:permitModes,allowed_ips:$("permit-cidrs").value.split(/\r?\n/).map(x=>x.trim()).filter(Boolean),authorization:$("permit-evidence").value.trim(),expires_at:$("permit-expiry").value.trim(),max_concurrency:number("permit-concurrency",1,state.info?.limits?.authorized?.max_concurrency||1000,"授权并发"),max_rps:number("permit-rate",1,state.info?.limits?.authorized?.max_start_rate||1000,"授权速率"),max_duration:number("permit-duration",1,300,"授权时长"),proxy_routes:[]};
      const proxyUrl=$("permit-proxy-url").value.trim(),proxyCidrs=$("permit-proxy-cidrs").value.split(/\r?\n/).map(x=>x.trim()).filter(Boolean),proxyEvidence=$("permit-proxy-evidence").value.trim();
      if(proxyUrl||proxyCidrs.length||proxyEvidence){if(!/^(http|socks5):\/\//.test(proxyUrl)||!proxyCidrs.length||!proxyEvidence)throw new Error("请完整填写 HTTP/SOCKS5 代理 URL、代理 CIDR 和授权依据。");record.proxy_routes=[{url:proxyUrl,allowed_ips:proxyCidrs,authorization:proxyEvidence}];}
      if(!record.name||!record.authorization||!record.allowed_ips.length||!Number.isFinite(Date.parse(record.expires_at)))throw new Error("请完整填写名称、IP/CIDR、授权依据及含时区到期时间。");
      state.permitBusy=true;state.permitDraft=null;renderControls();permitControls();const result=await request("workspace",{operation:"authorization.prepare",payload:{targets:[...persistentPermits(),record]}});
      if(!result||typeof result.draftToken!=="string"||!Array.isArray(result.targets)||!Number.isSafeInteger(result.revision)||!Number.isFinite(result.expiresIn)||result.expiresIn<=0)throw new Error("授权预览格式无效，不能确认。");
      if(result.revision!==state.info.auth_revision)throw new Error("授权配置已变，请先重新读取，再登记；未保存任何授权。");
      state.permitDraft={...result,deadline:Date.now()+result.expiresIn*1000};$("permit-preview-text").textContent=JSON.stringify(result.targets,null,2);$("permit-side-effects").textContent=(result.sideEffects||[]).join("；");$("permit-preview").hidden=false;
    }catch(e){message("permit-error",errorText(e));}finally{state.permitBusy=false;renderControls();permitControls();}
  }
  async function confirmPermit(){
    const draft=state.permitDraft;if(!draft||locked()||Date.now()>=draft.deadline){message("permit-error","预览已过期或当前不可修改，请重新预览。");return;}
    state.permitBusy=true;state.permitUnknown=true;state.permitDraft=null;renderControls();permitControls();
    try{await request("workspace",{operation:"authorization.confirm",payload:{draftToken:draft.draftToken,revision:draft.revision}});await info();state.permitUnknown=false;$("permit-preview").hidden=true;message("permit-error","");renderPermits();renderTargets();}
    catch(e){message("permit-error",`${errorText(e)} 不会重发确认。请重新读取授权后核对。`);}
    finally{state.permitBusy=false;renderControls();permitControls();}
  }
  async function reloadPermits(){
    if(state.permitBusy||state.nativeWrites)return;state.permitBusy=true;state.permitDraft=null;renderControls();permitControls();
    try{await request("workspace",{operation:"authorization.reload",payload:{}});await info();if(state.info.authorization_error)throw new Error(state.info.authorization_error);state.permitUnknown=false;$("permit-preview").hidden=true;message("permit-error","");renderPermits();renderTargets();}catch(e){message("permit-error",errorText(e));}finally{state.permitBusy=false;renderControls();permitControls();}
  }
  window.siteCatalog.subscribe(snapshot=>{state.catalog=snapshot?.version===2&&Array.isArray(snapshot.applications)?snapshot:null;if(state.catalog){state.groups=new Set([...state.groups].filter(id=>snapshot.groups.some(g=>g.id===id)));state.bindings=new Map([...state.bindings].filter(([id])=>snapshot.sites.some(s=>s.id===id)));state.excludedApplications=new Set([...state.excludedApplications].filter(key=>snapshot.groups.some(g=>g.application_ids.some(id=>`${g.id}/${id}`===key))));}state.preview=null;renderGroups();renderControls();renderTargets();});
  window.appShell.subscribe(source=>{if(source!=="batch")renderControls();});
  window.addEventListener("aonet-proxy",render);
  $("batch-form").addEventListener("submit",start);$("batch-stop").addEventListener("click",()=>void stop());$("batch-reconnect").addEventListener("click",()=>void connect());$("batch-refresh-catalog").addEventListener("click",()=>void window.siteCatalog.reload());
  for(const input of document.querySelectorAll('input[name="batch-mode"]'))input.addEventListener("change",()=>{state.preview=null;state.bindings.clear();message("batch-error","");renderGroups();render();renderTargets();});
  for(const id of ["batch-concurrency","batch-connections","batch-queue","batch-attempts","batch-rate","batch-timeout","batch-duration","batch-per-site","batch-hold"])$(id).addEventListener("input",()=>{state.preview=null;renderControls();});
  $("batch-confirm-cancel").addEventListener("click",()=>{state.preview=null;$("batch-confirm-dialog").close();renderControls();});$("batch-confirm-start").addEventListener("click",()=>{if(!state.preview||locked())return;const body={...state.preview,confirmed_authorized:true};$("batch-confirm-dialog").close();void execute(body);});
  $("batch-confirm-dialog").addEventListener("cancel",()=>{state.preview=null;renderControls();});
  $("batch-authorizations").addEventListener("click",()=>{if(locked())return;renderPermits();permitControls();$("permit-dialog").showModal();});
  $("permit-form").addEventListener("submit",event=>void preparePermit(event));$("permit-confirm").addEventListener("click",()=>void confirmPermit());$("permit-reload").addEventListener("click",()=>void reloadPermits());
  $("permit-discard").addEventListener("click",()=>{state.permitDraft=null;$("permit-preview").hidden=true;});$("permit-close").addEventListener("click",()=>{if(state.permitBusy||state.nativeWrites)return;state.permitDraft=null;$("permit-preview").hidden=true;$("permit-dialog").close();});$("permit-dialog").addEventListener("cancel",event=>{if(state.permitBusy||state.nativeWrites)event.preventDefault();else state.permitDraft=null;});
  window.addEventListener("pagehide",()=>{state.epoch++;clearTimeout(state.timer);});
  render();void connect();
})();
