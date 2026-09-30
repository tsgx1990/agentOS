// 测试专用：在 session_start 上报 getAllTools/getActiveTools 的名字（P6-A --tools 白名单回归）。
export default function (pi: any) {
  pi.on("session_start", async (_e: any, ctx: any) => {
    const names = (xs: any[]) => xs.map((t) => (typeof t === "string" ? t : t.name));
    ctx.ui.notify("ACTIVE_TOOLS_PROBE:" + JSON.stringify({ all: names(pi.getAllTools()), active: names(pi.getActiveTools()) }), "info");
  });
}
