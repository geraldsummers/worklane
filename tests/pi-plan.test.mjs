import assert from "node:assert/strict";
import test from "node:test";
import {
  extractPlan,
  isPlanToolAllowed,
  PLAN_TOOLS,
} from "../assets/pi-pilot/plan-utils.mjs";

test("planning exposes only read and question tools", () => {
  assert.deepEqual(PLAN_TOOLS, [
    "read",
    "grep",
    "find",
    "ls",
    "web_search",
    "fetch_content",
    "questionnaire",
  ]);
  for (const name of ["bash", "edit", "write", "browser", "unknown"]) {
    assert.equal(isPlanToolAllowed(name), false);
  }
  for (const name of PLAN_TOOLS) assert.equal(isPlanToolAllowed(name), true);
});

test("only a numbered plan triggers review", () => {
  assert.equal(extractPlan("I need to ask a question first."), null);
  assert.equal(extractPlan("Plan:\nNo steps yet."), null);
  assert.deepEqual(extractPlan("Plan:\n1. Inspect\n2) Implement\n3. Verify"), [
    "Inspect",
    "Implement",
    "Verify",
  ]);
});
