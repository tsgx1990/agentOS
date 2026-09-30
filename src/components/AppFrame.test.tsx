import { render, screen, waitFor } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { AppFrame } from "./AppFrame";

beforeEach(() => invokeMock.mockReset());

test("应用头部权限按钮旁显示待批数徽标", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_staged_calls") return Promise.resolve([{ id: "s1" }, { id: "s2" }]);
    return Promise.resolve();
  });
  render(<AppFrame slot={1} appId="notes" onClose={() => {}} onUninstall={() => {}} />);
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("list_staged_calls", { appId: "notes" }));
  await waitFor(() => expect(screen.getByText("2")).toBeTruthy());
});

test("待批数为 0 时不显示徽标", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_staged_calls") return Promise.resolve([]);
    return Promise.resolve();
  });
  render(<AppFrame slot={1} appId="notes" onClose={() => {}} onUninstall={() => {}} />);
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("list_staged_calls", { appId: "notes" }));
  expect(screen.queryByText("0")).toBeNull();
});

test("每 5 秒轮询一次待批数（setInterval 间隔 5000ms）", async () => {
  const setIntervalSpy = vi.spyOn(window, "setInterval");
  invokeMock.mockResolvedValue([]);
  render(<AppFrame slot={1} appId="notes" onClose={() => {}} onUninstall={() => {}} />);
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("list_staged_calls", { appId: "notes" }));
  expect(setIntervalSpy).toHaveBeenCalledWith(expect.any(Function), 5000);
  setIntervalSpy.mockRestore();
});
