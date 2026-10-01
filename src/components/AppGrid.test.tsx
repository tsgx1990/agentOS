import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { AppGrid } from "./AppGrid";
import type { InstalledApp } from "../lib/registry";

const app = (id: string, trusted: boolean): InstalledApp => ({
  app_id: id, name: id, version: "1.0.0", display_name: id,
  category: "life", icon: null, trusted, domains: [],
});

beforeEach(() => invokeMock.mockReset());

test("渲染卡片、未验证标识、点击回调", async () => {
  invokeMock.mockResolvedValue({ sandboxed: false, platform: "linux", restricted: true });
  const onOpen = vi.fn();
  render(<AppGrid apps={[app("first", true), app("third", false)]} onOpen={onOpen} />);
  expect(screen.getByText("first")).toBeTruthy();
  expect(screen.getByText(/未验证/)).toBeTruthy(); // 仅 third 有
  fireEvent.click(screen.getByText("first"));
  expect(onOpen).toHaveBeenCalledWith("first");
});

test("三种沙盒状态分别渲染对应徽标（不阻塞卡片渲染）", async () => {
  invokeMock.mockImplementation((cmd: string, args: { appId: string }) => {
    if (cmd !== "app_sandbox_status") return Promise.resolve();
    const map: Record<string, { sandboxed: boolean; platform: string; restricted: boolean }> = {
      sandboxed: { sandboxed: true, platform: "macos", restricted: false },
      warned: { sandboxed: false, platform: "linux", restricted: true },
      localtrust: { sandboxed: false, platform: "linux", restricted: false },
    };
    return Promise.resolve(map[args.appId]);
  });
  render(
    <AppGrid
      apps={[app("sandboxed", false), app("warned", false), app("localtrust", true)]}
      onOpen={() => {}}
    />,
  );
  // 卡片本体先于异步状态渲染出来
  expect(screen.getByText("sandboxed")).toBeTruthy();
  await waitFor(() => expect(screen.getByText("沙盒中")).toBeTruthy());
  expect(screen.getByText("未沙盒·受限")).toBeTruthy();
  expect(screen.getByText("本地信任")).toBeTruthy();
});

test("dormant 含的应用显示「休眠」，其余仍显示「空闲」", async () => {
  invokeMock.mockResolvedValue({ sandboxed: true, platform: "macos", restricted: false });
  render(<AppGrid apps={[app("first", true), app("second", true)]} onOpen={() => {}} dormant={["second"]} />);
  expect(screen.getAllByText("休眠")).toHaveLength(1);
  expect(screen.getAllByText("空闲")).toHaveLength(1);
});
