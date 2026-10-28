# v0.8.0 联合验收与macOS交付

2026-10-28。应用管理与统一代理两会话明确停止源码写入后进行联合验收。包含应用目录v2、多端点分类、UDP DNS/QUIC、HTTP CONNECT/SOCKS5统一路由、页脚代理端点健康检查、批量测试及FD资源误拦截修复。

## 检查结果

- 当前联合源码Rust全套268项通过；JavaScript+版本129项通过。
- Clippy all-targets warnings=deny、Tauri locked/offline check、git diff --check通过。
- 首轮fmt检查发现src/lib.rs模块排序未格式化，执行cargo fmt后两个manifest检查通过，不是测试功能失败。
- 命令使用已有镜像缓存+offline/locked，不修改全局Cargo配置。版本脚本仅同步应用版本为0.8.0，没有在本次打包步骤升级依赖；采用两个功能已更新的锁文件。

## 浏览器实际补验

- 隔离9916环境，新增HTTP类型本机端点127.0.0.1:9911并启用，页脚自动显示端点可达、完成时间和0.2ms级耗时。该端点为本会话受控fixture；HTTP健康只做TCP连通，不验证CONNECT转发。
- 跨页仍显示健康状态。代理开启时QUIC开始禁用，并说明不会自动直连；关闭代理后页脚恢复“直连 · 未启用代理”。
- 373x812视口scrollWidth=373；footer固定bottom=812，实际高94.6875px，对应预留95px；已恢复桌面视口，保存截图，console无错误。
- 应用管理实际显示64个内置应用和57个共享端点、GDS6项和通信应用3项，目录列表正常；未对公共端点发起协议/负载检查。
- 完整HTTP/SOCKS转发、UDP DNS、QUIC、取消和授权策略以受控自动化fixtures覆盖；未声称本轮浏览器手工走遍所有原生/协议路径。

## 包与启动

- `CARGO_NET_OFFLINE=true AONET_BUILD_MIRROR=1 bash scripts/build-macos.sh` release构建成功。
- `dist/AoNet.app`约21MiB，`dist/AoNet-macOS-arm64.zip`约7.0MiB。
- Info.plist=0.8.0，codesign --verify --deep --strict及unzip完整性通过。
- ZIP SHA256：`fb621f428967ad98b64a104599c56303dec935db6862d3caef05ca4355637c3c`。
- 正常退出旧版后打开dist中新版，已确认新进程与可见窗口。旧app/zip由脚本备份，未删除用户数据。
- 本机ad-hoc签名，无Apple公证。既有Windows STATIC_VCRUNTIME弃用警告不影响macOS构建。Windows未原生验收。
- 本批未提交或推送源码。

## 升级注意

旧目录仅在内存迁移，首次明确保存应用目录时持久化v2。v2不能由旧版本读取，回退应用时必须配合旧数据备份。页面的代理端点可达不等于目标转发、业务登录或交易正常。
