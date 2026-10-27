"use strict";
// package.json is the release-version source; keep native manifests and locks aligned.
const fs = require("node:fs");
const path = require("node:path");
const release = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?![\s\S])/;

function versionPlan(root, requested) {
  const read = name => fs.readFileSync(path.join(root, name), "utf8");
  const version = requested ?? JSON.parse(read("package.json")).version;
  if (!release.test(version)) throw new Error("版本号必须是 MAJOR.MINOR.PATCH，例如 0.3.0。");
  const changes = [];
  function patch(name, pattern, replacement, count = 1) {
    const before = read(name);
    const matches = [...before.matchAll(new RegExp(pattern.source, "g"))];
    if (matches.length !== count) throw new Error(`${name}：版本字段数量异常，未写入任何文件。`);
    const after = before.replace(new RegExp(pattern.source, "g"), replacement);
    if (after !== before) changes.push({name, content:after});
  }
  for (const name of ["package.json", "src-tauri/tauri.conf.json"]) {
    patch(name, /("version"\s*:\s*")[^"]+(\")/, (_match, prefix, suffix) => `${prefix}${version}${suffix}`);
  }
  const lock = JSON.parse(read("package-lock.json"));
  if (lock.name !== "aonet-desktop" || lock.packages?.[""]?.name !== "aonet-desktop") throw new Error("package-lock.json 根包信息无效。");
  patch("package-lock.json", /("name": "aonet-desktop",\s*"version": ")[^"]+(\")/, (_match, prefix, suffix) => `${prefix}${version}${suffix}`, 2);
  for (const name of ["Cargo.toml", "src-tauri/Cargo.toml"]) {
    patch(name, /(\[package\]\s*name = "aonet(?:-desktop)?"\s*version = ")[^"]+(\")/, (_match, prefix, suffix) => `${prefix}${version}${suffix}`);
  }
  for (const [name, count] of [["Cargo.lock", 1], ["src-tauri/Cargo.lock", 2]]) {
    patch(name, /(\[\[package\]\]\s*name = "aonet(?:-desktop)?"\s*version = ")[^"]+(\")/, (_match, prefix, suffix) => `${prefix}${version}${suffix}`, count);
  }
  return {version, changes};
}

if (require.main === module) {
  try {
    const args = process.argv.slice(2);
    if (args.length !== 1 || args[0] !== "--check" && !release.test(args[0])) throw new Error("用法：node scripts/version.cjs --check 或 node scripts/version.cjs 0.3.0");
    const root = path.resolve(__dirname, "..");
    const check = args[0] === "--check";
    const {version, changes} = versionPlan(root, check ? undefined : args[0]);
    if (check && changes.length) throw new Error(`版本不一致（应为 ${version}）：${changes.map(x => x.name).join("、")}。请运行版本同步命令。`);
    for (const change of changes) fs.writeFileSync(path.join(root, change.name), change.content);
    console.log(`AoNet v${version}：${check ? "版本一致" : `已同步 ${changes.length} 个文件`}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
module.exports = {versionPlan};
