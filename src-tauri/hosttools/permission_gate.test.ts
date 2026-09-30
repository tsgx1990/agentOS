// permission_gate.ts 的行为测试（P2）：验证它降级为 advisory/审计层之后——
// 1) 早拒绝/放行的判断逻辑本身不变（沿用 guard.ts 的纯函数，guard.test.ts 单独覆盖）；
// 2) 无论放行还是拒绝，都会尝试上报一个 {app_id, tool, args, verdict} 的
//    advisory 审计事件（这里用假 pi.appendEntry 记录调用，断言其被正确调用）；
// 3) 审计上报失败不应影响真正的放行/拒绝判断（advisory 层，不是硬边界）。
import { test, expect, beforeEach } from "vitest";
import permissionGate from "./permission_gate";

function makeFakePi() {
  const handlers: Record<string, (event: unknown) => unknown> = {};
  const appended: Array<{ customType: string; data: any }> = [];
  return {
    on: (event: string, handler: (event: unknown) => unknown) => {
      handlers[event] = handler;
    },
    appendEntry: (customType: string, data: unknown) => {
      appended.push({ customType, data });
    },
    handlers,
    appended,
  };
}

beforeEach(() => {
  process.env.SUPERAGENT_APP_DATA = "/data/apps/x";
  process.env.SUPERAGENT_APP_ID = "x";
  delete process.env.SUPERAGENT_READ_PATHS;
});

test("放行的写入不 block，且上报 verdict=allowed 的审计事件", () => {
  const pi = makeFakePi();
  permissionGate(pi);
  const result = pi.handlers["tool_call"]({
    toolName: "write",
    input: { path: "/data/apps/x/notes.json" },
  });
  expect(result).toBeUndefined();
  expect(pi.appended).toHaveLength(1);
  expect(pi.appended[0].customType).toBe("permission_gate_audit");
  expect(pi.appended[0].data).toMatchObject({
    app_id: "x",
    tool: "write",
    verdict: "allowed",
  });
});

test("越权写入仍被早拒绝（advisory UX），并上报 blocked 前缀的 verdict", () => {
  const pi = makeFakePi();
  permissionGate(pi);
  const result = pi.handlers["tool_call"]({
    toolName: "write",
    input: { path: "/etc/passwd" },
  });
  expect(result).toEqual({ block: true, reason: "只能写入应用自己的数据区" });
  expect(pi.appended[0].data.verdict).toBe("blocked:只能写入应用自己的数据区");
});

test("危险命令仍被早拒绝，未匹配 DANGER 名单的命令放行", () => {
  const pi = makeFakePi();
  permissionGate(pi);
  const blocked = pi.handlers["tool_call"]({ toolName: "bash", input: { command: "rm -rf /" } });
  expect(blocked).toEqual({ block: true, reason: "命令被安全策略拒绝" });

  const allowed = pi.handlers["tool_call"]({ toolName: "bash", input: { command: "ls -la" } });
  expect(allowed).toBeUndefined();
  expect(pi.appended[1].data.verdict).toBe("allowed");
});

test("未被网关管辖的工具（default 分支）也上报 allowed，不 block", () => {
  const pi = makeFakePi();
  permissionGate(pi);
  const result = pi.handlers["tool_call"]({ toolName: "__host_ui_emit__", input: { event: "x" } });
  expect(result).toBeUndefined();
  expect(pi.appended[0].data).toMatchObject({ tool: "__host_ui_emit__", verdict: "allowed" });
});

test("appendEntry 抛异常不影响真正的放行/拒绝判断（advisory，非硬边界）", () => {
  const pi = makeFakePi();
  pi.appendEntry = () => {
    throw new Error("boom");
  };
  permissionGate(pi);
  const result = pi.handlers["tool_call"]({ toolName: "bash", input: { command: "rm -rf /" } });
  expect(result).toEqual({ block: true, reason: "命令被安全策略拒绝" });
});

test("pi 没有 appendEntry（旧版本/未实现）也不影响放行/拒绝判断", () => {
  const pi = makeFakePi() as any;
  delete pi.appendEntry;
  permissionGate(pi);
  const result = pi.handlers["tool_call"]({
    toolName: "write",
    input: { path: "/data/apps/x/notes.json" },
  });
  expect(result).toBeUndefined();
});
