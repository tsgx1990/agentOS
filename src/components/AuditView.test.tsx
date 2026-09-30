import { render, screen, waitFor } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { AuditView } from "./AuditView";

beforeEach(() => invokeMock.mockReset());

test("渲染审计条目列表（时间/应用/工具/裁决）", async () => {
  invokeMock.mockResolvedValue([
    { ts: "2026-07-18T00:00:00Z", app_id: "todo-notes", tool: "write", args: "{}", verdict: "allow" },
    { ts: "2026-07-18T00:01:00Z", app_id: "todo-notes", tool: "bash", args: "{}", verdict: "deny" },
  ]);
  render(<AuditView />);
  await waitFor(() => expect(screen.getByText("write")).toBeTruthy());
  expect(screen.getByText("allow")).toBeTruthy();
  expect(screen.getByText("bash")).toBeTruthy();
  expect(screen.getByText("deny")).toBeTruthy();
  expect(screen.getByText("2026-07-18T00:00:00Z")).toBeTruthy();
  expect(screen.getAllByText("todo-notes").length).toBe(2);
  expect(invokeMock).toHaveBeenCalledWith("list_audit", { filter: {} });
});

test("空审计日志显示空态", async () => {
  invokeMock.mockResolvedValue([]);
  render(<AuditView />);
  await waitFor(() => expect(screen.getByText(/还没有审计记录/)).toBeTruthy());
});
