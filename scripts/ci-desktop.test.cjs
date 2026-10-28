"use strict";
const {test}=require("node:test");
const assert=require("node:assert/strict");
const {validateEnvironment,testFiles,releaseTag,artifactName}=require("./ci-desktop.cjs");
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
test("GitHub release tags and renamed artifact paths are explicit",()=>{
  assert.equal(releaseTag({GITHUB_REF_TYPE:"branch",GITHUB_REF_NAME:"main"}),undefined);
  assert.equal(releaseTag({GITHUB_REF_TYPE:"tag",GITHUB_REF_NAME:"v0.9.0"}),"v0.9.0");
  assert.equal(releaseTag({CI_COMMIT_TAG:"v0.9.0"}),"v0.9.0");
  assert.throws(()=>releaseTag({CI_COMMIT_TAG:"v1.0.0",GITHUB_REF_TYPE:"tag",GITHUB_REF_NAME:"v0.9.0"}),/Conflicting/);
  assert.equal(artifactName("windows-x64","0.9.0"),"spring-net-0.9.0-windows-x64-setup.exe");
  assert.equal(artifactName("macos-arm64","0.9.0"),"spring-net-0.9.0-macos-arm64.zip");
});
test("CI discovers explicit test paths without shell wildcard expansion",()=>{
  const files=testFiles();assert.ok(files.includes("scripts/version.test.cjs"));assert.ok(files.includes("scripts/ci-desktop.test.cjs"));assert.ok(files.includes("web/network-tools.test.cjs"));assert.equal(files.length,new Set(files).size);
});
