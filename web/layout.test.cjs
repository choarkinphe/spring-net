"use strict";
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs");
const html=fs.readFileSync(`${__dirname}/legacy-workbench.html`,"utf8");
const css=fs.readFileSync(`${__dirname}/styles.css`,"utf8");
test("layout keeps unique controls and moves monitor configuration into its dialog",()=>{
  const ids=[...html.matchAll(/\bid="([^"]+)"/g)].map(x=>x[1]);
  assert.equal(ids.length,new Set(ids).size,"all IDs must stay unique");
  for(const id of ["monitor-form","run-form","batch-form","target-picker"])assert.equal(ids.filter(x=>x===id).length,1);
  const dialog=html.slice(html.indexOf('<dialog id="monitor-dialog"'),html.indexOf('</dialog>',html.indexOf('<dialog id="monitor-dialog"')));
  assert.match(dialog,/<form id="monitor-form"/);
  const homepage=html.slice(html.indexOf('<section id="page-monitor"'),html.indexOf('<section id="page-console"'));
  assert.doesNotMatch(homepage,/<form/);assert.match(homepage,/id="monitor-open"/);assert.match(homepage,/id="monitor-search"/);
});
test("console layout uses normal document order and mutually exclusive run/batch surfaces",()=>{
  assert.doesNotMatch(css,/display:contents/);
  assert.match(css,/#page-console\[data-view=run\] \.batch-panel/);
  assert.match(css,/#page-console\[data-view=batch\] \.observations>/);
  assert.match(html,/href="#batch" data-nav="batch"/);assert.match(html,/href="#load" data-nav="load"/);
});
