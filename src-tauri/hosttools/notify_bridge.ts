// 宿主工具：应用向通知中心投递一条通知（P6-A notifications 能力）。
// 只在清单声明 system.notifications 时被宿主注入；宿主侧 CapabilityRegistry::dispatch 再次校验声明，
// 本文件不做任何授权判断（宿主是唯一执行点）。
import { Type } from "typebox";
import { hostCall } from "./mcp_transport";

const NOTIFY = "__host_notify__";

export default function (pi: any) {
  pi.registerTool({
    name: NOTIFY,
    label: "发送通知",
    description: "向用户的通知中心发送一条通知（标题 ≤200 字，正文 ≤2000 字，每分钟最多 10 条）。",
    parameters: Type.Object({ title: Type.String(), body: Type.String() }),
    async execute(_id: string, params: unknown) {
      try {
        return await hostCall(NOTIFY, params);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        return { ok: false, error: `通知发送传输失败：${message}` };
      }
    },
  });
}
