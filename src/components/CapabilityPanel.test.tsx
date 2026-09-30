import { render, screen, waitFor } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { CapabilityPanel } from "./CapabilityPanel";
import type { CapabilityReport } from "../lib/registry";

const reports: CapabilityReport[] = [
  { key: "connectors", declared: true, human: ["使用文件系统连接器（读写，写操作需你确认）"], enforcement: ["launch", "host_method"], tools: ["mcp__fs1__read_file"], error: null },
  { key: "filesystem", declared: true, human: ["读取：下载文件夹"], enforcement: ["sandbox"], tools: [], error: null },
  { key: "system.notifications", declared: false, human: [], enforcement: ["launch", "host_method"], tools: [], error: null },
];

describe("CapabilityPanel", () => {
  it("renders only declared capabilities with enforcement chips", () => {
    render(<CapabilityPanel reports={reports} sandboxed={true} />);
    expect(screen.getByText("使用文件系统连接器（读写，写操作需你确认）")).toBeTruthy();
    expect(screen.getByText("读取：下载文件夹")).toBeTruthy();
    expect(screen.getAllByText("宿主复核")).toHaveLength(1);
    expect(screen.getByText("OS 沙盒")).toBeTruthy();
    expect(screen.queryByText("向你发送通知")).toBeNull();
  });
  it("shows platform hint when sandbox unavailable", () => {
    render(<CapabilityPanel reports={reports} sandboxed={false} />);
    expect(screen.getByText(/本平台无硬沙盒/)).toBeTruthy();
  });
  it("shows fallback when nothing declared", () => {
    render(<CapabilityPanel reports={[reports[2]]} sandboxed={true} />);
    expect(screen.getByText("仅在自己的数据区内活动，无额外权限")).toBeTruthy();
  });
  it("renders a capability's error line when launch() failed (F1)", () => {
    const withError: CapabilityReport = {
      key: "filesystem", declared: true, human: ["修改：桌面/导出"], enforcement: ["sandbox"], tools: [], error: "路径逃逸",
    };
    render(<CapabilityPanel reports={[withError]} sandboxed={true} />);
    expect(screen.getByText("路径逃逸")).toBeTruthy();
  });

  it("传入 appId 时拉取 skill_grants 并显示「已授予 N 个（M 个启用）」", async () => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "skill_grants") {
        return Promise.resolve([
          [{ meta: { id: "s1", name: "s1", description: "", license: null, compatibility: null, allowed_tools: [], disable_model_invocation: false, has_scripts: false }, source: { kind: "local", url: null, sha256: null }, trusted: true, installed_at: 0, scan: { has_scripts: false, script_files: [], findings: [], total_bytes: 0, file_count: 0 } }, true],
          [{ meta: { id: "s2", name: "s2", description: "", license: null, compatibility: null, allowed_tools: [], disable_model_invocation: false, has_scripts: false }, source: { kind: "local", url: null, sha256: null }, trusted: true, installed_at: 0, scan: { has_scripts: false, script_files: [], findings: [], total_bytes: 0, file_count: 0 } }, false],
        ]);
      }
      return Promise.resolve();
    });
    render(<CapabilityPanel reports={[]} sandboxed={true} appId="notes" />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("skill_grants", { appId: "notes" }));
    await waitFor(() => expect(screen.getByText("已授予 2 个（1 个启用）")).toBeTruthy());
  });

  it("不传 appId 时不拉取 skill_grants、不渲染技能行", async () => {
    invokeMock.mockReset();
    render(<CapabilityPanel reports={reports} sandboxed={true} />);
    expect(invokeMock).not.toHaveBeenCalled();
    expect(screen.queryByText(/已授予/)).toBeNull();
  });
});
