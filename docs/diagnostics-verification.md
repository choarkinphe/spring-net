# 分层诊断验收记录

日期：2026-10-27。环境：macOS arm64，Rust共享后端、浏览器Preview。

## 自动检查通过

- `cargo test --offline --quiet`：76项通过、0失败。
- `node --test web/*.test.cjs`：37项通过、0失败。
- Rust fmt、Clippy all-targets `-D warnings`、app.js/transport.js语法通过。
- `cargo check --manifest-path src-tauri/Cargo.toml --locked --offline`通过。所需signal-hook-registry通过正常依赖获取补齐，未修改全局Cargo配置；两套lock保留明确ring TLS provider，不引入aws-lc。

新增覆盖包含严格输入/地址策略、全DNS答案验证与固定IP、专用解析线程超时draining与有界退出、UUID幂等/冲突/旧stop、实验与诊断双向互斥、报告分流、HTTP守卫、路由格式与不可达标记、输出边界/子进程kill+reap、本机TLS正常校验/错误名字与信任/超时取消及单连接复用。测试TLS信任仅在测试中注入专用CA，生产仍使用Mozilla根，没有危险verifier。

## 实际浏览器证据

- 通过项目统一入口启动9893，独立`target/diagnostics-verification/`数据与空授权，健康日志确认ready；未终止用户9876/9878或上一批9891实例。
- 设置显式开启本机自检，选择服务实际拥有的TCP端口仅填写草稿，再点击明确开始。
- TCP自检完成：IP字面量DNS不适用、回环路由不适用、TCP成功、TLS关闭。页面四层状态与事实一致。
- 在同一自有plain TCP服务显式选择TLS，观察TCP成功、TLS握手错误（非TLS服务reset），没有将TCP成功覆盖为失败。
- 输入localhost并允许LAN，实际系统解析返回`::1`和`127.0.0.1`；由于自检只拥有指定IPv4地址，全部答案校验阻止未授权IPv6回环，TCP/TLS未执行。
- UUID绑定JSON报告GET200，类型application/json，partial=false，内容保留TCP成功/TLS错误与same_tcp_stream=true，不含会话token。
- 最终源码重启验收实例后再次完成TCP自检；console无错误；资源记录无第三方请求。启动就绪前曾出现本地连接拒绝，重试后页面/API正常，并非从未出现失败请求。
- 375×812下页面scrollWidth=375，无全页横向溢出；四层证据存在，样式检查确认长内容换行。结束后恢复desktop视口。

## 未执行 / 不作保证

- 没有对公共航司、OTA、GDS或任何公网目标执行DNS/路由/TCP/TLS测试，没有登录、API或业务载荷请求。
- 系统traceroute实际网络执行未验收：macOS本地man/固定参数已核实，路由输出与进程清理由离线样本/受控子进程测试验证；不冒充已测得真实路由。
- Windows只有固定系统路径/参数适配与输出样本验证，没有Windows原生运行结果；其他平台路由明确不支持，不阻止TCP/TLS。
- 实际页面全局停止在活动握手期间的点击矩阵未完成；自动测试已覆盖取消、EOF、DNSdrain和子进程回收。补充浏览器操作时自动权限判断服务暂不可用，两次输入调用失败，因此没有绕过权限继续操作。
- 桌面原生命令与后端编译检查通过，但没有构建/打开包含新功能的安装包，没有验证原生报告保存面板实际落盘。
- 正向TLS成功通过本机测试CA fixture验证，浏览器自检只有普通TCP服务，不声称页面已测得生产信任根下TLS成功。
- 仅保留最近一次诊断；没有持久化诊断历史、HTTP业务验证、连接容量试验或运营商PCDN自动归因。

使用方式与结论边界见[分层诊断说明](layered-diagnostics.md)，总体分期见[产品需求](product-requirements.md)。
