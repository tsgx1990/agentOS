// call_agent_bridge.ts 的行为测试（P5 T4）：
// 1) 注册一个工具，名字必须与 mcp_socket.rs::process_request 的 call_agent 分发分支
//    逐字一致：`__host_call_agent__`；
// 2) execute() 经共享的 hostCall 把 {method, params} 转发到 SUPERAGENT_MCP_SOCKET，
//    并把宿主返回的 CallResult（{ok,text,error}）原样透传；
// 3) SUPERAGENT_MCP_SOCKET 缺失时不抛异常，降级返回 {ok:false,...}。
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";
import { test, expect, beforeEach, afterEach } from "vitest";

import callAgentBridge from "./call_agent_bridge";

let server: net.Server | undefined;
let socketPath: string | undefined;
let received: Array<{ method: string; params: unknown }> = [];
let nextResponse: unknown = { ok: true, text: "", error: null };

function startMockSocketServer(): Promise<string> {
  return new Promise((resolve) => {
    const sockPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "call-bridge-test-")), "s.sock");
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
  nextResponse = { ok: true, text: "", error: null };
  socketPath = await startMockSocketServer();
  process.env.SUPERAGENT_MCP_SOCKET = socketPath;
});

afterEach(() => {
  server?.close();
  server = undefined;
  if (socketPath) fs.rmSync(path.dirname(socketPath), { recursive: true, force: true });
  delete process.env.SUPERAGENT_MCP_SOCKET;
});

test("注册单个工具，名字与宿主分发分支逐字一致", () => {
  const pi = makeFakePi();
  callAgentBridge(pi);
  expect(pi.registered.map((t: any) => t.name)).toEqual(["__host_call_agent__"]);
});

test("execute 转发 {method, params} 到 socket，原样回传 CallResult", async () => {
  nextResponse = { ok: true, text: "3 条要点：a、b、c", error: null };
  const pi = makeFakePi();
  callAgentBridge(pi);

  const tool = pi.registered.find((t: any) => t.name === "__host_call_agent__");
  const args = { target: "@superagent/summarizer", prompt: "精简这段" };
  const result = await tool.execute("call-1", args, undefined, undefined, {});

  expect(received).toHaveLength(1);
  expect(received[0]).toEqual({ method: "__host_call_agent__", params: args });
  expect(result).toEqual({ ok: true, text: "3 条要点：a、b、c", error: null });
});

test("宿主返回业务失败 {ok:false} 时原样透传（不被当传输错误吞掉）", async () => {
  nextResponse = { ok: false, text: "", error: "未声明调用 @superagent/x 的权限" };
  const pi = makeFakePi();
  callAgentBridge(pi);

  const tool = pi.registered.find((t: any) => t.name === "__host_call_agent__");
  const result = await tool.execute("call-2", { target: "@superagent/x", prompt: "y" }, undefined, undefined, {});

  expect(result).toEqual({ ok: false, text: "", error: "未声明调用 @superagent/x 的权限" });
});

test("SUPERAGENT_MCP_SOCKET 缺失时不抛异常，降级返回 {ok:false}", async () => {
  delete process.env.SUPERAGENT_MCP_SOCKET;
  const pi = makeFakePi();
  expect(() => callAgentBridge(pi)).not.toThrow();

  const tool = pi.registered.find((t: any) => t.name === "__host_call_agent__");
  const result = await tool.execute("call-3", { target: "@superagent/x", prompt: "y" }, undefined, undefined, {});

  expect((result as any).ok).toBe(false);
});
