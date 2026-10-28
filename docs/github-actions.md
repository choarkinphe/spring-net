# spring-net GitHub 托管打包

目标仓库：https://github.com/choarkinphe/spring-net 。无需自建Windows/Mac Runner；GitHub托管任务的费用和可用额度以仓库/组织当前政策为准。私有代码仅推送用户明确指定的目标，不自动改变仓库可见性。

## 执行

- main push、PR：Ubuntu 24.04运行版本与Node测试。
- Actions → spring-net checks and desktop packages → Run workflow：先Linux检查，再三个原生构建。
- `v<package.json版本>` 标签：同样打包；tag与版本不一致会拒绝。本流程不自动创建tag或GitHub Release。
- 原生矩阵：`windows-2022` → Windows x64；`macos-15` → Mac arm64；`macos-15-intel` → Mac Intel。脚本校验实际Node架构和Rust host，标签架构变化时失败，不上传错误架构的包。
- Node 22、本地locked Tauri CLI、Rust stable（项目至少1.89）。Rust/Node测试成功才构建，矩阵失败互不取消。任务只需contents:read，不配置个人Token给workflow。
- Actions运行详情的Artifacts下载：`spring-net-<版本>-windows-x64-setup.exe`、`spring-net-<版本>-macos-arm64.zip`、`spring-net-<版本>-macos-x64.zip` 和对应SHA256SUMS.txt，保留30天。下载外层artifact ZIP后再解压取得安装包。

Windows包未证书签名；Mac为ad-hoc临时签名、未Apple公证。CI成功只证明构建/测试成功，不代表对应系统完整GUI与安装升级验收通过，不要关闭系统安全保护。

## 更名与数据兼容

- 产品、Cargo package、npm包及新安装包采用spring-net名称；根CLI和桌面binary均为spring-net（在各自target目录中）。
- Rust library保留aonet/aonet_desktop命名空间，桌面依赖用package alias，不改变业务代码接口。
- bundle identifier仍为local.aonet.desktop，因此沿用既有app_data_dir、data.lock及数据schema。AONET_*、AONET_READY、health app=aonet、X-AoNet-Token是兼容接口，不是漏改品牌。
- 新旧应用可能同时存在，禁止并行共享数据。Windows旧AoNet安装到新名称的升级/快捷方式/卸载行为需实机验证，不能仅因identifier一致就视为已通过。
- 版本仍0.9.0；仅更名不升级第三方依赖，不移动源码所在用户目录，不重写旧验收文档。

## 推送与触发

保留GitLab origin，另加github remote。发布只推main，不force/mirror，不自动推所有tags；远端存在分叉历史时先停下核对。gh登录与SSH是两套认证；gh网页登录成功不代表SSH可用。禁止将Token写入仓库/聊天。

```bash
gh auth login --hostname github.com --web
```

workflow进入默认分支且有Actions执行权限后可手动触发：

```bash
gh workflow run desktop-build.yml --repo choarkinphe/spring-net --ref main
```

实现时本机gh尚未登录、GitHub SSH连接关闭，官方在线文档读取也失败。因此远端仓库访问、托管runner可用性及三平台实际构建必须以后续GitHub运行结果为准，不能把本地检查当成云端成功。

## 本次本地验证（2026-10-28）

- 303项Rust、163项Node/版本测试通过，Clippy、fmt、Tauri locked/offline check和diff检查通过。
- 本机macOS arm64新包构建成功：spring-net.app，版本0.9.0，identifier仍local.aonet.desktop；codesign严格校验、ZIP完整性通过。
- ZIP SHA256：`f62616220493235ea969918482fd459a2f30a65cce12ea8dc36edf8aa427c835`。
- Preview启动被权限机制阻止，未完成更名后的浏览器实测；没有替换或打开正在运行的旧应用。
- GitHub未登录、SSH端口22连接被关闭，远端推送与Actions尚未执行。不把读取refs失败误判为空仓库。

参考：https://docs.github.com/en/actions/reference/runners/github-hosted-runners 、https://docs.github.com/en/actions/using-workflows/manually-running-a-workflow 。
