import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { SkillPendingConfirm } from "./SkillPendingConfirm";

const PENDING = {
  confirm_id: "c1",
  meta: {
    id: "auto-note", name: "auto-note", description: "自动记笔记",
    license: null, compatibility: null, allowed_tools: ["mcp__fs1__write_file"],
    disable_model_invocation: false, has_scripts: false,
  },
  scan: { has_scripts: false, script_files: [], findings: [], total_bytes: 50, file_count: 1 },
};

beforeEach(() => invokeMock.mockReset());

test("拉取 list_pending_skill_installs 并渲染待确认技能", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_pending_skill_installs") return Promise.resolve([PENDING]);
    return Promise.resolve();
  });
  render(<SkillPendingConfirm />);
  await waitFor(() => expect(screen.getByText("auto-note")).toBeTruthy());
});

test("无 pending 项时不渲染任何内容", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_pending_skill_installs") return Promise.resolve([]);
    return Promise.resolve();
  });
  const { container } = render(<SkillPendingConfirm />);
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("list_pending_skill_installs"));
  expect(container.firstChild).toBeNull();
});

test("点「批准」调用 skill_respond_install_confirm(confirmId, true) 并刷新列表", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_pending_skill_installs") return Promise.resolve([PENDING]);
    if (cmd === "skill_respond_install_confirm") return Promise.resolve(null);
    return Promise.resolve();
  });
  render(<SkillPendingConfirm />);
  await waitFor(() => expect(screen.getByText("auto-note")).toBeTruthy());
  fireEvent.click(screen.getByText("批准"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("skill_respond_install_confirm", { confirmId: "c1", allow: true }),
  );
});

test("点「拒绝」调用 skill_respond_install_confirm(confirmId, false)", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_pending_skill_installs") return Promise.resolve([PENDING]);
    if (cmd === "skill_respond_install_confirm") return Promise.resolve(null);
    return Promise.resolve();
  });
  render(<SkillPendingConfirm />);
  await waitFor(() => expect(screen.getByText("auto-note")).toBeTruthy());
  fireEvent.click(screen.getByText("拒绝"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("skill_respond_install_confirm", { confirmId: "c1", allow: false }),
  );
});
