use crate::mcp::McpManager;
use crate::mcp_socket::McpSocketListener;
use crate::rpc::RpcSession;
use crate::scheme::{SlotPool, SLOT_COUNT};
use crate::supervisor::{ConcurrencyGate, MAX_CONCURRENT_AGENTS};
use crate::usage::UsageAccumulator;
use std::collections::HashMap;
use tokio::sync::Mutex;

pub struct AppState {
    /// 主助手会话（P0），`start_main_session` 仍使用此字段。
    pub main_session: Mutex<Option<RpcSession>>,
    /// 每个已打开应用的独立 pi 子进程会话（app_id -> RpcSession）。
    pub app_sessions: Mutex<HashMap<String, RpcSession>>,
    /// 全局 MCP server 连接池（Task5-7）。`session_mgr::open_app`（Task9）在
    /// 打开一个 app 时用它算 `authorized_tools`，决定要不要往该 app 的 pi 会话
    /// 注入 `mcp-bridge` 扩展 + `SUPERAGENT_MCP_TOOLS`/`SUPERAGENT_MCP_SOCKET`
    /// env。`McpManager` 自身内部已经是 `Arc<Mutex<..>>` 级别的并发安全（见该
    /// 类型文档），这里不需要再套一层 `tokio::sync::Mutex`。
    pub mcp: McpManager,
    /// 每个已打开应用、需要 socket 的贡献对应的宿主 unix socket 监听器（Task9b，
    /// app_id -> McpSocketListener）。`session_mgr::open_app_after_acquire` 在
    /// `CapabilityRegistry::launch` 算出的 `LaunchContribution.needs_socket` 为真
    /// （即至少一个已声明能力——MCP 连接器/notifications/agents.call/maker/router
    /// 等——需要宿主 socket）时才在此登记一条；`close_app` 据此把该 app 的监听器
    /// `stop()`（中止 accept 循环 + 删 socket 文件），仿 `app_sessions` 的 per-app
    /// 资源生命周期模式。
    pub mcp_sockets: Mutex<HashMap<String, McpSocketListener>>,
    /// webview 自定义协议槽位池（sagent0..sagent11），每应用独立 origin。
    ///
    /// 用 `std::sync::Mutex`（非 `tokio::sync::Mutex`）：T16 的自定义 scheme 协议
    /// 处理器（`register_asynchronous_uri_scheme_protocol`）是同步闭包，需要在
    /// 非 async 上下文里同步加锁读取 slot->appId 映射；SlotPool 上的操作本身都是
    /// 极快的纯内存操作，不会跨 `.await` 长时间持有，用 std Mutex 更直接（调用方
    /// 在 `session_mgr.rs` 里用 `.lock().unwrap()`，且都在单条语句内完成、不跨
    /// await 点持有 guard——否则该 guard 非 Send，跨 await 持有会导致异步任务无法
    /// 编译/发送）。
    pub slots: std::sync::Mutex<SlotPool>,
    /// 全局并发闸门，限制同时运行的 agent 子进程数量。
    pub gate: Mutex<ConcurrencyGate>,
    /// per-app token 用量状态（P3 Task16 引入，Task18 修复数据来源）：
    /// `session_mgr.rs`（per-app 事件循环）与 `lib.rs`（主会话事件循环）在每轮
    /// `agent_end` 后发一次 `get_session_stats` 查询，收到响应
    /// （`rpc::PiEvent::SessionStats`）时调用 `set_latest` 覆盖式记录该 app 当前
    /// 累计用量；`lib.rs::app_usage` 命令调用 `usage_response` 供前端查询。
    /// 唯一共享实例——见 `usage::UsageAccumulator` 模块文档，不允许各调用点
    /// 各自新建一份（会导致彼此看不到对方已经写入的最新值）。
    pub usage: UsageAccumulator,
    /// P6-A：进程级唯一能力注册表（notifications 限速窗口等状态依赖单实例，
    /// 见 `capabilities::notifications::NotificationsCapability` 文档）。
    /// `mcp_socket::McpSocketListener::start_with_identity` 需要一份
    /// `Arc<CapabilityRegistry>` 才能被多个 accept 出来的连接任务共享持有；
    /// `session_mgr::open_app_after_acquire` 用 `.clone()`（浅拷贝这个 Arc
    /// 指针）传给每个前台监听器，确保同一进程内所有 app 的限速窗口互相隔离
    /// （按 app_id 分桶，见该能力实现）但共享同一份注册表实例本身。
    pub capabilities: std::sync::Arc<crate::capability::CapabilityRegistry>,
    /// P6-F：每应用的打开时刻 / 最近活动 / 回合状态 / 休眠集合（空闲回收与资源面板用）。
    pub activity: crate::idle::ActivityTracker,
}

// 手写 Default：SlotPool/ConcurrencyGate 需要带参数构造（容量/上限），
// 无法用 #[derive(Default)]。
impl Default for AppState {
    fn default() -> Self {
        Self {
            main_session: Mutex::new(None),
            app_sessions: Mutex::new(HashMap::new()),
            mcp: McpManager::new(),
            mcp_sockets: Mutex::new(HashMap::new()),
            slots: std::sync::Mutex::new(SlotPool::new(SLOT_COUNT)),
            gate: Mutex::new(ConcurrencyGate::new(MAX_CONCURRENT_AGENTS)),
            usage: UsageAccumulator::new(),
            capabilities: std::sync::Arc::new(crate::capabilities::builtin()),
            activity: crate::idle::ActivityTracker::default(),
        }
    }
}
