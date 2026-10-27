"use strict";
// Offline behavioral tests only. All transport calls are mocked.
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const source=fs.readFileSync(`${__dirname}/app.js`,"utf8");
const transportSource=fs.readFileSync(`${__dirname}/transport.js`,"utf8");
const id="00000000-0000-4000-8000-000000000001";
const secondId="00000000-0000-4000-8000-000000000002";
const plain=value=>JSON.parse(JSON.stringify(value));
const flush=()=>new Promise(resolve=>setImmediate(resolve));
function deferred(){let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};}
function capability(){return {enabled:true,limits:{dns_timeout_secs:5,route_timeout_secs:30,tcp_timeout_secs:5,tls_timeout_secs:5,max_hops:20,max_output_bytes:65536,start_interval_secs:5,host_interval_secs:30},trust_source:"Mozilla, not system roots",self_test_targets:[]};}
function snapshot(status="running",requestId=id){return {id:requestId,request_id:requestId,status,config:{request_id:requestId,target:"service.test",port:443,tls:true,allow_lan:false},selected_ip:null,started_at:"2026-10-27T00:00:00Z",updated_at:"2026-10-27T00:00:00Z",finished_at:["completed","cancelled","failed"].includes(status)?"2026-10-27T00:00:10Z":null,warnings:[],limitations:[],stages:Object.fromEntries(["dns","route","tcp","tls"].map(name=>[name,{status:"pending",elapsed_ms:null,error:null,observations:{}}]))};}
function harness(request=async()=>null){
  const nodes=new Map(),calls=[],timers=new Map();let timer=0;
  const node=id=>{if(!nodes.has(id))nodes.set(id,{value:"",checked:false,dataset:{},textContent:"",hidden:false,disabled:false,children:[],attributes:{},classList:{add(){},toggle(){}},style:{setProperty(){}},replaceChildren(...values){this.children=values;},append(...values){this.children.push(...values);},querySelectorAll(){return [];},setAttribute(key,value){this.attributes[key]=value;},focus(){},addEventListener(){},close(){this.open=false;}});return nodes.get(id);};
  const context=vm.createContext({window:{AoNetTransport:{isDesktop:true,request:async(path,options)=>{calls.push({path,options});return request(path,options);}}},document:{getElementById:node,createElement:()=>node(Symbol()),querySelectorAll:()=>[]},TextEncoder,URL,Error,TypeError,Date,console,crypto:{randomUUID:()=>id},location:{hash:""},setTimeout:(fn,delay)=>{timers.set(++timer,{fn,delay});return timer;},clearTimeout:key=>timers.delete(key)});
  vm.runInContext(source.slice(0,source.indexOf('  $("diagnostic-form").addEventListener'))+`
    function updateControls() { updateDiagnosticControls(); }
    function closeDialog() {}
    window.subject={state,validDiagnostic,validDiagnosticConfig,validateDiagnosticCapability,diagnosticDraft,diagnosticDraftError,diagnosticEndpointDraft,buildDiagnosticChoices,diagnosticConclusion,fillDiagnosticDraft,renderDiagnostic,renderDiagnosticCapability,activeDiagnostic,locked,startDiagnostic,stopDiagnostic,fetchDiagnosticState,startDiagnosticPolling,stopDiagnosticPolling,configError,workspace};
  })();`,context);
  const s=context.window.subject;s.state.bootstrap={diagnostics:capability(),authorized:[],lists:[]};s.state.ready=s.state.stateKnown=s.state.diagnosticKnown=s.state.batchKnown=true;s.state.monitorKnown=true;
  node("diagnostic-target").value="service.test";node("diagnostic-port").value="443";node("diagnostic-tls").value="true";
  return {...s,node,calls,timers,context};
}
function text(node){return [node.textContent,...node.children.map(text)].join(" ");}
test("search combines clearly labelled homepage and concrete connection references without requests",()=>{
  const h=harness();const data={lists:[{name:"中国航司",builtin:true,targets:[{id:"air-china",name:"中国国际航空",url:"https://airchina.test/"}]}],connection_presets:[
    {provider:"Travelport",product:"Cloud",environment:"生产",endpoints:[{kind:"url",address:"https://sp.test/",port:443,purpose:"浏览器入口"},{kind:"wildcard",address:"*.test",port:443,purpose:"放行规则"}]},
    {provider:"Sabre",product:"Workspace",endpoints:[{kind:"host",address:null,port:null,purpose:"业务连接"}],sources:[{url:"https://source.test"}]}
  ]};
  const air=h.buildDiagnosticChoices(data,"国航");assert.equal(air.length,1);assert.match(air[0].label,/官网入口/);assert.equal(air[0].draft.tls,"");
  const cloud=h.buildDiagnosticChoices(data,"Travelport Cloud");assert.equal(cloud.length,1);assert.match(cloud[0].label,/生产/);assert.equal(cloud[0].draft.target,"https://sp.test/");
  const sabre=h.buildDiagnosticChoices(data,"Sabre");assert.equal(sabre.length,1);assert.equal(sabre[0].draft,null);assert.match(sabre[0].label,/待提供/);assert.equal(h.calls.length,0);
});
test("conclusions distinguish live status, DNS blocking, route gaps, TCP and TLS outcomes",()=>{
  const h=harness(),s=snapshot("completed");
  assert.match(h.diagnosticConclusion(null,true,true,null).title,/选择目标/);
  assert.match(h.diagnosticConclusion(null,false,false,null).title,/正在连接/);
  assert.match(h.diagnosticConclusion(s,false,false,null).title,/未确认/);
  assert.match(h.diagnosticConclusion(s,true,true,{request_id:id}).title,/不确定/);
  assert.match(h.diagnosticConclusion(snapshot("draining"),true,true,null).title,/收尾/);
  assert.match(h.diagnosticConclusion(snapshot("cancelled"),true,true,null).title,/取消/);
  s.stages.dns.status="blocked";assert.match(h.diagnosticConclusion(s,true,true,null).title,/安全范围/);
  s.stages.dns.status="error";assert.match(h.diagnosticConclusion(s,true,true,null).title,/地址/);
  s.stages.dns.status="success";s.stages.tcp.status="error";assert.match(h.diagnosticConclusion(s,true,true,null).title,/TCP/);
  s.stages.tcp.status="success";s.stages.tls.status="error";let result=h.diagnosticConclusion(s,true,true,null);assert.match(result.title,/TCP 已连接，TLS 未通过/);assert.match(result.next,/不要关闭/);
  s.stages.route={status:"timeout",elapsed_ms:1,error:null,observations:{outcome:"timeout"}};s.stages.tls.status="skipped";result=h.diagnosticConclusion(s,true,true,null);assert.match(result.title,/TLS 未测试/);assert.match(result.detail,/不代表登录/);assert.match(result.detail,/路由未完整/);
  s.stages.tls.status="success";result=h.diagnosticConclusion(s,true,true,null);assert.match(result.title,/TLS 身份验证通过/);assert.doesNotMatch(JSON.stringify(result),/业务正常|故障在第/);
  s.selected_ip="127.0.0.1";result=h.diagnosticConclusion(s,true,true,null);assert.match(result.detail,/不经过办公网络出口/);assert.match(result.next,/实际业务目标/);
});
test("strict snapshots distinguish null from absent and reject malformed identities/status/stages",()=>{
  const h=harness();assert.equal(h.validDiagnostic(null),true);assert.equal(h.validDiagnostic(undefined),false);assert.equal(h.validDiagnostic(snapshot()),true);
  for(const change of [s=>delete s.selected_ip,s=>delete s.finished_at,s=>delete s.stages.dns.elapsed_ms,s=>delete s.stages.tls.error,s=>s.status="done",s=>s.id="not-uuid",s=>s.config.request_id=secondId,s=>s.stages.dns.status="done",s=>s.stages.tcp.observations=[],s=>s.stages.route.observations={hops:[{ttl:1,addresses:[],rtt_ms:[-1],stars:1,raw:"*"}]}]){const s=snapshot();change(s);assert.equal(h.validDiagnostic(s),false);}
  assert.equal(h.validDiagnostic(snapshot("completed")),true);const s=snapshot("stopping");s.finished_at=s.started_at;assert.equal(h.validDiagnostic(s),false);
});
test("draft requires explicit TLS, LAN defaults false, rejects unsafe targets, never sends HTTP payload",()=>{
  const h=harness();h.node("diagnostic-tls").value="";assert.match(h.diagnosticDraftError(),/TLS/);h.node("diagnostic-tls").value="false";
  assert.deepEqual(plain(h.diagnosticDraft()),{target:"service.test",port:443,tls:false,allow_lan:false});assert.equal(h.diagnosticDraftError(),"");
  for(const target of ["*.test","10.1.0.0/16","https://user:pw@host.test/","https://host.test/?x=1","https://host.test/#secret","-option","host.test:443","fe80::1%en0"]){h.node("diagnostic-target").value=target;assert.notEqual(h.diagnosticDraftError(),"");}
  h.node("diagnostic-target").value="https://service.test/path";assert.equal(h.diagnosticDraftError(),"");assert.equal(h.node("diagnostic-tls").value,"false");assert.equal(h.calls.length,0);
});
test("catalog fills only concrete endpoint drafts, never source URLs or TLS inferred from 443",()=>{
  const h=harness();assert.equal(h.diagnosticEndpointDraft({kind:"wildcard",address:"*.test",port:443}),null);assert.equal(h.diagnosticEndpointDraft({kind:"host",address:null,port:443,source_url:"https://source.test"}),null);assert.equal(h.diagnosticEndpointDraft({kind:"host",address:"service.test",port:null}),null);
  const draft=h.diagnosticEndpointDraft({kind:"host",address:"service.test",port:443,protocol:"unknown"});assert.equal(draft.tls,"");
  h.fillDiagnosticDraft(draft);assert.equal(h.node("diagnostic-lan").checked,false);assert.equal(h.calls.length,0);assert.equal(h.node("diagnostic-tls").value,"");
  assert.equal(h.diagnosticEndpointDraft({kind:"url",address:"https://service.test/path",port:443}).tls,"");
});
test("load and diagnostics block each other's start/mutations including draining and unknown state",async()=>{
  const h=harness();h.state.run={status:"running"};assert.equal(h.locked(),true);await h.startDiagnostic({preventDefault(){}});assert.equal(h.calls.length,0);
  h.state.run=null;for(const status of ["running","stopping","draining"]){h.state.diagnostic=snapshot(status);assert.equal(h.activeDiagnostic(),true);assert.equal(h.locked(),true);await assert.rejects(h.workspace("selftest",{enabled:true}),/不能修改/);}
  h.state.diagnostic=null;h.state.diagnosticKnown=false;assert.equal(h.locked(),true);h.state.diagnosticKnown=true;h.state.diagnosticUncertain={request_id:id};assert.equal(h.locked(),true);
});
test("route raw evidence is text only and TLS failure preserves successful TCP",()=>{
  const h=harness(),s=snapshot("completed");s.stages.tcp={status:"success",elapsed_ms:0,error:null,observations:{remote_addr:"192.0.2.1:443"}};s.stages.tls={status:"error",elapsed_ms:2,error:"certificate mismatch",observations:{trust_source:"Mozilla"}};s.stages.route={status:"error",elapsed_ms:100,error:null,observations:{method:"UDP",outcome:"incomplete",raw_stdout:"<script>bad</script>",raw_stderr:"",hops:[{ttl:1,addresses:[],rtt_ms:[],stars:1,raw:"1 *"}],parse_incomplete:true,reached:false,truncated:false,exit_code:0}};
  assert.equal(h.validDiagnostic(s),true);h.state.diagnostic=s;h.renderDiagnostic();const output=text(h.node("diagnostic-stages"));assert.match(output,/TCP 建连/);assert.match(output,/成功 · 0 ms/);assert.match(output,/certificate mismatch/);assert.match(output,/<script>bad<\/script>/);assert.match(output,/解析不完整/);assert.equal(h.node("diagnostic-stages").innerHTML,undefined);
});
test("uncertain start submits once and reconciles only the same request UUID",async()=>{
  let result="uncertain";const h=harness(async path=>{if(path==="/api/diagnostics"){const error=new Error("unknown");error.uncertain=true;throw error;}if(result==="uncertain")return null;return snapshot("running",result);});
  await h.startDiagnostic({preventDefault(){}});assert.equal(h.state.diagnosticUncertain.request_id,id);await h.startDiagnostic({preventDefault(){}});assert.equal(h.calls.filter(c=>c.path==="/api/diagnostics").length,1);
  await h.fetchDiagnosticState(h.state.diagnosticGeneration,true);assert.ok(h.state.diagnosticUncertain,"null cannot prove an uncertain start was not executed");result=secondId;await h.fetchDiagnosticState(h.state.diagnosticGeneration,true);assert.ok(h.state.diagnosticUncertain);result=id;await h.fetchDiagnosticState(h.state.diagnosticGeneration,true);assert.equal(h.state.diagnosticUncertain,null);
  const body=JSON.parse(h.calls[0].options.body);assert.deepEqual(Object.keys(body).sort(),["allow_lan","port","request_id","target","tls"]);
});
test("diagnostic polling waits for actual read, drops old generations and rejects missing snapshots",async()=>{
  const pending=[];const h=harness(()=>{const p=deferred();pending.push(p);return p.promise;});
  const old=h.fetchDiagnosticState();await flush();h.stopDiagnosticPolling();const fresh=h.fetchDiagnosticState();await flush();assert.equal(pending.length,1);pending[0].resolve(snapshot("running"));await old;await flush();assert.equal(h.state.diagnostic,null);assert.equal(pending.length,2);pending[1].resolve(snapshot("completed"));await fresh;assert.equal(h.state.diagnostic.status,"completed");
  const invalid=assert.rejects(h.fetchDiagnosticState(),/格式无效/);await flush();pending[2].resolve(undefined);await invalid;assert.equal(h.state.diagnostic.status,"completed");assert.equal(h.state.diagnosticKnown,false);
});
test("stop uses diagnostic UUID and stopping is not treated as completion",async()=>{
  const h=harness(async()=>snapshot("stopping"));h.state.diagnostic=snapshot();await h.stopDiagnostic();assert.equal(h.calls[0].path,"/api/diagnostics/stop");assert.deepEqual(JSON.parse(h.calls[0].options.body),{id});assert.equal(h.activeDiagnostic(),true);assert.equal(h.state.diagnostic.finished_at,null);await h.stopDiagnostic();assert.equal(h.calls.length,1);
});
test("capability absent is supported without diagnosis; selftest grants only supplied loopback draft",()=>{
  const h=harness();assert.doesNotThrow(()=>h.validateDiagnosticCapability(undefined));delete h.state.bootstrap.diagnostics;h.renderDiagnosticCapability();assert.match(h.node("diagnostic-capability").textContent,/不支持/);assert.equal(h.node("diagnostic-start").disabled,true);assert.equal(h.calls.length,0);
  const cap=capability();cap.self_test_targets=[{name:"fixture",target:"127.0.0.1",port:9999,tls:false}];assert.doesNotThrow(()=>h.validateDiagnosticCapability(cap));cap.self_test_targets[0].target="192.168.1.1";assert.throws(()=>h.validateDiagnosticCapability(cap),/自检目标/);
});
test("pagehide generation discards a late start snapshot and does not restart polling",async()=>{
  const pending=deferred(),h=harness(()=>pending.promise);
  const started=h.startDiagnostic({preventDefault(){}});await flush();h.state.diagnosticPaused=true;h.stopDiagnosticPolling();pending.resolve(snapshot());await started;
  assert.equal(h.state.diagnostic,null);assert.equal(h.state.diagnosticTimer,null);assert.equal(h.timers.size,0);
});
test("TLS absent negotiated values stay null and skipped stages do not invent route evidence",()=>{
  const h=harness(),s=snapshot("completed");s.stages.tls={status:"skipped",elapsed_ms:null,error:null,observations:{reason:"TLS disabled",protocol:null,cipher:null}};s.stages.route={status:"unsupported",elapsed_ms:null,error:null,observations:{method:"none",outcome:"unsupported"}};
  assert.equal(h.validDiagnostic(s),true);s.stages.route.observations.outcome="definitely broken router";assert.equal(h.validDiagnostic(s),false);
});
test("proxy diagnostic conclusions identify tunnels rather than direct target connections",()=>{
  const h=harness(),s=snapshot("completed");s.config.proxy={url:"http://127.0.0.1:7890"};s.stages.dns.status="success";s.stages.tcp.status="success";s.stages.tls.status="success";
  const conclusion=h.diagnosticConclusion(s,true,true,null);assert.match(conclusion.title,/CONNECT/);assert.match(conclusion.detail,/无法独立确认/);assert.doesNotMatch(conclusion.title,/TCP 已连接/);
  s.stages.tcp.status="error";assert.match(h.diagnosticConclusion(s,true,true,null).detail,/未回退直连/);
});
test("native proxy requests preserve settings and refuse unsupported credential fields",async()=>{
  const calls=[],window={__TAURI__:{core:{invoke:async(command,args)=>{calls.push({command,args});return true;}}}};
  vm.runInNewContext(transportSource,{window,TextEncoder,setTimeout,clearTimeout,URL});const t=window.AoNetTransport,proxy={url:"http://127.0.0.1:7890"};
  await t.request('/api/diagnostics',{method:'POST',body:JSON.stringify({...snapshot().config,proxy})});
  await t.request('/api/batch',{method:'POST',body:JSON.stringify({request_id:id,list_ids:['x'],target_ids:['x/y'],concurrency:1,rounds:1,timeout_secs:20,route:'proxy',proxy})});
  await t.exportDialog('batch',id);assert.deepEqual(plain(calls[0].args.config.proxy),proxy);assert.deepEqual(plain(calls[1].args.request.proxy),proxy);assert.equal(calls[2].args.kind,'batch');
  await assert.rejects(t.request('/api/diagnostics',{method:'POST',body:JSON.stringify({...snapshot().config,proxy:{url:'http://user:pass@proxy.test:8080'}})}));
});
test("native diagnostics use narrow commands, UUID export, and serialize actual IPC after timeout",async()=>{
  const pending=deferred(),calls=[],timers=[];let reads=0;
  const window={__TAURI__:{core:{invoke:(command,args)=>{calls.push({command,args});if(command==="diagnostic_state")return ++reads===1?pending.promise:null;return true;}}}};
  vm.runInNewContext(transportSource,{window,TextEncoder,Error,TypeError,setTimeout:fn=>{timers.push(fn);return timers.length;},clearTimeout(){}});
  const transport=window.AoNetTransport;
  const first=assert.rejects(transport.request("/api/diagnostics/state"),/超时/);timers[0]();await first;
  const second=transport.request("/api/diagnostics/state");await flush();assert.equal(reads,1);pending.resolve(null);await second;assert.equal(reads,2);
  await transport.request("/api/diagnostics",{method:"POST",body:JSON.stringify(snapshot().config)});await transport.request("/api/diagnostics/stop",{method:"POST",body:JSON.stringify({id})});await transport.exportDialog("diagnostic",id);
  assert.deepEqual(plain(calls.slice(2)),[{command:"start_diagnostic",args:{config:snapshot().config}},{command:"stop_diagnostic",args:{id}},{command:"export_dialog",args:{kind:"diagnostic",id}}]);
  await assert.rejects(transport.exportDialog("diagnostic"),/无效/);await assert.rejects(transport.request("/api/diagnostics",{method:"POST",body:JSON.stringify({...snapshot().config,allow_loopback:true})}),/配置无效/);await assert.rejects(transport.request("/api/diagnostics/report?id="+id),/不支持/);
});
