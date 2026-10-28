"use strict";
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const source=fs.readFileSync(`${__dirname}/application-catalog.js`,"utf8");
const css=fs.readFileSync(`${__dirname}/application-catalog.css`,"utf8");
const controllerSource=fs.readFileSync(`${__dirname}/catalog-controller.js`,"utf8");
function snapshot(revision=4){
  const site={id:"bs",name:"Preset endpoint",address:"https://preset.invalid/",port:null,transport:"tcp",protocol:"https",purpose:"portal",alpn:null,builtin:true,source:"catalog",checked_at:"2026-10-27",capabilities:["http","tls"],unavailable_reasons:[]};
  const customSite={...site,id:"cs",name:"Shared endpoint",address:"host.invalid",port:443,protocol:"tls",purpose:"auth",builtin:false,source:"",checked_at:"",capabilities:["tls"]};
  const base={category:"custom",provider:"",product:"",aliases:[],access_type:"",reference_rules:[],sources:[],missing:[],limitations:[],summary:"",status:"",environment:null,version:null,supplement:null};
  const builtinApp={...base,id:"ba",name:"Preset website",kind:"website",category:"web",provider:"Vendor",product:"Cloud",access_type:"browser",site_ids:["bs"],tag_ids:["bt"],builtin:true,summary:"Preset summary"};
  const customApp={...base,id:"ca",name:"My app",kind:"application",aliases:["alias"],site_ids:["cs"],tag_ids:["ct"],reference_rules:[{id:"r1",kind:"unknown",address:null,transport:null,protocol:null,ports:"dynamic",purpose:"login",evidence:"",notes:"",source_ids:[]}],builtin:false};
  return {version:3,revision,sites:[site,customSite],applications:[builtinApp,customApp],groups:[{id:"bg",name:"Preset group",description:"",tag_ids:["bt"],application_ids:["ba"],builtin:true},{id:"cg",name:"My group",description:"",tag_ids:["ct"],application_ids:["ca"],builtin:false}],tags:[{id:"bt",name:"Official tag",builtin:true},{id:"ct",name:"Personal tag",builtin:false}],tag_assignments:[{resource_id:"ca",tag_ids:["ct"]}],resource_supplements:[]};
}
function domHarness(initial=snapshot()) {
  const nodes=new Map(),calls=[],events={},roots=[];let title,uuid=0,server=JSON.parse(JSON.stringify(initial));
  const document={activeElement:null};
  const descendants=node=>[node,...node.children.flatMap(descendants)];
  class Event {
    constructor(type,options={}) { this.type=type;this.bubbles=Boolean(options.bubbles);this.cancelable=Boolean(options.cancelable);this.defaultPrevented=false;this.target=null;this.currentTarget=null; }
    preventDefault(){if(this.cancelable)this.defaultPrevented=true;}
    stopPropagation(){this.stopped=true;}
  }
  function matches(node,selector) {
    return selector.split(",").some(part=>{
      part=part.trim();const tag=part.match(/^[a-z][a-z0-9-]*/i),classes=[...part.matchAll(/\.([\w-]+)/g)],attrs=[...part.matchAll(/\[([^=\]]+)(?:=["']?([^\]"']*)["']?)?\]/g)];
      if(tag&&node.tagName!==tag[0].toUpperCase())return false;
      if(classes.some(([,name])=>!node.className.split(/\s+/).includes(name)))return false;
      return attrs.every(([,key,value])=>{const actual=key.startsWith("data-")?node.dataset[key.slice(5).replace(/-([a-z])/g,(_,c)=>c.toUpperCase())]:key in node?node[key]:node.attributes[key];return actual!==undefined&&(value===undefined||String(actual)===value);});
    });
  }
  function element(tag="div") {
    let text="",checked=false;
    const node={tagName:tag.toUpperCase(),value:"",className:"",hidden:false,disabled:false,open:false,dataset:{},children:[],attributes:{},handlers:{},parentElement:null,
      append(...children){for(const child of children){if(child.parentElement)child.parentElement.children=child.parentElement.children.filter(n=>n!==child);child.parentElement=this;this.children.push(child);}},
      replaceChildren(...children){for(const child of this.children)child.parentElement=null;this.children=[];text="";this.append(...children);},
      addEventListener(type,fn){(this.handlers[type]??=[]).push(fn);},
      dispatchEvent(event){if(!event.target)event.target=this;event.currentTarget=this;for(const fn of this.handlers[event.type]||[])fn.call(this,event);if(event.bubbles&&!event.stopped)this.parentElement?.dispatchEvent(event);return !event.defaultPrevented;},
      setAttribute(key,value){this.attributes[key]=String(value);},
      focus(){if(this.isConnected&&!this.disabled)document.activeElement=this;},
      contains(target){return this===target||this.children.some(c=>c.contains(target));},
      querySelectorAll(selector){return this.children.flatMap(descendants).filter(n=>matches(n,selector));},
      querySelector(selector){return this.querySelectorAll(selector)[0]||null;},
      click(){let ancestor=this;while(ancestor){if(ancestor.disabled)return;ancestor=ancestor.parentElement;}this.focus();if(this.type==="radio"||this.type==="checkbox"){const previous=this.checked;this.checked=this.type==="radio"?true:!this.checked;if(previous!==this.checked)this.dispatchEvent(new Event("change",{bubbles:true}));}this.dispatchEvent(new Event("click",{bubbles:true,cancelable:true}));},
      showModal(){this.open=true;},close(){this.open=false;}
    };
    node.classList={add(...classes){node.className=[...new Set([...node.className.split(/\s+/).filter(Boolean),...classes])].join(" ");}};
    Object.defineProperties(node,{
      id:{set(id){this._id=id;nodes.set(id,this);},get(){return this._id;}},
      textContent:{get(){return text+this.children.map(n=>n.textContent).join("");},set(value){this.replaceChildren();text=String(value??"");}},
      isConnected:{get(){return roots.some(root=>root.contains(this));}},
      checked:{get(){return checked;},set(value){checked=Boolean(value);if(checked&&this.type==="radio"&&this.name&&this.isConnected)for(const other of roots.flatMap(descendants))if(other!==this&&other.type==="radio"&&other.name===this.name)other.checked=false;}}
    });return node;
  }
  document.createElement=element;document.getElementById=id=>{const node=nodes.get(id);return node?.isConnected?node:null;};
  document.querySelectorAll=selector=>roots.flatMap(root=>[...(matches(root,selector)?[root]:[]),...root.querySelectorAll(selector)]);
  for(const id of ["catalog-page","applications-page","quick-group","quick-app","quick-site","apply-site"]) {const n=element(id==="apply-site"?"button":"div");n.id=id;roots.push(n);}
  title=element("h1");title.id="catalog-title";nodes.get("catalog-page").append(title);
  const window={Event,__TAURI__:{core:{invoke:async(command,args)=>{
    // Native IPC crosses a JSON boundary; retain the actual serialized DTO.
    calls.push({command,args:args===undefined?undefined:JSON.parse(JSON.stringify(args))});
    if(command==="save_site_catalog"){
      const request=args.request,base=snapshot(),previous=server;
      const supplements=request.resource_supplements;
      const applications=[...previous.applications.filter(a=>a.builtin).map(a=>{
        const old=a.supplement,personal=supplements.find(s=>s.resource_id===a.id)||null;
        return {...a,site_ids:[...a.site_ids.filter(id=>!old?.site_ids.includes(id)),...(personal?.site_ids||[])],reference_rules:[...a.reference_rules.filter(r=>!old?.reference_rules.some(x=>x.id===r.id)),...(personal?.reference_rules||[])],supplement:personal};
      }),...request.applications.map(a=>({...base.applications[1],...a,builtin:false,tag_ids:[]}))];
      for(const a of applications){const old=previous.tag_assignments.find(t=>t.resource_id===a.id)?.tag_ids||[];const official=a.builtin?a.tag_ids.filter(id=>!old.includes(id)):[];a.tag_ids=[...new Set([...official,...(request.tag_assignments.find(t=>t.resource_id===a.id)?.tag_ids||[])])];}
      server={version:3,revision:server.revision+1,sites:[...previous.sites.filter(s=>s.builtin),...request.sites.map(s=>({...base.sites[1],...s,builtin:false}))],applications,tags:[...previous.tags.filter(t=>t.builtin),...request.tags.map(t=>({...t,builtin:false}))],tag_assignments:request.tag_assignments,resource_supplements:supplements,groups:[...previous.groups.filter(g=>g.builtin),...request.groups.map(g=>({...g,builtin:false}))]};
      for(const g of server.groups)g.application_ids=server.applications.filter(a=>a.tag_ids.some(id=>g.tag_ids.includes(id))).map(a=>a.id);
    }
    return JSON.parse(JSON.stringify(server));
  }}},appShell:{setLocked(){},isLockedExcept(){return false;},subscribe(){},show(page){calls.push({page});}},networkToolDraft:{applySite(site){calls.push({draft:site});return true;},applyEndpoint(site,tool){calls.push({draft:site,tool});return true;}},addEventListener(type,fn){events[type]=fn;}};
  const context=vm.createContext({window,document,Event,console,Error,Set,Map,JSON,Object,Array,String,Number,Boolean,AbortController,URL,setTimeout,clearTimeout,crypto:{randomUUID:()=>`test-uuid-${++uuid}`}});
  vm.runInContext(controllerSource,context);
  vm.runInContext(source,context);
  return {window,document,Event,nodes,calls,events,title,descendants,ready:()=>new Promise(resolve=>setImmediate(resolve))};
}
function fire(h,node,type){assert.ok(node,`missing ${type} target`);node.dispatchEvent(new h.Event(type,{bubbles:true,cancelable:true}));}
function find(h,text,root=h.nodes.get("catalog-page")){return h.descendants(root).find(n=>n.tagName==="BUTTON"&&n.textContent===text);}
function openApp(h,id){const node=h.nodes.get(h.window.siteCatalog.snapshot().applications.find(a=>a.id===id)?.kind==="website"?"catalog-page":"applications-page").querySelectorAll("button[data-site-id]").find(n=>n.dataset.siteId===id);assert.ok(node);node.click();}
function choice(root,title,value){const field=root.querySelectorAll("fieldset").find(n=>n.children[0]?.textContent===title);assert.ok(field,`missing choices ${title}`);const input=field.querySelectorAll('input[type="radio"]').find(n=>n.value===value);assert.ok(input,`missing choice ${title}=${value}`);return input;}
function quickChoice(h,key,value){const input=h.nodes.get(`quick-${key}`).querySelectorAll('input[type="radio"]').find(n=>n.value===value);assert.ok(input,`missing quick ${key}=${value}`);return input;}
function input(h,node,value){node.value=value;fire(h,node,"input");}
async function submitEditor(h,prefix="app-ac-"){fire(h,h.nodes.get(`${prefix}editor-dialog`).querySelector("form"),"submit");await h.ready();}
function editorField(h,name,prefix="app-ac-"){return h.nodes.get(`${prefix}editor-dialog`).querySelectorAll("input,textarea").find(n=>n.name===name);}

const appPage=h=>h.nodes.get("applications-page"),webPage=h=>h.nodes.get("catalog-page");
function nav(h,selection,page=appPage(h)){const button=page.querySelectorAll("button[data-selection]").find(n=>n.dataset.selection===selection);assert.ok(button,selection);button.click();}
function dialog(h,prefix="app-ac-"){return h.nodes.get(`${prefix}editor-dialog`);}
function cancel(h,prefix="app-ac-"){find(h,"取消",dialog(h,prefix)).click();}
function check(root,value){const input=root.querySelectorAll('input[type="checkbox"]').find(n=>n.value===value);assert.ok(input,value);return input;}
test("dual pages preserve titles, have independent IDs and share one read without detection",async()=>{
 const h=domHarness();await h.ready();assert.equal(h.title.textContent,"网站管理");assert.equal(h.nodes.get("applications-title").textContent,"应用管理");
 assert.equal(h.calls.length,1);assert.equal(h.calls[0].command,"site_catalog");assert.equal(h.window.siteCatalog.snapshot().version,3);
 assert.deepEqual(webPage(h).querySelectorAll("button[data-site-id]").map(n=>n.dataset.siteId),["ba"]);assert.deepEqual(appPage(h).querySelectorAll("button[data-site-id]").map(n=>n.dataset.siteId),["ca"]);
 const ids=h.document.querySelectorAll("[id]").map(n=>n.id);assert.equal(new Set(ids).size,ids.length);
 for(const prefix of ["ac-","app-ac-"])for(const [id,label] of [["new-group","新增分组"],["new-tag","新增标签"],["new-application",prefix==="ac-"?"新增网站":"新增应用"],["new-site","新增连接"]]){const node=h.nodes.get(prefix+id);assert.equal(node.textContent,"+");assert.equal(node.attributes["aria-label"],label);}
 assert.equal(h.document.querySelectorAll("select").length,0);
});
test("compact list opens detail, returns focus and each page retains its own search and filter",async()=>{
 const snap=snapshot();snap.applications[1].site_ids.push("bs");const h=domHarness(snap);await h.ready();const page=appPage(h),search=h.nodes.get("app-ac-search");
 assert.equal(page.querySelectorAll("li.ac-endpoint").length,0);assert.match(page.querySelector("li.ac-site-row").textContent,/2 条连接/);
 input(h,search,"alias");input(h,h.nodes.get("ac-search"),"preset.invalid");openApp(h,"ca");assert.equal(h.document.activeElement.textContent,"My app");assert.equal(page.querySelectorAll("li.ac-endpoint").length,2);
 find(h,"← 返回列表",page).click();assert.equal(h.document.activeElement.dataset.siteId,"ca");assert.equal(search.value,"alias");assert.equal(h.nodes.get("ac-search").value,"preset.invalid");
 nav(h,"group:cg");assert.equal(search.value,"");nav(h,"untagged");assert.equal(page.querySelectorAll("li.ac-site-row").length,0);assert.equal(webPage(h).querySelectorAll("li.ac-site-row").length,1);assert.equal(h.calls.length,1);
});
test("tag/group OR preview spans both kinds, empty groups match zero and saves never send member projection",async()=>{
 const h=domHarness();await h.ready();h.nodes.get("app-ac-new-group").click();input(h,editorField(h,"name"),"Both kinds");assert.match(dialog(h).textContent,/0 个网站 · 0 个应用 · 空分组/);
 check(dialog(h),"bt").click();check(dialog(h),"ct").click();assert.match(dialog(h).textContent,/1 个网站 · 1 个应用/);assert.equal(dialog(h).querySelectorAll('input[type="checkbox"]').length,2);
 await submitEditor(h);const req=h.calls.find(c=>c.command==="save_site_catalog").args.request,group=req.groups.find(g=>g.name==="Both kinds");assert.deepEqual(group.tag_ids,["bt","ct"]);assert.equal("application_ids" in group,false);assert.equal(req.schema_version,3);
 nav(h,`group:${group.id}`);nav(h,`group:${group.id}`,webPage(h));assert.equal(appPage(h).querySelectorAll("li.ac-site-row").length,1);assert.equal(webPage(h).querySelectorAll("li.ac-site-row").length,1);
 h.nodes.get("ac-new-group").click();input(h,editorField(h,"name","ac-"),"Empty");await submitEditor(h,"ac-");const empty=h.window.siteCatalog.snapshot().groups.find(g=>g.name==="Empty");nav(h,`group:${empty.id}`,webPage(h));assert.equal(webPage(h).querySelectorAll("li.ac-site-row").length,0);
});
test("cross-page save broadcasts while stale editor must reopen and cannot overwrite",async()=>{
 const h=domHarness();await h.ready();h.nodes.get("app-ac-new-application").click();input(h,editorField(h,"name"),"Stale app");h.nodes.get("ac-new-tag").click();input(h,editorField(h,"name","ac-"),"Shared tag");await submitEditor(h,"ac-");assert.match(appPage(h).textContent,/Shared tag/);
 await submitEditor(h);assert.match(dialog(h).textContent,/重新打开编辑/);assert.equal(h.calls.filter(c=>c.command==="save_site_catalog").length,1);cancel(h);
});
test("tag CRUD has stable IDs, builtin tags read-only but assignable, deletion lists all affected kinds and groups",async()=>{
 const h=domHarness();await h.ready();openApp(h,"ba");find(h,"管理标签",webPage(h)).click();check(dialog(h,"ac-"),"ct").click();await submitEditor(h,"ac-");
 openApp(h,"ca");find(h,"管理标签",appPage(h)).click();check(dialog(h),"bt").click();await submitEditor(h);assert.ok(h.window.siteCatalog.snapshot().applications.find(a=>a.id==="ca").tag_ids.includes("bt"));
 nav(h,"tag:bt");assert.equal(find(h,"编辑标签",appPage(h)),undefined);nav(h,"tag:ct");find(h,"编辑标签",appPage(h)).click();input(h,editorField(h,"name"),"Renamed");await submitEditor(h);assert.equal(h.window.siteCatalog.snapshot().tags.find(t=>t.id==="ct").name,"Renamed");
 find(h,"删除标签",appPage(h)).click();const del=h.nodes.get("app-ac-delete-dialog");assert.match(del.textContent,/2 个资源/);assert.match(del.textContent,/Preset website · 网站/);assert.match(del.textContent,/My app · 应用/);assert.match(del.textContent,/My group/);assert.equal(h.window.siteCatalog.snapshot().tags.length,2);
 find(h,"确认删除",del).click();await h.ready();const snap=h.window.siteCatalog.snapshot();assert.equal(snap.tags.length,1);assert.deepEqual(snap.groups.find(g=>g.id==="cg").application_ids,[]);assert.equal(snap.applications.length,2);assert.equal(snap.groups.length,2);
});
test("builtin official detail is readonly but personal connections/rules support add, reuse, edit and delete",async()=>{
 const snap=snapshot();snap.applications[0].kind="application";const h=domHarness(snap);await h.ready();openApp(h,"ba");const page=appPage(h);
 assert.equal(find(h,"编辑应用",page),undefined);assert.match(page.textContent,/预设资料 · 功能连接/);assert.match(page.textContent,/我的配置/);assert.equal(page.querySelector("li.ac-endpoint").querySelectorAll("button").some(n=>n.textContent==="编辑"),false);
 find(h,"我的连接与规则",page).click();check(dialog(h),"cs").click();await submitEditor(h);let req=h.calls.find(c=>c.command==="save_site_catalog").args.request;assert.equal(req.applications.some(a=>a.id==="ba"),false);assert.deepEqual(req.resource_supplements[0],{resource_id:"ba",site_ids:["cs"],reference_rules:[]});
 const addRule=page.querySelectorAll("button").find(n=>n.title==="新增参考规则");addRule.click();input(h,editorField(h,"purpose"),"Personal rule");await submitEditor(h);assert.ok(find(h,"编辑规则",page));find(h,"编辑规则",page).click();input(h,editorField(h,"notes"),"Personal note");await submitEditor(h);
 find(h,"删除规则",page).click();find(h,"确认删除",h.nodes.get("app-ac-delete-dialog")).click();await h.ready();assert.equal(h.window.siteCatalog.snapshot().resource_supplements[0].reference_rules.length,0);
 h.nodes.get("app-ac-new-site").click();input(h,editorField(h,"name"),"Added personal");input(h,editorField(h,"address"),"personal.invalid");await submitEditor(h);assert.equal(h.window.siteCatalog.snapshot().resource_supplements[0].site_ids.length,2);
 find(h,"我的连接与规则",page).click();check(dialog(h),"cs").click();await submitEditor(h);assert.ok(h.window.siteCatalog.snapshot().sites.some(s=>s.id==="cs"));
});
test("website validation is explicit, switching custom kind never guesses protocol or silently drops connections",async()=>{
 const h=domHarness();await h.ready();openApp(h,"ca");find(h,"编辑应用",appPage(h)).click();choice(dialog(h),"资源类型","website").click();await submitEditor(h);assert.match(dialog(h).textContent,/完整 HTTP/);assert.equal(h.calls.length,1);assert.deepEqual(h.window.siteCatalog.snapshot().applications.find(a=>a.id==="ca").site_ids,["cs"]);cancel(h);
 h.nodes.get("ac-new-application").click();input(h,editorField(h,"name","ac-"),"Empty website");await submitEditor(h,"ac-");let req=h.calls.find(c=>c.command==="save_site_catalog").args.request;assert.equal(req.applications.find(a=>a.name==="Empty website").kind,"website");assert.deepEqual(req.applications.find(a=>a.name==="Empty website").site_ids,[]);
 h.nodes.get("ac-new-site").click();input(h,editorField(h,"name","ac-"),"Page");input(h,editorField(h,"address","ac-"),"https://other.invalid/api");await submitEditor(h,"ac-");assert.match(dialog(h,"ac-").textContent,/对应 HTTP/);choice(dialog(h,"ac-"),"传输协议","tcp").click();choice(dialog(h,"ac-"),"连接协议","https").click();await submitEditor(h,"ac-");assert.equal(dialog(h,"ac-").open,false);
});
test("orphan connections stay reusable; hidden checkbox selections and strict flat DTO survive save",async()=>{
 const snap=snapshot();snap.sites.push({...snap.sites[1],id:"orphan",name:"Unused connection",address:"orphan.invalid",port:null,transport:"unspecified",protocol:"unspecified"});const h=domHarness(snap);await h.ready();h.nodes.get("app-ac-new-application").click();input(h,editorField(h,"name"),"Reuse orphan");const reuse=dialog(h).querySelector("details.ac-reuse"),search=reuse.querySelector('input[type="search"]');check(reuse,"orphan").click();input(h,search,"nothing");assert.equal(check(reuse,"orphan").parentElement.hidden,true);assert.equal(check(reuse,"orphan").checked,true);await submitEditor(h);
 const req=h.calls.find(c=>c.command==="save_site_catalog").args.request;assert.deepEqual(req.applications.find(a=>a.name==="Reuse orphan").site_ids,["orphan"]);assert.deepEqual(req.sites.find(s=>s.id==="orphan"),{id:"orphan",name:"Unused connection",address:"orphan.invalid",port:null,transport:"unspecified",protocol:"unspecified",purpose:"auth",alpn:null});assert.equal(appPage(h).querySelectorAll("li.ac-endpoint").length,1);
});
test("tool capability radios require explicit choice, lock blocks mutation/apply but not browse, draft never auto-runs",async()=>{
 const h=domHarness();await h.ready();openApp(h,"ba");let row=webPage(h).querySelector("li.ac-endpoint"),apply=find(h,"填入草稿",row);assert.deepEqual(row.querySelectorAll('input[type="radio"]').map(n=>n.value),["http","tls"]);assert.equal(apply.disabled,true);row.querySelectorAll('input[type="radio"]')[1].click();assert.equal(apply.disabled,false);
 h.window.siteCatalog.setLocked(true);assert.equal(h.nodes.get("ac-new-application").disabled,true);row=webPage(h).querySelector("li.ac-endpoint");assert.equal(find(h,"填入草稿",row).disabled,true);nav(h,"group:cg");h.window.siteCatalog.setLocked(false);row=webPage(h).querySelector("li.ac-endpoint");find(h,"填入草稿",row).click();assert.equal(h.calls[1].tool,"tls");assert.equal(h.calls[1].draft.id,"bs");assert.equal(h.calls[2].page,"tools");assert.equal(h.calls.some(c=>/start/.test(c.command||"")),false);
});
test("copy endpoint remains an independent personal DTO; cancel and detached triggers restore reachable focus",async()=>{
 const h=domHarness();await h.ready();openApp(h,"ba");find(h,"复制连接",webPage(h)).click();assert.match(editorField(h,"name","ac-").value,/副本/);assert.equal(editorField(h,"address","ac-").value,"https://preset.invalid/");input(h,editorField(h,"name","ac-"),"Independent");assert.equal(h.window.siteCatalog.snapshot().sites[0].name,"Preset endpoint");cancel(h,"ac-");
 openApp(h,"ca");find(h,"编辑应用",appPage(h)).click();h.window.siteCatalog.setLocked(true);fire(h,dialog(h),"cancel");assert.equal(dialog(h).open,false);assert.equal(h.document.activeElement.textContent,"My app");assert.equal(h.document.activeElement.isConnected,true);
});
test("rule enum radios preserve null versus explicit unspecified and independent connection protocol choices",async()=>{
 const h=domHarness();await h.ready();openApp(h,"ca");find(h,"编辑规则",appPage(h)).click();assert.equal(editorField(h,"address").disabled,true);choice(dialog(h),"规则类型","host").click();input(h,editorField(h,"address"),"rule.invalid");choice(dialog(h),"传输","unspecified").click();choice(dialog(h),"协议","unspecified").click();await submitEditor(h);
 let req=h.calls.filter(c=>c.command==="save_site_catalog").at(-1).args.request;assert.equal(req.applications[0].reference_rules[0].transport,"unspecified");find(h,"编辑规则",appPage(h)).click();choice(dialog(h),"传输","").click();choice(dialog(h),"协议","").click();input(h,editorField(h,"address"),"");choice(dialog(h),"规则类型","unknown").click();await submitEditor(h);req=h.calls.filter(c=>c.command==="save_site_catalog").at(-1).args.request;assert.equal(req.applications[0].reference_rules[0].transport,null);assert.equal(req.applications[0].reference_rules[0].address,null);
 h.nodes.get("app-ac-new-site").click();input(h,editorField(h,"name"),"QUIC");input(h,editorField(h,"address"),"quic.invalid");choice(dialog(h),"传输协议","udp").click();choice(dialog(h),"连接协议","quic").click();input(h,editorField(h,"port"),"443");await submitEditor(h);req=h.calls.filter(c=>c.command==="save_site_catalog").at(-1).args.request;const site=req.sites.find(s=>s.name==="QUIC");assert.equal(site.protocol,"quic");assert.equal(site.transport,"udp");assert.equal(site.port,443);assert.equal(site.alpn,null);
});
test("quick range/tag/group choices filter without losing hidden selection and upstream clears downstream",async()=>{
 const snap=snapshot();for(let i=0;i<40;i++)snap.applications.push({...snap.applications[1],id:`extra-${i}`,name:`Extra ${i}`});const h=domHarness(snap);await h.ready();choice(h.nodes.get("quick-group"),"资源范围","application").click();quickChoice(h,"group","tag:ct").click();quickChoice(h,"app","extra-39").click();quickChoice(h,"site","cs").click();const search=h.nodes.get("quick-app").querySelector('input[type="search"]');input(h,search,"Extra 0");assert.equal(quickChoice(h,"app","extra-39").parentElement.hidden,true);assert.equal(quickChoice(h,"app","extra-39").checked,true);assert.equal(h.nodes.get("apply-site").disabled,false);
 h.window.siteCatalog.setLocked(true);assert.equal(h.nodes.get("apply-site").disabled,true);h.window.siteCatalog.setLocked(false);quickChoice(h,"app","extra-0").click();assert.equal(quickChoice(h,"site","cs").checked,false);choice(h.nodes.get("quick-group"),"资源范围","website").click();assert.equal(h.nodes.get("quick-app").querySelectorAll('input[type="radio"]').some(n=>n.checked),false);assert.equal(h.nodes.get("apply-site").disabled,true);assert.equal(h.calls.length,1);
});
test("group deletion keeps resources/tags/endpoints and shared endpoint deletion shows all references",async()=>{
 const h=domHarness();await h.ready();nav(h,"group:cg");find(h,"删除分组",appPage(h)).click();const del=h.nodes.get("app-ac-delete-dialog");assert.match(del.textContent,/不删除任何应用、网站、标签/);find(h,"确认删除",del).click();await h.ready();assert.equal(h.window.siteCatalog.snapshot().applications.length,2);assert.equal(h.window.siteCatalog.snapshot().tags.length,2);
 openApp(h,"ca");find(h,"永久删除",appPage(h)).click();assert.match(del.textContent,/My app/);find(h,"确认删除",del).click();await h.ready();assert.equal(h.window.siteCatalog.snapshot().sites.length,1);assert.equal(h.window.siteCatalog.snapshot().applications.find(a=>a.id==="ca").site_ids.length,0);assert.equal(h.window.siteCatalog.snapshot().applications.find(a=>a.id==="ca").reference_rules.length,1);
});
test("all live paths remain safe DOM, radio/search-only, scoped responsive layout and no automatic execution",async()=>{
 const h=domHarness();await h.ready();for(const id of ["app-ac-new-group","app-ac-new-tag","app-ac-new-application"]){h.nodes.get(id).click();assert.equal(h.document.querySelectorAll("select").length,0);cancel(h);}assert.doesNotMatch(source,/innerHTML|insertAdjacentHTML|network_start|\/api\/network-tools\/start/);assert.match(css,/@media\(max-width:540px\)/);assert.match(css,/\.application-catalog \.ac-dialog/);assert.match(css,/\.picker-fields:has\(#quick-app\)/);assert.equal(h.calls.length,1);
});

test("unsupported endpoint has no selectable capability and cannot apply any draft",async()=>{
 const snap=snapshot();snap.sites[1].capabilities=[];snap.sites[1].unavailable_reasons=["缺少可用检测参数"];const h=domHarness(snap);await h.ready();openApp(h,"ca");const row=appPage(h).querySelector("li.ac-endpoint");assert.equal(row.querySelectorAll('input[type="radio"]').length,0);assert.equal(find(h,"填入草稿",row).disabled,true);assert.match(row.textContent,/缺少可用检测参数/);assert.equal(h.calls.length,1);
});
test("editing shared connection cannot break a website reference, including builtin personal supplement",async()=>{
 const snap=snapshot();snap.sites[1].address="https://personal.invalid/api";snap.sites[1].protocol="https";snap.applications[0].site_ids.push("cs");snap.applications[0].supplement={resource_id:"ba",site_ids:["cs"],reference_rules:[]};snap.resource_supplements=[snap.applications[0].supplement];
 const h=domHarness(snap);await h.ready();openApp(h,"ca");find(h,"编辑",appPage(h)).click();choice(dialog(h),"传输协议","udp").click();await submitEditor(h);assert.match(dialog(h).textContent,/网站连接/);assert.equal(h.calls.length,1);assert.equal(h.window.siteCatalog.snapshot().sites.find(s=>s.id==="cs").transport,"tcp");
});
test("new resource kind is page-defined and trimmed tag name matches strict normalized response",async()=>{
 const h=domHarness();await h.ready();h.nodes.get("app-ac-new-application").click();input(h,editorField(h,"name")," Service ");await submitEditor(h);let req=h.calls.filter(c=>c.command==="save_site_catalog").at(-1).args.request;assert.equal(req.applications.find(a=>a.name==="Service").kind,"application");assert.equal("tag_ids" in req.applications[0],false);assert.equal("builtin" in req.tags[0],false);
 h.nodes.get("ac-new-tag").click();input(h,editorField(h,"name","ac-"),"  Work  ");await submitEditor(h,"ac-");req=h.calls.filter(c=>c.command==="save_site_catalog").at(-1).args.request;assert.equal(req.tags.find(t=>t.name==="Work").name,"Work");assert.equal(h.window.catalogController.state.unknown,false);
});
