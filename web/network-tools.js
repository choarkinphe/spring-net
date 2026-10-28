"use strict";
(() => {
  const $=id=>document.getElementById(id);
  const native=Boolean(window.__TAURI__?.core?.invoke);
  const activeStatuses=new Set(["resolving","running","stopping","draining"]);
  const tools={ping:"Ping",traceroute:"Traceroute",dns:"系统 DNS",tcp:"端口检测",tls:"TLS 握手",http:"HTTP 响应",udp_dns:"UDP DNS",quic:"QUIC 握手"};
  const portTools=["tcp","tls","udp_dns","quic"];
  const udpTools=["udp_dns","quic"];
  function selectedTool(){return document.querySelector('input[name="tool"]:checked')?.value || "ping";}
  function udpProxyIssue(tool){
    if(!udpTools.includes(tool))return "";
    if(!window.proxySettings?.isKnown?.()||typeof window.proxySettings.enabled!=="function")return "代理设置尚未核对，UDP DNS / QUIC 暂不可开始。";
    return window.proxySettings.enabled()?"UDP DNS / QUIC 不支持当前 HTTP CONNECT / SOCKS5 代理，请先在代理设置明确关闭代理；不会自动直连。":"";
  }
  function validQuery(name){return typeof name==="string"&&name.length>0&&name.length<=253&&name.split(".").every(label=>label.length<=63&&/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/i.test(label));}
  function validAlpn(value){return typeof value==="string"&&/^[\x21-\x7e]{1,255}$/.test(value)&&!value.includes(",");}
  function validateTarget(target,tool){
    if(!target)throw new Error(tool==="http"?"请输入完整的 HTTP(S) 网址。":"请输入单个 IP 或域名。");
    if(target.length>2048||/[\s\x00-\x1f\x7f\\]/.test(target))throw new Error("地址含空白、控制字符或长度超限。");
    if(tool==="http"){
      let url;try{url=new URL(target);}catch{throw new Error("HTTP检测请输入完整的 http:// 或 https:// 网址。");}
      if(!/^https?:\/\//i.test(target)||!["http:","https:"].includes(url.protocol)||!url.hostname)throw new Error("HTTP检测请输入完整的 http:// 或 https:// 网址。");
      const authority=target.split("://")[1].split(/[/?#]/)[0];
      if(authority.includes("@")||url.username||url.password||target.includes("?")||target.includes("#"))throw new Error("HTTP网址不接受账号密码、查询参数或片段。");
      return;
    }
    if(/[\/@?#%*,;|&$`()]/.test(target)||target.startsWith("-")||target.endsWith("."))throw new Error("请输入单个 IP 或域名，不包含协议、路径、端口或额外参数。");
    if(target.includes(":")){
      const ip=target.startsWith("[")&&target.endsWith("]")?target.slice(1,-1):target;
      if(!/^[0-9a-f:]+$/i.test(ip))throw new Error("IP / 域名和端口须分开填写。");
      try{new URL(`http://[${ip}]/`);}catch{throw new Error("IPv6 地址无效，端口须单独填写。");}
    }else{
      let url;try{url=new URL(`http://${target}/`);}catch{throw new Error("IP 或域名格式无效。");}
      if(url.hostname.split(".").some(label=>!label||label.length>63||!/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/i.test(label)))throw new Error("IP 或域名格式无效。");
    }
  }
  function buildRequest(requestId){
    const target=$("target").value.trim(),tool=selectedTool();
    if(!Object.hasOwn(tools,tool))throw new Error("请选择一个支持的工具。");
    validateTarget(target,tool);
    const proxyIssue=udpProxyIssue(tool);if(proxyIssue)throw new Error(proxyIssue);
    const body={request_id:requestId,target,tool};
    if(portTools.includes(tool)){
      const raw=$("port").value;
      if(raw){const port=Number(raw);if(!/^\d+$/.test(raw)||!Number.isInteger(port)||port<1||port>65535)throw new Error("端口必须是1～65535的整数，不支持范围。");body.port=port;}
      else if(tool==="tcp"||tool==="quic")throw new Error(tool==="tcp"?"请在端口框填写一个明确的 TCP 端口。":"请在端口框填写一个明确端口。");
    }
    if(tool==="udp_dns"){
      const name=$("dns-query-name").value.trim().replace(/\.$/,"");
      if(!validQuery(name))throw new Error("请填写合法的 DNS 查询域名。");
      const record_type=$("dns-query-type").value;
      if(!["A","AAAA"].includes(record_type))throw new Error("请选择 A 或 AAAA 查询类型。");
      body.udp_dns={query_name:name.toLowerCase(),record_type};
    }
    if(tool==="quic"){
      const alpn=$("quic-alpn").value;
      if(!validAlpn(alpn))throw new Error("请明确填写单个 ALPN 标识（可见 ASCII，不接受逗号列表，不自动猜 h3）。");
      body.alpn=alpn;
    }
    return body;
  }
  const labels={resolving:"正在解析地址…",running:"运行中",stopping:"正在停止并清理…",draining:"等待系统解析收尾…",completed:"执行结束",cancelled:"已停止",failed:"执行失败",error:"执行失败，请查看结果",timeout:"已超时",unsupported:"当前平台不支持"};
  const state={token:null,known:false,pending:false,uncertain:null,snapshot:null,timer:null,read:null,epoch:0};
  const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  function valid(snapshot){
    if(snapshot===null)return true;
    if(!snapshot||!uuid.test(snapshot.id)||!uuid.test(snapshot.request_id)||!Object.hasOwn(tools,snapshot.tool)||typeof snapshot.target!=="string"||!Object.hasOwn(labels,snapshot.status)||typeof snapshot.output!=="string"||typeof snapshot.summary!=="string"||!(snapshot.resolved_ip===null||typeof snapshot.resolved_ip==="string"))return false;
    const {tool,port,udp_dns,alpn}=snapshot;
    if(portTools.includes(tool)){if(!Number.isInteger(port)||port<1||port>65535)return false;}else if(port!==undefined)return false;
    if(tool==="udp_dns"){
      if(!udp_dns||Object.keys(udp_dns).sort().join(",")!=="query_name,record_type"||!validQuery(udp_dns.query_name)||!["A","AAAA"].includes(udp_dns.record_type))return false;
    }else if(udp_dns!==undefined)return false;
    if(tool==="quic"){if(!validAlpn(alpn))return false;}else if(alpn!==undefined)return false;
    return true;
  }
  function showError(message){$("error").textContent=message;$("error").hidden=!message;}
  function render(){
    const s=state.snapshot,active=Boolean(s&&activeStatuses.has(s.status));
    const ownLocked=!state.known||state.pending||Boolean(state.uncertain)||active;
    window.appShell?.setLocked("tools",ownLocked);
    const locked=ownLocked||window.appShell?.isLockedExcept("tools");
    $("target").disabled=Boolean(locked);document.querySelectorAll('input[name="tool"]').forEach(n=>n.disabled=Boolean(locked));
    $("port").disabled=Boolean(locked||!portTools.includes(selectedTool()));
    $("port-label").hidden=!portTools.includes(selectedTool());
    $("port").required=false;
    $("port").placeholder=selectedTool()==="tls"?"默认 443":selectedTool()==="udp_dns"?"默认 53":"例如 443";
    $("udp-dns-fields").hidden=selectedTool()!=="udp_dns";
    $("quic-fields").hidden=selectedTool()!=="quic";
    $("dns-query-name").disabled=Boolean(locked||selectedTool()!=="udp_dns");
    $("dns-query-type").disabled=Boolean(locked||selectedTool()!=="udp_dns");
    $("quic-alpn").disabled=Boolean(locked||selectedTool()!=="quic");
    $("input-fields").classList.toggle("has-port",portTools.includes(selectedTool()));
    $("start").disabled=Boolean(locked||!$("target").value.trim()||udpProxyIssue(selectedTool()));
    $("stop").disabled=!state.known||state.pending||!active||["stopping","draining"].includes(s?.status);
    $("clear-uncertain").hidden=!(state.uncertain&&state.known&&!active&&!state.pending);
    $("status").textContent=state.uncertain?"操作结果尚未核对，请重新连接":!state.known?"后台尚未连接":s?labels[s.status]:"等待开始";
    $("destination").textContent=s?`${tools[s.tool]} · ${s.target}${s.port?` · 端口 ${s.port}`:""}${s.resolved_ip?` → ${s.resolved_ip}`:""}${s.route?` · ${window.proxySettings?.label(s.tool,s.route)||""}`:""}`:"尚未发起网络检查。";
    const output=$("output"),atBottom=output.scrollHeight-output.clientHeight-output.scrollTop<32;
    const text=s?.output|| (active?"等待系统工具输出…":"运行输出会显示在这里。");
    if(output.textContent!==text){output.textContent=text;if(atBottom)output.scrollTop=output.scrollHeight;}
    $("summary").textContent=s?.summary||"输入站点后，选择工具并点击开始。";
    const tool=selectedTool();
    const hints={ping:"Ping：4 次，间隔 1 秒，最长 10 秒。",traceroute:"Traceroute：最多 20 跳，最长 30 秒；星号不等于故障点。",dns:"系统名称解析：显示 IP，可能使用 hosts / 缓存，不连接目标。",tcp:"仅检测一个 TCP 端口，5 秒超时；不发送应用数据、不扫描。",tls:"单次 TCP + TLS 握手，正常证书校验；信任随包 Mozilla 根，不是系统/企业 CA。",http:"仅一次 HEAD，10 秒超时；不跟随跳转、不重试、不读取正文，不自动改用 GET。",udp_dns:"向指定 DNS 服务器发送一次 UDP A/AAAA 查询，5 秒；响应可不完整，不回退 TCP，不证明其他 UDP 应用可用。",quic:"单目标、8 秒 QUIC/TLS 握手并校验所填 ALPN；协议内部可能重传，不发送 HTTP/3 或业务请求。全局 CONNECT/SOCKS5 代理启用时禁止，需明确关闭代理。"};
    $("budget").textContent=hints[tool];
    $("tool-route").textContent=udpProxyIssue(tool)||window.proxySettings?.label(tool)||"代理设置尚未核对";
    $("target-label").textContent=tool==="http"?"完整网址":udpTools.includes(tool)?"DNS 服务器 / QUIC 主机":"IP / 域名";
    const help={ping:"输入单个 IP 或域名，不包含协议、路径或端口。",traceroute:"输入单个 IP 或域名；仅追踪这个地址，不扫描网段。",dns:"输入要解析的域名；IP 输入会显示无需解析。",tcp:"左侧填写 IP / 域名，右侧填写单个端口，不能填写端口范围。",tls:"填写需要验证的 IP / 域名与端口；端口留空时使用 443。",http:"请输入完整 HTTP(S) 网址；保留路径，不接受账号密码、查询参数或片段。",udp_dns:"服务器填单个 IP/域名，查询名另填。仅验证匹配 DNS 响应；超时不等于端口关闭。",quic:"仅填写明确支持 QUIC 的服务器、端口和 ALPN，不对聊天/GDS未知UDP端口盲发数据。"};
    $("target-help").textContent=help[tool];
    $("target").placeholder=tool==="http"?"https://example.com/health":tool==="udp_dns"?"例如企业 DNS 服务器 10.0.0.53":"例如 example.com 或 192.168.1.1";
    if(!window.appShell)window.siteCatalog?.setLocked(Boolean(locked));
  }
  function applySite(site){return applyEndpoint(site,selectedTool());}
  function applyEndpoint(site,tool){
    if(!state.known||state.pending||state.uncertain||activeStatuses.has(state.snapshot?.status)||window.appShell?.isLockedExcept("tools"))return false;
    if(!Object.hasOwn(tools,tool))return false;
    if(!site||typeof site.address!=="string"||!Array.isArray(site.capabilities)||!site.capabilities.includes(tool)){showError(`该端点未提供 ${tools[tool]} 能力，请在应用管理中选择可用检测能力。`);return false;}
    const proxyIssue=udpProxyIssue(tool);if(proxyIssue){showError(proxyIssue);return false;}
    const isUrl=/^https?:\/\//i.test(site.address);
    let url;try{url=isUrl?new URL(site.address):null;validateTarget(tool==="http"?site.address:url?url.hostname.replace(/^\[|\]$/g,""):site.address,tool);}catch(error){showError(error.message);return false;}
    if(tool==="quic"&&!validAlpn(site.alpn)){showError("QUIC 端点缺少有效的明确 ALPN，请先编辑端点。");return false;}
    if(portTools.includes(tool)&&!url&&(!Number.isInteger(site.port)||site.port<1||site.port>65535)){showError("该端点缺少明确的单个端口，请先编辑端点。");return false;}
    document.querySelectorAll('input[name="tool"]').forEach(n=>n.checked=n.value===tool);
    $("target").value=tool==="http"?(isUrl?site.address:""):(url?url.hostname.replace(/^\[|\]$/g,""):site.address);
    $("port").value=site.port??(url?(url.port||(url.protocol==="https:"?443:80)):"");
    $("quic-alpn").value=tool==="quic"?(site.alpn||""):"";
    if(tool==="udp_dns"){$("dns-query-name").value="";$("dns-query-type").value="A";}
    $("draft-note").textContent=tool==="http"&&!isUrl?`${site.name} 未保存完整网址，请手动填写 http:// 或 https:// 地址。`:`已填入「${site.name}」。${portTools.includes(tool)?($("port").value?"端口来自站点配置或网址。":"该站点未设置默认端口。 "):""}请手动点击开始。`;
    showError("");render();return true;
  }
  window.networkToolDraft={applySite,applyEndpoint,isLocked:()=>!state.known||state.pending||Boolean(state.uncertain)||activeStatuses.has(state.snapshot?.status)};
  async function request(kind,body){
    if(native)return window.__TAURI__.core.invoke({info:"network_info",state:"network_state",start:"network_start",stop:"network_stop"}[kind],body?{request:body}:undefined);
    const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),8000);
    try{
      const response=await fetch(`/api/network-tools/${kind}`,{method:body?"POST":"GET",headers:body?{"Content-Type":"application/json","X-AoNet-Token":state.token||""}:{Accept:"application/json"},body:body?JSON.stringify(body):undefined,signal:controller.signal,credentials:"same-origin",cache:"no-store"});
      let data;try{data=await response.json();}catch{const error=new Error("后台响应无法解析，请重新连接核对，不要重复开始。");error.uncertain=Boolean(body);throw error;}if(!response.ok)throw new Error(data.error||`请求失败（${response.status}）`);return data;
    }catch(error){if(error.name==="AbortError"||error instanceof TypeError){error.uncertain=Boolean(body);}throw error;}finally{clearTimeout(timer);}
  }
  async function readState(){
    if(state.read)return state.read;
    const epoch=state.epoch;
    const pending=request("state").then(snapshot=>{if(epoch!==state.epoch)return;if(!valid(snapshot))throw new Error("后台结果格式无效，保留最近输出。");state.snapshot=snapshot;state.known=true;if(state.uncertain&&snapshot?.request_id===state.uncertain)state.uncertain=null;render();}).catch(error=>{if(epoch===state.epoch){state.known=false;showError(error.message);render();}});
    state.read=pending;try{await pending;}finally{if(state.read===pending)state.read=null;}
  }
  function poll(){clearTimeout(state.timer);const epoch=state.epoch;state.timer=setTimeout(async()=>{await readState();if(epoch===state.epoch)poll();},500);}
  async function connect(){
    state.known=false;render();showError("");
    try{const info=await request("info");if(typeof info.version!=="string"||(!native&&typeof info.token!=="string"))throw new Error("初始化信息无效。");state.token=info.token;$("version").textContent=`v${info.version}`;await readState();poll();}catch(error){showError(error.message);render();}
  }
  async function start(event){
    event.preventDefault();if(!state.known||state.pending||state.uncertain||activeStatuses.has(state.snapshot?.status)||window.appShell?.isLockedExcept("tools"))return;
    const target=$("target").value.trim();if(!target){showError("请输入站点地址。");return;}
    if(!globalThis.crypto?.randomUUID){showError("当前环境无法生成安全请求编号。");return;}
    let body;try{body=buildRequest(crypto.randomUUID());}catch(error){showError(error.message);return;}
    state.pending=true;showError("");render();
    try{const snapshot=await request("start",body);if(!valid(snapshot)||!snapshot||snapshot.request_id!==body.request_id){const e=new Error("启动结果无法核对，请重新连接，不要重复开始。");e.uncertain=true;throw e;}state.snapshot=snapshot;state.known=true;}catch(error){if(error.uncertain||(native&&!(typeof error==="string"))){state.uncertain=body.request_id;state.known=false;}showError(typeof error==="string"?error:error.message);}finally{state.pending=false;render();}
  }
  async function stop(){
    const current=state.snapshot;if(!current||!activeStatuses.has(current.status)||state.pending)return;
    state.pending=true;render();showError("");
    try{const snapshot=await request("stop",{id:current.id});if(!valid(snapshot)||!snapshot||snapshot.id!==current.id)throw new Error("停止结果无法核对，请重新连接。");state.snapshot=snapshot;}catch(error){state.known=false;showError(typeof error==="string"?error:error.message);}finally{state.pending=false;render();}
  }
  $("clear-uncertain").addEventListener("click",()=>{if(!state.known||state.pending||activeStatuses.has(state.snapshot?.status))return;state.uncertain=null;showError("已解除结果不确定锁定。若再次开始，可能重复之前已执行的检查。");render();});
  $("tool-form").addEventListener("submit",start);$("stop").addEventListener("click",stop);$("reconnect").addEventListener("click",()=>void connect());$("target").addEventListener("input",render);document.querySelectorAll('input[name="tool"]').forEach(n=>n.addEventListener("change",()=>{showError("");render();}));
  window.appShell?.subscribe(source=>{if(source!=="tools")render();});
  window.addEventListener("aonet-proxy",render);
  window.addEventListener("pagehide",()=>{state.epoch++;clearTimeout(state.timer);});
  render();void connect();
})();
