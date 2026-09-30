import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { InstallDialog } from "./InstallDialog";

beforeEach(() => invokeMock.mockReset());

test("预览权限后安装", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "preview_install") return Promise.resolve({
      display_name: "待办便签", category: "life",
      permissions: ["写入文件：$APP_DATA"], existing_version: null,
      capabilities: [
        { key: "filesystem", declared: true, human: ["写入文件：$APP_DATA"], enforcement: ["sandbox"], tools: [] },
      ],
      sandboxed: true,
    });
    if (cmd === "install_app") return Promise.resolve({ app_id: "x" });
    return Promise.resolve();
  });
  const onDone = vi.fn();
  render(<InstallDialog onDone={onDone} onCancel={() => {}} />);
  fireEvent.change(screen.getByPlaceholderText(/应用文件夹路径/), { target: { value: "/pkgs/todo" } });
  fireEvent.click(screen.getByText("预览"));
  await waitFor(() => expect(screen.getByText(/写入文件/)).toBeTruthy());
  fireEvent.click(screen.getByText("安装"));
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("install_app", { sourcePath: "/pkgs/todo", trusted: false }));
  await waitFor(() => expect(onDone).toHaveBeenCalled());
});
