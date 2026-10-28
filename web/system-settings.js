"use strict";
(() => {
  const $=id=>document.getElementById(id),native=Boolean(window.__TAURI__?.core?.invoke);
  const state={snapshot:null,loading:false,saving:false,unknown:false,editor:null,deletion:null,focus:null,read:null};
  const fields=s=>({profiles:s.profiles.map(p=>({id:p.id,name:p.name,url:p.url})),active_id:s.active_id});
  function valid(s){return Boolean(s&&s.version===1&&Number.isSafeInteger(s.revision)&&s.revision>=0&&Array.isArray(s.profiles)&&s.profiles.length<=30&&s.profiles.every(p=>typeof p.id==="string"&&typeof p.name==="string"&&typeof p.url==="string"&&/^(http|socks5):\/\//.test(p.url))&&(s.active_id===null||s.profiles.some(p=>p.id===s.active_id)));}
  function text(id,value){$(id).textContent=value;$(id).hidden=!value;}
  function errorText(e){return String(e?.message||e||"未知错误");}
  function ownLocked(){return !state.snapshot||state.loading||state.saving||state.unknown;}
  function blocked(){return ownLocked()||Boolean(window.appShell?.isLockedExcept("settings"));}
  function routeLabel(tool,route){
    if(["ping","traceroute","dns"].includes(tool)||route?.local_direct)return tool==="dns"?"本机系统解析 · 不经过代理":"本机直连 · 不经过代理";
    if(route)return route.proxy?`${route.proxy.url.startsWith("socks5:")?"SOCKS5":"HTTP CONNECT"} · ${route.profile_name||route.proxy.url} · ${route.proxy.url}（目标域名在本机解析）`:"直连 · 不使用代理";
    if(!state.snapshot||state.unknown)return "代理设置尚未核对，连接检测暂不可开始";
    const profile=state.snapshot.profiles.find(p=>p.id===state.snapshot.active_id);
    return profile?`${profile.url.startsWith("socks5:")?"SOCKS5":"HTTP CONNECT"} · ${profile.name} · ${profile.url}（目标域名在本机解析）`:"直连 · 不使用代理";
  }
  function notify(){window.dispatchEvent(new CustomEvent("aonet-proxy"));}
  function element(tag,value){const n=document.createElement(tag);if(value!==undefined)n.textContent=value;return n;}
  function button(value,fn,disabled){const n=element("button",value);n.type="button";n.disabled=disabled;n.addEventListener("click",fn);return n;}
  async function transport(body){
    if(native)return window.__TAURI__.core.invoke(body?"save_proxy_settings":"proxy_settings",body?{request:body}:undefined);
    const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),8000);
    try{
      let token;
      if(body){const response=await fetch("/api/network-tools/info",{signal:controller.signal,credentials:"same-origin",cache:"no-store"});if(!response.ok)throw new Error("无法获取会话令牌");token=(await response.json()).token;if(typeof token!=="string")throw new Error("会话令牌无效");}
      const response=await fetch("/api/proxy-settings",{method:body?"PUT":"GET",headers:body?{"Content-Type":"application/json","X-AoNet-Token":token}:{Accept:"application/json"},body:body?JSON.stringify(body):undefined,signal:controller.signal,credentials:"same-origin",cache:"no-store"});
      const raw=await response.text();let data;try{data=JSON.parse(raw);}catch{throw new Error(raw||"代理设置响应无法核对");}if(!response.ok)throw new Error(data.error||`请求失败（${response.status}）`);return data;
    }finally{clearTimeout(timer);}
  }
  async function load(){
    if(state.saving||state.editor||state.deletion)return;
    if(state.read)return state.read;
    state.loading=true;render();
    const pending=(async()=>{try{const s=await transport();if(!valid(s))throw new Error("代理设置格式无效");state.snapshot=s;state.unknown=false;text("proxy-error","");}
      catch(e){state.unknown=true;text("proxy-error",`${errorText(e)}。请重新进入代理设置核对；不会自动切换为直连。`);}
      finally{state.loading=false;render();notify();}})();
    state.read=pending;try{await pending;}finally{if(state.read===pending)state.read=null;}
  }
  function same(a,b){return JSON.stringify({active_id:a.active_id,profiles:[...a.profiles].sort((x,y)=>x.id.localeCompare(y.id))})===JSON.stringify({active_id:b.active_id,profiles:[...b.profiles].sort((x,y)=>x.id.localeCompare(y.id))});}
  async function save(data){
    if(blocked())throw new Error("请等待测试停止并收尾、设置核对完成后再修改。");
    const revision=state.snapshot.revision;state.saving=true;state.unknown=true;render();
    try{const s=await transport({expected_revision:revision,...data});if(!valid(s))throw new Error("保存结果无法核对");state.snapshot=s;state.unknown=false;text("proxy-error","");text("proxy-message","已保存。设置仅影响后续新测试；未发起任何目标检测。");}
    catch(error){try{const latest=await transport();if(!valid(latest))throw new Error("核对响应无效");state.snapshot=latest;state.unknown=false;if(latest.revision===revision+1&&same(fields(latest),data)){text("proxy-message","保存结果已通过只读核对确认。");return;}throw new Error(`${errorText(error)}。已读取最新设置，请取消后重新编辑；未重发保存。`);}catch(e){if(state.unknown)text("proxy-error","保存结果未知，已锁定修改。请关闭弹窗并重新进入代理设置核对，不会自动重发或直连。");throw e;}}
    finally{state.saving=false;render();notify();}
  }
  function render(){
    window.appShell?.setLocked("settings",ownLocked());
    const disabled=blocked();$("proxy-add").disabled=disabled;$("proxy-editor-save").disabled=disabled;$("proxy-editor-cancel").disabled=state.saving;$("proxy-delete-confirm").disabled=disabled;$("proxy-delete-cancel").disabled=state.saving;
    for(const id of ["proxy-name","proxy-type","proxy-host","proxy-port"])$(id).disabled=disabled;
    $("proxy-current").textContent=state.snapshot&&!state.unknown?`当前采用：${routeLabel()}`:"代理设置尚未核对；不会自动使用直连。";
    $("proxy-busy").textContent=window.appShell?.isLockedExcept("settings")?"检测运行、收尾或状态不确定期间，代理设置只读。":"启用后每 30 秒自动检查代理端点；不建立常驻隧道，不验证目标转发。";
    const list=$("proxy-list");list.replaceChildren();if(!state.snapshot)return;
    function row(profile){const li=element("li"),label=element("label"),radio=element("input"),info=element("span"),name=element("strong",profile?.name||"关闭代理 / 直连"),address=element("small",profile?.url||"后续连接测试不通过代理");li.className="proxy-row";radio.type="radio";radio.name="active-proxy";radio.value=profile?.id||"";radio.checked=state.snapshot.active_id===(profile?.id||null);radio.disabled=disabled;
      radio.addEventListener("change",async()=>{if(blocked()||!radio.checked)return;try{const data=fields(state.snapshot);data.active_id=profile?.id||null;await save(data);}catch(e){text("proxy-error",errorText(e));render();}});
      info.append(name,address);label.append(radio,info);li.append(label);
      if(profile){const actions=element("div");actions.className="row-actions";actions.append(button("编辑",()=>openEditor(profile),disabled),button("删除",()=>openDelete(profile),disabled));li.append(actions);}list.append(li);
    }
    row(null);state.snapshot.profiles.forEach(row);
  }
  function openEditor(profile=null){
    if(blocked())return;state.focus=document.activeElement;state.editor={id:profile?.id||null,revision:state.snapshot.revision};
    $("proxy-editor-title").textContent=profile?"编辑代理":"新增代理";$("proxy-name").value=profile?.name||"";const url=profile?new URL(profile.url):null;
    document.querySelectorAll('input[name="proxy-type"]').forEach(n=>n.checked=n.value===(url?.protocol==="socks5:"?"socks5":"http"));$("proxy-host").value=url?.hostname.replace(/^\[|\]$/g,"")||"";
    $("proxy-port").value=url?(url.port||(url.protocol==="http:"?"80":"")):"";text("proxy-editor-error","");$("proxy-editor").showModal();$("proxy-name").focus();
  }
  function closeEditor(){if(state.saving)return;$("proxy-editor").close();state.editor=null;restoreFocus();}
  function restoreFocus(){if(state.focus?.isConnected)state.focus.focus();else $("proxy-add").focus();}
  function profileDraft(id){
    const name=$("proxy-name").value.trim(),host=$("proxy-host").value.trim(),raw=$("proxy-port").value,type=document.querySelector('input[name="proxy-type"]:checked')?.value;
    if(!name||new TextEncoder().encode(name).length>200)throw new Error("名称不能为空且不能超过 200 个 UTF-8 字节。");
    if(!host||/[\s/@?#\\]/.test(host)||host.includes("://")||!['http','socks5'].includes(type))throw new Error("主机请仅填写域名或 IP，不包含协议、路径或账号密码。");
    const port=Number(raw);if(!/^\d+$/.test(raw)||!Number.isInteger(port)||port<1||port>65535)throw new Error("端口必须是 1～65535 的整数。");
    const bracketed=host.includes(":")&&!host.startsWith("[")?`[${host}]`:host;
    return {id,name,url:`${type}://${bracketed}:${port}`};
  }
  async function submit(event){event.preventDefault();if(blocked()||!state.editor)return;
    try{if(state.editor.revision!==state.snapshot.revision)throw new Error("代理设置已变化，请取消后重新编辑。");const profile=profileDraft(state.editor.id||`proxy-${crypto.randomUUID()}`),data=fields(state.snapshot);data.profiles=state.editor.id?data.profiles.map(p=>p.id===profile.id?profile:p):[...data.profiles,profile];await save(data);closeEditor();}
    catch(e){text("proxy-editor-error",errorText(e));}
  }
  function openDelete(profile){if(blocked())return;state.focus=document.activeElement;state.deletion={id:profile.id,revision:state.snapshot.revision};$("proxy-delete-text").textContent=`删除「${profile.name}」？${state.snapshot.active_id===profile.id?"这是当前启用项，确认后将关闭代理，后续测试改为直连。":"不会影响其他配置。"}`;$("proxy-delete").showModal();$("proxy-delete-cancel").focus();}
  function closeDelete(){if(state.saving)return;$("proxy-delete").close();state.deletion=null;restoreFocus();}
  async function remove(){if(blocked()||!state.deletion)return;try{if(state.deletion.revision!==state.snapshot.revision)throw new Error("代理设置已更新，请重新核对。");const data=fields(state.snapshot);data.profiles=data.profiles.filter(p=>p.id!==state.deletion.id);if(data.active_id===state.deletion.id)data.active_id=null;await save(data);closeDelete();}catch(e){text("proxy-error",errorText(e));closeDelete();}}
  window.proxySettings={label:routeLabel,enabled:()=>Boolean(state.snapshot&&!state.unknown&&state.snapshot.active_id!==null),isKnown:()=>Boolean(state.snapshot&&!state.unknown&&!state.loading&&!state.saving),refresh:load};
  $("proxy-add").addEventListener("click",()=>openEditor());$("proxy-editor-form").addEventListener("submit",submit);$("proxy-editor-cancel").addEventListener("click",closeEditor);$("proxy-delete-cancel").addEventListener("click",closeDelete);$("proxy-delete-confirm").addEventListener("click",()=>void remove());
  $("proxy-editor").addEventListener("cancel",e=>{e.preventDefault();closeEditor();});$("proxy-delete").addEventListener("cancel",e=>{e.preventDefault();closeDelete();});
  window.addEventListener("aonet-page",()=>void load());window.appShell?.subscribe(source=>{if(source!=="settings")render();});render();void load();
})();
