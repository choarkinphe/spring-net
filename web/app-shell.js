"use strict";
(() => {
  const locks=new Map([["tools",true],["batch",true]]),listeners=new Set(),pages=new Set(["tools","catalog","batch","settings"]);
  let page="tools";
  function show(next){
    if(!pages.has(next))return;
    page=next;
    for(const key of pages){document.getElementById(`${key}-page`).hidden=key!==next;document.getElementById(`nav-${key}`).setAttribute("aria-current",key===next?"page":"false");}
    document.getElementById(next==="tools"?"tool-title":`${next}-title`).focus();
    window.dispatchEvent(new CustomEvent("aonet-page",{detail:next}));
  }
  window.appShell={
    show,getPage:()=>page,
    isLockedExcept:source=>[...locks].some(([key,value])=>key!==source&&value),
    setLocked(source,value){value=Boolean(value);if(locks.get(source)===value)return;locks.set(source,value);for(const notify of listeners)notify(source);},
    subscribe(notify){listeners.add(notify);return ()=>listeners.delete(notify);}
  };
  for(const key of pages)document.getElementById(`nav-${key}`).addEventListener("click",()=>show(key));
})();
