import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { ModelSettings } from "./ModelSettings";

const nat = (id: string, display: string, region: "cn" | "intl", configured: boolean, presets: string[]) => ({
  id, display, native: true, region, configured, base_url: null, api: null, presets,
});
const PROVIDERS = [
  nat("anthropic", "Anthropic（Claude）", "intl", true, ["claude-sonnet-5", "claude-opus-5"]),
  nat("openai", "OpenAI", "intl", false, ["gpt-5.5"]),
  nat("deepseek", "DeepSeek 深度求索", "cn", true, ["deepseek-v4-flash", "deepseek-v4-pro"]),
  nat("moonshotai-cn", "月之暗面开放平台（Moonshot）", "cn", false, ["kimi-k3"]),
  { id: "custom-empty", display: "我的自建服务", native: false, region: null, configured: true, base_url: "https://llm.example.com/v1", api: "openai-completions", presets: [] },
];
const SETTINGS = {
  global: { provider: "anthropic", model: "claude-sonnet-5" },
  apps: [
    {
      app_id: "code-reviewer",
      manifest_model: "claude-sonnet-5",
      app_override: { provider: "deepseek", model: "deepseek-v4-pro" },
      effective: { provider: "deepseek", model: "deepseek-v4-pro", source: "app" },
    },
  ],
};
const ROWS = [
  { app_id: "code-reviewer", provider: "deepseek", model: "deepseek-v4-pro", input: 1000, output: 200, cost: 0.5 },
  { app_id: "code-reviewer", provider: "anthropic", model: "claude-sonnet-5", input: 300, output: 40, cost: 0.25 },
];
const TOTAL = { input: 1700, output: 300, cost: 0.9 };
const PROBE_OK = { ok: true, kind: "ok", latency_ms: 312, provider: "anthropic", model: "claude-sonnet-5", message: "连通", detail: "" };
const PROBE_BAD = { ok: false, kind: "invalid_key", latency_ms: 90, provider: "anthropic", model: "claude-sonnet-5", message: "密钥无效或已被撤销", detail: "" };
const PROBE_RATE = { ok: false, kind: "rate_limited", latency_ms: 90, provider: "anthropic", model: "claude-sonnet-5", message: "服务商限流", detail: "" };

function installMock(probe: unknown = PROBE_OK) {
  invokeMock.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "list_providers": return Promise.resolve(PROVIDERS);
      case "custom_provider_presets": return Promise.resolve([]);
      case "get_model_settings": return Promise.resolve(SETTINGS);
      case "usage_by_model": return Promise.resolve(ROWS);
      case "app_usage": return Promise.resolve(TOTAL);
      case "test_provider": return Promise.resolve(probe);
      default: return Promise.resolve(undefined);
    }
  });
}

async function mount() {
  render(<ModelSettings />);
  await screen.findByRole("button", { name: /DeepSeek 深度求索/ });
}
const pick = (name: string) => fireEvent.click(screen.getByRole("button", { name: new RegExp(name) }));

beforeEach(() => {
  invokeMock.mockReset();
  installMock();
});

test("国内组出现「DeepSeek 深度求索」，国际组出现 Anthropic", async () => {
  await mount();
  expect(screen.getByText("国内")).toBeTruthy();
  expect(screen.getByText("国际")).toBeTruthy();
  const nav = screen.getByRole("navigation", { name: "服务列表" });
  expect(within(nav).getByText("DeepSeek 深度求索")).toBeTruthy();
  expect(within(nav).getByText("Anthropic（Claude）")).toBeTruthy();
});

test("已配置的 provider 显示「已配置」徽标，未配置的显示「未配置」", async () => {
  await mount();
  const item = screen.getByRole("button", { name: /DeepSeek 深度求索/ });
  expect(within(item).getByText("已配置")).toBeTruthy();
  const idle = screen.getByRole("button", { name: /^OpenAI/ });
  expect(within(idle).getByText("未配置")).toBeTruthy();
});

test("保存密钥调用 set_api_key（provider 为所选），随后清空输入框且从不回显", async () => {
  await mount();
  pick("^OpenAI");
  const input = screen.getByLabelText("API 密钥") as HTMLInputElement;
  expect(input.type).toBe("password");
  fireEvent.change(input, { target: { value: "sk-test-fake" } });
  fireEvent.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_api_key", { provider: "openai", key: "sk-test-fake" }));
  await waitFor(() => expect((screen.getByLabelText("API 密钥") as HTMLInputElement).value).toBe(""));
  expect(screen.queryByDisplayValue("sk-test-fake")).toBeNull();
});

test("清除没人在用的 provider 的密钥：直接调用 clear_api_key，不弹确认", async () => {
  await mount();
  pick("我的自建服务");
  fireEvent.click(screen.getByRole("button", { name: "清除" }));
  expect(screen.queryByRole("alertdialog")).toBeNull();
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("clear_api_key", { provider: "custom-empty" }));
});

test("测试失败渲染 message；测试按钮旁有费用提示", async () => {
  installMock(PROBE_BAD);
  await mount();
  pick("DeepSeek 深度求索");
  expect(screen.getByText("会发送一条极短请求，产生少量费用")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "测试连通性" }));
  await screen.findByText("密钥无效或已被撤销");
  expect(invokeMock).toHaveBeenCalledWith("test_provider", { provider: "deepseek", model: "deepseek-v4-flash" });
  expect(screen.queryByText("偶发限流可稍后重试")).toBeNull();
});

test("测试失败且 kind 为 rate_limited 时提示偶发限流可稍后重试", async () => {
  installMock(PROBE_RATE);
  await mount();
  pick("DeepSeek 深度求索");
  fireEvent.click(screen.getByRole("button", { name: "测试连通性" }));
  await screen.findByText("偶发限流可稍后重试");
});

test("测试成功渲染延迟", async () => {
  await mount();
  pick("DeepSeek 深度求索");
  fireEvent.click(screen.getByRole("button", { name: "测试连通性" }));
  await screen.findByText("312 ms");
});

test("新增自定义：id 自动加 custom- 前缀并调用 create_custom_provider（新建不走覆盖路径）", async () => {
  await mount();
  fireEvent.click(screen.getByRole("button", { name: /其它（OpenAI 兼容）/ }));
  fireEvent.change(screen.getByLabelText("服务 ID"), { target: { value: "mine" } });
  fireEvent.change(screen.getByLabelText("显示名"), { target: { value: "我的服务" } });
  fireEvent.change(screen.getByLabelText("接口地址（base URL）"), { target: { value: "https://llm.example.com/v1" } });
  fireEvent.change(screen.getByLabelText("模型 id（逗号分隔）"), { target: { value: "m-1, m-2" } });
  fireEvent.click(screen.getByRole("button", { name: "添加服务" }));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("create_custom_provider", {
      provider: {
        id: "custom-mine",
        display: "我的服务",
        base_url: "https://llm.example.com/v1",
        api: "openai-completions",
        models: ["m-1", "m-2"],
      },
    }),
  );
});

test("默认模型：只列已配置的 provider；换 provider 调用 set_global_model 并带该 provider 的首个预设", async () => {
  await mount();
  const sel = screen.getByLabelText("默认模型服务");
  const names = within(sel).getAllByRole("option").map((o) => o.textContent);
  expect(names).toContain("DeepSeek 深度求索");
  expect(names).not.toContain("OpenAI");
  fireEvent.change(sel, { target: { value: "deepseek" } });
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_global_model", {
      choice: { provider: "deepseek", model: "deepseek-v4-flash" },
    }),
  );
});

test("默认模型：从该 provider 的预设里换模型", async () => {
  await mount();
  fireEvent.change(screen.getByLabelText("默认模型名称"), { target: { value: "claude-opus-5" } });
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_global_model", {
      choice: { provider: "anthropic", model: "claude-opus-5" },
    }),
  );
});

test("默认模型：手填模型 id", async () => {
  await mount();
  fireEvent.change(screen.getByLabelText("默认模型名称"), { target: { value: "__manual__" } });
  fireEvent.change(screen.getByLabelText("手填模型 id"), { target: { value: "my-own-model" } });
  fireEvent.click(screen.getByRole("button", { name: "应用" }));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_global_model", {
      choice: { provider: "anthropic", model: "my-own-model" },
    }),
  );
});

test("按应用：覆盖下拉能看出 provider；撤销覆盖调用 set_app_model 且 choice 为 null", async () => {
  await mount();
  const sel = screen.getByLabelText("code-reviewer 的模型覆盖") as HTMLSelectElement;
  expect(sel.selectedOptions[0].textContent).toBe("DeepSeek 深度求索 / deepseek-v4-pro");
  fireEvent.change(sel, { target: { value: "" } });
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_app_model", { appId: "code-reviewer", choice: null }),
  );
  expect(screen.getByText(/已打开的应用下次打开时生效/)).toBeTruthy();
});

test("用量按 provider 分组，并有「其他（工具 / 压缩）」行 = 应用总量 − 各模型之和", async () => {
  await mount();
  const usage = await screen.findByRole("region", { name: "用量" });
  await within(usage).findByText("其他（工具 / 压缩）");
  // 分组标题
  expect(within(usage).getAllByText("DeepSeek 深度求索").length).toBeGreaterThan(0);
  expect(within(usage).getAllByText("Anthropic（Claude）").length).toBeGreaterThan(0);
  // 其他 = 1700-1300=400 输入，300-240=60 输出，0.9-0.75=0.15 费用
  const other = within(usage).getByText("其他（工具 / 压缩）").closest("tbody") as HTMLElement;
  expect(within(other).getByText("400")).toBeTruthy();
  expect(within(other).getByText("60")).toBeTruthy();
  expect(within(other).getByText("$0.1500")).toBeTruthy();
});

test("默认模型：选没有预设的自定义服务时出现手填输入，手填后才写入", async () => {
  await mount();
  fireEvent.change(screen.getByLabelText("默认模型服务"), { target: { value: "custom-empty" } });
  const input = await screen.findByLabelText("手填模型 id");
  expect(invokeMock).not.toHaveBeenCalledWith("set_global_model", expect.anything());
  fireEvent.change(input, { target: { value: "my-model" } });
  fireEvent.click(screen.getByRole("button", { name: "应用" }));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_global_model", {
      choice: { provider: "custom-empty", model: "my-model" },
    }),
  );
});

test("保存新密钥后清掉该 provider 上一次的测试结果", async () => {
  installMock(PROBE_BAD);
  await mount();
  pick("DeepSeek 深度求索");
  fireEvent.click(screen.getByRole("button", { name: "测试连通性" }));
  await screen.findByText("密钥无效或已被撤销");
  fireEvent.change(screen.getByLabelText("API 密钥"), { target: { value: "sk-test-fake" } });
  fireEvent.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() => expect(screen.queryByText("密钥无效或已被撤销")).toBeNull());
});

test("新建自定义服务撞已有 id：展示后端的错误，不收起表单", async () => {
  const base = invokeMock.getMockImplementation()!;
  invokeMock.mockImplementation((cmd: string, ...r: unknown[]) =>
    cmd === "create_custom_provider" ? Promise.reject("这个服务 id 已存在：custom-empty") : base(cmd, ...r));
  await mount();
  fireEvent.click(screen.getByRole("button", { name: /其它（OpenAI 兼容）/ }));
  fireEvent.change(screen.getByLabelText("服务 ID"), { target: { value: "empty" } });
  fireEvent.change(screen.getByLabelText("显示名"), { target: { value: "x" } });
  fireEvent.change(screen.getByLabelText("接口地址（base URL）"), { target: { value: "https://evil.example.com/v1" } });
  fireEvent.click(screen.getByRole("button", { name: "添加服务" }));
  expect((await screen.findByRole("alert")).textContent).toContain("已存在");
  expect(screen.getByLabelText("服务 ID")).toBeTruthy();
});

test("修改已有自定义服务的 base_url：先确认，确认后才调用 save_custom_provider", async () => {
  await mount();
  pick("我的自建服务");
  fireEvent.click(screen.getByRole("button", { name: "修改此服务" }));
  expect((screen.getByLabelText("服务 ID") as HTMLInputElement).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("接口地址（base URL）"), { target: { value: "https://other.example.com/v1" } });
  fireEvent.click(screen.getByRole("button", { name: "保存修改" }));
  expect(screen.getByText("改地址后密钥会发往新地址，确定？")).toBeTruthy();
  expect(invokeMock).not.toHaveBeenCalledWith("save_custom_provider", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "确定修改" }));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("save_custom_provider", {
      provider: expect.objectContaining({ id: "custom-empty", base_url: "https://other.example.com/v1" }),
    }),
  );
});

test("修改自定义服务但不改地址：不需要确认", async () => {
  await mount();
  pick("我的自建服务");
  fireEvent.click(screen.getByRole("button", { name: "修改此服务" }));
  fireEvent.change(screen.getByLabelText("显示名"), { target: { value: "改名" } });
  fireEvent.click(screen.getByRole("button", { name: "保存修改" }));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("save_custom_provider", expect.anything()));
});

test("删除自定义服务：先出确认，确认后才调用 remove_custom_provider", async () => {
  await mount();
  pick("我的自建服务");
  fireEvent.click(screen.getByRole("button", { name: "删除此自定义服务" }));
  expect(screen.getByText(/钥匙串里的密钥也会一并删除/)).toBeTruthy();
  expect(invokeMock).not.toHaveBeenCalledWith("remove_custom_provider", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(invokeMock).not.toHaveBeenCalledWith("remove_custom_provider", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "删除此自定义服务" }));
  fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("remove_custom_provider", { id: "custom-empty" }));
});

test("两个 provider 同时在测：先结束的那个不让另一个的按钮解锁", async () => {
  const resolvers: Record<string, (v: unknown) => void> = {};
  const base = invokeMock.getMockImplementation()!;
  invokeMock.mockImplementation((cmd: string, args: { provider?: string }) =>
    cmd === "test_provider"
      ? new Promise((res) => { resolvers[args.provider!] = res; })
      : base(cmd, args));
  await mount();
  pick("DeepSeek 深度求索");
  fireEvent.click(screen.getByRole("button", { name: "测试连通性" }));
  pick("^Anthropic");
  fireEvent.click(screen.getByRole("button", { name: "测试连通性" }));
  await waitFor(() => expect(Object.keys(resolvers).sort()).toEqual(["anthropic", "deepseek"]));
  resolvers.anthropic(PROBE_OK);
  await screen.findByText("312 ms");
  pick("DeepSeek 深度求索");
  const btn = screen.getByRole("button", { name: "测试中…" }) as HTMLButtonElement;
  expect(btn.disabled).toBe(true);
  resolvers.deepseek(PROBE_OK);
  await waitFor(() => expect((screen.getByRole("button", { name: "测试连通性" }) as HTMLButtonElement).disabled).toBe(false));
});

test("清除正被默认模型 / 应用覆盖使用的 provider 的密钥：先确认并写明谁在用", async () => {
  await mount();
  pick("DeepSeek 深度求索"); // 被 code-reviewer 的覆盖使用
  fireEvent.click(screen.getByRole("button", { name: "清除" }));
  expect(screen.getByRole("alertdialog").textContent).toContain("应用 code-reviewer正在用它，清除后这些会话将无法调用模型");
  expect(invokeMock).not.toHaveBeenCalledWith("clear_api_key", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "确认清除" }));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("clear_api_key", { provider: "deepseek" }));
});

test("清除全局默认所用 provider 的密钥：确认文案含「默认模型」", async () => {
  await mount();
  pick("^Anthropic");
  fireEvent.click(screen.getByRole("button", { name: "清除" }));
  expect(screen.getByRole("alertdialog").textContent).toContain("默认模型正在用它");
});

test("全局默认指向未配置的 provider：默认模型处显示告警", async () => {
  const base = invokeMock.getMockImplementation()!;
  invokeMock.mockImplementation((cmd: string, ...r: unknown[]) =>
    cmd === "get_model_settings"
      ? Promise.resolve({ ...SETTINGS, global: { provider: "openai", model: "gpt-5.5" } })
      : base(cmd, ...r));
  await mount();
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("未配置密钥"));
  expect(screen.getByRole("alert").textContent).toContain("OpenAI");
});

test("全局默认指向已配置的 provider：没有告警", async () => {
  await mount();
  expect(screen.queryByRole("alert")).toBeNull();
});
