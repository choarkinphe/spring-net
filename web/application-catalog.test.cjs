"use strict";
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const source=fs.readFileSync(`${__dirname}/application-catalog.js`,"utf8");
const css=fs.readFileSync(`${__dirname}/application-catalog.css`,"utf8");
function harness() {
  const context=vm.createContext({window:{},document:{getElementById(){return null;}},console,Error,Set,Map,JSON,Object,Array,String,Number,Boolean,AbortController,setTimeout,clearTimeout});
  const marker="  if (!mount()) return;";
  assert.ok(source.includes(marker));
  vm.runInContext(`${source.slice(0,source.indexOf(marker))}window.__test={state,custom,valid,sameCustom,deleteData,filteredApps,siteFields,ruleFields,appFields,groupFields};})();`,context);
  return context.window.__test;
}
function ioHarness(invoke) {
  const context=vm.createContext({window:{__TAURI__:{core:{invoke}}},document:{getElementById(){return null;}},console,Error,Set,Map,JSON,Object,Array,String,Number,Boolean,AbortController,setTimeout,clearTimeout});
  const marker="  if (!mount()) return;";
  vm.runInContext(`${source.slice(0,source.indexOf(marker))}render=()=>{};ui.error={};ui.notice={};window.__test={state,custom,save,load};})();`,context);
  const h=context.window.__test;h.state.snapshot=snapshot();h.state.locked=false;return h;
}
function snapshot(revision=4) {
  const builtinSite={id:"bs",name:"Preset endpoint",address:"https://preset.invalid/",port:null,transport:"tcp",protocol:"https",purpose:"portal",alpn:null,builtin:true,source:"catalog",checked_at:"2026-10-27",capabilities:["http","tls"],unavailable_reasons:[]};
  const customSite={id:"cs",name:"Shared endpoint",address:"host.invalid",port:443,transport:"tcp",protocol:"tls",purpose:"auth",alpn:null,builtin:false,source:"",checked_at:"",capabilities:["tls"],unavailable_reasons:[]};
  const builtinApp={id:"ba",name:"Preset app",category:"web",provider:"Vendor",product:"Cloud",aliases:[],access_type:"browser",site_ids:["bs"],reference_rules:[],builtin:true,sources:[],missing:[],limitations:[],summary:"Preset summary",status:"supported",environment:null,version:null};
  const customApp={id:"ca",name:"My app",category:"custom",provider:"",product:"",aliases:["alias"],access_type:"",site_ids:["cs"],reference_rules:[{id:"r1",kind:"unknown",address:null,transport:null,protocol:null,ports:"dynamic",purpose:"login",evidence:"",notes:"",source_ids:[]}],builtin:false,sources:[],missing:[],limitations:[],summary:"",status:"",environment:null,version:null};
  const builtinGroup={id:"bg",name:"Preset group",description:"",application_ids:["ba"],builtin:true};
  const customGroup={id:"cg",name:"My group",description:"",application_ids:["ca"],builtin:false};
  return {version:2,revision,sites:[builtinSite,customSite],applications:[builtinApp,customApp],groups:[builtinGroup,customGroup]};
}
function domHarness() {
  const nodes=new Map(),calls=[],events={};let title;
  const document={activeElement:null};
  function element(tag="div") {
    const node={tagName:tag.toUpperCase(),value:"",textContent:"",hidden:false,disabled:false,open:false,dataset:{},children:[],attributes:{},handlers:{},classList:{add(){}},append(...children){this.children.push(...children)},replaceChildren(...children){this.children=children},addEventListener(type,fn){this.handlers[type]=fn},setAttribute(key,value){this.attributes[key]=value},focus(){document.activeElement=this},contains(target){return this===target||this.children.some(c=>c.contains?.(target))},querySelector(){return this.children.find(c=>["INPUT","SELECT","TEXTAREA"].includes(c.tagName))||this.children.map(c=>c.querySelector?.()).find(Boolean)},showModal(){this.open=true},close(){this.open=false},isConnected:true};
    Object.defineProperty(node,"id",{set(id){this._id=id;nodes.set(id,this)},get(){return this._id}});return node;
  }
  document.createElement=element;document.getElementById=id=>nodes.get(id)||null;
  for(const id of ["catalog-page","catalog-title","quick-group","quick-app","quick-site","apply-site"]) {const n=element(id.startsWith("quick-")?"select":"div");n.id=id;if(id==="catalog-title")title=n;}
  let locked=false;
  const window={__TAURI__:{core:{invoke:async(command,args)=>{calls.push({command,args});return snapshot()}}},appShell:{setLocked(){},isLockedExcept(){return locked},subscribe(){},show(page){calls.push({page})}},networkToolDraft:{applySite(site){calls.push({draft:site});return true},applyEndpoint(site,tool){calls.push({draft:site,tool});return true}},addEventListener(type,fn){events[type]=fn}};
  const context=vm.createContext({window,document,console,Error,Set,Map,JSON,Object,Array,String,Number,Boolean,AbortController,setTimeout,clearTimeout,crypto:{randomUUID:()=>"test-uuid"}});
  vm.runInContext(source,context);
  const descendants=(node)=>[node,...node.children.flatMap(descendants)];
  return {window,nodes,calls,events,title,descendants,ready:()=>new Promise(resolve=>setImmediate(resolve))};
}
test("mount preserves catalog-title, creates accessible plus actions, and never starts detection",async()=>{
  const h=domHarness();await h.ready();assert.equal(h.nodes.get("catalog-title"),h.title);assert.equal(h.title.textContent,"应用管理");
  for(const [id,label] of [["ac-new-group","新增组合"],["ac-new-application","新增应用"],["ac-new-site","新增端点"]]){const node=h.nodes.get(id);assert.equal(node.textContent,"+");assert.equal(node.title,label);assert.equal(node.attributes["aria-label"],label);}
  assert.equal(h.calls.length,1);assert.equal(h.calls[0].command,"site_catalog");assert.equal(h.window.siteCatalog.snapshot().version,2);
  const snap=h.window.siteCatalog.snapshot();snap.applications.pop();assert.equal(h.window.siteCatalog.snapshot().applications.length,2);
});
test("browsing and category switch clear search while detection lock blocks mutation and apply",async()=>{
  const h=domHarness();await h.ready();h.window.siteCatalog.setLocked(true);
  const nav=()=>h.descendants(h.nodes.get("catalog-page")).filter(n=>n.dataset.selection!==undefined);
  h.nodes.get("ac-search").value="unmatched";
  const category=nav().find(n=>n.dataset.selection==="category:custom");assert.equal(category.disabled,false);category.handlers.click();
  assert.equal(h.nodes.get("ac-search").value,"");assert.equal(h.nodes.get("ac-new-application").disabled,true);
  h.nodes.get("quick-app").value="ca";h.nodes.get("quick-app").handlers.change();h.nodes.get("quick-site").value="cs";h.nodes.get("quick-site").handlers.change();
  assert.equal(h.nodes.get("apply-site").disabled,true);h.nodes.get("apply-site").handlers.click();assert.equal(h.calls.length,1);
  h.window.siteCatalog.setLocked(false);assert.equal(h.nodes.get("apply-site").disabled,false);h.nodes.get("apply-site").handlers.click();assert.equal(h.calls[1].draft.id,"cs");assert.equal(h.calls[2].page,"tools");
});
test("application additions are accessible plus actions and endpoint copy is independent",async()=>{
 const h=domHarness();await h.ready();const all=()=>h.descendants(h.nodes.get("catalog-page"));
 for(const label of ["新增端点","新增参考规则"]){const nodes=all().filter(n=>n.title===label);assert.ok(nodes.length>0);for(const node of nodes){assert.equal(node.textContent,"+");assert.equal(node.attributes["aria-label"],label);}}
 const copy=all().find(n=>n.textContent==="复制端点");assert.ok(copy);copy.handlers.click();assert.equal(h.nodes.get("ac-editor-title").textContent,"复制具体端点");
 const name=all().find(n=>n.name==="name"),address=all().find(n=>n.name==="address");assert.match(name.value,/副本/);assert.equal(address.value,"https://preset.invalid/");name.value="Independent";assert.equal(h.window.siteCatalog.snapshot().sites[0].name,"Preset endpoint");assert.equal(h.calls.length,1);assert.equal(all().some(n=>n.textContent==="复制组合"),false);
});
test("subscription immediately clones v2 snapshot and unsubscribe stops notifications",async()=>{
  const h=domHarness();await h.ready();const received=[];const unsubscribe=h.window.siteCatalog.subscribe(s=>received.push(s));assert.equal(received.length,1);received[0].groups.length=0;assert.equal(h.window.siteCatalog.snapshot().groups.length,2);await h.window.siteCatalog.reload();assert.equal(received.length,2);unsubscribe();await h.window.siteCatalog.reload();assert.equal(received.length,2);
});
test("v2 snapshot validates flattened endpoint/application/group metadata",()=>{const h=harness();assert.equal(h.valid(snapshot()),true);const old=snapshot();old.version=1;assert.equal(h.valid(old),false);const bad=snapshot();bad.applications[1].site_ids=["missing"];assert.equal(h.valid(bad),false);const duplicate=snapshot();duplicate.groups[1].id="ca";assert.equal(h.valid(duplicate),false);});
test("write DTO is strict custom-only v2 and retains references, not metadata",()=>{const h=harness(),data=h.custom(snapshot());assert.deepEqual(Object.keys(data),["sites","applications","groups"]);assert.deepEqual(Object.keys(data.sites[0]),["id","name","address","port","transport","protocol","purpose","alpn"]);assert.equal(data.applications.length,1);assert.equal(data.groups.length,1);assert.equal(data.applications[0].reference_rules[0].source_ids.length,0);assert.equal(data.applications[0].site_ids[0],data.sites[0].id);assert.equal(data.groups[0].application_ids[0],data.applications[0].id);assert.equal("builtin" in data.applications[0],false);assert.equal("summary" in data.applications[0],false);});
test("save sends strict v2 CAS DTO and one lost response is reconciled by GET, never replayed",async()=>{
  const calls=[];let request;
  const latest=snapshot(5);
  const h=ioHarness(async(command,args)=>{calls.push({command,args});if(command==="save_site_catalog"){request=args.request;throw new Error("response lost");}return latest;});
  const data=h.custom();await h.save(data,4);
  assert.deepEqual(calls.map(c=>c.command),["save_site_catalog","site_catalog"]);
  assert.deepEqual(Object.keys(request),["schema_version","expected_revision","sites","applications","groups"]);
  assert.equal(request.schema_version,2);assert.equal(request.expected_revision,4);assert.equal(h.state.unknown,false);assert.equal(h.state.snapshot.revision,5);
});
test("unreadable ambiguous write locks editing until an explicit successful read",async()=>{
  let fail=true;const calls=[];
  const h=ioHarness(async(command)=>{calls.push(command);if(command==="save_site_catalog"||fail)throw new Error("offline");return snapshot(5);});
  await assert.rejects(h.save(h.custom(),4),/offline/);assert.equal(h.state.unknown,true);
  await assert.rejects(h.save(h.custom(),4),/重新读取/);assert.equal(calls.length,2);
  fail=false;await h.load();assert.equal(calls.length,3);assert.equal(h.state.unknown,false);assert.equal(h.state.snapshot.revision,5);
});
test("stale compare-and-swap editor cannot issue any write",async()=>{
  const calls=[];const h=ioHarness(async(command)=>{calls.push(command);return snapshot(5);});
  await assert.rejects(h.save(h.custom(),3),/重新打开编辑/);assert.deepEqual(calls,[]);
});
test("canonical comparison ignores ordering but detects value changes",()=>{const h=harness(),a=h.custom(snapshot()),b=JSON.parse(JSON.stringify(a));b.applications[0].site_ids.reverse();assert.equal(h.sameCustom(a,b),true);b.sites[0].purpose="changed";assert.equal(h.sameCustom(a,b),false);});
test("deleting an application preserves shared sites and removes group references",()=>{const h=harness(),snap=snapshot(),data=h.deleteData(snap,"application","ca");assert.deepEqual(data.sites.map(s=>s.id),["cs"]);assert.deepEqual(data.applications,[]);assert.deepEqual(data.groups[0].application_ids,[]);});
test("deleting a shared endpoint removes references but keeps applications and rules",()=>{const h=harness(),snap=snapshot(),data=h.deleteData(snap,"site","cs");assert.deepEqual(data.sites,[]);assert.deepEqual(data.applications[0].site_ids,[]);assert.equal(data.applications[0].reference_rules[0].id,"r1");});
test("builtin references prevent deleting their dependent application or endpoint",()=>{const h=harness(),snap=snapshot();snap.groups[0].application_ids.push("ca");assert.throws(()=>h.deleteData(snap,"application","ca"),/内置组合引用/);snap.groups[0].application_ids=["ba"];snap.applications[0].site_ids.push("cs");assert.throws(()=>h.deleteData(snap,"site","cs"),/内置应用引用/);});
test("category and group filters use only application ids",()=>{const h=harness();h.state.snapshot=snapshot();assert.deepEqual(h.filteredApps("category:custom").map(a=>a.id),["ca"]);assert.deepEqual(h.filteredApps("group:cg").map(a=>a.id),["ca"]);assert.deepEqual(h.filteredApps("group:bg").map(a=>a.id),["ba"]);});
test("module uses safe DOM mount, shared native/HTTP transport and no automatic start",()=>{assert.match(source,/page\.replaceChildren\(title/);assert.match(source,/title\.textContent = "应用管理"/);assert.doesNotMatch(source,/innerHTML|insertAdjacentHTML|network_start|\/api\/network-tools\/start/);assert.match(source,/"save_site_catalog" : "site_catalog"/);assert.match(source,/\/api\/site-catalog/);assert.match(source,/schema_version:2,expected_revision:expectedRevision/);assert.match(source,/window\.appShell\?\.setLocked\("catalog-mutation"/);assert.match(source,/event\.detail === "catalog"/);assert.match(source,/applyEndpoint\(clone\(site\),tool\)/);assert.match(source,/applySite\(clone\(site\)\)/);assert.match(source,/quick-group/);assert.match(source,/quick-app/);assert.match(source,/quick-site/);assert.match(source,/title = \$\("catalog-title"\)/);assert.match(css,/\.application-catalog \.ac-layout/);});
test("application catalog has responsive panes and scoped application styling",()=>{assert.match(css,/@media\(max-width:540px\)/);assert.match(css,/\.application-catalog \.ac-dialog/);assert.match(css,/\.application-catalog \.ac-nav-item/);assert.match(css,/\.picker-fields:has\(#quick-app\)/);});
