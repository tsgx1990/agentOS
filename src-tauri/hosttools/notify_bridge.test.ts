import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("./mcp_transport", () => ({ hostCall: vi.fn() }));
import { hostCall } from "./mcp_transport";
import register from "./notify_bridge";

function fakePi() {
  const tools: any[] = [];
  return { tools, registerTool: (t: any) => tools.push(t) };
}

describe("notify_bridge", () => {
  // 块体、丢弃返回值：`mockReset()` 返回 mock 自身（一个函数）——若用表达式体隐式
  // return，vitest 会把这个返回的函数当成本条 beforeEach 的隐式 afterEach 清理回调，
  // 测试结束后自动再调一次 hostCall（此时仍绑定着 mockRejectedValue），产生一个
  // 没人 await/catch 的真实 rejected promise，报成本用例失败——不是宿主桥代码的问题。
  // 同一约定见 mcp_bridge.test.ts（`mockedHostMcpCall.mockReset();` 单独一条语句）。
  beforeEach(() => { vi.mocked(hostCall).mockReset(); });

  it("registers __host_notify__ with title/body schema", () => {
    const pi = fakePi();
    register(pi);
    expect(pi.tools).toHaveLength(1);
    expect(pi.tools[0].name).toBe("__host_notify__");
    expect(Object.keys(pi.tools[0].parameters.properties)).toEqual(["title", "body"]);
  });

  it("forwards params via hostCall and returns host result", async () => {
    vi.mocked(hostCall).mockResolvedValue({ ok: true, id: "n1" });
    const pi = fakePi();
    register(pi);
    const out = await pi.tools[0].execute("call-1", { title: "早报", body: "三件事" });
    expect(hostCall).toHaveBeenCalledWith("__host_notify__", { title: "早报", body: "三件事" });
    expect(out).toEqual({ ok: true, id: "n1" });
  });

  it("turns transport failure into ok:false", async () => {
    vi.mocked(hostCall).mockRejectedValue(new Error("boom"));
    const pi = fakePi();
    register(pi);
    const out = (await pi.tools[0].execute("c", { title: "t", body: "b" })) as any;
    expect(out.ok).toBe(false);
    expect(out.error).toContain("boom");
  });
});
