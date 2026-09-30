import { invoke } from "@tauri-apps/api/core";

/**
 * 一个已配置的 MCP server。凭据（`env` 里的密钥等）只落 OS 钥匙串
 * （`src-tauri/src/vault.rs`），从不落 registry/文件/日志。与 Rust
 * `vault::ServerConfig`（Serialize/Deserialize，无 rename）对齐，序列化即
 * snake_case。`category` 供连接器按 app 清单里的 `connectors[].category`
 * 匹配可见性（见 Task6）；`transport` 当前固定 `"stdio"`（vault.rs 文档）。
 */
export type ServerConfig = {
  id: string;
  category: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  transport: string;
};

/**
 * 调用后端 `list_servers` 列出所有已配置的 MCP server（`vault::list_servers`
 * 的薄 Tauri 命令封装——该自由函数本身在 Task1 已实现+测试，见其文档）。
 */
export function listServers(): Promise<ServerConfig[]> {
  return invoke("list_servers");
}

/** 调用后端 `put_server` 新增/更新一个 MCP server 配置（按 `id` upsert）。 */
export function putServer(config: ServerConfig): Promise<void> {
  return invoke("put_server", { config });
}

/** 调用后端 `delete_server` 按 id 删除一个 MCP server 配置（幂等）。 */
export function deleteServer(id: string): Promise<void> {
  return invoke("delete_server", { id });
}
