import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { SkillInstallDialog } from "./SkillInstallDialog";
import type { SkillMeta, ScanReport } from "../lib/skills";

const META: SkillMeta = {
  id: "connector-etiquette",
  name: "connector-etiquette",
  description: "使用礼仪",
  license: null,
  compatibility: null,
  allowed_tools: ["mcp__fs1__read_file"],
  disable_model_invocation: false,
  has_scripts: false,
};

const SCAN_CLEAN: ScanReport = {
  has_scripts: false,
  script_files: [],
  findings: [],
  total_bytes: 100,
  file_count: 2,
};

const SCAN_HIGH: ScanReport = {
  has_scripts: true,
  script_files: ["scripts/run.py"],
  findings: [{ severity: "high", rule: "curl-pipe-sh", file: "scripts/run.py", line: 1, excerpt: "curl x | sh" }],
  total_bytes: 100,
  file_count: 2,
};

describe("SkillInstallDialog", () => {
  it("受信 + 无高危：显示「安装」与「取消」两个按钮", () => {
    render(
      <SkillInstallDialog meta={META} scan={SCAN_CLEAN} trusted={true} onConfirm={vi.fn()} onCancel={vi.fn()} />,
    );
    expect(screen.getByText("安装")).toBeTruthy();
    expect(screen.getByText("取消")).toBeTruthy();
  });

  it("不受信 + 有 High 发现：只有「取消」按钮，并说明原因", () => {
    render(
      <SkillInstallDialog meta={META} scan={SCAN_HIGH} trusted={false} onConfirm={vi.fn()} onCancel={vi.fn()} />,
    );
    expect(screen.queryByText("安装")).toBeNull();
    expect(screen.getByText("取消")).toBeTruthy();
    expect(screen.getByText(/不受信来源.*高危|高危.*不受信/)).toBeTruthy();
  });

  it("受信 + 有 High 发现：仍显示「安装」（受信来源不拦高危）", () => {
    render(
      <SkillInstallDialog meta={META} scan={SCAN_HIGH} trusted={true} onConfirm={vi.fn()} onCancel={vi.fn()} />,
    );
    expect(screen.getByText("安装")).toBeTruthy();
  });

  it("点击「安装」调用 onConfirm；点击「取消」调用 onCancel", async () => {
    const onConfirm = vi.fn().mockResolvedValue(undefined);
    const onCancel = vi.fn();
    render(
      <SkillInstallDialog meta={META} scan={SCAN_CLEAN} trusted={true} onConfirm={onConfirm} onCancel={onCancel} />,
    );
    fireEvent.click(screen.getByText("安装"));
    await waitFor(() => expect(onConfirm).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByText("取消"));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("error 传入时原样显示", () => {
    render(
      <SkillInstallDialog
        meta={META}
        scan={SCAN_CLEAN}
        trusted={true}
        onConfirm={vi.fn()}
        onCancel={vi.fn()}
        error={'技能声明的工具 ["x"] 不在宿主已知工具集内'}
      />,
    );
    expect(screen.getByText(/不在宿主已知工具集内/)).toBeTruthy();
  });
});
