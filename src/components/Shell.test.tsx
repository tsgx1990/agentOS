import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { test, expect, vi } from "vitest";
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
const invokeMock = vi.fn().mockImplementation((cmd: string) => {
  if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
  if (cmd === "list_providers") return Promise.resolve([{ id: "anthropic", display: "Anthropic（Claude）", native: true, region: "intl", configured: true, base_url: null, api: null, presets: [] }]);
  if (cmd === "list_staged_calls" || cmd === "list_approval_rules") return Promise.resolve([]);
  return Promise.resolve(0);
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { Shell } from "./Shell";

test("有应用时中区显示应用网格", async () => {
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
});

test("点击「审批中心」入口切到 ApprovalCenter", async () => {
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  fireEvent.click(screen.getByText("审批中心"));
  await waitFor(() => expect(screen.getByText("没有待批操作")).toBeTruthy());
});

test("点击「技能」入口切到 SkillsView", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
    if (cmd === "list_providers") return Promise.resolve([{ id: "anthropic", display: "Anthropic（Claude）", native: true, region: "intl", configured: true, base_url: null, api: null, presets: [] }]);
    if (cmd === "list_staged_calls" || cmd === "list_approval_rules") return Promise.resolve([]);
    if (cmd === "list_skills" || cmd === "skill_grants" || cmd === "market_fetch_index") return Promise.resolve([]);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  fireEvent.click(screen.getByText("技能"));
  await waitFor(() => expect(screen.getByText("还没有安装技能")).toBeTruthy());
});

test("首次启动（无任何已配置 provider）渲染引导向导而非主工作台", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
    if (cmd === "list_providers") return Promise.resolve([{ id: "anthropic", display: "Anthropic（Claude）", native: true, region: "intl", configured: false, base_url: null, api: null, presets: [] }]);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText(/欢迎使用/)).toBeTruthy());
  expect(screen.queryByText("待办")).toBeNull();
});

test("list_providers 报错（配置文件损坏）→ 不进向导，进主工作台并给出错误提示", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
    if (cmd === "list_providers") return Promise.reject("/x/providers.json 解析失败：bad（请修复或删除该文件）");
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  expect(screen.queryByText(/欢迎使用/)).toBeNull();
  expect(screen.getByRole("alert").textContent).toContain("请修复或删除该文件");
});

test("任一 provider 已配置（不一定是 Anthropic）→ 不显示向导", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
    if (cmd === "list_providers") return Promise.resolve([
      { id: "anthropic", display: "Anthropic（Claude）", native: true, region: "intl", configured: false, base_url: null, api: null, presets: [] },
      { id: "deepseek", display: "DeepSeek 深度求索", native: true, region: "cn", configured: true, base_url: null, api: null, presets: [] },
    ]);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  expect(screen.queryByText(/欢迎使用/)).toBeNull();
});

test("点击「模型与密钥」入口切到模型设置页", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
    if (cmd === "list_providers") return Promise.resolve([
      { id: "deepseek", display: "DeepSeek 深度求索", native: true, region: "cn", configured: true, base_url: null, api: null, presets: ["deepseek-v4-flash"] },
    ]);
    if (cmd === "get_model_settings") return Promise.resolve({ global: null, apps: [] });
    if (cmd === "custom_provider_presets" || cmd === "usage_by_model") return Promise.resolve([]);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  fireEvent.click(screen.getByText("模型与密钥"));
  await waitFor(() => expect(screen.getByRole("heading", { name: "模型与密钥" })).toBeTruthy());
});
