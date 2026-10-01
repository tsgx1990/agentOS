import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { SessionPanel } from "./SessionPanel";

beforeEach(() => invokeMock.mockReset());

test("非精简模式下渲染主会话用量(tokens/真实花费)", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "app_usage") return Promise.resolve({ input: 1200, output: 340, cost: 0.00891 });
    return Promise.resolve();
  });
  render(<SessionPanel />);
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_usage", { appId: "main" }));
  await waitFor(() => expect(screen.getByText("1540")).toBeTruthy());
  expect(screen.getByText("$0.0089")).toBeTruthy();
});

test("精简模式不拉取用量（不渲染用量卡片）", async () => {
  invokeMock.mockResolvedValue({ input: 1, output: 1, cost: 0.001 });
  render(<SessionPanel compact />);
  expect(screen.queryByText("用量")).toBeNull();
  expect(invokeMock).not.toHaveBeenCalledWith("app_usage", { appId: "main" });
});

test("传入 onApprovals 时渲染「审批中心」入口按钮，点击调用回调", async () => {
  const { fireEvent } = await import("@testing-library/react");
  invokeMock.mockResolvedValue({ input: 0, output: 0, cost: 0 });
  const onApprovals = vi.fn();
  render(<SessionPanel onApprovals={onApprovals} />);
  fireEvent.click(screen.getByText("审批中心"));
  expect(onApprovals).toHaveBeenCalledTimes(1);
});

test("不传 onApprovals 时不渲染「审批中心」入口按钮", async () => {
  invokeMock.mockResolvedValue({ input: 0, output: 0, cost: 0 });
  render(<SessionPanel />);
  expect(screen.queryByText("审批中心")).toBeNull();
});

test("传入 onSkills 时渲染「技能」入口按钮，点击调用回调", async () => {
  const { fireEvent } = await import("@testing-library/react");
  invokeMock.mockResolvedValue({ input: 0, output: 0, cost: 0 });
  const onSkills = vi.fn();
  render(<SessionPanel onSkills={onSkills} />);
  fireEvent.click(screen.getByText("技能"));
  expect(onSkills).toHaveBeenCalledTimes(1);
});

test("不传 onSkills 时不渲染「技能」入口按钮", async () => {
  invokeMock.mockResolvedValue({ input: 0, output: 0, cost: 0 });
  render(<SessionPanel />);
  expect(screen.queryByText("技能")).toBeNull();
});

test("传入 onModelSettings 时渲染「模型与密钥」入口按钮，点击调用回调", async () => {
  const { fireEvent } = await import("@testing-library/react");
  invokeMock.mockResolvedValue({ input: 0, output: 0, cost: 0 });
  const onModelSettings = vi.fn();
  render(<SessionPanel onModelSettings={onModelSettings} />);
  fireEvent.click(screen.getByText("模型与密钥"));
  expect(onModelSettings).toHaveBeenCalledTimes(1);
});

test("不传 onModelSettings 时不渲染「模型与密钥」入口按钮", async () => {
  invokeMock.mockResolvedValue({ input: 0, output: 0, cost: 0 });
  render(<SessionPanel />);
  expect(screen.queryByText("模型与密钥")).toBeNull();
});

test("传入 onResources 时渲染「资源」按钮，点击调用回调", async () => {
  invokeMock.mockResolvedValue({ input: 0, output: 0, cost: 0 });
  const onResources = vi.fn();
  render(<SessionPanel onResources={onResources} />);
  fireEvent.click(screen.getByText("资源"));
  expect(onResources).toHaveBeenCalledTimes(1);
});

test("不传 onResources 时不渲染「资源」按钮", async () => {
  invokeMock.mockResolvedValue({ input: 0, output: 0, cost: 0 });
  render(<SessionPanel />);
  expect(screen.queryByText("资源")).toBeNull();
});
