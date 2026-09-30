// maker_bridge.ts 的行为测试（Task8；Task7/P6-B 补第四个工具
// `__host_maker_install_skill__`）：
// 1) 注册四个工具，名字必须与 mcp_socket.rs::process_request 的分发分支
//    逐字一致：`__host_maker_stage_write__`/`__host_maker_preview__`/
//    `__host_maker_install__`/`__host_maker_install_skill__`（见该文件
//    Task3/Task7 文档注释）；
// 2) execute() 经共享的 `hostCall(method, params)` 低层 socket 客户端把
//    `{method, params}` 转发到 `SUPERAGENT_MCP_SOCKET`，并把宿主返回的响应
//    原样（verbatim）透传回去——maker 三个方法的响应形状（`{ok,...}`/
//    `{pending_confirm,...}`）不套用 `__host_mcp_call__` 的
//    `{result:...}`/`{error:...}` 解包规则，所以这里用一个真实的 mock socket
//    服务端而不是 mock 掉整个传输模块，好证明"原样透传"这条约束；
// 3) SUPERAGENT_MCP_SOCKET 缺失时不抛异常，execute 返回一个可辨识的错误对象
//    （降级行为对齐 mcp_bridge.ts：宿主传输不可用时不让整个 pi 进程崩掉）。
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";
import { test, expect, beforeEach, afterEach } from "vitest";

import makerBridge from "./maker_bridge";

let server: net.Server | undefined;
let socketPath: string | undefined;
let received: Array<{ method: string; params: unknown }> = [];
let nextResponse: unknown = { ok: true };

function startMockSocketServer(): Promise<string> {
  return new Promise((resolve) => {
    const sockPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "maker-bridge-test-")), "s.sock");
    const srv = net.createServer((conn) => {
      let buffer = "";
      conn.on("data", (chunk) => {
        buffer += chunk.toString("utf8");
        const nl = buffer.indexOf("\n");
        if (nl === -1) return;
        const line = buffer.slice(0, nl);
        const req = JSON.parse(line);
        received.push(req);
        conn.write(`${JSON.stringify(nextResponse)}\n`);
      });
    });
    srv.listen(sockPath, () => resolve(sockPath));
    server = srv;
  });
}

function makeFakePi() {
  const registered: any[] = [];
  return {
    registerTool: (tool: any) => {
      registered.push(tool);
    },
    registered,
  };
}

beforeEach(async () => {
  delete process.env.SUPERAGENT_MCP_SOCKET;
  received = [];
  nextResponse = { ok: true };
  socketPath = await startMockSocketServer();
  process.env.SUPERAGENT_MCP_SOCKET = socketPath;
});

afterEach(() => {
  server?.close();
  server = undefined;
  if (socketPath) {
    fs.rmSync(path.dirname(socketPath), { recursive: true, force: true });
  }
  delete process.env.SUPERAGENT_MCP_SOCKET;
});

test("注册四个工具，名字与宿主分发分支逐字一致", () => {
  const pi = makeFakePi();
  makerBridge(pi);

  const names = pi.registered.map((t: any) => t.name);
  expect(names).toEqual([
    "__host_maker_stage_write__",
    "__host_maker_preview__",
    "__host_maker_install__",
    "__host_maker_install_skill__",
  ]);
});

test("stage_write execute 转发 {method, params} 到 socket，原样回传响应", async () => {
  nextResponse = { ok: true, bytes_written: 42 };
  const pi = makeFakePi();
  makerBridge(pi);

  const tool = pi.registered.find((t: any) => t.name === "__host_maker_stage_write__");
  const args = { draft_id: "d1", rel_path: "src/index.ts", content: "hello" };
  const result = await tool.execute("call-1", args, undefined, undefined, {});

  expect(received).toHaveLength(1);
  expect(received[0]).toEqual({
    method: "__host_maker_stage_write__",
    params: args,
  });
  expect(result).toEqual({ ok: true, bytes_written: 42 });
});

test("preview execute 转发 {draft_id} 并原样回传 pending_confirm 形状响应", async () => {
  nextResponse = { pending_confirm: true, confirm_id: "c-1", message: "预览就绪" };
  const pi = makeFakePi();
  makerBridge(pi);

  const tool = pi.registered.find((t: any) => t.name === "__host_maker_preview__");
  const result = await tool.execute("call-2", { draft_id: "d1" }, undefined, undefined, {});

  expect(received[0]).toEqual({ method: "__host_maker_preview__", params: { draft_id: "d1" } });
  expect(result).toEqual({ pending_confirm: true, confirm_id: "c-1", message: "预览就绪" });
});

test("install execute 转发 {draft_id} 并原样回传响应", async () => {
  nextResponse = { ok: true, installed: "app-x" };
  const pi = makeFakePi();
  makerBridge(pi);

  const tool = pi.registered.find((t: any) => t.name === "__host_maker_install__");
  const result = await tool.execute("call-3", { draft_id: "d1" }, undefined, undefined, {});

  expect(received[0]).toEqual({ method: "__host_maker_install__", params: { draft_id: "d1" } });
  expect(result).toEqual({ ok: true, installed: "app-x" });
});

test("install_skill execute 转发 {draft_id} 并原样回传 pending_confirm+meta+scan 形状响应", async () => {
  nextResponse = {
    pending_confirm: true,
    confirm_id: "skill-confirm-0",
    meta: { id: "good-skill", name: "good-skill" },
    scan: { has_scripts: false, findings: [] },
  };
  const pi = makeFakePi();
  makerBridge(pi);

  const tool = pi.registered.find((t: any) => t.name === "__host_maker_install_skill__");
  const result = await tool.execute("call-5", { draft_id: "d1" }, undefined, undefined, {});

  expect(received[0]).toEqual({
    method: "__host_maker_install_skill__",
    params: { draft_id: "d1" },
  });
  expect(result).toEqual(nextResponse);
});

test("SUPERAGENT_MCP_SOCKET 缺失时不抛异常，execute 降级返回错误对象", async () => {
  delete process.env.SUPERAGENT_MCP_SOCKET;
  const pi = makeFakePi();
  expect(() => makerBridge(pi)).not.toThrow();

  const tool = pi.registered.find((t: any) => t.name === "__host_maker_stage_write__");
  const result = await tool.execute(
    "call-4",
    { draft_id: "d1", rel_path: "a.ts", content: "x" },
    undefined,
    undefined,
    {},
  );

  expect(result).toBeTruthy();
  expect((result as any).ok).not.toBe(true);
});
