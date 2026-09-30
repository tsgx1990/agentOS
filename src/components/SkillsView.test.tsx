import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { SkillsView } from "./SkillsView";

const SKILL_A = {
  meta: {
    id: "connector-etiquette", name: "connector-etiquette", description: "连接器礼仪",
    license: null, compatibility: null, allowed_tools: ["mcp__fs1__read_file"],
    disable_model_invocation: false, has_scripts: false,
  },
  source: { kind: "builtin", url: null, sha256: null },
  trusted: true,
  installed_at: 100,
  scan: { has_scripts: false, script_files: [], findings: [], total_bytes: 500, file_count: 2 },
};

const APP_A = {
  app_id: "notes", name: "notes", version: "1.0.0", display_name: "笔记",
  category: "life", icon: null, trusted: true, domains: [],
};

const APP_B = {
  app_id: "docs", name: "docs", version: "1.0.0", display_name: "文档",
  category: "life", icon: null, trusted: true, domains: [],
};

const MARKET_APP_ENTRY = {
  name: "@superagent/researcher", display_name: "研究员", version: "1.0.0", category: "automation",
  icon: null, description: "研究员", source: "researcher", permissions: [], kind: "app",
  download_url: null, sha256: null, size: null, author: null,
};

const MARKET_SKILL_ENTRY = {
  name: "connector-etiquette-market", display_name: "连接器礼仪（市场版）", version: "1.0.0", category: "skill",
  icon: null, description: "市场技能条目", source: "skills/connector-etiquette", permissions: [], kind: "skill",
  download_url: null, sha256: null, size: null, author: "superagent",
};

function baseInvoke(overrides: Record<string, unknown> = {}) {
  return (cmd: string) => {
    if (cmd in overrides) return Promise.resolve(overrides[cmd]);
    if (cmd === "list_skills") return Promise.resolve([]);
    if (cmd === "list_apps") return Promise.resolve([]);
    if (cmd === "skill_grants") return Promise.resolve([]);
    if (cmd === "market_fetch_index") return Promise.resolve([]);
    return Promise.resolve();
  };
}

beforeEach(() => invokeMock.mockReset());

test("没有已装技能时显示空态文案", async () => {
  invokeMock.mockImplementation(baseInvoke());
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("还没有安装技能")).toBeTruthy());
});

test("渲染已装技能：名称/描述/来源标签/信任标签/扫描发现数", async () => {
  invokeMock.mockImplementation(baseInvoke({ list_skills: [SKILL_A] }));
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("connector-etiquette")).toBeTruthy());
  expect(screen.getByText("连接器礼仪")).toBeTruthy();
  expect(screen.getByText("内置")).toBeTruthy();
  expect(screen.getByText("受信")).toBeTruthy();
  expect(screen.getByText(/扫描发现 0 项/)).toBeTruthy();
});

test("市场区只显示 kind===\"skill\" 的条目，市场没有技能时显示空态", async () => {
  invokeMock.mockImplementation(baseInvoke({ market_fetch_index: [MARKET_APP_ENTRY] }));
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("市场里暂无技能")).toBeTruthy());
  expect(screen.queryByText("研究员")).toBeNull();
});

test("市场区点安装调用 skill_market_install", async () => {
  invokeMock.mockImplementation(baseInvoke({ market_fetch_index: [MARKET_SKILL_ENTRY] }));
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("连接器礼仪（市场版）")).toBeTruthy());
  const marketSection = screen.getByTestId("skills-market-section");
  fireEvent.click(within(marketSection).getByText("安装"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("skill_market_install", { entry: MARKET_SKILL_ENTRY }),
  );
});

test("本地导入：预览调用 preview_skill，成功后打开安装确认对话框", async () => {
  invokeMock.mockImplementation(
    baseInvoke({ preview_skill: { meta: SKILL_A.meta, scan: SKILL_A.scan, trusted: false } }),
  );
  render(<SkillsView />);
  const input = screen.getByPlaceholderText("技能目录路径");
  fireEvent.change(input, { target: { value: "/tmp/my-skill" } });
  fireEvent.click(screen.getByText("预览"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("preview_skill", { path: "/tmp/my-skill" }));
  await waitFor(() => expect(screen.getByText("安装技能确认")).toBeTruthy());
});

test("本地导入对话框确认安装调用 install_skill_from_path 并刷新已装列表", async () => {
  invokeMock.mockImplementation(
    baseInvoke({
      preview_skill: { meta: SKILL_A.meta, scan: SKILL_A.scan, trusted: false },
      install_skill_from_path: SKILL_A,
    }),
  );
  render(<SkillsView />);
  fireEvent.change(screen.getByPlaceholderText("技能目录路径"), { target: { value: "/tmp/my-skill" } });
  fireEvent.click(screen.getByText("预览"));
  await waitFor(() => expect(screen.getByText("安装技能确认")).toBeTruthy());
  fireEvent.click(screen.getByText("安装"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("install_skill_from_path", { path: "/tmp/my-skill" }),
  );
  await waitFor(() => expect(screen.queryByText("安装技能确认")).toBeNull());
});

test("勾选「授予给」某应用调用 grant_skill，取消勾选调用 revoke_skill", async () => {
  invokeMock.mockImplementation(baseInvoke({ list_skills: [SKILL_A], list_apps: [APP_A] }));
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("connector-etiquette")).toBeTruthy());
  const grantBox = await screen.findByRole("checkbox", { name: "授予给笔记" });
  fireEvent.click(grantBox);
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("grant_skill", { appId: "notes", skillId: "connector-etiquette" }),
  );
});

test("授予失败时原样显示后端错误", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_skills") return Promise.resolve([SKILL_A]);
    if (cmd === "list_apps") return Promise.resolve([APP_A]);
    if (cmd === "skill_grants") return Promise.resolve([]);
    if (cmd === "market_fetch_index") return Promise.resolve([]);
    if (cmd === "grant_skill") return Promise.reject(new Error("技能需要工具 [\"x\"]，目标应用未获得"));
    return Promise.resolve();
  });
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("connector-etiquette")).toBeTruthy());
  const grantBox = await screen.findByRole("checkbox", { name: "授予给笔记" });
  fireEvent.click(grantBox);
  await waitFor(() => expect(screen.getByText(/目标应用未获得/)).toBeTruthy());
});

test("点「卸载」调用 uninstall_skill", async () => {
  invokeMock.mockImplementation(baseInvoke({ list_skills: [SKILL_A], uninstall_skill: true }));
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("connector-etiquette")).toBeTruthy());
  fireEvent.click(screen.getByText("卸载"));
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("uninstall_skill", { id: "connector-etiquette" }),
  );
});

test("已授予时显示启用开关（带可见文字标签「启用」），切换调用 set_skill_enabled", async () => {
  invokeMock.mockImplementation(
    baseInvoke({ list_skills: [SKILL_A], list_apps: [APP_A], skill_grants: [[SKILL_A, true]] }),
  );
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("connector-etiquette")).toBeTruthy());
  expect(screen.getByText("启用")).toBeTruthy();
  const enableToggle = await screen.findByRole("checkbox", { name: "笔记已启用" });
  fireEvent.click(enableToggle);
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_skill_enabled", {
      appId: "notes", skillId: "connector-etiquette", enabled: false,
    }),
  );
});

test("两行并发授予操作互不清空对方的 busy 态", async () => {
  let resolveA!: () => void;
  let resolveB!: () => void;
  const pA = new Promise<void>((r) => { resolveA = r; });
  const pB = new Promise<void>((r) => { resolveB = r; });
  invokeMock.mockImplementation((cmd: string, args?: { appId?: string }) => {
    if (cmd === "list_skills") return Promise.resolve([SKILL_A]);
    if (cmd === "list_apps") return Promise.resolve([APP_A, APP_B]);
    if (cmd === "skill_grants") return Promise.resolve([]);
    if (cmd === "market_fetch_index") return Promise.resolve([]);
    if (cmd === "grant_skill") {
      if (args?.appId === "notes") return pA.then(() => undefined);
      if (args?.appId === "docs") return pB.then(() => undefined);
    }
    return Promise.resolve();
  });
  render(<SkillsView />);
  await waitFor(() => expect(screen.getByText("connector-etiquette")).toBeTruthy());
  const boxA = await screen.findByRole("checkbox", { name: "授予给笔记" });
  const boxB = await screen.findByRole("checkbox", { name: "授予给文档" });

  fireEvent.click(boxA);
  fireEvent.click(boxB);
  await waitFor(() => expect((boxA as HTMLInputElement).disabled).toBe(true));
  expect((boxB as HTMLInputElement).disabled).toBe(true);

  resolveA();
  await waitFor(() => expect((boxA as HTMLInputElement).disabled).toBe(false));
  // A 的请求已经完成，但 B 仍在途——不该被 A 的 finally 连带清空。
  expect((boxB as HTMLInputElement).disabled).toBe(true);

  resolveB();
  await waitFor(() => expect((boxB as HTMLInputElement).disabled).toBe(false));
});


test("本地导入「预览」进行中禁用按钮并显示「预览中…」", async () => {
  let resolvePreview!: (v: unknown) => void;
  const pending = new Promise((r) => { resolvePreview = r; });
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_skills") return Promise.resolve([]);
    if (cmd === "list_apps") return Promise.resolve([]);
    if (cmd === "skill_grants") return Promise.resolve([]);
    if (cmd === "market_fetch_index") return Promise.resolve([]);
    if (cmd === "preview_skill") return pending;
    return Promise.resolve();
  });
  render(<SkillsView />);
  fireEvent.change(screen.getByPlaceholderText("技能目录路径"), { target: { value: "/tmp/my-skill" } });
  const previewBtn = screen.getByText("预览");
  fireEvent.click(previewBtn);
  await waitFor(() => expect(screen.getByText("预览中…")).toBeTruthy());
  expect((screen.getByText("预览中…") as HTMLButtonElement).disabled).toBe(true);

  resolvePreview({
    meta: {
      id: "connector-etiquette", name: "connector-etiquette", description: "连接器礼仪",
      license: null, compatibility: null, allowed_tools: [], disable_model_invocation: false, has_scripts: false,
    },
    scan: { has_scripts: false, script_files: [], findings: [], total_bytes: 10, file_count: 1 },
    trusted: false,
  });
  await waitFor(() => expect(screen.getByText("安装技能确认")).toBeTruthy());
  expect(screen.getByText("预览")).toBeTruthy();
});