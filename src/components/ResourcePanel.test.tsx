import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import { test, expect, vi, beforeEach, afterEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { ResourcePanel } from "./ResourcePanel";

const MB = 1024 * 1024;
const g = (label: string, procs: number, bytes: number, cpu: number) => ({
  label, root_pids: [1], proc_count: procs, rss_bytes: bytes, cpu_percent: cpu,
});
const app = (id: string, over: Record<string, unknown> = {}) => ({
  app_id: id, usage: g(id, 3, 100 * MB, 2), background_sessions: 0, opened_at: 1000,
  last_activity_at: 1000, idle_secs: 60, in_turn: false, exempt: null, ...over,
});
let report: Record<string, unknown>;
let policy: Record<string, unknown>;
let disk: Record<string, unknown>;
const names = [{ app_id: "alpha", display_name: "阿尔法" }, { app_id: "beta", display_name: "贝塔" }];

beforeEach(() => {
  invokeMock.mockReset();
  report = {
    sampled_at: Math.floor(Date.now() / 1000), cpu_ready: true,
    host: g("host", 3, 200 * MB, 1.5), main: g("main", 2, 310 * MB, 0.5),
    apps: [app("alpha", { in_turn: true, exempt: "正在回复" }), app("beta", { idle_secs: 600 })],
    mcp_servers: [g("fs", 1, 50 * MB, 0)], other: g("other", 0, 0, 0),
    total_rss_bytes: 1024 * MB, total_cpu_percent: 4, total_proc_count: 9,
  };
  policy = { enabled: true, timeout_secs: 900, exempt_apps: [] };
  disk = {
    root_bytes: 2 * 1024 * MB, audit_bytes: MB, notifications_bytes: MB, maker_staging_bytes: MB,
    main_sessions_bytes: MB, incomplete: false, threshold_bytes: 200 * MB,
    apps: [{ app_id: "alpha", sessions_bytes: 300 * MB, data_bytes: MB, agent_home_bytes: MB, sessions_over_threshold: true, incomplete: false }, { app_id: "beta", sessions_bytes: MB, data_bytes: MB, agent_home_bytes: MB, sessions_over_threshold: false, incomplete: true }],
  };
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "resource_report") return Promise.resolve(report);
    if (cmd === "get_idle_policy") return Promise.resolve(policy);
    if (cmd === "disk_report") return Promise.resolve(disk);
    if (cmd === "clear_caches") return Promise.resolve({ freed_bytes: 5 * MB, removed_files: 2, removed_drafts: 1, kept_active: [], refused: [] });
    return Promise.resolve();
  });
});
afterEach(() => vi.useRealTimers());

const count = (cmd: string) => invokeMock.mock.calls.filter((c) => c[0] === cmd).length;

test("渲染汇总与宿主/主助手行", async () => {
  render(<ResourcePanel apps={names} />);
  await waitFor(() => expect(screen.getByText("1.0 GB")).toBeTruthy());
  expect(screen.getByText("宿主")).toBeTruthy();
  expect(screen.getByText("200.0 MB")).toBeTruthy();
  expect(screen.getByText("主助手")).toBeTruthy();
  expect(screen.getByText("310.0 MB")).toBeTruthy();
});

test("cpu_ready=false 时 CPU 显示「—」", async () => {
  report.cpu_ready = false;
  render(<ResourcePanel apps={names} />);
  await waitFor(() => expect(screen.getByText("1.0 GB")).toBeTruthy());
  expect(screen.queryByText("4.0%")).toBeNull();
  expect(screen.getAllByText("—").length).toBeGreaterThan(0);
});

test("应用行显示 exempt 文案、回复中与休眠倒计时，内存占比注明占总内存", async () => {
  report.apps = [app("alpha", { in_turn: true, exempt: "正在回复" }), app("beta", { idle_secs: 600 }), app("gamma", { exempt: "后台任务运行中" })];
  render(<ResourcePanel apps={names} />);
  await waitFor(() => expect(screen.getByText("回复中")).toBeTruthy());
  expect(screen.getByText("后台任务运行中")).toBeTruthy();
  expect(screen.getByText("约 5 分钟后休眠")).toBeTruthy();
  expect(screen.getAllByText(/占总内存 \d+%/).length).toBe(3);
});

test("每 5 秒轮询一次，卸载后停止", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: false });
  const { unmount } = render(<ResourcePanel apps={names} />);
  await act(async () => { await Promise.resolve(); });
  expect(count("resource_report")).toBe(1);
  await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
  expect(count("resource_report")).toBe(2);
  unmount();
  await act(async () => { await vi.advanceTimersByTimeAsync(15000); });
  expect(count("resource_report")).toBe(2);
});

test("「不休眠」开关调用 set_idle_policy，exempt_apps 含该应用", async () => {
  render(<ResourcePanel apps={names} />);
  const box = await screen.findByLabelText("贝塔 不休眠");
  await waitFor(() => expect((box as HTMLInputElement).disabled).toBe(false));
  fireEvent.click(box);
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_idle_policy", {
      policy: { enabled: true, timeout_secs: 900, exempt_apps: ["beta"] },
    }),
  );
});

test("阈值 3 分钟时保存禁用；20 分钟保存传 timeout_secs: 1200", async () => {
  render(<ResourcePanel apps={names} />);
  const input = await screen.findByLabelText("空闲阈值（分钟）");
  await waitFor(() => expect((input as HTMLInputElement).value).toBe("15"));
  fireEvent.change(input, { target: { value: "3" } });
  const save = screen.getByText("保存") as HTMLButtonElement;
  expect(save.disabled).toBe(true);
  expect(screen.getByText(/5–1440/)).toBeTruthy();
  fireEvent.change(input, { target: { value: "20" } });
  expect(save.disabled).toBe(false);
  fireEvent.click(save);
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("set_idle_policy", {
      policy: { enabled: true, timeout_secs: 1200, exempt_apps: [] },
    }),
  );
});

test("超阈值磁盘项显示提示；incomplete 显示统计不完整", async () => {
  disk.incomplete = true;
  render(<ResourcePanel apps={names} />);
  await waitFor(() => expect(screen.getByText("会话文件超过 200 MB")).toBeTruthy());
  expect(screen.getByText(/部分目录层级过深/)).toBeTruthy();
  // 应用行：超阈值与「统计不完整」是两条不同的提示
  expect(screen.getAllByText("目录层级过深，统计不完整")).toHaveLength(1);
  expect(screen.getAllByText("会话文件超过 200 MB")).toHaveLength(1);
});

test("清理缓存需二次确认，确认后调 clear_caches 并显示释放量", async () => {
  render(<ResourcePanel apps={names} />);
  await waitFor(() => expect(screen.getByText("会话文件超过 200 MB")).toBeTruthy());
  fireEvent.click(screen.getByText("清理缓存"));
  expect(count("clear_caches")).toBe(0);
  expect(screen.getByText(/只删旧的会话记录与无主草稿/)).toBeTruthy();
  fireEvent.click(screen.getByText("确认清理"));
  await waitFor(() => expect(screen.getByText(/已释放 5.0 MB/)).toBeTruthy());
  expect(count("clear_caches")).toBe(1);
  await waitFor(() => expect(count("disk_report")).toBe(2));
});

test("清理结果 refused 非空时说明有几项未处理", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "resource_report") return Promise.resolve(report);
    if (cmd === "get_idle_policy") return Promise.resolve(policy);
    if (cmd === "disk_report") return Promise.resolve(disk);
    if (cmd === "clear_caches") return Promise.resolve({ freed_bytes: 0, removed_files: 0, removed_drafts: 0, kept_active: [], refused: ["sessions/x", "maker-staging"] });
    return Promise.resolve();
  });
  render(<ResourcePanel apps={names} />);
  await waitFor(() => expect(screen.getByText("清理缓存")).toBeTruthy());
  fireEvent.click(screen.getByText("清理缓存"));
  fireEvent.click(screen.getByText("确认清理"));
  await waitFor(() => expect(screen.getByText(/另有 2 项/)).toBeTruthy());
});

test("「关闭」调用 onCloseApp(appId)", async () => {
  const onCloseApp = vi.fn();
  render(<ResourcePanel apps={names} onCloseApp={onCloseApp} />);
  await waitFor(() => expect(screen.getAllByText("阿尔法").length).toBeGreaterThan(0));
  fireEvent.click(screen.getAllByText("关闭")[0]);
  await waitFor(() => expect(onCloseApp).toHaveBeenCalledWith("alpha"));
});

test("没有打开的应用时显示空状态", async () => {
  report.apps = [];
  render(<ResourcePanel apps={names} />);
  await waitFor(() => expect(screen.getByText("当前没有打开的应用")).toBeTruthy());
});
