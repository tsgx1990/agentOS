import "./styles/tokens.css";
import { Shell } from "./components/Shell";

/**
 * 首次启动检测（BYOK key 是否已配置）与对应的引导向导挂载点都收拢进
 * `Shell`（见其文档注释与 Task12），这里不再重复检测——避免旧版本
 * `App.tsx` 自己查一遍 `has_api_key` + `Shell` 又查一遍的重复调用。
 */
export default function App() {
  return <Shell />;
}
