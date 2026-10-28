# v0.9.0 联合构建与macOS试用

日期：2026-10-28。本记录包含早期 v0.9.0 试用构建及目录 schema v3 合并后的最终重打包。用户随后明确要求汇总所有改动、提交、重新打包，并推送至 `git@git.wiguo.cn:application/aonet.git`。下文早期截图/原生打开证据不冒充最终包的重新交互验收。

## 包含

- 活动页面固定枚举使用radio分段tab：代理类型、DNS记录类型、站点/连接协议等。
- 站点/连接快捷选择、成员、授权绑定及执行历史使用搜索和直接点选，不使用select或自定义下拉。
- 站点管理为类型/分组导航、紧凑站点列表和原位连接详情；选择工具仅填入草稿。
- 批量测试为保存任务→详情→预检确认执行→持久报告，支持再次执行与历史隔离。
- 保留统一HTTP/SOCKS5代理、代理端点后台健康状态和30px单行footer。

## 检查

- 完整Rust 286项通过，前端+版本149项通过。
- cargo fmt --all --check、Clippy all-targets warnings=deny、Tauri locked/offline check、git diff --check通过。
- 首轮Rust遇既有TLS测试fixture在macOS关闭连接时得到ConnectionReset，不是数据字节；仅测试中与UnexpectedEof一样接受无字节关闭，成功TLS身份及零业务数据断言不变。修正后完整Rust通过。
- 浏览器证据来自本轮各已完成交互验收：HTTP/SOCKS5和A/AAAA切换；站点列表/连接详情、搜索、多选、工具草稿；批量授权搜索radio、执行历史按ID搜索点选且不改变active/stop对象。375px无横溢出，footer30px，未发公共站点测试。原生本轮只确认新进程和可见窗口，不宣称完整原生操作矩阵均已覆盖。

## 构建与打开

- 版本工具同步0.9.0，`CARGO_NET_OFFLINE=true AONET_BUILD_MIRROR=1 bash scripts/build-macos.sh`成功，使用已缓存的命令级镜像，不改全局Cargo设置。
- Info.plist版本0.9.0；codesign严格校验和ZIP完整性通过。
- `dist/AoNet.app`约22MiB，`dist/AoNet-macOS-arm64.zip`约7.3MiB。
- ZIP SHA256：`88098a30e2395f1d88d83ef371315978e5d134fe05fc0f438471481a8deb4ed0`。
- 正常quit旧版后open新版，确认新进程与可见窗口；构建脚本保留旧app/zip备份，不改用户数据。
- 本机临时签名，无Apple公证；既有Windows STATIC_VCRUNTIME弃用警告未影响macOS构建，Windows未原生验收。

## schema v3 合并后的最终重打包

本次以当前磁盘 schema v3 为准：网站/应用分离、多标签和标签 OR 分组、预设补充 overlay、旧目录迁移；持久化批量任务和去下拉界面一并纳入。见 [v3 目录说明](catalog-v3.md) 与 [批量任务说明](batch-tasks.md)。不要用旧应用覆盖新数据文件。

- 重新运行：303 项 Rust 测试、159 项前端及版本测试全部通过；Clippy all-targets `-D warnings`、fmt、Tauri offline/locked check、diff check 通过。
- `CARGO_NET_OFFLINE=true bash scripts/build-macos.sh` 成功；Info.plist 0.9.0、codesign 严格校验与 ZIP 完整性通过。
- 最终应用约 23 MiB、ZIP 约 7.4 MiB；SHA-256：`f370f23dbf6571a04fadd20840a4f47c9741bb4888870ac9069dfc14d96f766b`。此值取代上文早期构建校验值。
- 原包按脚本备份，用户数据不改。本次仅重打包，不额外退出或重开正在使用的应用；未重复执行完整浏览器/原生交互验收。
- 远端初始未返回任何 refs；按用户明确要求将源码提交推送至 origin/main。包、运行数据、本地配置不纳入 Git。临时签名、未 Apple 公证，Windows未原生验收。
