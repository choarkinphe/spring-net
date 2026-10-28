"use strict";
(() => {
  const $=id=>document.getElementById(id);
  const native=Boolean(window.__TAURI__?.core?.invoke);
  const state={snapshot:null,selectedGroup:"",loading:false,saving:false,unknown:false,locked:true,editor:null,deletion:null,focus:null};
  const siteFields=s=>({id:s.id,name:s.name,address:s.address,port:s.port??null});
  const groupFields=g=>({id:g.id,name:g.name,description:g.description,site_ids:[...g.site_ids]});
  function custom(snapshot=state.snapshot){return {sites:snapshot.sites.filter(s=>!s.builtin).map(siteFields),groups:snapshot.groups.filter(g=>!g.builtin).map(groupFields)};}
  function valid(s){return Boolean(s&&s.version===1&&Number.isSafeInteger(s.revision)&&s.revision>=0&&Array.isArray(s.sites)&&Array.isArray(s.groups)&&s.sites.every(x=>typeof x.id==="string"&&typeof x.name==="string"&&typeof x.address==="string"&&typeof x.builtin==="boolean")&&s.groups.every(x=>typeof x.id==="string"&&typeof x.name==="string"&&typeof x.description==="string"&&typeof x.builtin==="boolean"&&Array.isArray(x.site_ids)));}
  function message(id,text){$(id).textContent=text;$(id).hidden=!text;}
  function blocked(){return !state.snapshot||state.loading||state.saving||state.unknown||state.locked;}
  function button(text,action,disabled=false){const b=document.createElement("button");b.type="button";b.textContent=text;b.disabled=disabled;b.addEventListener("click",action);return b;}
  function options(select,rows,placeholder){const previous=select.value;select.replaceChildren();const first=document.createElement("option");first.value="";first.textContent=placeholder;select.append(first);for(const row of rows){const option=document.createElement("option");option.value=row.id;option.textContent=row.name+(row.builtin?" · 内置":"");select.append(option);}select.value=rows.some(x=>x.id===previous)?previous:"";}
  const subscribers=new Set();
  function catalogSnapshot(){return state.snapshot?JSON.parse(JSON.stringify(state.snapshot)):null;}
  function publish(){for(const notify of subscribers)notify(catalogSnapshot());}
  function showPage(catalog){window.appShell.show(catalog?"catalog":"tools");}
  async function transport(body){
    if(native)return window.__TAURI__.core.invoke(body?"save_site_catalog":"site_catalog",body?{request:body}:undefined);
    const controller=new AbortController(),timeout=setTimeout(()=>controller.abort(),8000);
    try{
      let token;
      if(body){const info=await fetch("/api/network-tools/info",{signal:controller.signal,credentials:"same-origin",cache:"no-store"});if(!info.ok)throw new Error("无法获取会话令牌。");token=(await info.json()).token;if(typeof token!=="string")throw new Error("会话令牌格式无效。");}
      const response=await fetch("/api/site-catalog",{method:body?"PUT":"GET",headers:body?{"Content-Type":"application/json","X-AoNet-Token":token}:{Accept:"application/json"},body:body?JSON.stringify(body):undefined,signal:controller.signal,credentials:"same-origin",cache:"no-store"});
      const text=await response.text();let result;try{result=JSON.parse(text);}catch{if(response.ok)throw new Error("目录响应无法解析。");throw new Error(text||`请求失败（${response.status}）`);}
      if(!response.ok)throw new Error(result.error||`请求失败（${response.status}）`);return result;
    }finally{clearTimeout(timeout);}
  }
  async function load(){
    if(state.loading||state.saving)return;state.loading=true;render();
    try{const snapshot=await transport();if(!valid(snapshot))throw new Error("站点目录格式无效。");state.snapshot=snapshot;state.unknown=false;message("catalog-error","");message("catalog-message","目录已读取。选择站点仅填入草稿，不会自动检测。");}
    catch(e){message("catalog-error",String(e.message||e));}finally{state.loading=false;render();publish();}
  }
  function sameCustom(a,b){const canonical=x=>JSON.stringify({sites:[...x.sites].sort((a,b)=>a.id.localeCompare(b.id)),groups:[...x.groups].sort((a,b)=>a.id.localeCompare(b.id)).map(g=>({...g,site_ids:[...g.site_ids].sort()}))});return canonical(a)===canonical(b);}
  async function save(data){
    if(blocked())throw new Error("请等待检测结束并重新读取站点目录后再保存。");
    state.saving=true;state.unknown=true;render();const revision=state.snapshot.revision;
    try{
      const snapshot=await transport({expected_revision:revision,...data});if(!valid(snapshot))throw new Error("保存响应无法核对。");state.snapshot=snapshot;state.unknown=false;message("catalog-error","");message("catalog-message","已保存。站点修改已同步到所有引用它的组合。");
    }catch(error){
      // Never repeat a write whose result is unknown; reconcile with one read instead.
      try{const latest=await transport();if(!valid(latest))throw new Error("目录格式无效");state.snapshot=latest;state.unknown=false;if(latest.revision===revision+1&&sameCustom(custom(latest),data)){message("catalog-message","保存结果已通过重新读取核对。");return;}throw new Error(`${String(error.message||error)} 已重新读取最新目录；未重发保存，请核对后重新编辑。`);}
      catch(readError){if(state.unknown)message("catalog-error","保存结果不确定，已锁定修改。请点击顶部“常用站点”重新进入以核对，不会自动重发。");throw readError;}
    }finally{state.saving=false;render();publish();}
  }
  function renderQuick(){
    const snapshot=state.snapshot;if(!snapshot)return;
    options($("quick-group"),snapshot.groups,"全部站点");
    const group=snapshot.groups.find(g=>g.id===$("quick-group").value);
    const sites=group?snapshot.sites.filter(s=>group.site_ids.includes(s.id)):snapshot.sites;
    options($("quick-site"),sites,"请选择站点");
    $("quick-group").disabled=state.loading||state.saving||state.locked;
    $("quick-site").disabled=state.loading||state.saving||state.locked;
    $("apply-site").disabled=blocked()||!$("quick-site").value;
  }
  function renderGroups(){
    const nav=$("catalog-groups"),focused=document.activeElement;
    const focusedId=nav.contains(focused)?focused.dataset.groupId:null;
    const buttons=[];
    function add(id,name,count){
      const b=button("",()=>{if(state.loading||state.saving)return;if(state.selectedGroup!==id)$("catalog-search").value="";state.selectedGroup=id;render();},state.loading||state.saving);
      b.dataset.groupId=id;b.setAttribute("aria-current",state.selectedGroup===id?"true":"false");
      const title=document.createElement("span"),total=document.createElement("span");title.textContent=name;total.textContent=String(count);total.className="group-total";b.append(title,total);buttons.push(b);nav.append(b);
    }
    nav.replaceChildren();add("","全部站点",state.snapshot.sites.length);
    for(const [builtin,label] of [[true,"内置分组"],[false,"我的分组"]]){
      const heading=document.createElement("p");heading.className="group-section-label";heading.textContent=label;nav.append(heading);
      const groups=state.snapshot.groups.filter(g=>g.builtin===builtin);
      for(const group of groups)add(group.id,group.name,group.site_ids.length);
      if(!groups.length){const empty=document.createElement("p");empty.className="hint";empty.textContent=builtin?"暂无内置分组":"暂无自定义分组";nav.append(empty);}
    }
    if(focusedId!==null)(buttons.find(b=>b.dataset.groupId===focusedId)||buttons[0]).focus();
  }
  function render(){
    window.appShell?.setLocked("catalog",state.saving||state.unknown);
    const disabled=blocked();$("new-site").disabled=disabled;$("new-group").disabled=disabled;
    $("editor-save").disabled=disabled;$("editor-cancel").disabled=state.saving;$("delete-confirm").disabled=disabled;$("delete-cancel").disabled=state.saving;
    if(!state.snapshot)return;
    if(!state.snapshot.groups.some(g=>g.id===state.selectedGroup))state.selectedGroup="";
    renderGroups();
    const group=state.snapshot.groups.find(g=>g.id===state.selectedGroup);
    $("group-name").textContent=group?group.name+(group.builtin?" · 内置组合":" · 自定义组合"):"全部站点";
    $("group-description").textContent=group?group.description:"一个站点可以加入多个组合；编辑站点会同时更新各组合。";
    $("group-actions").replaceChildren();
    if(group&&!group.builtin)$("group-actions").append(button("编辑",()=>openEditor("group",group),disabled),button("删除",()=>openDelete("group",group),disabled));
    const query=$("catalog-search").value.trim().toLocaleLowerCase();
    const sites=state.snapshot.sites.filter(s=>(!group||group.site_ids.includes(s.id))&&(!query||`${s.name} ${s.address}`.toLocaleLowerCase().includes(query)));
    $("catalog-count").textContent=`${sites.length} 个站点${state.locked?" · 检测期间暂不可修改或填入地址":""}`;
    $("catalog-sites").replaceChildren();
    for(const site of sites){
      const li=document.createElement("li"),info=document.createElement("div"),title=document.createElement("strong"),address=document.createElement("p"),meta=document.createElement("p"),actions=document.createElement("div");
      title.textContent=site.name;address.className="site-address";address.textContent=site.address+(site.port?` · 默认端口 ${site.port}`:"");meta.className="hint";
      const memberships=state.snapshot.groups.filter(g=>g.site_ids.includes(site.id)).map(g=>g.name);
      meta.textContent=`${site.builtin?"内置":"自定义"} · ${memberships.length?memberships.join("、"):"未加入组合"}`;
      if(site.builtin&&site.source){const source=document.createElement("p");source.className="hint provenance";source.textContent=`来源：${site.source}${site.checked_at?` · 核实日期 ${site.checked_at}`:""}`;info.append(title,address,meta,source);}else info.append(title,address,meta);
      actions.className="row-actions";actions.append(button("用于检测",()=>{if(window.networkToolDraft.applySite(site))showPage(false);},disabled),button("复制",()=>openEditor("site",site,true),disabled));
      if(!site.builtin)actions.append(button("编辑",()=>openEditor("site",site),disabled),button("删除",()=>openDelete("site",site),disabled));
      li.append(info,actions);$("catalog-sites").append(li);
    }
    if(!sites.length){const empty=document.createElement("li"),total=group?group.site_ids.length:state.snapshot.sites.length;empty.textContent=query?`本${group?"分组":"站点库"}共 ${total} 个站点，没有匹配「${$("catalog-search").value.trim()}」的结果。清空搜索可查看全部。`:"该分组暂无站点。可以新增站点，或编辑组合添加成员。";$("catalog-sites").append(empty);}
    renderQuick();
  }
  function openEditor(kind,item=null,copy=false){
    if(blocked())return;state.focus=document.activeElement;state.editor={kind,id:item&&!copy?item.id:null,revision:state.snapshot.revision};
    $("editor-title").textContent=`${copy?"复制":item?"编辑":"新增"}${kind==="site"?"站点":"组合"}`;
    $("editor-name").value=(item?.name||"")+(copy?"（副本）":"");$("editor-address").value=item?.address||"";$("editor-port").value=item?.port??"";$("editor-description").value=item?.description||"";
    $("site-fields").hidden=kind!=="site";$("group-fields").hidden=kind!=="group";$("editor-address").required=kind==="site";$("editor-address").disabled=kind!=="site";$("editor-port").disabled=kind!=="site";
    $("editor-hint").textContent=kind==="site"?(copy?"复制后为独立站点，修改不会影响原站点。":"修改会同步到所有引用此站点的组合。默认端口仅供 TCP / TLS 填入草稿使用。"):(copy?"仅复制组合，继续引用原站点；如需独立修改，请另行复制站点。":"勾选统一站点库中的站点；删除组合不会删除站点。");
    $("editor-members").replaceChildren();if(kind==="group")for(const site of state.snapshot.sites){const label=document.createElement("label"),input=document.createElement("input"),text=document.createElement("span");input.type="checkbox";input.value=site.id;input.checked=Boolean(item?.site_ids.includes(site.id));text.textContent=`${site.name} · ${site.address}`;label.append(input,text);$("editor-members").append(label);}
    message("editor-error","");$("catalog-dialog").showModal();$("editor-name").focus();
  }
  function closeEditor(){if(state.saving)return;$("catalog-dialog").close();state.editor=null;restoreFocus();}
  function restoreFocus(){const target=state.focus;if(target?.isConnected)target.focus();else $("new-site").focus();}
  async function submit(event){
    event.preventDefault();if(blocked()||!state.editor)return;
    try{
      if(state.editor.revision!==state.snapshot.revision)throw new Error("目录已更新，请取消并重新打开编辑，避免覆盖其他修改。");
      const {kind,id}=state.editor,data=custom(),name=$("editor-name").value.trim();if(!name)throw new Error("请填写名称。");const key=id||`custom-${crypto.randomUUID()}`;
      if(kind==="site"){
        const raw=$("editor-port").value;let port=null;if(raw){port=Number(raw);if(!/^\d+$/.test(raw)||!Number.isInteger(port)||port<1||port>65535)throw new Error("默认端口必须为 1～65535 的整数。");}
        const site={id:key,name,address:$("editor-address").value.trim(),port};if(!site.address)throw new Error("请填写地址。");data.sites=id?data.sites.map(s=>s.id===id?site:s):[...data.sites,site];
      }else{const group={id:key,name,description:$("editor-description").value.trim(),site_ids:[...$("editor-members").querySelectorAll("input:checked")].map(n=>n.value)};data.groups=id?data.groups.map(g=>g.id===id?group:g):[...data.groups,group];}
      await save(data);closeEditor();
    }catch(e){message("editor-error",String(e.message||e));}
  }
  function openDelete(kind,item){
    if(blocked()||item.builtin)return;state.focus=document.activeElement;state.deletion={kind,id:item.id};
    const groups=state.snapshot.groups.filter(g=>g.site_ids.includes(item.id));
    $("delete-impact").textContent=kind==="group"?`删除组合「${item.name}」？只删除组合与成员引用，不会删除任何站点。`:`删除站点「${item.name}」？${groups.length?`同时从以下 ${groups.length} 个组合移除：${groups.map(g=>g.name).join("、")}。`:"此站点未加入任何组合。"}此操作不可撤销。`;
    $("delete-dialog").showModal();$("delete-cancel").focus();
  }
  function closeDelete(){if(state.saving)return;$("delete-dialog").close();state.deletion=null;restoreFocus();}
  async function remove(){
    if(blocked()||!state.deletion)return;const {kind,id}=state.deletion,data=custom();
    if(kind==="group")data.groups=data.groups.filter(g=>g.id!==id);else{data.sites=data.sites.filter(s=>s.id!==id);data.groups=data.groups.map(g=>({...g,site_ids:g.site_ids.filter(member=>member!==id)}));}
    try{await save(data);closeDelete();}catch(e){message("catalog-error",String(e.message||e));closeDelete();}
  }
  window.siteCatalog={snapshot:catalogSnapshot,subscribe(notify){subscribers.add(notify);notify(catalogSnapshot());return ()=>subscribers.delete(notify);},reload:load,setLocked(value){if(state.locked!==value){state.locked=value;render();}}};
  window.appShell?.subscribe(source=>{if(source!=="catalog")window.siteCatalog.setLocked(window.appShell.isLockedExcept("catalog"));});
  window.addEventListener("aonet-page",event=>{if(event.detail==="catalog")void load();});
  $("catalog-search").addEventListener("input",render);
  $("quick-group").addEventListener("change",renderQuick);$("quick-site").addEventListener("change",renderQuick);$("apply-site").addEventListener("click",()=>{const site=state.snapshot?.sites.find(s=>s.id===$("quick-site").value);if(site&&!blocked())window.networkToolDraft.applySite(site);});
  $("new-site").addEventListener("click",()=>openEditor("site"));$("new-group").addEventListener("click",()=>openEditor("group"));
  $("catalog-editor").addEventListener("submit",submit);$("editor-cancel").addEventListener("click",closeEditor);$("delete-cancel").addEventListener("click",closeDelete);$("delete-confirm").addEventListener("click",()=>void remove());
  $("catalog-dialog").addEventListener("cancel",event=>{event.preventDefault();closeEditor();});$("delete-dialog").addEventListener("cancel",event=>{event.preventDefault();closeDelete();});
  state.locked=window.appShell?window.appShell.isLockedExcept("catalog"):window.networkToolDraft.isLocked();void load();
})();
