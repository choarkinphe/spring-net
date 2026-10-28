"use strict";
// One cross-platform entry keeps native command failures fatal, including PowerShell.
const fs = require("node:fs");
const path = require("node:path");
const {spawnSync} = require("node:child_process");
const {createHash} = require("node:crypto");
const root = path.resolve(__dirname, "..");
const targets = {
  "verify-linux": {platform:"linux", arch:"x64"},
  "windows-x64": {platform:"win32", arch:"x64", host:"x86_64-pc-windows-msvc"},
  "macos-arm64": {platform:"darwin", arch:"arm64", host:"aarch64-apple-darwin"},
  "macos-x64": {platform:"darwin", arch:"x64", host:"x86_64-apple-darwin"},
};
function validateEnvironment(mode, platform, arch, nodeVersion, tag, version) {
  const target = targets[mode];
  if (!target) throw new Error(`Unknown CI mode: ${mode}`);
  if (platform !== target.platform || arch !== target.arch) throw new Error(`Expected ${target.platform}/${target.arch}; got ${platform}/${arch}. Check runner tags and native Node installation.`);
  if (Number(nodeVersion.split(".")[0]) < 22) throw new Error("Node.js 22+ is required.");
  if (tag && tag !== `v${version}`) throw new Error(`Tag ${tag} does not match package version v${version}.`);
  return target;
}
function run(command, args, capture = false) {
  const result = spawnSync(command, args, {cwd:root, env:process.env, encoding:"utf8", stdio:capture?"pipe":"inherit"});
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.signal || result.status}): ${result.stderr || "see job log"}`);
  return result.stdout || "";
}
function testFiles() {
  return ["scripts", "web"].flatMap(dir => fs.readdirSync(path.join(root, dir)).filter(name=>name.endsWith(".test.cjs")).sort().map(name=>`${dir}/${name}`));
}
function main(mode) {
  const version = JSON.parse(fs.readFileSync(path.join(root,"package.json"),"utf8")).version;
  const target = validateEnvironment(mode, process.platform, process.arch, process.versions.node, process.env.CI_COMMIT_TAG, version);
  console.log(`CI target ${mode}; Node ${process.version}; runner executable ${process.env.CI_RUNNER_EXECUTABLE_ARCH || "local"}`);
  run(process.execPath,["scripts/version.cjs","--check"]);
  if (mode === "verify-linux") {
    run(process.execPath,["--test",...testFiles()]);
    return;
  }
  if (process.env.CARGO_TARGET_DIR || process.env.CARGO_BUILD_TARGET) throw new Error("Unset CARGO_TARGET_DIR/CARGO_BUILD_TARGET: package scripts use native target/release paths.");
  const rust = run("rustc",["-vV"],true);
  if (!rust.split(/\r?\n/).includes(`host: ${target.host}`)) throw new Error(`Expected native Rust host ${target.host}:\n${rust}`);
  // Require clean output instead of deleting unknown files on a shell runner.
  for (const dir of ["dist","ci-artifacts","src-tauri/target/release/bundle"]) {
    if (fs.existsSync(path.join(root,dir))) throw new Error(`Existing ${dir}: use a fresh CI checkout without target/dist caches.`);
  }
  if (process.platform === "win32") run("cmd.exe",["/d","/s","/c","npm ci"]);
  else run("npm",["ci"]);
  run("cargo",["test","--locked"]);
  run(process.execPath,["--test",...testFiles()]);
  if (process.platform === "win32") run("pwsh",["-NoLogo","-NoProfile","-NonInteractive","-File","scripts/build-windows.ps1"]);
  else run("bash",["scripts/build-macos.sh"]);
  const files = fs.readdirSync(path.join(root,"dist")).filter(name=> mode === "windows-x64" ? name.endsWith("-setup.exe") : name === `AoNet-macOS-${process.arch === "arm64" ? "arm64" : "x86_64"}.zip`);
  if (files.length !== 1) throw new Error(`Expected exactly one fresh installer/archive, got ${files.length}.`);
  const input = path.join(root,"dist",files[0]);
  if (!fs.statSync(input).isFile() || !fs.statSync(input).size) throw new Error("Empty or invalid package output.");
  const suffix = mode === "windows-x64" ? "-setup.exe" : ".zip";
  const name = `AoNet-${version}-${mode}${suffix}`;
  fs.mkdirSync(path.join(root,"ci-artifacts"));
  const output = path.join(root,"ci-artifacts",name);
  fs.copyFileSync(input,output);
  const hash = createHash("sha256").update(fs.readFileSync(output)).digest("hex");
  fs.writeFileSync(path.join(root,"ci-artifacts","SHA256SUMS.txt"),`${hash}  ${name}\n`);
  console.log(`Artifact: ci-artifacts/${name}`);
}
if (require.main === module) {
  try { main(process.argv[2]); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
module.exports = {validateEnvironment, testFiles};
