import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { test, expect, vi } from "vitest";
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
import { PublishButton } from "./PublishButton";

// 同 MarketView/OnboardingWizard：不用 beforeEach(mockReset)，每个 test 内联 reset，
// 规避 rejected-promise 组合触发的假阳性 unhandled-rejection 判定。

const ENTRY = {
  name: "@superagent/researcher",
  display_name: "研究员",
  version: "1.0.0",
  category: "automation",
  icon: null,
  description: "",
  source: "superagent__researcher",
  permissions: ["调用其他应用：@superagent/summarizer"],
};

test("点发布调用 publish_app(appId) 并提示产出", async () => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(ENTRY);
  render(<PublishButton appId="superagent__researcher" />);
  fireEvent.click(screen.getByText("发布"));

  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("publish_app", { appId: "superagent__researcher" }),
  );
  await waitFor(() => expect(screen.getByText(/已导出「研究员」/)).toBeTruthy());
  expect(screen.getByText(/推到你的市场仓库/)).toBeTruthy();
});

test("发布失败展示错误", async () => {
  invokeMock.mockReset();
  invokeMock.mockRejectedValue(new Error("应用未安装"));
  render(<PublishButton appId="superagent__x" />);
  fireEvent.click(screen.getByText("发布"));
  await waitFor(() => expect(screen.getByText(/发布失败/)).toBeTruthy());
});
