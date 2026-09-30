import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { MarketView } from "./MarketView";

// 同 OnboardingWizard.test.tsx：不用 beforeEach(mockReset)——它与"rejected promise
// 模拟失败"组合会触发当前 vitest/RTL 版本的假阳性 unhandled-rejection 判定；改成每个
// test 内联 mockReset() 规避，不影响断言真实性。

const DEMO = [
  {
    name: "@superagent/researcher",
    display_name: "研究员",
    version: "1.0.0",
    category: "automation",
    icon: null,
    description: "研究并精简",
    source: "researcher",
    permissions: ["调用其他应用：@superagent/summarizer"],
    kind: "app",
    download_url: null,
    sha256: null,
    size: null,
    author: null,
  },
  {
    name: "@superagent/summarizer",
    display_name: "精简器",
    version: "1.0.0",
    category: "automation",
    icon: null,
    description: "把文本精简成 3 条要点",
    source: "summarizer",
    permissions: ["仅在自己的数据区内活动，无额外权限"],
    kind: "app",
    download_url: null,
    sha256: null,
    size: null,
    author: null,
  },
];

const SKILL_ENTRY = {
  name: "connector-etiquette",
  display_name: "连接器使用礼仪",
  version: "1.0.0",
  category: "skill",
  icon: null,
  description: "使用已连接的外部服务时应遵循的礼仪与克制原则",
  source: "skills/connector-etiquette",
  permissions: ["仅在自己的数据区内活动，无额外权限"],
  kind: "skill",
  download_url: null,
  sha256: null,
  size: null,
  author: "superagent",
};

test("拉取市场索引并渲染条目卡 + 权限摘要", async () => {
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "market_fetch_index") return Promise.resolve(DEMO);
    return Promise.resolve();
  });
  render(<MarketView />);
  await waitFor(() => expect(screen.getByText("研究员")).toBeTruthy());
  expect(screen.getByText("精简器")).toBeTruthy();
  expect(screen.getByText("调用其他应用：@superagent/summarizer")).toBeTruthy();
});

test("缺省不传 source 拉取（内置 demo）", async () => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(DEMO);
  render(<MarketView />);
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("market_fetch_index", { source: null }),
  );
});

test("点安装调用 install_builtin_sample 传条目的 source（内置样例名）", async () => {
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "market_fetch_index") return Promise.resolve(DEMO);
    return Promise.resolve();
  });
  render(<MarketView />);
  await waitFor(() => expect(screen.getByText("研究员")).toBeTruthy());

  const buttons = screen.getAllByText("安装");
  fireEvent.click(buttons[0]);

  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("install_builtin_sample", { name: "researcher" }),
  );
  // 装完按钮变「已安装」。
  await waitFor(() => expect(screen.getByText("已安装")).toBeTruthy());
});

test("加载失败显示错误态", async () => {
  invokeMock.mockReset();
  invokeMock.mockRejectedValue(new Error("network down"));
  render(<MarketView />);
  await waitFor(() => expect(screen.getByText(/加载市场失败/)).toBeTruthy());
});


test("市场里 kind===\"skill\" 的条目显示「技能」标签，安装走 skill_market_install", async () => {
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "market_fetch_index") return Promise.resolve([SKILL_ENTRY]);
    if (cmd === "skill_market_install") {
      return Promise.resolve({
        meta: {
          id: "connector-etiquette", name: "connector-etiquette", description: "礼仪",
          license: null, compatibility: null, allowed_tools: [], disable_model_invocation: false, has_scripts: false,
        },
        source: { kind: "market", url: null, sha256: null },
        trusted: false,
        installed_at: 0,
        scan: { has_scripts: false, script_files: [], findings: [], total_bytes: 10, file_count: 1 },
      });
    }
    return Promise.resolve();
  });
  render(<MarketView />);
  await waitFor(() => expect(screen.getByText("连接器使用礼仪")).toBeTruthy());
  expect(screen.getByText("技能")).toBeTruthy();

  fireEvent.click(screen.getByText("安装"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("skill_market_install", { entry: SKILL_ENTRY }),
  );
  await waitFor(() => expect(screen.getByText("已安装")).toBeTruthy());
});

test("技能条目装回 scan.has_scripts=true 时额外提示含脚本", async () => {
  invokeMock.mockReset();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "market_fetch_index") return Promise.resolve([SKILL_ENTRY]);
    if (cmd === "skill_market_install") {
      return Promise.resolve({
        meta: {
          id: "connector-etiquette", name: "connector-etiquette", description: "礼仪",
          license: null, compatibility: null, allowed_tools: [], disable_model_invocation: false, has_scripts: true,
        },
        source: { kind: "market", url: null, sha256: null },
        trusted: false,
        installed_at: 0,
        scan: { has_scripts: true, script_files: ["scripts/run.py"], findings: [], total_bytes: 10, file_count: 2 },
      });
    }
    return Promise.resolve();
  });
  render(<MarketView />);
  await waitFor(() => expect(screen.getByText("连接器使用礼仪")).toBeTruthy());
  fireEvent.click(screen.getByText("安装"));
  await waitFor(() => expect(screen.getByText(/含脚本/)).toBeTruthy());
});
