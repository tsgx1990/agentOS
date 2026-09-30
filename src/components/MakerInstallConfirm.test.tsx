import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi, beforeEach } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { MakerInstallConfirm } from "./MakerInstallConfirm";

beforeEach(() => invokeMock.mockReset());

test("渲染一条 pending install（app 名 + 权限预览）", async () => {
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_pending_installs") return Promise.resolve([
      { confirm_id: "confirm-1", display_name: "Maker示例", permissions: ["读取文件：$DOWNLOADS", "发送系统通知"] },
    ]);
    return Promise.resolve();
  });
  render(<MakerInstallConfirm />);
  await waitFor(() => expect(screen.getByText("Maker示例")).toBeTruthy());
  expect(screen.getByText("读取文件：$DOWNLOADS")).toBeTruthy();
  expect(screen.getByText("发送系统通知")).toBeTruthy();
  expect(invokeMock).toHaveBeenCalledWith("list_pending_installs");
});

test("无 pending install 时不渲染任何内容", async () => {
  invokeMock.mockResolvedValue([]);
  const { container } = render(<MakerInstallConfirm />);
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("list_pending_installs"));
  expect(container.firstChild).toBeNull();
});

test("点击「批准」调用 maker_respond_install_confirm(allow:true) 并刷新列表", async () => {
  let resolved = false;
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_pending_installs") {
      return Promise.resolve(
        resolved ? [] : [{ confirm_id: "confirm-1", display_name: "Maker示例", permissions: ["仅在自己的数据区内活动，无额外权限"] }],
      );
    }
    if (cmd === "maker_respond_install_confirm") {
      resolved = true;
      return Promise.resolve({ app_id: "superagent__maker-demo" });
    }
    return Promise.resolve();
  });
  render(<MakerInstallConfirm />);
  await waitFor(() => expect(screen.getByText("Maker示例")).toBeTruthy());

  fireEvent.click(screen.getByText("批准"));

  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("maker_respond_install_confirm", { confirmId: "confirm-1", allow: true }));
  await waitFor(() => expect(screen.queryByText("Maker示例")).toBeNull());
});

test("点击「拒绝」调用 maker_respond_install_confirm(allow:false) 并刷新列表", async () => {
  let resolved = false;
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_pending_installs") {
      return Promise.resolve(
        resolved ? [] : [{ confirm_id: "confirm-1", display_name: "Maker示例", permissions: ["仅在自己的数据区内活动，无额外权限"] }],
      );
    }
    if (cmd === "maker_respond_install_confirm") {
      resolved = true;
      return Promise.resolve(null);
    }
    return Promise.resolve();
  });
  render(<MakerInstallConfirm />);
  await waitFor(() => expect(screen.getByText("Maker示例")).toBeTruthy());

  fireEvent.click(screen.getByText("拒绝"));

  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("maker_respond_install_confirm", { confirmId: "confirm-1", allow: false }));
  await waitFor(() => expect(screen.queryByText("Maker示例")).toBeNull());
});
