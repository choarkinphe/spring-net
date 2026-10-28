"use strict";
const {test}=require("node:test");
const assert=require("node:assert/strict");
const {validateEnvironment,testFiles}=require("./ci-desktop.cjs");
test("CI dispatch checks actual platform architecture and release tag",()=>{
  for(const [mode,platform,arch] of [["verify-linux","linux","x64"],["windows-x64","win32","x64"],["macos-arm64","darwin","arm64"],["macos-x64","darwin","x64"]]){
    assert.ok(validateEnvironment(mode,platform,arch,"22.0.0","v0.9.0","0.9.0"));
    assert.throws(()=>validateEnvironment(mode,platform,"ia32","22.0.0",null,"0.9.0"),/Expected/);
  }
  assert.throws(()=>validateEnvironment("windows-x64","linux","x64","22.0.0",null,"0.9.0"),/Expected/);
  assert.throws(()=>validateEnvironment("verify-linux","linux","x64","20.0.0",null,"0.9.0"),/22/);
  assert.throws(()=>validateEnvironment("verify-linux","linux","x64","22.0.0","v1.0.0","0.9.0"),/does not match/);
  assert.throws(()=>validateEnvironment("unknown","linux","x64","22.0.0",null,"0.9.0"),/Unknown/);
});
test("CI discovers explicit test paths without shell wildcard expansion",()=>{
  const files=testFiles();assert.ok(files.includes("scripts/version.test.cjs"));assert.ok(files.includes("scripts/ci-desktop.test.cjs"));assert.ok(files.includes("web/network-tools.test.cjs"));assert.equal(files.length,new Set(files).size);
});
