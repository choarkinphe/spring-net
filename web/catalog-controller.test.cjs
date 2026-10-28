"use strict";
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const source=fs.readFileSync(`${__dirname}/catalog-controller.js`,"utf8");
function snapshot(revision=4){
  const rule={id:"r",kind:"unknown",address:null,transport:null,protocol:null,ports:"dynamic",purpose:"login",evidence:"",notes:"",source_ids:[]};
  const site={id:"s",name:"Connection",address:"host.invalid",port:443,transport:"tcp",protocol:"tls",purpose:"auth",alpn:null,builtin:false,source:"",checked_at:"",capabilities:["tls"],unavailable_reasons:[]};
  const app={id:"a",name:"Application",kind:"application",category:"custom",provider:"",product:"",aliases:[],access_type:"",site_ids:["s"],reference_rules:[rule],tag_ids:["t"],supplement:null,builtin:false,sources:[],missing:[],limitations:[],summary:"",status:"",environment:null,version:null};
  return {version:3,revision,sites:[site],applications:[app],groups:[{id:"g",name:"Group",description:"",tag_ids:["t"],application_ids:["a"],builtin:false}],tags:[{id:"t",name:"Tag",builtin:false}],tag_assignments:[{resource_id:"a",tag_ids:["t"]}],resource_supplements:[]};
}
function harness(invoke=async()=>snapshot()){
  const window={__TAURI__:{core:{invoke}},appShell:{setLocked(){},subscribe(){},isLockedExcept(){return false;}}};
  const context=vm.createContext({window,console,Error,Set,Map,JSON,Object,Array,String,Number,Boolean,AbortController,setTimeout,clearTimeout});
  vm.runInContext(source,context);const c=window.catalogController;c.state.snapshot=snapshot();return {c,window,context};
}
const plain=x=>JSON.parse(JSON.stringify(x));
test("v3 validates flat metadata and rejects old, incomplete and dangling snapshots",()=>{
  const {c}=harness();assert.equal(c.valid(snapshot()),true);
  for(const edit of [s=>s.version=2,s=>delete s.tag_assignments,s=>delete s.applications[0].kind,s=>delete s.applications[0].supplement,s=>s.groups[0].tag_ids.push("missing"),s=>s.applications[0].site_ids.push("missing"),s=>s.tags[0].id="a",s=>s.resource_supplements.push({resource_id:"a",site_ids:["s"],reference_rules:[]})]){const s=snapshot();edit(s);assert.equal(c.valid(s),false);}
});
test("strict writable projection excludes runtime fields and persists all six arrays",()=>{
  const {c}=harness(),data=c.custom(snapshot());
  assert.deepEqual(Object.keys(data),["sites","applications","groups","tags","tag_assignments","resource_supplements"]);
  assert.deepEqual(Object.keys(data.groups[0]),["id","name","description","tag_ids"]);
  assert.deepEqual(Object.keys(data.applications[0]),["id","name","kind","category","provider","product","aliases","access_type","site_ids","reference_rules"]);
  assert.deepEqual(Object.keys(data.sites[0]),["id","name","address","port","transport","protocol","purpose","alpn"]);
  assert.deepEqual(Object.keys(data.tags[0]),["id","name"]);
  for(const key of ["builtin","summary","sources","tag_ids","supplement"])assert.equal(key in data.applications[0],false);
});
test("builtin supplements and assignments are writable but official resources are never projected",()=>{
  const s=snapshot();s.applications[0].builtin=true;s.applications[0].supplement={resource_id:"a",site_ids:["s"],reference_rules:[]};s.resource_supplements=[s.applications[0].supplement];s.tags[0].builtin=true;s.groups[0].builtin=true;
  const {c}=harness(),data=c.custom(s);assert.equal(c.valid(s),true);assert.equal(data.applications.length,0);assert.equal(data.groups.length,0);assert.equal(data.tags.length,0);assert.equal(data.tag_assignments.length,1);assert.equal(data.resource_supplements.length,1);assert.equal(data.sites.length,1);
});
test("singleton initialization and concurrent reload use exactly one read",async()=>{
  let calls=0,resolve;const {c,window,context}=harness(()=>{calls++;return new Promise(r=>resolve=r);});
  vm.runInContext(source,context);assert.equal(c,window.catalogController);
  const a=c.load(),b=window.siteCatalog.reload();assert.equal(a,b);await Promise.resolve();assert.equal(calls,1);resolve(snapshot(5));await a;assert.equal(c.state.snapshot.revision,5);
});
test("subscribe clones snapshots, immediately notifies and unsubscribe removes observer",async()=>{
  const {window}=harness();const received=[];const off=window.siteCatalog.subscribe(s=>received.push(s));assert.equal(received.length,1);received[0].applications.length=0;assert.equal(window.siteCatalog.snapshot().applications.length,1);await window.siteCatalog.reload();assert.equal(received.length,2);off();await window.siteCatalog.reload();assert.equal(received.length,2);
});
test("lost save response is reconciled by one GET, never replayed",async()=>{
  const calls=[];let request;const {c}=harness(async(command,args)=>{calls.push(command);if(command==="save_site_catalog"){request=args.request;throw Error("lost");}return snapshot(5);});
  await c.save(c.custom(),4);assert.deepEqual(calls,["save_site_catalog","site_catalog"]);assert.deepEqual(Object.keys(request),["schema_version","expected_revision","sites","applications","groups","tags","tag_assignments","resource_supplements"]);assert.equal(request.schema_version,3);assert.equal(request.expected_revision,4);assert.equal(c.state.unknown,false);
});
test("unreadable ambiguous save locks both views until a successful explicit read",async()=>{
  let fail=true;const calls=[];const {c,window}=harness(async command=>{calls.push(command);if(command==="save_site_catalog"||fail)throw Error("offline");return snapshot(5);});
  await assert.rejects(c.save(c.custom(),4),/offline/);assert.equal(c.state.unknown,true);assert.equal(c.blocked(),true);await assert.rejects(c.save(c.custom(),4),/重新读取/);assert.equal(calls.length,2);fail=false;await window.siteCatalog.reload();assert.equal(c.state.unknown,false);assert.equal(c.blocked(),false);
});
test("stale revision, locked state and incomplete DTO cannot issue writes",async()=>{
  const calls=[];const {c}=harness(async command=>{calls.push(command);return snapshot(5);});await assert.rejects(c.save(c.custom(),3),/重新打开编辑/);await assert.rejects(c.save({sites:[]},4),/完整 v3/);c.state.locked=true;await assert.rejects(c.save(c.custom(),4),/等待检测/);assert.deepEqual(calls,[]);
});
test("save is globally singleflight and input is copied at boundary",async()=>{
  let release,request;const {c}=harness((command,args)=>{if(command==="save_site_catalog"){request=plain(args.request);return new Promise(r=>release=r);}return snapshot(5);});const data=c.custom();const first=c.save(data,4);data.applications[0].name="mutated";await assert.rejects(c.save(data,4));assert.equal(request.applications[0].name,"Application");release(snapshot(5));await first;assert.equal(c.state.saving,false);
});
test("reconciled differing server revision invalidates stale editor instead of claiming success",async()=>{
  const {c}=harness(async command=>{if(command==="save_site_catalog")throw Error("CAS conflict");return snapshot(8);});await assert.rejects(c.save(c.custom(),4),/未重发保存/);assert.equal(c.state.snapshot.revision,8);assert.equal(c.state.unknown,false);
});
test("canonical comparison ignores collection ordering but detects writable changes",()=>{const {c}=harness(),a=c.custom(),b=plain(a);b.applications[0].site_ids.reverse();assert.equal(c.sameCustom(a,b),true);b.sites[0].purpose="changed";assert.equal(c.sameCustom(a,b),false);});

test("HTTP transport uses same-origin token and complete v3 PUT, no implicit execution",async()=>{
 const calls=[],window={networkToolDraft:{isLocked:()=>false}};
 const fetch=async(url,options)=>{calls.push({url,options});if(url==="/api/network-tools/info")return {ok:true,json:async()=>({token:"session-token"})};return {ok:true,text:async()=>JSON.stringify(snapshot(options.method==="PUT"?5:4))};};
 const context=vm.createContext({window,fetch,console,Error,Set,Map,JSON,Object,Array,String,Number,Boolean,AbortController,setTimeout,clearTimeout});vm.runInContext(source,context);await window.siteCatalog.reload();await window.catalogController.save(window.catalogController.custom(),4);
 assert.deepEqual(calls.map(c=>[c.url,c.options.method]),[["/api/site-catalog","GET"],["/api/network-tools/info",undefined],["/api/site-catalog","PUT"]]);assert.equal(calls[2].options.headers["X-AoNet-Token"],"session-token");assert.equal(JSON.parse(calls[2].options.body).schema_version,3);assert.ok(calls.every(c=>c.options.credentials==="same-origin"&&c.options.cache==="no-store"));
});
test("failed initial read remains fail-closed and rejects v2 fallback without mutating old snapshot",async()=>{
 const invalid=snapshot();invalid.version=2;const {c}=harness(async()=>invalid);c.state.snapshot=null;await c.load();assert.equal(c.state.snapshot,null);assert.equal(c.blocked(),true);assert.match(c.state.error,/v3/);
});
