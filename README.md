# spring-net

原名 AoNet。源码目标仓库：[choarkinphe/spring-net](https://github.com/choarkinphe/spring-net)。更名后保留 `local.aonet.desktop` 数据标识、`AONET_*` 环境变量和原有控制协议，确保已有配置/历史兼容；旧版本文档及包名保留历史事实。不要同时运行新旧应用共享同一数据目录。

**Rust 驱动的本地网络实验台。** 中文浏览器 GUI，多站点列表选择、低频连通性探测、授权 HTTP 并发测试和真实 TCP 连接保持测试。

产品方向为无需预先录入拓扑的通用办公网络诊断，详见 [产品需求与分期验收](docs/product-requirements.md)。新增的业务连接资料目录用于查看 Galileo/Travelport、Sabre、Amadeus、IBE+ 的官方配置依据和未知项，**不自动探测，也不等于业务可用性验证**。现新增独立[分层诊断](docs/layered-diagnostics.md)：系统DNS、系统路由追踪、一次TCP建连及同一连接TLS握手，支持停止和UUID绑定JSON报告，不发送业务HTTP。路由星号不等于故障点，TLS成功不等于业务正常。连接容量和运营商根因归属仍未实现。[连接目录验收记录](docs/connection-catalog-verification.md)记录上一批资料功能；分层诊断的平台与执行证据另见[本批验收](docs/diagnostics-verification.md)。v0.4.0已提供macOS arm64桌面包，签名与ZIP检查通过；监控、批量和代理预览及边界见[本版验收](docs/release-0.4.0.md)，整体桌面原生交互仍请试用，见[桌面交付状态](docs/desktop-status.md)。

> 公共网站预设不是压测授权。航司、OTA、常用网站只能低频探测。最多 1000 并发仅针对自有/明确获授权且列入配置的端点；默认不启动自检或流量；可在 GUI 登记真实授权端点。禁止用于未授权施压、连接耗尽或影响第三方服务。

## Tauri 2 跨平台桌面版

**macOS Apple Silicon 本地构建：** 新产物为 `dist/spring-net.app` 和 `dist/spring-net-macOS-arm64.zip`；此前 AoNet 包保留，不自动删除。可将应用拖入 Applications。无需启动终端或浏览器服务器；应用仅使用本机临时签名，未通过 Apple 公证，跨机器下载可能触发系统安全提示，不要关闭系统安全保护。

2026-10-27 已实现 GUI 授权管理、测试方案/工作区持久化、有界历史及按轮导出；50 项核心测试、21 项前端测试通过。已验证独立受控服务 HTTP/TCP 浏览器全链路及原生窗口/IPC/HTTP/历史。原生文件写入完整矩阵仍未验收通过。详见 [桌面验收状态](docs/desktop-status.md)。Windows 已提供原生构建入口，但**尚未生成/验收 Windows 安装包**；Mac最低版本配置为13.0，仅本机实际系统完成运行检查，不代表所有版本均已测试。

实现为 Tauri 2 + 同进程 Rust 引擎，通过窄 IPC 调用，**不使用子进程 stdout/READY/HTTP 控制端口**。旧 Swift 失败实验包已隔离在 `legacy-swift/artifacts/`，不要使用。

### 可重复构建入口

开发者需要 Node/npm、本地 Rust（含 `File::try_lock` 的 Rust 1.89+，建议当前 stable）。Mac 需要 Xcode；Windows 需要 MSVC C++ Build Tools/SDK 和 WebView2。先在项目根目录获取本地 CLI：

```bash
npm ci
```

已生成并保留 `package-lock.json` 和 `src-tauri/Cargo.lock`，构建脚本将 `--locked` 传给 Cargo。Mac：

```bash
bash scripts/build-macos.sh
```

Windows 在 PowerShell 中运行项目 `scripts/build-windows.ps1`，生成 NSIS `-setup.exe`；脚本仅在 Windows 执行。本机 Mac 不替代 Windows 验收，不承诺跨编译安装包。WebView2 未安装时安装器默认联网引导。构建成功后脚本才复制产物至 `dist/`，不会使用旧 Swift 包。

推荐使用 [GitHub Actions 托管构建](docs/github-actions.md)：main/PR 自动检查，手动或版本标签触发 Windows x64、macOS arm64/Intel 原生打包，产物及 SHA256 上传为 artifacts；不需要自建原生 Runner。原 [GitLab CI 与自建 Runner 说明](docs/gitlab-ci.md) 保留兼容，打包默认关闭。

本机曾遇官方源 TLS 下载失败；获准后使用临时 Cargo 镜像取得依赖，没有修改全局配置。若本机仅镜像缓存可用，可单次运行：

```bash
CARGO_NET_OFFLINE=true AONET_BUILD_MIRROR=1 bash scripts/build-macos.sh
```

正常网络直接使用上述默认构建命令。桌面 release 的 build-override 保留过程宏调试信息以兼容当前 macOS 链接器。

### 桌面数据与权限设计

- 数据根使用 Tauri `app_data_dir()`，标识为 `local.aonet.desktop`：Mac 通常为 `~/Library/Application Support/local.aonet.desktop/`，Windows 为用户 AppData 的对应标识目录；以平台实际返回值为准。
- 根下 `data/` 保存列表/冷却/方案/设置/运行历史，`authorized-targets.json` 首次以空清单创建，不覆盖用户配置。与旧 Swift 的 `.../AoNet/` 路径不同，**不静默迁移**。
- 旧数据可从浏览器版导出列表后显式导入；若迁移旧目录，须完全退出两端、保留冷却记录、不覆盖现有配置。授权文件单独审核，不从列表导入。
- 桌面源配置原生 JSON 选择/保存面板；取消不写入，IPC 不暴露任意文件路径、shell 或任意 URL 请求。界面包内加载，远程导航不授予 IPC。
- 关闭退出按取消→等待测试/演示任务→退出处理，单实例插件及文件锁保护数据。Mac自定义退出菜单将 Cmd-Q 送入同一清理路径；原生菜单退出已验证，文件面板完整操作矩阵仍见验收限制。
- 构建脚本的 Mac 临时签名不等于 Developer ID 或公证；Windows 当前未配置证书签名。不要关闭系统安全保护。

## 浏览器开发模式启动

需要 Rust 稳定工具链（建议 1.88+）及 Cargo，首次构建需要访问 crates.io。无需 Node、npm 或前端构建。

```bash
bash .claude/start-dev.sh
```

打开 **http://127.0.0.1:9876**。启动脚本等待健康检查，复用健康 AoNet，不终止占用端口的其他程序。Ctrl-C 只停止本次启动的服务。Claude Desktop Preview 使用项目 `.claude/launch.json` 的 **AoNet** 配置。VSCode 安装 CodeLLDB 后使用“单进程调试”配置。

源码布局：`src/` Rust 后端，`web/` 编译嵌入 GUI，`presets/` 分类 JSON，`config/` 授权配置，`data/` 本机运行数据。修改内嵌前端文件后需要重新编译并重启。站点预设在启动时读取，授权可在 GUI 审核保存或重新加载，因此分发二进制时需同时携带 `presets/` 和 `config/`。

可选环境变量：`AONET_PORT`（0 为系统分配端口）、`AONET_ROOT`（资源根目录）、`AONET_DATA_DIR`（自定义列表/冷却数据目录）、`AONET_AUTH_FILE`（独立授权文件）。`AONET_MANAGED=1` 与 `AONET_INSTANCE_ID` 只为 CLI 托管集成测试保留；Tauri 桌面不依赖进程管道，普通浏览器模式不依赖 stdin。不要同时启动共享同一数据目录的多个实例。

## 当前主界面：基础检测、常用站点与批量测试

当前入口保持简洁：**基础检测**提供 Ping、Traceroute、DNS、单端口 TCP、TLS、HTTP；**常用站点**用于管理站点及组合；新增源码中的 **批量测试** 支持多个组合去重后的公共低频 HTTP、授权 HTTP 与授权 TCP 保持。队列、次数、并发、连接上限及逐站明细见[批量测试说明](docs/catalog-batch.md)。以下历史工作台说明不代表当前首页提供所有入口。

- 内置 7 个组合、55 个站点：首页地址附官方来源与核实日期，只读，可复制成自定义。
- 自定义站点可新增、编辑、复制、删除；组合可新建、复制、编辑成员和删除。一个站点可以属于多个组合，修改一次同步到所有引用；复制站点才会产生独立记录，复制组合仍引用原站点。允许空组合及未分组站点。
- 基础检测展开“选择常用站点”，或在管理页点“用于检测”，只填写草稿，仍需手动点开始。TCP/TLS 使用已保存默认端口或完整 URL 对应端口；裸域名不猜端口。HTTP 必须有完整 HTTP(S) URL，默认端口字段不会隐式改变 HTTP 地址。
- 保存只验证语法，不解析 DNS、不连接站点，也不授予执行或压测权限。执行仍使用各工具原有地址和端口规则。
- 新目录原子保存到数据目录 `site-catalog.json`，版本与修订号用于防止多窗口覆盖；最多 1000 个自定义站点、50 个自定义组合、10000 个成员引用，最终 JSON 不超过 1 MiB。保存结果未知时仅重新读取核对，不自动重发。
- 新文件不存在时，旧 `lists.json` 的自定义列表只在内存迁移为共享站点和引用组合；首次明确保存新目录才写入。旧文件不改，两套数据不双向同步。损坏文件明确报错，不静默覆盖。运行期间从外部修改目录文件会阻止后续保存，需先备份并重启；“重新读取”读取服务内存，不从外部文件热加载。不同进程不可共用数据目录。

本批已按用户要求打包为 v0.6.0 macOS arm64 桌面应用并打开；签名与 ZIP 完整性检查通过。验收及原生交互限制见[统一站点库说明](docs/site-catalog.md)。

## 历史工作台操作

1. **公共站点探测**：按分类勾选中国航司、国际航司、OTA、主流网站中的实际目标后检查摘要并开始。固定上限 4 并发、2 RPS、同保守域名桶至少 30 秒一次、每轮最长 120 秒。默认只发 HEAD，不抓取页面资源，不自动重试、跳转或改用 GET。大列表在短时间内可能未全部调度。
2. **授权 HTTP**：先在“授权目标”登记专用受控端点，再选择该端点，设置并发、起发 RPS、时长、超时、爬升时间。默认 10 并发/10 RPS/30 秒。默认复用连接；可选独立 HTTP/1.1 短连接。
3. **授权 TCP**：选择专用 TCP 端点，逐步建立并保持实际 socket，不发送请求内容，不自动无限补连。建连速率与目标连接数单独配置。
4. 点击停止后等状态进入终态；此时 worker 与 socket 已释放，才能新开测试。JSON 导出含配置、汇总和时间序列；CSV 为每站点汇总。终态自动保存历史（默认最多100轮/100MiB），可按轮查看/导出；归档失败会明确报错。
5. “站点列表”支持新增、编辑、复制、删除、JSON 导入/导出，粘贴每行 `名称 URL` 或 URL。自定义数据原子保存到 `data/lists.json`，损坏时明确报错、不静默覆盖。导入上限 1 MiB/1000 目标/50 列表。

## 指标的真实含义

- **请求并发 ≠ TCP 连接数**。连接池及 HTTP 协议可复用连接；HTTP 模式只报告活动请求。
- TCP “连接数”指本机已建立且尚未释放的 socket；不能保证远端持续存活，不代表 TLS、HTTP 或业务成功。
- “收到响应”与“HTTP 2xx/3xx”分开；403/405/429/503 是远端返回的 HTTP 状态，不一定是你的网络故障。429/503 会暂停本轮同域请求。
- 延迟统计为收到 HTTP 响应的尝试总耗时（HEAD 到响应头；GET 到读取结束或截断），包含 4xx/5xx，不等于业务成功延迟；已收到响应后读取失败的耗时也包含在内。未收到响应的失败及取消不纳入。TCP 为建连耗时，连接结束时汇总。分位数使用固定 1 ms 分桶，向上取整；不提供虚构的 DNS/TLS 分段耗时。
- 实际技术栈为 Axum 0.7.9 + Reqwest 0.12.28，利用本机可用缓存完成离线构建。域名额度采用保守末两级域名共享而非完整 PSL：例如 `*.co.uk` 共用限额，可能更严格合并但不会因轮换子域而放宽。
- GET 每次只读取最多 64 KiB，导出标记截断；“字节”是读取的应用层字节，不包含 TCP/TLS/HTTP 头等开销，不是线路带宽。每轮读取总预算 256 MiB，最多 100000 次起发。
- 图表保留最近 600 个快照点。RPS 是快照区间完成次数/时间；TCP 完成表示连接生命周期结束，不能把它当 HTTP RPS。
- 本机演示不经过路由器/运营商外网，只验证工具。测真实网络需要在你控制的另一设备/远端部署端点，结合远端资源、路由设备指标和分级实验判断瓶颈，不能仅靠本工具自动断言“网络最大承载”。
- 1000 是可配置上限而非性能承诺。启动前检查文件描述符软限制，本机演示需同时容纳客户端/服务端 socket；不足时拒绝，不自动修改系统配置。

## 登记授权端点

在 GUI 的“授权目标”新增/编辑，核对完整规范化预览后明确确认保存。也可编辑固定 `config/authorized-targets.json` 后点击重新加载；桌面使用自己的应用数据授权文件。文件默认空清单；本机自检默认关闭，仅在“设置”显式开启。仅登记你拥有或已经明确取得许可的 **专用低成本测试端点**，不要将公共官网、第三方在线接口或云元数据登记进去。

示例（文档保留地址，需替换为你的真实受控服务与获授权 IP）：

```json
{
  "version": 1,
  "targets": [{
    "id": "my-lab",
    "name": "已授权测试服务",
    "url": "https://lab.your-domain.test/health",
    "modes": ["http"],
    "allowed_ips": ["192.0.2.10/32"],
    "max_concurrency": 100,
    "max_rps": 50,
    "max_duration": 60,
    "authorization": "记录服务所有者、许可范围及内部工单",
    "expires_at": "2027-01-01T00:00:00Z"
  }]
}
```

- 精确 URL 限制 scheme/host/port/path，不支持 query、凭据、fragment、通配域名或任意端口。TCP 使用 `http://host:port/` 表达地址，但 `modes` 只写 `tcp`，不会发送 HTTP。
- DNS 所有结果必须在授权 CIDR 内；实际连接固定到一个校验过的地址，保留原域名 SNI 与证书验证，不关闭 TLS 校验。不继承环境代理，不跟随重定向，不自动重试。
- 许可范围不能超过程序上限（1000 并发/1000 RPS/300 秒）。公共预设所属注册域不能登记为承载目标；复制到自定义列表不会解锁权限。
- 每轮授权都检查有效期，必须覆盖整轮时长。配置只是显式操作边界，并不替你获得法律或服务商授权。

## 安全与边界

控制服务只监听回环，校验 Host/Origin、会话 token，禁止泛 CORS，配置 CSP；公共目标仅允许 80/443、公网地址，拒绝私网、回环、链路本地和特殊用途 IP（IPv4/IPv6）。本机演示端口与控制 API 隔离，控制端口不能作为测试目标。

冷却记录保存到 `data/cooldowns.json`，跨测试/重启保持，不要删除它来绕过频率保护。当前是可信本机单用户工具，不是暴露到局域网/互联网的多用户服务；不要用反向代理公开，不提供任意请求脚本、代理池或分布式压测。

## 开发检查

```bash
cargo fmt --check
```

```bash
cargo clippy --all-targets -- -D warnings
```

```bash
cargo test
```

回归测试使用本机 mock 服务，不访问公共预设。正式真实网络实验应由你在 GUI 中主动启动，从低并发逐步增加。

站点研究来源与核对范围见 [docs/preset-research.md](docs/preset-research.md)。清单是精选代表，不是全球实时排名；官方地址资料核对不保证当前可达，也不代表授权压测。
