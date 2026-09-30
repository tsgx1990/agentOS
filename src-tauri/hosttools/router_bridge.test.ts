// router_bridge.ts 的行为测试（P5 T5）：注册单个 `__host_list_agents__` 工具，execute
// 经 hostCall 转发并原样透传宿主返回的 {ok, agents}；socket 缺失时降级不抛。
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";
import { test, expect, beforeEach, afterEach } from "vitest";

import routerBridge from "./router_bridge";

let server: net.Server | undefined;
let socketPath: string | undefined;
let received: Array<{ method: string; params: unknown }> = [];
let nextResponse: unknown = { ok: true, agents: [] };

function startMockSocketServer(): Promise<string> {
  return new Promise((resolve) => {
    const sockPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "router-bridge-test-")), "s.sock");
    const srv = net.createServer((conn) => {
      let buffer = "";
      conn.on("data", (chunk) => {
        buffer += chunk.toString("utf8");
        const nl = buffer.indexOf("\n");
        if (nl === -1) return;
        received.push(JSON.parse(buffer.slice(0, nl)));
        conn.write(`${JSON.stringify(nextResponse)}\n`);
      });
    });
    srv.listen(sockPath, () => resolve(sockPath));
    server = srv;
  });
}

function makeFakePi() {
  const registered: any[] = [];
  return { registerTool: (tool: any) => registered.push(tool), registered };
}

beforeEach(async () => {
  delete process.env.SUPERAGENT_MCP_SOCKET;
  received = [];
  nextResponse = { ok: true, agents: [] };
  socketPath = await startMockSocketServer();
  process.env.SUPERAGENT_MCP_SOCKET = socketPath;
});

afterEach(() => {
  server?.close();
  server = undefined;
  if (socketPath) fs.rmSync(path.dirname(socketPath), { recursive: true, force: true });
  delete process.env.SUPERAGENT_MCP_SOCKET;
});

test("注册单个 list_agents 工具，名字逐字一致", () => {
  const pi = makeFakePi();
  routerBridge(pi);
  expect(pi.registered.map((t: any) => t.name)).toEqual(["__host_list_agents__"]);
});

test("execute 转发到 socket 并原样回传 {ok, agents}", async () => {
  nextResponse = {
    ok: true,
    agents: [{ app_id: "superagent__summarizer", name: "@superagent/summarizer", display_name: "精简器", category: "life" }],
  };
  const pi = makeFakePi();
  routerBridge(pi);

  const tool = pi.registered.find((t: any) => t.name === "__host_list_agents__");
  const result = await tool.execute("call-1", {}, undefined, undefined, {});

  expect(received[0]).toEqual({ method: "__host_list_agents__", params: {} });
  expect((result as any).ok).toBe(true);
  expect((result as any).agents).toHaveLength(1);
});

test("SUPERAGENT_MCP_SOCKET 缺失时降级返回 {ok:false}，不抛", async () => {
  delete process.env.SUPERAGENT_MCP_SOCKET;
  const pi = makeFakePi();
  expect(() => routerBridge(pi)).not.toThrow();

  const tool = pi.registered.find((t: any) => t.name === "__host_list_agents__");
  const result = await tool.execute("call-2", {}, undefined, undefined, {});
  expect((result as any).ok).toBe(false);
});
