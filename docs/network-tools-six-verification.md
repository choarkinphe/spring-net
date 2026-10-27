# Ping / Traceroute 增补 DNS、端口、TLS、HTTP 验收

日期：2026-10-27。界面保持单页基础工具；本轮没有重新打包桌面应用。

## 工具行为

- **Ping**：固定系统工具、单个数值IP、默认4次/1秒，最长10秒。摘要解析依赖C locale；未知本地化输出保留原文且不虚构统计。
- **Traceroute**：固定系统命令、数值IP、20跳上限和30秒截止；星号不指故障点。macOS真实公网路径未探测；参数按本机man确认。Windows仅编写固定系统路径适配/样本验证，没有原生运行证据。
- **DNS**：仅系统名称解析，显示全部去重返回地址、解析耗时；可能使用hosts/cache。不连接任何地址；不声称权威DNS、服务器、TTL、CNAME、DNSSEC或纯DNS网络RTT。IP字面量标记无需解析。
- **TCP端口检测**：每次只连接一个明确端口，域名校验全部DNS答案后固定首IP，一次建连、5秒超时，成功立即关闭且不发送应用数据。拒绝/超时不直接推断根因；拒绝目标端口指向实际AoNet listener（通过测试注入），避免本工具循环调用自身控制接口。
- **TLS**：独立TCP连接加一次握手，使用固定IP及原域名身份，正常证书链/名称验证、Mozilla随包根，不使用系统/企业CA、不关闭校验、不发HTTP。不代表登录或业务健康。
- **HTTP**：完整HTTP(S) URL，保留路径与Host，显式冲突端口拒绝；一次HEAD，10秒，不跟随重定向、不重试、不读正文、405不切GET、429/5xx仍表示收到响应。

同一页面一次只执行一个工具。TCP需显式端口或URL显式端口；TLS可默认443，允许改端口；HTTP从完整URL决定端口及路径；DNS/Ping/Traceroute不接受端口字段。HTTPS scheme本身可确定HTTP/TLS意图，任意主机的数值443则不会触发这些工具的TLS/HTTP行为。

## 自动检查

- 后端最近一次独立完整复跑157项Rust测试通过，fmt、Clippy all-targets `-D warnings`、Tauri `--locked --offline`编译检查通过。重复回归出现过旧TLS fixture的ConnectionReset间歇失败，已兼容无载荷连接被重置的关闭方式；另一次合并检查未保留具体失败项，紧接独立复跑通过，因此不宣称重复运行稳定性已完全证明。
- 前端64项测试通过，包含六工具调用输入形态、隐藏端口不挡提交、结果状态文本、旧工作台HTML独立保留和IPC/旧实现回归。
- 本机fixtures覆盖特殊DNS答案只显示不连接、control-port拒绝、固定地址/不回退、TCP拒绝/超时、TLS可信/不可信/错误名称/取消、HTTP HEAD状态/Location/无正文/单次请求。

## 实际验证及限制

- 系统`/sbin/ping -n -c 4 -i 1 127.0.0.1`真实回环命令收到4/4、0丢包。
- Desktop Browser Preview新主界面加载成功：一个输入框、Ping/Traceroute/DNS/端口/TLS/HTTP六个radio和同一输出区，App版本显示0.4.0；无旧工作台侧栏。已显示DNS/TCP等条件端口显隐逻辑由自动化测试覆盖。
- 自动权限判断服务在该轮Preview交互时暂时不可用，实际GUI Ping、Traceroute、TCP/TLS、HTTP按钮运行和最终截图未能完成；未通过shell/curl/直接JS伪装UI的网络请求来规避。Preview资源/HTTP状态不可用，不能声称已完成端到端UI操作。
- 未执行公共航司/OTA/GDS的解析、Ping、Traceroute、端口、TLS或HTTP请求。安全策略按限制实现。
- 新六工具仍只在当前源码/Preview；桌面版本0.4.0旧包未包含这一页和工具扩展，未重新构建/替换原生包。
