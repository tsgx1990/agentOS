import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { test, expect, vi } from "vitest";
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
const invokeMock = vi.fn().mockImplementation((cmd: string) => {
  if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
  if (cmd === "has_api_key") return Promise.resolve(true);
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
    if (cmd === "has_api_key") return Promise.resolve(true);
    if (cmd === "list_staged_calls" || cmd === "list_approval_rules") return Promise.resolve([]);
    if (cmd === "list_skills" || cmd === "skill_grants" || cmd === "market_fetch_index") return Promise.resolve([]);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText("待办")).toBeTruthy());
  fireEvent.click(screen.getByText("技能"));
  await waitFor(() => expect(screen.getByText("还没有安装技能")).toBeTruthy());
});

test("首次启动（无 key）渲染引导向导而非主工作台", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_apps") return Promise.resolve([{ app_id: "a", name: "a", version: "1.0.0", display_name: "待办", category: "life", icon: null, trusted: true, domains: [] }]);
    if (cmd === "has_api_key") return Promise.resolve(false);
    return Promise.resolve(0);
  });
  render(<Shell />);
  await waitFor(() => expect(screen.getByText(/欢迎使用/)).toBeTruthy());
  expect(screen.queryByText("待办")).toBeNull();
});
