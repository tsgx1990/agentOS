// 宿主工具：向宿主 UI 推送结构化事件，供界面按 contract.json 渲染。
// 当前为占位实现，仅返回 "ok"，不做实际事件分发。
import { Type } from "typebox";
export default function (pi: any) {
  pi.registerTool({
    name: "__host_ui_emit__",
    label: "UI Emit",
    description: "向宿主界面推送一个结构化事件（event 名 + payload）。界面据 contract.json 渲染。",
    parameters: Type.Object({ event: Type.String(), payload: Type.Any() }),
    async execute(_id: string, _params: unknown) {
      return { content: [{ type: "text", text: "ok" }], details: {} };
    },
  });
}
