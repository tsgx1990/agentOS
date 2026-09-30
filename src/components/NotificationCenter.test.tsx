import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { NotificationCenter } from "./NotificationCenter";

beforeEach(() => invokeMock.mockReset());

test("渲染任务结果通知（时间/种类/应用/标题/正文）", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_notifications") return Promise.resolve([
      { id: "notif-1", ts: "2026-07-18T00:00:00Z", kind: "task_result", app_id: "todo-notes", title: "笔记摘要完成", body: "已生成 3 条摘要", acked: false },
    ]);
    return Promise.resolve();
  });
  render(<NotificationCenter />);
  await waitFor(() => expect(screen.getByText("笔记摘要完成")).toBeTruthy());
  expect(screen.getByText("已生成 3 条摘要")).toBeTruthy();
  expect(screen.getByText("todo-notes")).toBeTruthy();
  expect(screen.getByText("2026-07-18T00:00:00Z")).toBeTruthy();
  expect(invokeMock).toHaveBeenCalledWith("list_notifications", { filter: {} });
});

test("空通知列表显示空态", async () => {
  invokeMock.mockResolvedValue([]);
  render(<NotificationCenter />);
  await waitFor(() => expect(screen.getByText(/还没有通知/)).toBeTruthy());
});

test("非确认类通知点击已读调用 ack_notification", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_notifications") return Promise.resolve([
      { id: "notif-1", ts: "2026-07-18T00:00:00Z", kind: "task_result", app_id: "todo-notes", title: "笔记摘要完成", body: "已生成 3 条摘要", acked: false },
    ]);
    return Promise.resolve();
  });
  render(<NotificationCenter />);
  await waitFor(() => expect(screen.getByText("笔记摘要完成")).toBeTruthy());
  fireEvent.click(screen.getByText("标记已读"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("ack_notification", { id: "notif-1" }));
});

test("confirm_request 条目渲染「去审批中心」按钮，点击回调 onOpenApprovals", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_notifications") return Promise.resolve([
      { id: "confirm-1", ts: "2026-07-18T00:00:00Z", kind: "confirm_request", app_id: "notes-writer", title: "写入确认", body: "请求写入 notes.md", acked: false },
    ]);
    return Promise.resolve();
  });
  const onOpenApprovals = vi.fn();
  render(<NotificationCenter onOpenApprovals={onOpenApprovals} />);
  await waitFor(() => expect(screen.getByText("写入确认")).toBeTruthy());
  expect(screen.queryByText("允许")).toBeNull();
  expect(screen.queryByText("总是允许")).toBeNull();
  expect(screen.queryByText("拒绝")).toBeNull();
  fireEvent.click(screen.getByText("去审批中心"));
  expect(onOpenApprovals).toHaveBeenCalledTimes(1);
  expect(invokeMock).not.toHaveBeenCalledWith("respond_confirm", expect.anything());
});
