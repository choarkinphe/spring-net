"use strict";
// Offline, dependency-free behavioral tests: node --test web/workbench.test.cjs
const {test} = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const source = fs.readFileSync(`${__dirname}/app.js`, "utf8");
const plain = value => JSON.parse(JSON.stringify(value));
function harness() {
  const calls = [], dialogs = [], nodes = new Map();
  const node = id => {
    if (!nodes.has(id)) nodes.set(id, {
      value:"", valueAsNumber:0, checked:false, hidden:false, textContent:"", disabled:false,
      dataset:{}, children:[], replaceChildren(...children){this.children=children;},
      append(...children){this.children.push(...children);}, attributes:{}, setAttribute(key,value){this.attributes[key]=value;},
      focus(){this.focused=true;}, addEventListener(name,listener){(this.listeners ||= {})[name]=listener;}, classList:{add(){},toggle(){}}, style:{setProperty(){}},
      querySelector(){return this.group;},querySelectorAll(){return this.headers||[];},insertBefore(group){this.group=group;},
    });
    return nodes.get(id);
  };
  const context = vm.createContext({
    window:{AoNetTransport:{isDesktop:true,async request(path,options){
      const body=JSON.parse(options.body);calls.push({path,...body});
      if(body.operation==="authorization.prepare")return {draftToken:"normalized",revision:7,expiresIn:300,targets:body.payload.targets,sideEffects:["保存授权，不发送流量"]};
      if(body.operation==="authorization.confirm")return {authorized:[]};
      if(body.operation==="profiles.save")return body.payload.profiles;
      return true;
    }}},
    document:{getElementById:node,querySelector:selector=>selector===".results-table"?node("results-table"):null,querySelectorAll:()=>[],createElement:()=>node(Symbol()),documentElement:node("root")},
    TextEncoder, URL, Error, TypeError, Date, setTimeout:()=>1, clearTimeout(){},
    location:{hash:""}, console, dialogs, calls,
  });
  const prefix=source.slice(0,source.indexOf('  $("edit-auth-json").addEventListener'));
  vm.runInContext(prefix+`
    function updateControls() {}
    function renderPicker() {}
    function renderResults() {}
    function updateConstraints() {}
    function updateWorkspaceControls() {}
    function initialize() { calls.push({initialize:true}); }
    function openDialog(id) { dialogs.push(id); }
    function closeDialog() {}
    function renderHistory() {}
    function renderCharts() {}
    function renderSeriesTable() {}
    function renderInventory() {}
    function renderAuthorized() {}
    window.subject={state,getConfig,configError,selectedItems,currentCaps,applyProfile,preparePermits,confirmAuthorization,persistentPermits,workspaceSubmit,workspace,importSchema,utf8Length,applyColumnWidths,setColumnWidth,setResultSort,switchResultTab,applyBootstrap,validateConnectionPresets,renderConnectionCatalog,showConnectionPreset,safeReferenceUrl,customLists,resolvePage,normalizeProxy,selectedProxy,setRouteDraft,diagnosticDraft,prepareAuthorization,editAuthorization};
  })();`,context);
  const subject=context.window.subject;
  subject.state.ready=true;subject.state.stateKnown=true;subject.state.batchKnown=true;
  subject.state.bootstrap={token:"test",authorization_error:null,authorized:[],lists:[{id:"one",name:"One",targets:[{id:"same",name:"First",url:"https://first.test"}]},{id:"two",name:"Two",targets:[{id:"same",name:"Second",url:"https://second.test"}]}],limits:{public_concurrency:4,public_rps:2,public_duration:120,public_cooldown:0,timeout:30}};
  for(const [id,value]of [["concurrency",1],["rps",1],["duration",30],["timeout",10],["ramp",0]]){node(id).value=String(value);node(id).valueAsNumber=value;}
  return {...subject,node,calls,dialogs};
}
function referenceCard() {
  return {id:"travelsky-ibe-plus",provider:"TravelSky",product:"IBE+ <script>not HTML</script>",aliases:["IBE+"],access_type:"API",environment:null,version:null,status:"unconfirmed",summary:"参考资料，不执行探测",limitations:["未知生产配置"],missing:["待确认实际产品"],protected_domains:[],endpoints:[{kind:"host",address:null,protocol:null,port:null,purpose:"生产连接",evidence:"unknown",source_ids:["official"],notes:"不猜测地址"}],sources:[{id:"official",title:"官方产品",url:"https://example.test/product",checked_at:"2026-10-27",published_at:"2014-06-23",scope:"仅产品归属"}]};
}
function allText(node) { return [node.textContent||"",...(node.children||[]).map(allText)].join(" "); }
test("reference catalog details are text-only, show unknowns and make no requests",()=>{
  const h=harness(),card=referenceCard();
  h.state.bootstrap.connection_presets=[card];
  h.validateConnectionPresets([card]);h.renderConnectionCatalog();
  assert.equal(h.node("connection-catalog").children.length,1);
  const button=h.node("connection-catalog").children[0].children[2];
  button.listeners.click();
  assert.equal(h.node("connection-dialog-title").textContent,card.product);
  const text=allText(h.node("connection-detail"));
  assert.match(text,/地址未公开/);assert.match(text,/端口 未明确/);assert.match(text,/2014-06-23/);assert.match(text,/待确认实际产品/);
  assert.deepEqual(h.dialogs,["connection-dialog"]);assert.equal(h.calls.length,0);
  assert.equal(h.selectedItems().length,0);assert.equal(h.customLists().some(x=>x.id===card.id),false);
  assert.equal(h.node("connection-detail").innerHTML,undefined);
});
test("reference catalog validates duplicates, invalid ports, unsafe sources and references",()=>{
  const h=harness(),card=referenceCard();
  assert.throws(()=>h.validateConnectionPresets([card,card]),/重复/);
  assert.throws(()=>h.validateConnectionPresets([{...card,sources:[{...card.sources[0],url:"javascript:alert(1)"}]}]),/来源/);
  assert.throws(()=>h.validateConnectionPresets([{...card,endpoints:[{...card.endpoints[0],port:0}]}]),/端口/);
  assert.throws(()=>h.validateConnectionPresets([{...card,endpoints:[{...card.endpoints[0],source_ids:["missing"]}]}]),/来源引用/);
  assert.equal(h.safeReferenceUrl("https://user:secret@example.test"),null);
  assert.equal(h.safeReferenceUrl("data:text/html,test"),null);
  assert.equal(h.safeReferenceUrl("https://example.test/path"),"https://example.test/path");
});
test("older bootstrap renders a missing catalog rather than inventing targets",()=>{
  const h=harness();h.applyBootstrap(h.state.bootstrap);
  assert.match(allText(h.node("connection-catalog")),/未提供/);
  assert.equal(h.state.bootstrap.lists.length,2);assert.equal(h.calls.length,0);
});
test("wildcard references remain rules and cannot enter selected target config",()=>{
  const h=harness(),card=referenceCard();card.endpoints=[{...card.endpoints[0],kind:"wildcard",address:"*."+"long-host-".repeat(15)+"example.test",port:443,evidence:"documented"}];
  h.state.bootstrap.connection_presets=[card];h.state.selectedTargets.add(card.id);h.showConnectionPreset(card.id);
  assert.match(allText(h.node("connection-detail")),/非可连接主机/);
  assert.deepEqual(plain(h.getConfig().target_ids),[]);assert.equal(h.calls.length,0);
});
test("probe config carries qualified selected subset; empty GUI is rejected",()=>{
  const h=harness();assert.match(h.configError(h.getConfig()),/至少勾选/);
  h.state.selectedTargets.add("two/same");h.state.selectedTargets.add("missing/stale");
  assert.deepEqual(plain(h.getConfig().target_ids),["two/same"]);
  assert.deepEqual(plain(h.getConfig().list_ids),["two"]);
  assert.equal(h.selectedItems()[0].targets[0].name,"Second");
  assert.equal(h.configError(h.getConfig()),"");
});
test("integer RPS, 30 second timeout and 120 second probe duration enforced",()=>{
  const h=harness();h.state.selectedTargets.add("one/same");
  assert.match(h.configError({...h.getConfig(),rps:1.5}),/整数/);
  assert.match(h.configError({...h.getConfig(),timeout_secs:31}),/超时/);
  assert.match(h.configError({...h.getConfig(),duration_secs:121}),/超出/);
});
test("expired authorizations are not selectable; authorization failure blocks run",()=>{
  const h=harness();h.state.bootstrap.authorized=[{id:"permit",name:"Permit",url:"https://owned.test",modes:["http"],expired:true,max_concurrency:10,max_rps:10,max_duration:300}];
  h.state.mode="http";h.state.selectedAuthorized.add("permit");assert.equal(h.selectedItems().length,0);
  h.state.bootstrap.authorized[0].expired=false;assert.equal(h.selectedItems().length,1);
  h.state.bootstrap.authorization_error="bad authorization";assert.match(h.configError(h.getConfig()),/修复/);
});
test("authorization preview excludes demo records and never confirms itself",async()=>{
  const h=harness();const permit={id:"owned",name:"Owned",url:"https://owned.test",modes:["http"],allowed_ips:["192.0.2.1/32"],max_concurrency:10,max_rps:5,max_duration:100,authorization:"explicit consent",expires_at:"2099-01-01T00:00:00Z",expired:false};
  h.state.bootstrap.authorized=[permit,{...permit,id:"demo-http"}];
  const records=h.persistentPermits();assert.equal(records.length,1);assert.equal("expired"in records[0],false);
  await h.preparePermits(records);
  assert.deepEqual(h.calls.map(c=>c.operation),["authorization.prepare"]);
  assert.deepEqual(plain(h.calls[0].payload.targets[0]),{id:permit.id,name:permit.name,url:permit.url,modes:permit.modes,allowed_ips:permit.allowed_ips,max_concurrency:10,max_rps:5,max_duration:100,authorization:permit.authorization,expires_at:permit.expires_at});
  assert.equal(h.state.authDraft.revision,7);assert.ok(h.node("auth-preview-content").textContent.includes("allowed_ips"));
  await h.confirmAuthorization();assert.equal(h.calls[1].operation,"authorization.confirm");assert.deepEqual(plain(h.calls[1].payload),{draftToken:"normalized",revision:7});
  await assert.rejects(h.preparePermits([{...permit,id:"demo-http"}]),/demo/);
});
test("proxy permits survive editing payload copies and proxy profiles never silently become direct",()=>{
  const h=harness();const routes=[{url:"http://127.0.0.1:8080",allowed_ips:["127.0.0.1/32"],authorization:"explicit proxy permission"}];
  h.state.bootstrap.authorized=[{id:"owned",proxy_routes:routes}];
  const copied=h.persistentPermits();assert.deepEqual(plain(copied[0].proxy_routes),routes);copied[0].proxy_routes[0].url="changed";assert.equal(routes[0].url,"http://127.0.0.1:8080");
  const before=plain(h.getConfig());h.applyProfile({config:{mode:"http",proxy:{url:"http://127.0.0.1:8080"},concurrency:99}});
  assert.deepEqual(plain(h.getConfig()),before);assert.match(h.node("banner-text").textContent,/未修改草稿/);assert.equal(h.calls.length,0);
});
test("proxy profiles roundtrip exactly and invalid proxy never changes path",()=>{
  const h=harness();h.state.bootstrap.proxy={enabled:true};
  const proxy={url:"http://127.0.0.1:7890"};h.applyProfile({config:{mode:"http",proxy,concurrency:2,authorized_ids:[]}});
  assert.deepEqual(plain(h.getConfig().proxy),proxy);assert.equal(h.node("run-route").value,"proxy");assert.equal(h.calls.length,0);
  h.applyProfile({config:{mode:"tcp",proxy:{url:"http://user:password@proxy.test:8080"}}});assert.equal(h.state.mode,"http");assert.deepEqual(plain(h.getConfig().proxy),proxy);
  assert.throws(()=>h.normalizeProxy({url:"http://proxy.test:0"}));assert.throws(()=>h.normalizeProxy({url:"http://proxy.test:8080/path"}));
  h.applyProfile({config:{mode:"probe",authorized_ids:[]}});assert.equal(h.node("run-route").value,"direct");assert.equal(h.getConfig().proxy,undefined);
});
test("normal authorization editing retains explicit proxy routes through preview",async()=>{
  const h=harness();const route={url:"http://127.0.0.1:8080",allowed_ips:["127.0.0.1/32"],authorization:"proxy owner permission"};
  const permit={id:"owned",name:"Owned",url:"https://owned.test",modes:["http"],allowed_ips:["192.0.2.1/32"],max_concurrency:2,max_rps:1,max_duration:20,authorization:"target permission",expires_at:"2099-01-01T00:00:00Z",proxy_routes:[route]};
  h.state.bootstrap.authorized=[permit];h.editAuthorization(permit);await h.prepareAuthorization({preventDefault(){}});
  assert.deepEqual(plain(h.calls[0].payload.targets[0].proxy_routes),[route]);
});
test("profile load only fills draft; demo IDs excluded; history clear is explicit",async()=>{
  const h=harness();h.state.bootstrap.authorized=[{id:"demo-http",expired:false},{id:"owned",expired:false}];
  h.applyProfile({config:{mode:"http",concurrency:2,rps:2,duration_secs:15,timeout_secs:5,ramp_secs:1,fresh_connections:true,authorized_ids:["owned","demo-http"],target_ids:[],list_ids:[]}});
  assert.deepEqual([...h.state.selectedAuthorized],["owned"]);assert.equal(h.calls.length,0);assert.equal(h.state.mode,"http");assert.equal(h.node("rps").value,"2");
  h.state.workspaceAction={type:"history-clear"};await h.workspaceSubmit({preventDefault(){}});
  assert.deepEqual(plain(h.calls[0]),{path:"/api/workspace",operation:"history.clear",payload:{confirm:true}});
});
test("workspace mutations are blocked while live run is active",async()=>{
  const h=harness();h.state.run={id:"run",status:"running"};await assert.rejects(h.workspace("selftest",{enabled:true}),/不能修改/);assert.equal(h.calls.length,0);
});
test("list and target names enforce UTF-8 bytes rather than JS length",()=>{
  const h=harness();assert.equal(h.utf8Length("测".repeat(67)),201);
  assert.throws(()=>h.importSchema([{name:"测".repeat(67),targets:[{url:"https://example.test"}]}]),/200/);
  assert.throws(()=>h.importSchema([{name:"valid",targets:[{name:"测".repeat(67),url:"https://example.test"}]}]),/200/);
});

test("column widths apply saved values and keyboard input remains bounded",()=>{
  const h=harness();h.node("column-select").value="name";
  h.state.settings.column_widths={name:320,errors:150};
  h.applyColumnWidths();
  const group=h.node("results-table").group;
  assert.equal(group.children[0].style.width,"320px");
  assert.equal(group.children[3].style.width,"150px");
  h.setColumnWidth(490);assert.equal(h.state.settings.column_widths.name,490);
  assert.equal(group.children[0].style.width,"490px");
  h.setColumnWidth(9999);assert.equal(h.state.settings.column_widths.name,800);
  h.setColumnWidth(1);assert.equal(h.state.settings.column_widths.name,60);
});
test("table header sort describes direction with aria-sort",()=>{
  const h=harness();h.node("results-table").headers=Array.from({length:8},(_,i)=>h.node(`header-${i}`));
  h.setResultSort("name");assert.equal(h.node("header-0").attributes["aria-sort"],"ascending");
  h.setResultSort("errors");assert.equal(h.node("header-0").attributes["aria-sort"],"none");assert.equal(h.node("header-3").attributes["aria-sort"],"descending");
});

test("result tabs keep table-first default and expose keyboard-selected panels",()=>{
  const h=harness();h.switchResultTab("results");
  assert.equal(h.node("view-results").hidden,false);assert.equal(h.node("view-curves").hidden,true);
  assert.equal(h.node("tab-results").attributes["aria-selected"],"true");
  h.switchResultTab("samples",true);assert.equal(h.node("view-results").hidden,true);
  assert.equal(h.node("view-samples").hidden,false);assert.equal(h.node("series-details").open,true);
  assert.equal(h.node("tab-samples").focused,true);
});
test("home defaults to monitoring while explicit old links remain valid",()=>{
  const h=harness();assert.equal(h.resolvePage(""),"monitor");assert.equal(h.resolvePage("#unknown"),"monitor");assert.equal(h.resolvePage("#diagnostics"),"diagnostics");assert.equal(h.resolvePage("#console"),"console");assert.equal(h.resolvePage("#batch"),"batch");assert.equal(h.resolvePage("#load"),"load");assert.equal(h.resolvePage("#lists"),"lists");
});
test("sidebar version uses backend version and never invents a fallback",()=>{
  const h=harness();h.applyBootstrap({...h.state.bootstrap,version:"0.3.0"});
  assert.equal(h.node("app-version").textContent,"v0.3.0");
  h.applyBootstrap({...h.state.bootstrap,version:undefined});
  assert.equal(h.node("app-version").textContent,"版本未知");
});
test("successful bootstrap clears only matching old authorization banner",()=>{
  const h=harness();h.state.bootstrap.authorization_error="broken file";
  h.node("banner").hidden=false;h.node("banner-text").textContent="授权文件需要修复：broken file";
  h.applyBootstrap({...h.state.bootstrap,authorization_error:null});assert.equal(h.node("banner").hidden,true);
  h.state.bootstrap.authorization_error="broken file";h.node("banner").hidden=false;h.node("banner-text").textContent="unrelated export error";
  h.applyBootstrap({...h.state.bootstrap,authorization_error:null});assert.equal(h.node("banner").hidden,false);
});
