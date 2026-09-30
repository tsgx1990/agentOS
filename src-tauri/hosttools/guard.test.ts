import { test, expect } from "vitest";
import { writeAllowed, readAllowed, commandAllowed } from "./guard";

test("写只允许 APP_DATA 内", () => {
  const app = "/data/apps/x";
  expect(writeAllowed("/data/apps/x/notes.json", app)).toBe(true);
  expect(writeAllowed("/data/apps/x/../y/evil", app)).toBe(false); // 规范化后逃逸
  expect(writeAllowed("/etc/passwd", app)).toBe(false);
});
test("读允许 APP_DATA + 声明只读区", () => {
  const app = "/data/apps/x";
  expect(readAllowed("/data/apps/x/a", app, [])).toBe(true);
  expect(readAllowed("/downloads/a.txt", app, ["/downloads"])).toBe(true);
  expect(readAllowed("/etc/shadow", app, ["/downloads"])).toBe(false);
});
test("命令拒绝明显危险", () => {
  expect(commandAllowed("ls -la")).toBe(true);
  expect(commandAllowed("rm -rf /")).toBe(false);
});
