import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { test, expect, vi } from "vitest";
const listeners = vi.hoisted(() => ({} as Record<string, (e: { payload: unknown }) => void>));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners[name] = cb;
    return Promise.resolve(() => {});
  },
}));
const invokeMock = vi.fn().mockImplementation((cmd: string) => {
  if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
  if (cmd === "list_providers") return Promise.resolve([{ id: "anthropic", display: "Anthropic（Claude）", native: true, region: "intl", configured: true, base_url: null, api: null, presets: [] }]);
  if (cmd === "list_staged_calls" || cmd === "list_approval_rules") return Promise.resolve([]);
  if (cmd === "list_dormant_apps") return Promise.resolve([]);
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
    if (cmd === "list_dormant_apps") return Promise.resolve([]);
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
    if (cmd === "list_dormant_apps") return Promise.resolve([]);
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
    if (cmd === "list_dormant_apps") return Promise.resolve([]);
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
    if (cmd === "list_dormant_apps") return Promise.resolve([]);
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
    if (cmd === "list_dormant_apps") return Promise.resolve([]);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  fireEvent.click(screen.getByText("模型与密钥"));
  await waitFor(() => expect(screen.getByRole("heading", { name: "模型与密钥" })).toBeTruthy());
});

test("providersErr 在「模型与密钥」页里修好（list_providers 恢复成功）后被清除", async () => {
  let broken = true;
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
    if (cmd === "list_providers") return broken
      ? Promise.reject("/x/providers.json 解析失败：bad（请修复或删除该文件）")
      : Promise.resolve([{ id: "anthropic", display: "Anthropic（Claude）", native: true, region: "intl", configured: true, base_url: null, api: null, presets: [] }]);
    if (cmd === "get_model_settings") return Promise.resolve({ default_model: null, apps: [], global: null });
    if (cmd === "custom_provider_presets" || cmd === "usage_by_model") return Promise.resolve([]);
    if (cmd === "list_dormant_apps") return Promise.resolve([]);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("请修复或删除该文件"));
  // 用户去设置页修好文件后重新进入：设置页拉到 list_providers 成功 → 回到工作台时不再显示旧错误。
  broken = false;
  fireEvent.click(screen.getByText("模型与密钥"));
  await waitFor(() => expect(invokeMock.mock.calls.filter((c) => c[0] === "list_providers").length).toBeGreaterThanOrEqual(2));
  fireEvent.click(await screen.findByRole("button", { name: /返回|关闭/ }));
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  expect(screen.queryByText(/读取模型服务配置失败/)).toBeNull();
});

test("收到 app-dormant 且是当前打开的应用 → 回到应用网格，并显示休眠角标", async () => {
  let dormantNow: string[] = [];
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
    if (cmd === "list_providers") return Promise.resolve([{ id: "anthropic", display: "Anthropic（Claude）", native: true, region: "intl", configured: true, base_url: null, api: null, presets: [] }]);
    if (cmd === "open_app") return Promise.resolve(1);
    if (cmd === "list_dormant_apps") return Promise.resolve(dormantNow);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  fireEvent.click(screen.getByText("待办"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("open_app", { appId: "a" }));
  // 打开后网格消失
  await waitFor(() => expect(screen.queryByText("空闲")).toBeNull());
  await waitFor(() => expect(listeners["app-dormant"]).toBeTruthy());
  dormantNow = ["a"];
  listeners["app-dormant"]({ payload: { app_id: "a", idle_secs: 900, freed_rss_bytes: 1000 } });
  await waitFor(() => expect(screen.getByText("休眠")).toBeTruthy());
});

test("点击「资源」入口切到资源面板", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([]);
    if (cmd === "list_providers") return Promise.resolve([{ id: "anthropic", display: "Anthropic（Claude）", native: true, region: "intl", configured: true, base_url: null, api: null, presets: [] }]);
    if (cmd === "resource_report") return Promise.resolve({
      sampled_at: Math.floor(Date.now() / 1000), cpu_ready: true,
      host: { label: "host", root_pids: [1], proc_count: 1, rss_bytes: 1024, cpu_percent: 0 },
      main: null, apps: [], mcp_servers: [],
      other: { label: "other", root_pids: [], proc_count: 0, rss_bytes: 0, cpu_percent: 0 },
      total_rss_bytes: 1024, total_cpu_percent: 0, total_proc_count: 1,
    });
    if (cmd === "get_idle_policy") return Promise.resolve({ enabled: true, timeout_secs: 900, exempt_apps: [] });
    if (cmd === "disk_report") return Promise.resolve({ root_bytes: 0, audit_bytes: 0, notifications_bytes: 0, maker_staging_bytes: 0, main_sessions_bytes: 0, incomplete: false, apps: [], threshold_bytes: 1 });
    if (cmd === "app_usage") return Promise.resolve({ input: 0, output: 0, cost: 0 });
    return Promise.resolve([]);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("资源")).toBeTruthy());
  fireEvent.click(screen.getByText("资源"));
  await waitFor(() => expect(screen.getByText("当前没有打开的应用")).toBeTruthy());
});
