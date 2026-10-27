"use strict";
const {test} = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const {versionPlan} = require("./version.cjs");
const root = path.resolve(__dirname, "..");
test("current release manifests and lockfiles agree", () => {
  assert.equal(versionPlan(root).changes.length, 0);
});
test("version planning changes only application packages, validates first and is idempotent", () => {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "aonet-version-"));
  const files = ["package.json", "package-lock.json", "Cargo.toml", "Cargo.lock", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "src-tauri/tauri.conf.json"];
  try {
    for (const name of files) {
      fs.mkdirSync(path.dirname(path.join(temp, name)), {recursive:true});
      fs.copyFileSync(path.join(root, name), path.join(temp, name));
    }
    const plan = versionPlan(temp, "1.2.3");
    assert.equal(plan.changes.length, 7);
    for (const change of plan.changes) fs.writeFileSync(path.join(temp, change.name), change.content);
    assert.equal(versionPlan(temp).changes.length, 0);
    assert.match(fs.readFileSync(path.join(temp,"Cargo.lock"),"utf8"), /name = "axum"\nversion = "0\.7\.9"/);
    for (const invalid of ["1.2", "01.2.3", "1.2.3\n", "1.2.3; rm"]) assert.throws(() => versionPlan(temp, invalid));
    fs.writeFileSync(path.join(temp,"src-tauri/Cargo.toml"), "broken");
    assert.throws(() => versionPlan(temp,"2.0.0"), /数量异常/);
    assert.equal(JSON.parse(fs.readFileSync(path.join(temp,"package.json"))).version, "1.2.3");
  } finally { fs.rmSync(temp, {recursive:true,force:true}); }
});
