# 监控、批量与代理后端交付记录

日期：2026-10-27。此记录只覆盖后端、共享HTTP/IPC与本机自动化验证；不替代前端或打包验收。

## 已实现

- `MonitorManager`：有界、版本化配置原子保存，保存和重开不启动；每次明确开始；串行TCP/可选TLS检查；每目标至少60秒；暂停/恢复、停止、20条有界样本和最近异常。浏览器15秒租约、代次校验；桌面随应用退出。停止与DNS收尾分开，不能取消的OS解析保留准入保护。
- 批量管理器：独立请求、最多4并发/3轮/120秒，每轮去重目标最多尝试一次、完成屏障、实际尝试及未完成/取消/策略跳过结果。相同UUID不重复执行，旧ID不能停止新任务，取消请求本身被丢弃也不丢失worker所有权。
- 单站诊断、监控、批量、旧HTTP/TCP引擎均可明确使用HTTP代理。无认证、仅HTTP CONNECT；拒绝凭据、SOCKS/PAC/隐式系统代理；407不降级、不直连。
- 目标DNS全部校验后固定一个数字IP；代理DNS全部校验后固定第一个代理地址，无隐式备用地址重试。CONNECT目标为数字IPv4/IPv6，隧道内HTTP origin-form保留Host；HTTPS/TLS保留原主机名SNI与Mozilla根验证。路由仅观察到代理的本地路径，代理后不可见。
- `RunConfig.proxy` / `DiagnosticConfig.proxy`可选，旧配置缺省直连。`Permit.proxy_routes`缺省空，旧授权仅直连；新增路由显式绑定代理URL、允许IP/CIDR和代理方许可说明，继承目标许可的并发/速率/时长/到期限制。
- 授权完整预览、确认、重载及bootstrap均保留代理路由。`settings.http_proxy`只保存规范化无秘密配置，不改变系统代理、不自动选择执行路径。
- 监控/批次/诊断/旧实验在共享服务准入处互斥；公共域名冷却在同一锁下检查并持久化预留，跨任务类型和代理切换不重置。429/503在响应头到达时立即退避；407停止本次代理流量。
- 目标DNS、代理DNS和监控DNS使用有界专用线程/准入计数，未来结果丢弃不启动额外网络阶段；应用退出有界等待并如实记录未终止OS查询。
- 批次JSON报告接口`/api/batch/report?id=...`与原生export kind `batch`；监控state带当前proxy和dns_draining，批次snapshot带规范化config。

## 重要解释

代理是受信任的转发路径。客户端可证实自己请求CONNECT的数字地址，不能证实恶意代理真正连接的远端IP；TLS证明目标身份，不证明物理路径。纯TCP代理负载计数必须在UI中称为“已建立代理隧道”，不能称为独立验证的远端TCP连接数。本机名称解析不经过HTTP代理。代理必须允许目标端口与IP形式CONNECT，不支持则明确失败。

代理HTTP分支当前使用独立HTTP/1.1隧道请求，不共享原reqwest直连连接池；界面不得把代理请求显示为已验证的连接池复用。HEAD只检查响应，GET最多64KiB并遵守总字节预算。公共站点不因代理或监控配置而获得负载授权。

## 验证

- `cargo test --offline --quiet`：130项通过，0失败。
- `cargo fmt --check`、`cargo clippy --offline --all-targets -- -D warnings`通过。
- `cargo check --manifest-path src-tauri/Cargo.toml --locked --offline`通过。
- 本机fake proxy/目标测试覆盖数字CONNECT、Host/SNI/证书、HEAD/GET、body上限、提前429、407无回退、停止/超时EOF、路由授权与DNS范围、旧配置serde兼容、批次轮次/退避/取消及监控租约。
- 没有发起公共航司、OTA、GDS网络测试或外网压力测试；没有自动改版本、打包、退出用户应用或修改系统代理。
- 原生UI、代理设置UI/profile保留、授权编辑round-trip、卡片布局及打包由主线程继续验收。Windows没有原生运行结果。
