#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
[[ "$(uname -s)" == Darwin ]] || { printf '请在 macOS 上构建 .app。\n' >&2; exit 1; }
[[ -x "$ROOT/node_modules/.bin/tauri" ]] || { printf '尚未安装项目 Tauri CLI；先在项目目录运行 npm ci。\n' >&2; exit 1; }
cd "$ROOT"
node "$ROOT/scripts/version.cjs" --check
CARGO_ARGS=(--locked)
if [[ "${AONET_BUILD_MIRROR:-0}" == 1 ]]; then
  CARGO_ARGS+=(--config 'source.crates-io.replace-with="aonet-build-mirror"' --config 'source.aonet-build-mirror.registry="sparse+https://rsproxy.cn/index/"')
fi
npm run desktop:build:mac -- --ci -- "${CARGO_ARGS[@]}"
APP="$ROOT/src-tauri/target/release/bundle/macos/AoNet.app"
[[ -d "$APP" ]] || { printf 'Tauri 未生成预期应用。\n' >&2; exit 1; }
codesign --verify --deep --strict "$APP"
mkdir -p "$ROOT/dist"
if [[ -e "$ROOT/dist/AoNet.app" ]]; then
  ID="$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$ROOT/dist/AoNet.app/Contents/Info.plist" 2>/dev/null || true)"
  [[ "$ID" == local.aonet.desktop ]] || { printf '拒绝替换未知应用。\n' >&2; exit 1; }
  mv "$ROOT/dist/AoNet.app" "$ROOT/dist/AoNet.previous.$(date +%Y%m%d-%H%M%S).app"
fi
ditto "$APP" "$ROOT/dist/AoNet.app"
ZIP="$ROOT/dist/AoNet-macOS-$(uname -m).zip"
[[ ! -e "$ZIP" ]] || mv "$ZIP" "$ZIP.previous.$(date +%Y%m%d-%H%M%S)"
ditto -c -k --sequesterRsrc --keepParent "$ROOT/dist/AoNet.app" "$ZIP"
printf 'Tauri 应用：%s\n压缩包：%s\n本机临时签名，未经 Apple 公证。\n' "$ROOT/dist/AoNet.app" "$ZIP"
