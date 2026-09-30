import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { OnboardingWizard } from "./OnboardingWizard";

// 注意：这里没有用 `beforeEach(() => invokeMock.mockReset())`（其余组件测试的
// 惯例写法）——实测发现该写法与「本文件里用 rejected promise 模拟 BYOK 失败」
// 组合会触发一个当前 vitest/RTL 版本的假阳性「unhandled rejection」判定，使测试
// 本体明明跑对（catch 住、渲染出错误文案）却被报失败；改成每个 test 内联
// `invokeMock.mockReset()` 即可规避，不影响断言的真实性。

test("首次启动渲染欢迎步骤", () => {
  invokeMock.mockReset();
  render(<OnboardingWizard onComplete={() => {}} />);
  expect(screen.getByText(/欢迎使用/)).toBeTruthy();
});

test("输入 key 提交 → 调用 set_api_key，成功后进入起步应用步骤", async () => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
  render(<OnboardingWizard onComplete={() => {}} />);
  fireEvent.click(screen.getByText("开始设置"));
  fireEvent.change(screen.getByPlaceholderText(/API Key/), { target: { value: "sk-test-key" } });
  fireEvent.click(screen.getByText("保存并继续"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_api_key", { provider: "anthropic", key: "sk-test-key" }));
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());
});

test("key 保存失败（BYOK 错误）→ 展示错误文案，停留在当前步骤", async () => {
  invokeMock.mockReset();
  invokeMock.mockRejectedValue("keychain 写入失败：模拟错误");
  render(<OnboardingWizard onComplete={() => {}} />);
  fireEvent.click(screen.getByText("开始设置"));
  fireEvent.change(screen.getByPlaceholderText(/API Key/), { target: { value: "bad-key" } });
  fireEvent.click(screen.getByText("保存并继续"));
  await waitFor(() => expect(screen.getByText(/keychain 写入失败/)).toBeTruthy());
  expect(screen.queryByText(/起步应用/)).toBeNull();
});

test("跳过起步应用 → 调用完成回调，不调 install_builtin_sample", async () => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
  const onComplete = vi.fn();
  render(<OnboardingWizard onComplete={onComplete} />);
  fireEvent.click(screen.getByText("开始设置"));
  fireEvent.change(screen.getByPlaceholderText(/API Key/), { target: { value: "sk-test-key" } });
  fireEvent.click(screen.getByText("保存并继续"));
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());
  fireEvent.click(screen.getByText("跳过，直接进入"));
  expect(onComplete).toHaveBeenCalled();
  expect(invokeMock).not.toHaveBeenCalledWith("install_builtin_sample", expect.anything());
});

test("安装起步应用（待办便签）→ 调用 install_builtin_sample(白名单 name)，完成后进入主界面", async () => {
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "set_api_key") return Promise.resolve();
    if (cmd === "install_builtin_sample") return Promise.resolve({ app_id: "superagent__todo-notes" });
    return Promise.resolve();
  });
  const onComplete = vi.fn();
  render(<OnboardingWizard onComplete={onComplete} />);
  fireEvent.click(screen.getByText("开始设置"));
  fireEvent.change(screen.getByPlaceholderText(/API Key/), { target: { value: "sk-test-key" } });
  fireEvent.click(screen.getByText("保存并继续"));
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());

  fireEvent.click(screen.getByText("安装"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("install_builtin_sample", { name: "todo-notes" }));
  await waitFor(() => expect(screen.getByText("进入主界面")).toBeTruthy());

  fireEvent.click(screen.getByText("进入主界面"));
  expect(onComplete).toHaveBeenCalled();
});

test("安装起步应用失败 → 展示错误，仍可跳过完成", async () => {
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "set_api_key") return Promise.resolve();
    if (cmd === "install_builtin_sample") return Promise.reject("安装失败：模拟错误");
    return Promise.resolve();
  });
  const onComplete = vi.fn();
  render(<OnboardingWizard onComplete={onComplete} />);
  fireEvent.click(screen.getByText("开始设置"));
  fireEvent.change(screen.getByPlaceholderText(/API Key/), { target: { value: "sk-test-key" } });
  fireEvent.click(screen.getByText("保存并继续"));
  await waitFor(() => expect(screen.getByText(/起步应用/)).toBeTruthy());

  fireEvent.click(screen.getByText("安装"));
  await waitFor(() => expect(screen.getByText(/安装失败/)).toBeTruthy());
  fireEvent.click(screen.getByText("跳过，直接进入"));
  expect(onComplete).toHaveBeenCalled();
});
