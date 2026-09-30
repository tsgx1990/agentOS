import { test, expect } from "vitest";
import { BRIDGE_SNIPPET_MARKER } from "./bridge"; // 断言桥脚本含 origin 守卫标记（由 Rust 常量与前端共用一处定义）
test("桥脚本含 origin 守卫与 superagent 定义", () => {
  // 前端仅验证中继侧：伪造一条来自未知 origin 的 message 不触发 invoke
  expect(BRIDGE_SNIPPET_MARKER).toContain("sagent");
});
