import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { ApprovalCenter } from "./ApprovalCenter";

const twoStagedSameAppDiffTool = [
  { id: "stg-1", app_id: "notes-writer", server: "fs1", tool: "write_file", args: { path: "a.md" }, created_at: Math.floor(Date.now() / 1000) - 300 },
  { id: "stg-2", app_id: "notes-writer", server: "fs1", tool: "delete_file", args: { path: "b.md" }, created_at: Math.floor(Date.now() / 1000) - 60 },
];

function mockInvoke(overrides: Record<string, unknown> = {}) {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_staged_calls") return Promise.resolve(overrides.staged ?? []);
    if (cmd === "list_approval_rules") return Promise.resolve(overrides.rules ?? []);
    if (cmd === "respond_staged") return Promise.resolve(overrides.respondResult ?? []);
    if (cmd === "revoke_approval_rule") return Promise.resolve(true);
    return Promise.resolve();
  });
}

beforeEach(() => invokeMock.mockReset());

test("按应用分组渲染两条同应用不同工具的待批项", async () => {
  mockInvoke({ staged: twoStagedSameAppDiffTool });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  expect(screen.getAllByText(/暂存于 \d+ 分钟前/)).toHaveLength(2);
  expect(invokeMock).toHaveBeenCalledWith("list_staged_calls", { appId: undefined });
});

test("不满一分钟的待批项显示「刚刚暂存」而不是「0 分钟前」", async () => {
  mockInvoke({
    staged: [
      { id: "stg-now", app_id: "notes-writer", server: "fs1", tool: "write_file", args: {}, created_at: Math.floor(Date.now() / 1000) },
    ],
  });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("刚刚暂存")).toBeTruthy());
  expect(screen.queryByText(/0 分钟前/)).toBeNull();
});

test("组级「全部允许」一次性验收该应用下所有待批项", async () => {
  mockInvoke({ staged: twoStagedSameAppDiffTool });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  fireEvent.click(screen.getByText("全部允许"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("respond_staged", { ids: ["stg-1", "stg-2"], allow: true, always: false })
  );
});

test("组级「全部拒绝」一次性拒绝该应用下所有待批项", async () => {
  mockInvoke({ staged: twoStagedSameAppDiffTool });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  fireEvent.click(screen.getByText("全部拒绝"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("respond_staged", { ids: ["stg-1", "stg-2"], allow: false, always: false })
  );
});

test("条级勾选「自动放行」后点允许 → always: true", async () => {
  mockInvoke({ staged: [twoStagedSameAppDiffTool[0]] });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  fireEvent.click(screen.getByLabelText("今后对此应用的这个工具自动放行"));
  fireEvent.click(screen.getByText("允许"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("respond_staged", { ids: ["stg-1"], allow: true, always: true })
  );
});

test("条级「拒绝」不受自动放行勾选影响", async () => {
  mockInvoke({ staged: [twoStagedSameAppDiffTool[0]] });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  fireEvent.click(screen.getByText("拒绝"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("respond_staged", { ids: ["stg-1"], allow: false, always: false })
  );
});

// 终审 Important 4：用户点「允许」，后端因验收前重新鉴权失败而拒绝执行
// （reason: "unauthorized"）——此前界面对这种情形完全静默（只有
// verdict === "error" 才会提示），用户会以为执行成功了。现在respond 之后应出
// 现点名「权限变化」的一行摘要。
test("重新鉴权失败（reason: unauthorized）时面板出现「权限变化」提示", async () => {
  mockInvoke({
    staged: [twoStagedSameAppDiffTool[0]],
    respondResult: [{ id: "stg-1", verdict: "rejected", delivered: false, reason: "unauthorized" }],
  });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  fireEvent.click(screen.getByText("允许"));
  await waitFor(() => expect(screen.getByText(/权限变化/)).toBeTruthy());
  expect(screen.getByText(/权限变化/).textContent).toContain("已执行 0 条");
});

// 用户主动点「拒绝」（reason: "user"）不应触发"权限变化"这句点名，与
// unauthorized 情形区分开。
test("用户主动拒绝（reason: user）不出现「权限变化」措辞", async () => {
  mockInvoke({
    staged: [twoStagedSameAppDiffTool[0]],
    respondResult: [{ id: "stg-1", verdict: "rejected", delivered: true, reason: "user" }],
  });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  fireEvent.click(screen.getByText("拒绝"));
  await waitFor(() => expect(screen.getByText(/已拒绝/)).toBeTruthy());
  expect(screen.queryByText(/权限变化/)).toBeNull();
});

// 组级「全部允许」批准成功后也应出现一行摘要，不只在失败时才有反馈。
test("批量允许全部成功执行后出现「已执行 2 条」摘要", async () => {
  mockInvoke({
    staged: twoStagedSameAppDiffTool,
    respondResult: [
      { id: "stg-1", verdict: "executed", delivered: true, result: {} },
      { id: "stg-2", verdict: "executed", delivered: true, result: {} },
    ],
  });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  fireEvent.click(screen.getByText("全部允许"));
  await waitFor(() => expect(screen.getByText("已执行 2 条")).toBeTruthy());
});

test("参数预览按 JSON.stringify 截断 200 字并加省略号", async () => {
  const longArgs = { path: "x".repeat(220) };
  mockInvoke({
    staged: [{ id: "stg-9", app_id: "notes-writer", server: "fs1", tool: "write_file", args: longArgs, created_at: Math.floor(Date.now() / 1000) }],
  });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("notes-writer")).toBeTruthy());
  const preview = JSON.stringify(longArgs).slice(0, 200) + "…";
  expect(screen.getByText(preview)).toBeTruthy();
});

test("空态显示「没有待批操作」", async () => {
  mockInvoke({ staged: [] });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("没有待批操作")).toBeTruthy());
});

test("已放行规则列表带撤销，点击调用 revoke_approval_rule", async () => {
  mockInvoke({
    staged: [],
    rules: [{ app_id: "notes-writer", server: "fs1", tool: "write_file", created_at: 1000 }],
  });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("已放行规则")).toBeTruthy());
  expect(screen.getByText("write_file")).toBeTruthy();
  fireEvent.click(screen.getByText("撤销"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("revoke_approval_rule", { appId: "notes-writer", server: "fs1", tool: "write_file" })
  );
});

test("invoke 报错时用 String(e) 在面板顶部显示错误", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_staged_calls") return Promise.reject(new Error("拉取失败"));
    return Promise.resolve([]);
  });
  render(<ApprovalCenter />);
  await waitFor(() => expect(screen.getByText("Error: 拉取失败")).toBeTruthy());
});
