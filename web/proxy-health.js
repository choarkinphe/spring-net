"use strict";
(() => {
  const $=id=>document.getElementById(id),native=Boolean(window.__TAURI__?.core?.invoke);
  const labels={unknown:"待检查",disabled:"直连",checking:"检查中",reachable:"端点可达",failed:"端点不可达",timeout:"超时",draining:"解析收尾",paused:"已暂停"};
  const state={snapshot:null,read:null,timer:null,epoch:0,alive:true,failed:false,receivedAt:0,announcement:""};
  function valid(s){return Boolean(s&&Object.hasOwn(labels,s.status)&&s.route&&Number.isSafeInteger(s.route.settings_revision)&&(s.route.proxy===null||typeof s.route.proxy?.url==="string")&&(s.checked_at===null||typeof s.checked_at==="string")&&(s.latency_ms===null||Number.isFinite(s.latency_ms))&&typeof s.evidence==="string");}
  function reserveSpace(){const height=$("proxy-footer").getBoundingClientRect().height;document.documentElement.style.setProperty("--proxy-footer-height",`${Math.ceil(height)}px`);}
  function render(){
    const s=state.snapshot,stale=state.failed||!s||Date.now()-state.receivedAt>6000;
    const status=stale?"unknown":s.status;
    const label=stale?"状态未知":labels[s.status];
    $("proxy-health-state").textContent=label;$("proxy-health-state").dataset.state=status;
    $("proxy-health-route").textContent=s?.route.proxy?`${s.route.profile_name||"代理"} · ${s.route.proxy.url}`:s&&!stale?"":"尚未核对当前配置";
    const checked=s?.checked_at?new Date(s.checked_at).toLocaleTimeString():"尚未完成检查";
    const elapsed=s?.latency_ms!==null&&s?.latency_ms!==undefined?` · ${s.latency_ms.toFixed(1)} ms`:"";
    $("proxy-health-detail").textContent=s?.route.proxy?`${s.evidence} · 上次完成 ${checked}${elapsed}${s.error?` · ${s.error}`:""}；仅代理端点，未验证目标转发能力。`:stale?"无法确认当前代理状态，不会据此改为直连。":"启用代理后每 30 秒检查一次端点；不连接目标网站。";
    $("proxy-health-latency").textContent=!stale&&s?.route.proxy&&s.status==="reachable"&&Number.isFinite(s.latency_ms)?`${s.latency_ms.toFixed(1)} ms`:"";
    $("proxy-footer").title=$("proxy-health-detail").textContent;
    $("proxy-health-route").title=$("proxy-health-route").textContent;
    const announcement=`${status}:${s?.route.profile_id||""}`;
    if(state.announcement!==announcement){$("proxy-health-announcement").textContent=label;state.announcement=announcement;}
    reserveSpace();
  }
  async function request(){
    if(native)return window.__TAURI__.core.invoke("proxy_health");
    const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),5000);
    try{const response=await fetch("/api/proxy-health",{headers:{Accept:"application/json"},credentials:"same-origin",cache:"no-store",signal:controller.signal});if(!response.ok)throw new Error("health read failed");return await response.json();}finally{clearTimeout(timer);}
  }
  async function read(){
    if(state.read||!state.alive)return state.read;
    const epoch=state.epoch;
    const pending=request().then(snapshot=>{if(epoch!==state.epoch||!state.alive)return;if(!valid(snapshot))throw new Error("invalid health state");state.snapshot=snapshot;state.failed=false;state.receivedAt=Date.now();render();}).catch(()=>{if(epoch===state.epoch&&state.alive){state.failed=true;render();}});
    state.read=pending;try{await pending;}finally{if(state.read===pending)state.read=null;}
  }
  function poll(){clearTimeout(state.timer);if(!state.alive)return;state.timer=setTimeout(async()=>{await read();poll();},1000);}
  function changed(){state.epoch++;state.failed=true;render();void read();}
  const observer=globalThis.ResizeObserver?new ResizeObserver(reserveSpace):null;observer?.observe($("proxy-footer"));
  window.addEventListener("resize",reserveSpace);window.addEventListener("aonet-proxy",changed);
  window.addEventListener("pagehide",()=>{state.alive=false;state.epoch++;clearTimeout(state.timer);observer?.disconnect();});
  render();void read().then(poll);
})();
