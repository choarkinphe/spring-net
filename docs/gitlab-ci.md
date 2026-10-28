# GitLab CI 与原生 Runner 安装

## 如何按系统和架构调度

GitLab 调度依据是 **job tags 与 Runner tags 匹配**，不是自动识别源码需要什么操作系统。Runner 必须包含 job 要求的全部 tags，同时在线、允许运行该项目/分支、还有并发容量。标签是管理员填写的能力声明，贴错标签也可能被选中。

本项目用一个组合标签表示系统和架构：

| job | 默认 Runner tag | 执行器 | 产物 |
|---|---|---|---|
| verify:linux | linux-x64 | Docker 或 Shell | 版本及 Node 回归检查 |
| package:windows-x64 | windows-x64 | Shell / pwsh | NSIS setup.exe |
| package:macos-arm64 | macos-arm64 | Shell / bash | Apple Silicon app ZIP |
| package:macos-x64 | macos-x64 | Shell / bash | Intel app ZIP |

`CI_RUNNER_EXECUTABLE_ARCH` 仅在任务中显示 Runner 程序本身的 OS/架构，不能用它在任务分配前选择 Runner，也不一定代表编译产物架构。`scripts/ci-desktop.cjs` 再校验 Node 的 platform/arch 和 rustc host；构建跟随原生工具链，不交叉编译、不生成 universal Mac 包。Mac arm64 请安装原生 ARM Node/Rust/Runner，不使用 Rosetta 工具链。

这里“Linux x86”按 **x86_64/amd64 64 位**处理；若现有机器实际为 i386/ia32 32 位，应升级为64位 Runner，Node 22 Docker镜像及本配置不支持32位。

## 当前只有 Linux Runner 时

1. 项目 Settings → CI/CD → Runners 查看现有 Runner tags 和 executor；将其添加 `linux-x64` 标签，或者设置项目 CI/CD 变量 `LINUX_RUNNER_TAG` 为现有匹配标签。
2. Docker executor 使用 CI 中的 `node:22-bookworm` 镜像；Shell executor 不使用 image，必须在服务账号 PATH 中预装 Node 22+、Git。Linux 任务不依赖 npm install 或 Rust，执行版本校验及全部 Node 测试。
3. `BUILD_WINDOWS`、`BUILD_MACOS_ARM64`、`BUILD_MACOS_X64` 默认字符串 `false`：原生打包任务不会创建，不会因为缺少 Runner 一直 Pending。
4. 默认分支 push、MR、标签和 Run pipeline 会创建流水线；打包仅在手动 Run pipeline 或 `vMAJOR.MINOR.PATCH` 标签流水线且对应开关为 `true` 时加入。任意非版本标签会在版本校验中拒绝；标签版本必须等于 package.json。

Runner tags 与开关可以在项目 Settings → CI/CD → Variables 配置，或 Run pipeline 时填写。这些变量不是凭据，不需要 masked。示例：

| 变量 | 示例 |
|---|---|
| BUILD_WINDOWS | true |
| WINDOWS_RUNNER_TAG | windows-x64 |
| BUILD_MACOS_ARM64 | true |
| MACOS_ARM64_RUNNER_TAG | macos-arm64 |
| BUILD_MACOS_X64 | false |
| LINUX_RUNNER_TAG | linux-x64 |

如果开关打开但没有匹配 Runner，任务会 Pending，GitLab不会改派到Linux或自动创建机器。当前 Linux 构建检查不能代替 Windows/macOS 编译及安装测试。

## 创建 Runner：通用步骤

推荐**专用项目 Runner + 专用低权限构建账号**，不要使用日常开发者账号。Shell job 能执行该账号权限范围内任何命令；只对可信提交开放。不要让不受信任的 fork/MR 在原生构建机上执行。原生 Runner 建议启用 Protected，仅用于受保护的 main 与受保护版本标签；Run pipeline 也请选择受保护 ref。

1. 在项目 Settings → CI/CD → Runners → New project runner 创建 Runner，填写上表对应 tag，关闭 Run untagged jobs，限制到此项目，按需要启用 Protected。
2. 使用该页面生成的 **Runner authentication token**（通常 `glrt-…`）。标签/Protected/锁定在 GitLab UI 设置；不要沿用旧 registration token 工作流。自建 GitLab 若版本较旧，以实例页面显示的注册步骤为准。
3. 在构建机安装与 GitLab 版本兼容的 GitLab Runner，执行交互注册，URL 填 `https://git.wiguo.cn/`（以实际 Web 地址为准），token 在交互提示中输入。不要把 token 写进仓库、CI变量示例、截图或命令历史。
4. 选择 `shell` 执行器。Windows shell 必须是 `pwsh`，Mac 是 `bash`。config.toml 通常位于服务账号配置目录，确认工作目录与读写权限。
5. 服务账号需能访问 GitLab、npm、crates.io 和 Tauri/NSIS 工具下载地址。内部 CA 应加入可信证书，不要关闭 TLS 验证。Runner配置含令牌，应限制访问。

## macOS Runner

### 环境

- Apple Silicon 或 Intel Mac，macOS 13+，完整 Xcode/Command Line Tools；首次启动 Xcode 完成组件安装及许可。
- Node 22+、Git、原生 Rust stable（至少1.89）、GitLab Runner。可使用官方安装包/rustup；Homebrew 路径需加入构建账号 PATH。
- 本项目使用 ad-hoc 本机签名，无需 Apple 凭据；不公证，不宣称下载后无 Gatekeeper 提示。需要正式签名时单独配置受保护凭据，勿提交证书。

安装 CLI 开发工具（完整 Xcode 请通过 Apple 安装渠道准备）：

```bash
xcode-select --install
```

如使用 Homebrew安装 Runner：

```bash
brew install gitlab-runner
```

在预定运行服务的构建账号下交互注册：

```bash
gitlab-runner register
```

服务安装与启动（不要和另一个 Homebrew服务重复启动同一个Runner）：

```bash
gitlab-runner install --working-directory "$HOME/gitlab-builds"
```

先创建上述专用工作目录，再安装服务；macOS 用户 LaunchAgent 需要用户登录会话，机器不可休眠。构建机的登录/重启自启策略由管理员设置。

```bash
gitlab-runner start
```

检查：`node -p process.arch` 应为 `arm64` 或 `x64`；`rustc -vV` host应分别为 `aarch64-apple-darwin` 或 `x86_64-apple-darwin`。注册 tag 对应 `macos-arm64` / `macos-x64`。

## Windows x64 Runner

1. 安装 Git for Windows、Node 22+ x64、PowerShell 7（`pwsh`）、Rust stable MSVC x64。
2. 安装 Visual Studio 2022 Build Tools：勾选 **Desktop development with C++**、MSVC x64/x86 编译工具、Windows SDK；安装 WebView2 Runtime 供运行验收。NSIS/WebView2引导下载需要网络。
3. 从 GitLab Runner 官方 Windows安装说明下载 amd64二进制至 `C:\GitLab-Runner\gitlab-runner.exe`，校验其官方校验值，限制该目录权限。以下在管理员 PowerShell 中执行，不把令牌放到命令行。

```powershell
C:\GitLab-Runner\gitlab-runner.exe register
```

选择 shell executor，检查 `C:\GitLab-Runner\config.toml` 中该 Runner 为 `shell = "pwsh"`；注册时从 `C:\GitLab-Runner` 运行或明确使用同一config路径，注册与服务必须读取同一配置。

```powershell
C:\GitLab-Runner\gitlab-runner.exe install --working-directory C:\GitLab-Runner\builds
```

先创建工作目录。安装服务后，启动前在 Windows Services 的 GitLab Runner → Properties → Log On 中改用专用 `aonet-ci` 账号并填写密码（需已创建并具有运行服务权限），不要保留默认高权限 LocalSystem。Rust等用户级工具应安装到该服务账号，而非管理员的个人目录。

```powershell
C:\GitLab-Runner\gitlab-runner.exe start
```

服务账号实际可用的命令：`git --version`、`node --version`、`npm.cmd --version`、`pwsh --version`、`rustc -vV`。Rust host必须是 `x86_64-pc-windows-msvc`。改变PATH后重启Runner服务。

## 运行与下载

- 提交CI配置后：Linux检查自动运行；配置好原生Runner后，选择 Build → Pipelines → Run pipeline，在main上设置对应 BUILD_* 为 true。
- 发布标签：先同步项目版本并提交，再创建与版本一致的 `v0.9.0` 等标签。只有已启用的原生平台会打包。本配置不自动创建标签/Release、不推包到外部存储。
- 原生任务执行：环境校验 → npm ci → cargo test --locked → 全部Node测试 → 原有平台打包脚本 → 非空产物检查 → SHA256。
- Pipeline/Jobs 的 artifacts 可下载：`AoNet-<版本>-windows-x64-setup.exe`、`AoNet-<版本>-macos-arm64.zip`、`AoNet-<版本>-macos-x64.zip` 及 SHA256SUMS.txt。保留30天（实例“保留最新成功artifacts”设置可能延长实际保留时间）。
- 不缓存 target/dist，不修改 CARGO_TARGET_DIR/CARGO_BUILD_TARGET。GIT_STRATEGY=clone 使用新检出；发现旧输出会失败而不是上传旧安装包。不上传 `.previous.*`。
- `ci-artifacts/`仅是CI打包输出；不得包含用户运行数据。既有 `dist/`、`data/`、授权文件不进入仓库。

## 配置验收与限制

通过 GitLab Build → Pipeline editor → Validate 使用实例自己的 CI Lint 校验 `.gitlab-ci.yml`；之后跑一次Linux流水线，再分别启用原生Runner。Node脚本单元测试不代表远端调度、服务账号环境或安装包运行已验收。本地Mac不能验证Windows原生安装；两种包均需在对应系统试装/启动。

参考官方文档（实施时仍以所用 GitLab/Runner 版本为准）：
- https://docs.gitlab.com/ci/runners/configure_runners/#use-tags-to-control-which-jobs-a-runner-can-run
- https://docs.gitlab.com/runner/register/
- https://docs.gitlab.com/runner/install/osx/
- https://docs.gitlab.com/runner/install/windows/
- https://docs.gitlab.com/runner/executors/shell/
- https://docs.gitlab.com/ci/jobs/job_artifacts/
