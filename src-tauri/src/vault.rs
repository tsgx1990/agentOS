use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const SERVICE: &str = "super-agent-os";
const INDEX_KEY: &str = "mcp-server/__index__";

/// 连接器信任分级（P6-C）：`Byo`（用户自填/bring-your-own，缺省）——
/// `mcp::classify_tool_with_trust` 对这类 server 的注解只看写指示，读指示一律
/// 忽略、落回前缀表，防恶意/不可信 server 自称 `readOnlyHint=true` 把未知写
/// 工具伪装成只读绕过写确认；`Vetted`——内置精选连接器（本轮列表为空，不开放
/// 用户手选，见 spec §8 裁决 3），沿用旧版"注解优先、可降危"规则。缺省
/// `Byo`：老配置（序列化时没有 `trust` 字段）反序列化后按最不信任处理，
/// 向后兼容且 fail-closed。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Trust {
    #[default]
    Byo,
    Vetted,
}

/// MCP server 配置（含密钥）。凭据只存 OS 钥匙串，绝不落盘文件/日志。
/// category 供 Task6 按连接器类别匹配授权；当前 transport 固定 "stdio"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub id: String,
    pub category: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub transport: String,
    /// 信任分级（P6-C）；缺省 `Byo`，见 `Trust` 文档。
    #[serde(default)]
    pub trust: Trust,
}

/// 凭据保管箱：按 keychain **service 名**隔离命名空间。
///
/// 生产用固定 service（`SERVICE`），行为与本结构体引入前完全一致。测试用
/// `with_service` 传入一个"本次 `cargo test` 运行独有"的 service 名（含 pid +
/// 纳秒时间戳，而非编译期常量），使测试的索引条目（`mcp-server/__index__`）
/// 也随之落在一个全新命名空间下，不会与任何上一次编译产物写入的旧条目共享同
/// 一个 keychain (service, account) 二元组。这从根上避免了"重签名后的 debug
/// 二进制读到上一次编译写入的共享索引条目 → macOS 弹交互式授权框 → headless
/// 环境下 `cargo test` 无限挂起"这一环境性 hazard（详见 HANDOFF.md）。
struct Vault {
    service: String,
}

impl Vault {
    /// 生产用保管箱：固定 service 名，与引入本结构体前的自由函数行为完全一致。
    fn production() -> Self {
        Vault {
            service: SERVICE.to_string(),
        }
    }

    /// 测试/隔离用保管箱。调用方必须保证 `service` 每次运行唯一（例如包含进程
    /// pid + 纳秒时间戳），否则起不到隔离效果。
    #[cfg(test)]
    fn with_service(service: String) -> Self {
        Vault { service }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, key).map_err(|e| format!("keychain 打开失败：{e}"))
    }

    /// 读取 id 索引；索引条目不存在（首次运行）视为空列表，不是错误。
    fn load_index(&self) -> Result<Vec<String>, String> {
        match self.entry(INDEX_KEY)?.get_password() {
            Ok(json) => serde_json::from_str(&json).map_err(|e| format!("索引解析失败：{e}")),
            Err(keyring::Error::NoEntry) => Ok(Vec::new()),
            Err(e) => Err(format!("keychain 读取索引失败：{e}")),
        }
    }

    fn save_index(&self, ids: &[String]) -> Result<(), String> {
        let json = serde_json::to_string(ids).map_err(|e| format!("索引序列化失败：{e}"))?;
        self.entry(INDEX_KEY)?
            .set_password(&json)
            .map_err(|e| format!("keychain 写入索引失败：{e}"))
    }

    /// 写入/更新一个 MCP server 配置（含密钥的 JSON 整体存入 keychain），并把 id 加入索引（去重）。
    ///
    /// entry 写入与索引更新是两次独立的 keychain 写操作，无法做到原子。若 entry
    /// 写入成功后索引更新（读取或保存）失败，会尝试补偿回滚：尽力删除刚写入的
    /// entry，避免留下一条"不在索引中因而 `list_servers` 看不到、但 `get_server`
    /// 能查到"的不可见密钥条目。回滚是尽力而为——若删除也失败，仍返回原始
    /// `Err`（此时状态不会比回滚前更差，只是留下一条已知有此风险的残留）。
    /// 本路径依赖真实 keychain 中途失败才能触发，无 mock 基础设施下不可单测，
    /// 属于防御性代码。
    fn put_server(&self, config: &ServerConfig) -> Result<(), String> {
        let json = serde_json::to_string(config).map_err(|e| format!("配置序列化失败：{e}"))?;
        self.entry(&server_key(&config.id))?
            .set_password(&json)
            .map_err(|e| format!("keychain 写入失败：{e}"))?;

        if let Err(e) = self.add_to_index(&config.id) {
            let _ = self.entry(&server_key(&config.id)).and_then(|entry| {
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(err) => Err(format!("keychain 删除失败：{err}")),
                }
            });
            return Err(e);
        }
        Ok(())
    }

    /// 把 id 加入索引（去重），已存在则跳过。抽出为独立方法，便于 `put_server`
    /// 在其失败时统一做补偿回滚。
    fn add_to_index(&self, id: &str) -> Result<(), String> {
        let mut ids = self.load_index()?;
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
            self.save_index(&ids)?;
        }
        Ok(())
    }

    /// 按 id 读取单个 server 配置；不存在返回 None（不是错误）。
    fn get_server(&self, id: &str) -> Result<Option<ServerConfig>, String> {
        match self.entry(&server_key(id))?.get_password() {
            Ok(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| format!("配置解析失败：{e}")),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("keychain 读取失败：{e}")),
        }
    }

    /// 列出所有已存的 server 配置（读索引→逐个加载）。索引中的 id 若解析不到
    /// 对应 entry（悬空 id——例如 `delete_server` 删完 entry 后索引保存失败），
    /// 会被跳过并**自愈**：从索引中剔除该 id 后重新持久化索引。剔除后的持久化
    /// 是尽力而为——若保存失败，不影响本次返回值，仍返回已成功加载到的配置列
    /// 表（下次调用会再次尝试自愈）。真正的 keychain 读取错误（而非"entry 不
    /// 存在"）仍会中止并返回错误。
    fn list_servers(&self) -> Result<Vec<ServerConfig>, String> {
        let ids = self.load_index()?;
        let mut out = Vec::with_capacity(ids.len());
        let mut alive_ids = Vec::with_capacity(ids.len());
        let mut has_dangling = false;
        for id in ids {
            match self.get_server(&id)? {
                Some(cfg) => {
                    alive_ids.push(id);
                    out.push(cfg);
                }
                None => has_dangling = true,
            }
        }
        if has_dangling {
            let _ = self.save_index(&alive_ids);
        }
        Ok(out)
    }

    /// 删除某个 server 配置：本就不存在视为成功（幂等），并把 id 从索引移除。
    fn delete_server(&self, id: &str) -> Result<(), String> {
        match self.entry(&server_key(id))?.delete_credential() {
            Ok(()) => {}
            Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(format!("keychain 删除失败：{e}")),
        }

        let mut ids = self.load_index()?;
        if let Some(pos) = ids.iter().position(|x| x == id) {
            ids.remove(pos);
            self.save_index(&ids)?;
        }
        Ok(())
    }
}

fn server_key(id: &str) -> String {
    format!("mcp-server/{id}")
}

/// 写入/更新一个 MCP server 配置（含密钥的 JSON 整体存入 keychain），并把 id 加入索引（去重）。
/// entry 写入成功后若索引更新失败，会尽力回滚刚写入的 entry 再返回错误（见
/// `Vault::put_server` 文档）。
pub fn put_server(config: &ServerConfig) -> Result<(), String> {
    Vault::production().put_server(config)
}

/// 按 id 读取单个 server 配置；不存在返回 None（不是错误）。
pub fn get_server(id: &str) -> Result<Option<ServerConfig>, String> {
    Vault::production().get_server(id)
}

/// 列出所有已存的 server 配置（读索引→逐个加载）。索引中解析不到 entry 的
/// 悬空 id 会被跳过并自愈剔除出索引（见 `Vault::list_servers` 文档），不会
/// 中止返回错误；只有真正的 keychain 读取错误才会中止并返回错误。
pub fn list_servers() -> Result<Vec<ServerConfig>, String> {
    Vault::production().list_servers()
}

/// 删除某个 server 配置：本就不存在视为成功（幂等），并把 id 从索引移除。
pub fn delete_server(id: &str) -> Result<(), String> {
    Vault::production().delete_server(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 生成本次测试进程独有的 keychain service 名：纳秒时间戳 + 原子计数器 + pid。
    /// 关键点：这不是编译期常量，而是**每次 `cargo test` 运行**（哪怕紧跟一次重编译）
    /// 都不同的字符串——因此本测试写入/读取的所有 keychain 条目（含索引条目）都落
    /// 在一个全新的 (service) 命名空间下，绝不会与上一次编译产物遗留的条目共享同一
    /// 个 keychain 条目，从根上消除"重签名后读旧条目触发交互式授权框 → headless
    /// `cargo test` 无限挂起"的 hazard。
    fn unique_test_service() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("super-agent-os-test-{ts}-{n}-{}", std::process::id())
    }

    fn config_with_secret(id: &str) -> ServerConfig {
        let mut env = BTreeMap::new();
        env.insert("API_KEY".to_string(), format!("sk-secret-{id}"));
        ServerConfig {
            id: id.to_string(),
            category: "dev".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "some-mcp-server".into()],
            env,
            transport: "stdio".into(),
            trust: Trust::Byo,
        }
    }

    /// Teardown 守卫：无论测试断言是否中途 panic，`Drop` 都会清掉本次测试在其
    /// 专属 service 命名空间下创建的每一个条目——包括索引条目本身。这是防御性
    /// 兜底：即便本测试的 service 名已经是每次运行唯一（不可能与任何过去/未来
    /// 的运行冲突），也不给同一台机器的 keychain 留下任何残留条目。
    struct Cleanup {
        vault: Vault,
        id: String,
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.vault.delete_server(&self.id);
            if let Ok(index_entry) = self.vault.entry(INDEX_KEY) {
                match index_entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(_) => {} // 清理是尽力而为，失败不影响测试结果本身
                }
            }
        }
    }

    #[test]
    #[ignore = "写真实 macOS 钥匙串：没有可交互安全会话的环境（例如由 launchd 拉起的 CI runner）里，报 errSecInteractionNotAllowed（User interaction is not allowed）；合并前在本机终端跑 `cargo test --lib vault:: -- --ignored`"]
    fn put_get_list_delete_roundtrip_real_keychain() {
        let service = unique_test_service();
        let id = format!("vault-test-{service}");
        let cfg = config_with_secret(&id);
        let vault = Vault::with_service(service.clone());
        let _cleanup = Cleanup {
            vault: Vault::with_service(service.clone()),
            id: id.clone(),
        };

        vault.put_server(&cfg).unwrap();

        // roundtrip：取回 == 存入
        let got = vault.get_server(&id).unwrap();
        assert_eq!(got, Some(cfg.clone()));

        // list 含之
        let all = vault.list_servers().unwrap();
        assert!(all.iter().any(|c| c.id == id));

        // 凭据只在 keychain：直接读原始钥匙串条目（本次测试独有的 service 命名空间下），
        // 值必须是能解析出密钥的 JSON（不落盘/不落日志）
        let raw = keyring::Entry::new(&service, &format!("mcp-server/{id}"))
            .unwrap()
            .get_password()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["env"]["API_KEY"], format!("sk-secret-{id}"));

        // delete 后 get = None，索引也移除
        vault.delete_server(&id).unwrap();
        assert_eq!(vault.get_server(&id).unwrap(), None);
        let all_after = vault.list_servers().unwrap();
        assert!(!all_after.iter().any(|c| c.id == id));

        // `_cleanup` 在此处离开作用域触发 Drop，兜底清掉索引条目（此时其值应已是 `[]`）。
    }

    /// `list_servers` 自愈测试：手工把索引污染成"额外含一个没有对应 entry 的
    /// 悬空 id"，模拟 `delete_server` 在 entry 已删、索引保存却失败时会留下的
    /// 状态（该失败注入路径本身无法在不 mock keyring 的前提下单测，故这里直
    /// 接构造其*结果状态*来验证自愈逻辑）。断言 `list_servers` 只返回两个真实
    /// 存在的 server，且悬空 id 已被剔除并持久化回索引（重新读索引不再含它）。
    #[test]
    #[ignore = "写真实 macOS 钥匙串：没有可交互安全会话的环境（例如由 launchd 拉起的 CI runner）里，报 errSecInteractionNotAllowed（User interaction is not allowed）；合并前在本机终端跑 `cargo test --lib vault:: -- --ignored`"]
    fn list_servers_self_heals_dangling_index_id() {
        let service = unique_test_service();
        let id_a = format!("vault-test-a-{service}");
        let id_b = format!("vault-test-b-{service}");
        let bogus_id = format!("vault-test-bogus-{service}");
        let cfg_a = config_with_secret(&id_a);
        let cfg_b = config_with_secret(&id_b);
        let vault = Vault::with_service(service.clone());
        let _cleanup_a = Cleanup {
            vault: Vault::with_service(service.clone()),
            id: id_a.clone(),
        };
        let _cleanup_b = Cleanup {
            vault: Vault::with_service(service.clone()),
            id: id_b.clone(),
        };

        vault.put_server(&cfg_a).unwrap();
        vault.put_server(&cfg_b).unwrap();

        // 直接操作索引条目，注入一个没有对应 entry 的悬空 id。
        let mut ids = vault.load_index().unwrap();
        assert!(ids.contains(&id_a));
        assert!(ids.contains(&id_b));
        ids.push(bogus_id.clone());
        vault.save_index(&ids).unwrap();

        // list_servers 只应返回两个真实存在的 server，悬空 id 被跳过。
        let all = vault.list_servers().unwrap();
        let mut got_ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
        got_ids.sort();
        let mut expected_ids = vec![id_a.as_str(), id_b.as_str()];
        expected_ids.sort();
        assert_eq!(got_ids, expected_ids);

        // 自愈：悬空 id 应已从持久化的索引中被剔除，真实 id 仍在。
        let persisted = vault.load_index().unwrap();
        assert!(!persisted.contains(&bogus_id));
        assert!(persisted.contains(&id_a));
        assert!(persisted.contains(&id_b));

        // `_cleanup_a` / `_cleanup_b` 离开作用域触发 Drop，清掉两个 server entry
        // 与索引条目本身。
    }

    /// `delete_server` 保持索引一致性的回归测试：两个 server 中删除一个后，
    /// `list_servers` 应精确返回剩下的另一个（不多不少），证明正常路径下
    /// entry 与索引不会互相脱节。
    ///
    /// 注：本文件描述的另外两条补偿/自愈路径——`put_server` 索引更新中途失败
    /// 后的补偿回滚、`delete_server` entry 删除成功但索引保存失败后留下的悬空
    /// id——都依赖真实 keychain 在两次写操作之间"精确失败"，在不引入 keyring
    /// mock 基础设施的前提下无法用真实 keychain 单测触发，属于文档化的防御性
    /// 代码（`put_server` 见其函数文档；悬空 id 的自愈效果已由上面
    /// `list_servers_self_heals_dangling_index_id` 通过直接构造结果状态覆盖）。
    #[test]
    #[ignore = "写真实 macOS 钥匙串：没有可交互安全会话的环境（例如由 launchd 拉起的 CI runner）里，报 errSecInteractionNotAllowed（User interaction is not allowed）；合并前在本机终端跑 `cargo test --lib vault:: -- --ignored`"]
    fn delete_server_keeps_index_consistent() {
        let service = unique_test_service();
        let id_a = format!("vault-test-keep-a-{service}");
        let id_b = format!("vault-test-keep-b-{service}");
        let cfg_a = config_with_secret(&id_a);
        let cfg_b = config_with_secret(&id_b);
        let vault = Vault::with_service(service.clone());
        let _cleanup_a = Cleanup {
            vault: Vault::with_service(service.clone()),
            id: id_a.clone(),
        };
        let _cleanup_b = Cleanup {
            vault: Vault::with_service(service.clone()),
            id: id_b.clone(),
        };

        vault.put_server(&cfg_a).unwrap();
        vault.put_server(&cfg_b).unwrap();

        vault.delete_server(&id_a).unwrap();

        let all = vault.list_servers().unwrap();
        let got_ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(got_ids, vec![id_b.as_str()]);
    }
}
