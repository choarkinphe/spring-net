# 版本号管理

当前发布版本：0.3.0。

## 单一来源

`package.json` 的 `version` 为发布版本来源，核心Cargo包、桌面Cargo包、Tauri配置与两套Cargo锁文件及npm锁文件保持一致。左下角通过共享bootstrap读取编译后的核心版本，显示`vMAJOR.MINOR.PATCH`；未取得实际版本时显示读取中/版本未知，不硬编码假版本。

版本仅采用正式三段数字。同步升级：

```bash
npm run release:version -- 0.3.1
```

检查一致性：

```bash
npm run version:check
```

同步脚本在写入前先读取并验证所有目标，只修改应用自身版本，不升级第三方依赖。macOS/Windows打包脚本会先检查版本一致性再构建，避免打出UI、Info.plist与实际核心版本不一致的包。

测试：

```bash
node --test scripts/version.test.cjs web/*.test.cjs
```

## 0.3.0 本次交付

- macOS arm64 release构建成功，Info.plist版本0.3.0，签名和ZIP完整性检查通过。
- 76项Rust测试通过，38项前端测试及2项版本管理测试通过。
- 浏览器实际左下角显示v0.3.0、控制台无错误。
- 正常发送退出请求后打开`dist/AoNet.app`，确认新版进程路径及原生窗口出现；没有强制终止其他进程，没有自动执行网络诊断。
- 仍是本机临时签名，未Apple公证；没有新增Windows原生验收。
