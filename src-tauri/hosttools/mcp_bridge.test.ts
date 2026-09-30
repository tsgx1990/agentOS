// mcp_bridge.ts 的行为测试（Task8）：
// 1) 按 SUPERAGENT_MCP_TOOLS 里每条授权工具注册一个 mcp__<server>__<tool>
//    工具，description/inputSchema 透传；
// 2) execute() 转发 {server, tool, args} 给 hostMcpCall——这里整体 mock 掉
//    ./mcp_transport（它是 Task9 才落地的传输开放项，见该文件头部注释：pi
//    没有扩展→宿主的请求/响应原语，本测试只关心 mcp_bridge 转发的 payload
//    对不对，不关心传输细节），并把 mock 返回的结果透传回去；
// 3) hostMcpCall 失败时 execute 把原因原样抛出（不吞掉）；
// 4) env 缺失/为空数组/JSON 损坏都不注册任何工具、不抛异常。
import { test, expect, beforeEach, vi } from "vitest";

vi.mock("./mcp_transport", () => ({
  hostMcpCall: vi.fn(),
}));

import { hostMcpCall } from "./mcp_transport";
import mcpBridge from "./mcp_bridge";

function makeFakePi() {
  const registered: any[] = [];
  return {
    registerTool: (tool: any) => {
      registered.push(tool);
    },
    registered,
  };
}

const mockedHostMcpCall = hostMcpCall as unknown as ReturnType<typeof vi.fn>;

beforeEach(() => {
  delete process.env.SUPERAGENT_MCP_TOOLS;
  mockedHostMcpCall.mockReset();
});

test("按授权清单注册 mcp__<server>__<tool> 工具，description/inputSchema 透传", () => {
  process.env.SUPERAGENT_MCP_TOOLS = JSON.stringify([
    {
      server: "fs",
      tool: "read_file",
      name: "读文件",
      description: "读取文件内容",
      inputSchema: { type: "object", properties: { path: { type: "string" } } },
    },
    {
      server: "fs",
      tool: "write_file",
      description: "写入文件内容",
      inputSchema: {
        type: "object",
        properties: { path: { type: "string" }, content: { type: "string" } },
      },
    },
  ]);
  const pi = makeFakePi();
  mcpBridge(pi);

  expect(pi.registered).toHaveLength(2);
  expect(pi.registered[0].name).toBe("mcp__fs__read_file");
  expect(pi.registered[0].description).toBe("读取文件内容");
  expect(pi.registered[0].parameters).toEqual({ type: "object", properties: { path: { type: "string" } } });
  expect(pi.registered[1].name).toBe("mcp__fs__write_file");
  expect(pi.registered[1].description).toBe("写入文件内容");
});

test("execute 转发 {server, tool, args} 给 hostMcpCall 并回传其结果", async () => {
  process.env.SUPERAGENT_MCP_TOOLS = JSON.stringify([
    { server: "fs", tool: "read_file", description: "d", inputSchema: {} },
  ]);
  mockedHostMcpCall.mockResolvedValue({ content: "file contents" });
  const pi = makeFakePi();
  mcpBridge(pi);

  const tool = pi.registered[0];
  const result = await tool.execute("call-1", { path: "/tmp/x" }, undefined, undefined, {});

  expect(mockedHostMcpCall).toHaveBeenCalledTimes(1);
  expect(mockedHostMcpCall).toHaveBeenCalledWith({
    server: "fs",
    tool: "read_file",
    args: { path: "/tmp/x" },
  });
  expect(result.details).toEqual({ content: "file contents" });
  expect(result.content[0].text).toContain("file contents");
});

test("hostMcpCall 拒绝/失败时 execute 把原因原样抛出，不吞掉", async () => {
  process.env.SUPERAGENT_MCP_TOOLS = JSON.stringify([
    { server: "fs", tool: "write_file", description: "d", inputSchema: {} },
  ]);
  mockedHostMcpCall.mockRejectedValue(new Error("unauthorized"));
  const pi = makeFakePi();
  mcpBridge(pi);

  const tool = pi.registered[0];
  await expect(tool.execute("call-2", { path: "/tmp/x" }, undefined, undefined, {})).rejects.toThrow(
    /unauthorized/,
  );
});

test("SUPERAGENT_MCP_TOOLS 缺失时不注册任何工具，不抛异常", () => {
  const pi = makeFakePi();
  expect(() => mcpBridge(pi)).not.toThrow();
  expect(pi.registered).toHaveLength(0);
  expect(mockedHostMcpCall).not.toHaveBeenCalled();
});

test("SUPERAGENT_MCP_TOOLS 是损坏 JSON 时不注册任何工具，不抛异常", () => {
  process.env.SUPERAGENT_MCP_TOOLS = "{not json";
  const pi = makeFakePi();
  expect(() => mcpBridge(pi)).not.toThrow();
  expect(pi.registered).toHaveLength(0);
});

test("SUPERAGENT_MCP_TOOLS 是空数组时不注册任何工具", () => {
  process.env.SUPERAGENT_MCP_TOOLS = "[]";
  const pi = makeFakePi();
  mcpBridge(pi);
  expect(pi.registered).toHaveLength(0);
});

test("SUPERAGENT_MCP_TOOLS 里形状不对的元素被过滤，不影响其它合法元素", () => {
  process.env.SUPERAGENT_MCP_TOOLS = JSON.stringify([
    { server: "fs" /* 缺 tool */ },
    { server: "fs", tool: "read_file", description: "d" },
  ]);
  const pi = makeFakePi();
  mcpBridge(pi);
  expect(pi.registered).toHaveLength(1);
  expect(pi.registered[0].name).toBe("mcp__fs__read_file");
});
