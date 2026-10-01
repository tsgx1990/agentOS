import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { OnboardingWizard } from "./OnboardingWizard";

const nat = (id: string, display: string, region: "cn" | "intl", presets: string[]) => ({
  id, display, native: true, region, configured: false, base_url: null, api: null, presets,
});
const PROVIDERS = [
  nat("anthropic", "Anthropic（Claude）", "intl", ["claude-sonnet-5", "claude-opus-5"]),
  nat("openai", "OpenAI", "intl", ["gpt-5.5"]),
  nat("deepseek", "DeepSeek 深度求索", "cn", ["deepseek-v4-flash", "deepseek-v4-pro"]),
];
const PROBE_OK = { ok: true, kind: "ok", latency_ms: 210, provider: "x", model: "y", message: "连通", detail: "" };
const PROBE_BAD = { ok: false, kind: "network", latency_ms: 0, provider: "x", model: "y", message: "网络不通，请检查代理", detail: "" };

/** 默认：没有任何已配置 provider、没有全局默认、测试成功；各命令可单独覆盖。 */
function setup(over: Record<string, (...a: unknown[]) => Promise<unknown>> = {}) {
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string, ...rest: unknown[]) => {
    if (over[cmd]) return over[cmd](...rest);
    switch (cmd) {
      case "list_providers": return Promise.resolve(PROVIDERS);
      case "get_model_settings": return Promise.resolve({ global: null, apps: [] });
      case "test_provider": return Promise.resolve(PROBE_OK);
      default: return Promise.resolve(undefined);
    }
  });
}

/** 欢迎 → 选服务 → 填密钥，停在填密钥一步。 */
async function toKeyStep(providerName: RegExp) {
  fireEvent.click(screen.getByText("开始设置"));
  fireEvent.click(await screen.findByRole("button", { name: providerName }));
  fireEvent.click(screen.getByRole("button", { name: "下一步" }));
  return screen.findByPlaceholderText(/API Key/);
}
async function saveWith(providerName: RegExp, key: string) {
  const input = await toKeyStep(providerName);
  fireEvent.change(input, { target: { value: key } });
  fireEvent.click(screen.getByText("保存并继续"));
}

// 注意：这里没有用 `beforeEach(() => invokeMock.mockReset())`（其余组件测试的
// 惯例写法）——实测发现该写法与「本文件里用 rejected promise 模拟 BYOK 失败」
// 组合会触发一个当前 vitest/RTL 版本的假阳性「unhandled rejection」判定，使测试
// 本体明明跑对（catch 住、渲染出错误文案）却被报失败；改成每个 test 内联
// `setup()`（内含 mockReset）即可规避，不影响断言的真实性。

test("首次启动渲染欢迎步骤", () => {
  setup();
  render(<OnboardingWizard onComplete={() => {}} />);
  expect(screen.getByText(/欢迎使用/)).toBeTruthy();
});

test("选服务一步：国内 / 国际两组，Anthropic 与 DeepSeek 带「推荐」标，OpenAI 没有", async () => {
  setup();
  render(<OnboardingWizard onComplete={() => {}} />);
  fireEvent.click(screen.getByText("开始设置"));
  const anthropic = await screen.findByRole("button", { name: /Anthropic/ });
  expect(anthropic.textContent).toContain("推荐");
  expect(screen.getByRole("button", { name: /DeepSeek/ }).textContent).toContain("推荐");
  expect(screen.getByRole("button", { name: /^OpenAI/ }).textContent).not.toContain("推荐");
  expect(screen.getByText("国内")).toBeTruthy();
  expect(screen.getByText("国际")).toBeTruthy();
  expect(screen.getByText(/稍后可在「模型与密钥」里添加/)).toBeTruthy();
});

test("选 Anthropic 输入 key 提交 → set_api_key，成功后进入起步应用步骤", async () => {
  setup();
  render(<OnboardingWizard onComplete={() => {}} />);
  await saveWith(/Anthropic/, "sk-test-key");
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_api_key", { provider: "anthropic", key: "sk-test-key" }));
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());
});

test("选 DeepSeek → set_api_key → test_provider → 尚无全局默认则 set_global_model(首个预设)", async () => {
  setup();
  render(<OnboardingWizard onComplete={() => {}} />);
  await saveWith(/DeepSeek/, "sk-test-key");
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_api_key", { provider: "deepseek", key: "sk-test-key" }));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("test_provider", { provider: "deepseek", model: "deepseek-v4-flash" }));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_global_model", {
      choice: { provider: "deepseek", model: "deepseek-v4-flash" },
    }),
  );
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());
});

test("已有全局默认时不覆盖它", async () => {
  setup({ get_model_settings: () => Promise.resolve({ global: { provider: "openai", model: "gpt-5.5" }, apps: [] }) });
  render(<OnboardingWizard onComplete={() => {}} />);
  await saveWith(/DeepSeek/, "sk-test-key");
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());
  expect(invokeMock).not.toHaveBeenCalledWith("set_global_model", expect.anything());
});

test("连通性测试失败 → 显示 message，「重新填写」留在本步，「仍然继续」可进入下一步", async () => {
  setup({ test_provider: () => Promise.resolve(PROBE_BAD) });
  render(<OnboardingWizard onComplete={() => {}} />);
  await saveWith(/DeepSeek/, "sk-test-key");
  await screen.findByText("网络不通，请检查代理");
  expect(screen.queryByText(/起步应用/)).toBeNull();
  expect(screen.getByRole("button", { name: "重新填写" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "仍然继续" }));
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());
});

test("测试失败且 kind 为 rate_limited → 提示偶发限流可稍后重试", async () => {
  setup({ test_provider: () => Promise.resolve({ ...PROBE_BAD, kind: "rate_limited", message: "服务商限流" }) });
  render(<OnboardingWizard onComplete={() => {}} />);
  await saveWith(/DeepSeek/, "sk-test-key");
  await screen.findByText("偶发限流可稍后重试");
});

test("key 保存失败（BYOK 错误）→ 展示错误文案，停留在当前步骤", async () => {
  setup({ set_api_key: () => Promise.reject("keychain 写入失败：模拟错误") });
  render(<OnboardingWizard onComplete={() => {}} />);
  await saveWith(/Anthropic/, "bad-key");
  await waitFor(() => expect(screen.getByText(/keychain 写入失败/)).toBeTruthy());
  expect(screen.queryByText(/起步应用/)).toBeNull();
  expect(invokeMock).not.toHaveBeenCalledWith("test_provider", expect.anything());
});

test("跳过起步应用 → 调用完成回调，不调 install_builtin_sample", async () => {
  setup();
  const onComplete = vi.fn();
  render(<OnboardingWizard onComplete={onComplete} />);
  await saveWith(/Anthropic/, "sk-test-key");
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());
  fireEvent.click(screen.getByText("跳过，直接进入"));
  expect(onComplete).toHaveBeenCalled();
  expect(invokeMock).not.toHaveBeenCalledWith("install_builtin_sample", expect.anything());
});

test("安装起步应用（待办便签）→ 调用 install_builtin_sample(白名单 name)，完成后进入主界面", async () => {
  setup({ install_builtin_sample: () => Promise.resolve({ app_id: "superagent__todo-notes" }) });
  const onComplete = vi.fn();
  render(<OnboardingWizard onComplete={onComplete} />);
  await saveWith(/Anthropic/, "sk-test-key");
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());

  fireEvent.click(screen.getByText("安装"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("install_builtin_sample", { name: "todo-notes" }));
  await waitFor(() => expect(screen.getByText("进入主界面")).toBeTruthy());

  fireEvent.click(screen.getByText("进入主界面"));
  expect(onComplete).toHaveBeenCalled();
});

test("安装起步应用失败 → 展示错误，仍可跳过完成", async () => {
  setup({ install_builtin_sample: () => Promise.reject("安装失败：模拟错误") });
  const onComplete = vi.fn();
  render(<OnboardingWizard onComplete={onComplete} />);
  await saveWith(/Anthropic/, "sk-test-key");
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());

  fireEvent.click(screen.getByText("安装"));
  await waitFor(() => expect(screen.getByText(/安装失败/)).toBeTruthy());
  fireEvent.click(screen.getByText("跳过，直接进入"));
  expect(onComplete).toHaveBeenCalled();
});
