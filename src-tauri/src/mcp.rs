/// MCP 工具危险分级：根据工具名称和注解判断工具是否为读操作（安全）还是写操作（危险）。
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Danger {
    /// 只读操作（如 read, list, get, search 等）
    Read,
    /// 写入/删除/修改操作（如 write, create, delete 等），或未知工具
    Write,
}

/// 根据工具名称、可选的注解与连接器信任等级判断工具的危险等级（P6-C）。
///
/// # 规则（优先级从高到低）：
/// 1. **写指示优先**：`annotation` 若含写指示（"write"/"create"/"delete"/
///    "modify"/"mutate"/"destructive"——最后一个对应 MCP 标准注解词汇
///    `destructiveHint`，不区分大小写）→ `Danger::Write`，与 `trust` 无关
///    （信任等级只能让工具"升危"更谨慎，绝不能反过来放宽写指示；即便工具名
///    落在读前缀表里，写指示注解也能把它升危——见
///    `byo_write_annotation_still_escalates_read_prefixed_tool`）。
/// 2. **读指示仅 `Trust::Vetted` 生效**：`trust == Vetted` 且 `annotation` 含
///    只读提示（"readOnly"/"read-only"/"read"）→ `Danger::Read`；
///    `trust == Byo` 下读指示被忽略、直接落到第 3 步前缀表——用户自填的
///    server 不可信，不能靠自称 `readOnlyHint=true` 把未知写工具伪装成只读、
///    绕过写确认（`vault::Trust` 文档）。
/// 3. **前缀表匹配**（不区分大小写，`trust` 双方共用）：
///    - Read 前缀：`read`, `list`, `get`, `search`, `fetch`, `query` → `Danger::Read`
///    - Write 前缀：`write`, `create`, `delete`, `send`, `update`, `exec`, `put`, `remove` → `Danger::Write`
/// 4. **保守默认**：未知工具 → `Danger::Write`（宁可错判为危险）
pub fn classify_tool_with_trust(name: &str, annotation: Option<&str>, trust: Trust) -> Danger {
    let lower_name = name.to_lowercase();

    // 规则 1/2：注解
    if let Some(anno) = annotation {
        let lower_anno = anno.to_lowercase();
        // 写指示先判：防 "readwrite"/"read-write" 这类含 "read" 子串却是写操作的注解
        // 被下面的 contains("read") 误判为只读（安全欠分类=写工具当读工具放行）。
        if lower_anno.contains("write")
            || lower_anno.contains("create")
            || lower_anno.contains("delete")
            || lower_anno.contains("modify")
            || lower_anno.contains("mutate")
            || lower_anno.contains("destructive")
        {
            return Danger::Write;
        }
        if trust == Trust::Vetted
            && (lower_anno.contains("readonly")
                || lower_anno.contains("read-only")
                || lower_anno.contains("read"))
        {
            return Danger::Read;
        }
    }

    // 规则 3：前缀表匹配
    // 检查名称是否以某个前缀开头（考虑下划线分隔或驼峰命名），不区分大小写
    let check_prefix = |prefixes: &[&str]| {
        for prefix in prefixes {
            if lower_name.starts_with(prefix) {
                // 验证前缀边界：前缀后面是下划线、数字、大写字母或结尾
                if lower_name.len() == prefix.len() {
                    // 前缀是整个名称
                    return true;
                }
                // 检查前缀后的第一个字符
                if let Some(first_char) = name.chars().nth(prefix.len()) {
                    if first_char == '_'
                        || first_char.is_ascii_digit()
                        || first_char.is_ascii_uppercase()
                    {
                        return true;
                    }
                }
            }
        }
        false
    };

    if check_prefix(&["read", "list", "get", "search", "fetch", "query"]) {
        return Danger::Read;
    }

    if check_prefix(&[
        "write", "create", "delete", "send", "update", "exec", "put", "remove",
    ]) {
        return Danger::Write;
    }

    // 规则 4：保守默认——未知工具视为写操作（危险）
    Danger::Write
}

/// `classify_tool_with_trust(name, annotation, Trust::Vetted)` 的别名，保留
/// 只为向后兼容旧调用方的函数签名——P6-C 之后生产路径（`parse_tools`/
/// `authorized_tools`）一律直接调 `classify_tool_with_trust` 带上连接器真实
/// 的 `trust`，不再调用本函数。
#[deprecated(note = "改用 classify_tool_with_trust(name, annotation, trust)；生产路径不再调用")]
pub fn classify_tool(name: &str, annotation: Option<&str>) -> Danger {
    classify_tool_with_trust(name, annotation, Trust::Vetted)
}

// ---------------------------------------------------------------------------
// McpManager：stdio MCP server 连接池（Task5）
// ---------------------------------------------------------------------------

use crate::paths::DataLayout;
use crate::permissions::{Access, ConnectorReq};
use crate::supervisor::Backoff;
use crate::vault::{ServerConfig, Trust};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex as AsyncMutex, Notify};

/// 握手（`initialize` → `tools/list`）单次读取的超时上限。防一个 spawn 成功但
/// 卡死不回应的 server（例如 `mock_mcp_server --hang`）把 `ensure_server` 挂到
/// 天荒地老——见 Important 2。超时后必须杀掉子进程再返回 `Err`，不留孤儿。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// `tools/call`（Task7 `call_tool`）单次往返的超时上限。握手阶段还持有 `Child`
/// 句柄，超时可以直接 kill 掉卡死的子进程；`ensure_server` 返回之后 `Child` 已经
/// 转交给 `spawn_crash_monitor` 那个任务持有，`call_tool` 这里已经拿不到 `Child`
/// 了——所以超时只能返回 `Err`，不能 kill（子进程是否卡死留给崩溃看护/下一次
/// 请求的超时自然暴露，不在这里越权处理生命周期）。
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// 单个工具的静态信息：名称、描述、危险等级、JSON Schema、原始注解。`danger`
/// 由 `classify_tool_with_trust(name, annotation, Trust::Vetted)` 依据
/// `tools/list` 里的 `annotations.readOnlyHint` 推导（见 `parse_tools`）——
/// 这里固定传 `Vetted` 只是为了给 `server_tools()` 之类不关心信任分级的调用方
/// 一个确定性的向后兼容值；`annotation` 原样保留（P6-C），供
/// `McpManager::authorized_tools` 结合该连接的真实 `McpConn::trust` 现场重新
/// 分级，不能只信这里缓存的 `danger`。`input_schema` 是该工具在 `tools/list`
/// 响应里原样携带的 `inputSchema`（Task9：注入进 app 侧
/// `SUPERAGENT_MCP_TOOLS` 时 `mcp-bridge.ts` 需要它原样喂给 `registerTool` 的
/// `parameters`）；缺失时落回 `serde_json::Value::Null`。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub danger: Danger,
    pub input_schema: serde_json::Value,
    pub annotation: Option<String>,
}

/// 一个已连接 MCP server 的运行态。字段：
///
/// - `stdin`：握手阶段打开、之后一直保留的 stdin。Task7 `call_tool` 用它发送
///   `tools/call` 请求；保持管道存活也防止 mock server 读到 stdin EOF 而提前
///   退出触发不必要的崩溃重启。
/// - `rx`：握手完成后仍保留的 stdout 响应 channel。握手只消费了 id=1/2 两条，
///   之后的帧——即 `tools/call` 的响应——全靠 `call_tool` 继续从这同一个 `rx`
///   里按 id 匹配读取；`spawn_and_handshake` 里那个 reader 任务只要 `rx` 还
///   活着就会一直转发，不会因为握手函数返回就停。
/// - `call_gate`：整个"发一条 `tools/call` 请求 → 等它匹配的响应"往返的串行
///   门。必要性——mpsc 是单消费者，不匹配 id 的帧一旦被 `.recv()` 取出但发现
///   对不上，就没有放回队列的机制，会被永久丢弃；如果两个并发 `call_tool`
///   调用同时对同一个 server 收发，其中一个可能会把另一个的响应帧吃掉后因为
///   id 不对而白白丢弃，导致另一个调用永久收不到自己的响应（直到超时）。
///   `call_gate` 把"发送+接收"锁成一个不可分割的临界区，同一个 server 上的
///   `call_tool` 调用天然排队、互不打扰；代价是同一个 server 不支持真正并发
///   的 tools/call（本任务范围内可接受，Task15 若需要更高并发可以在这里升级
///   为按 id 分发的路由表）。
/// - `next_id`：每次 `call_tool` 用的 JSON-RPC id 生成器，从 3 开始（1、2 被
///   握手 initialize/tools/list 占用，不会与后续调用的 id 撞车）。
/// - `call_count`：本连接上真实发生的 `tools/call` 次数（不含握手），纯供
///   测试断言"这个工具确实/绝对没有被执行过"（同 `McpManager::spawn_count`
///   的设计意图）。
/// - `tools`/`pid`/`category`：握手时缓存的 `tools/list` 结果、子进程 pid
///   （供诊断/`server_pid` 观测）、连接时记录的 category（Task6：
///   `authorized_tools` 靠它做 app connector 的 category 匹配，见该方法文档）。
struct McpConn {
    stdin: AsyncMutex<ChildStdin>,
    rx: AsyncMutex<mpsc::Receiver<serde_json::Value>>,
    call_gate: AsyncMutex<()>,
    next_id: AtomicI64,
    call_count: AtomicUsize,
    tools: Vec<ToolInfo>,
    pid: Option<u32>,
    category: String,
    /// 连接时从 `cfg.trust` 带入（P6-C）：`authorized_tools` 用它现场重新分级
    /// `AuthedTool.danger`，而不是直接信 `ToolInfo.danger`（那个字段固定按
    /// `Trust::Vetted` 算，见 `ToolInfo` 文档）。
    trust: Trust,
}

/// app 在 `authorized_tools()` 之后实际可见的一个工具：归属的 serverId、工具名、
/// 危险等级（随手带出，Task7 host 侧二次复核调用时不必再回查一次
/// `server_tools`）、描述与 JSON Schema（P6-A：`capabilities::connectors::
/// ConnectorsCapability::launch` 拼装 `SUPERAGENT_MCP_TOOLS` 注入条目时需要，
/// 直接从 `authorized_tools()` 里带出，不必再回查一次 `server_tools` 逐个匹配
/// `ToolInfo`）。
#[derive(Debug, Clone, PartialEq)]
pub struct AuthedTool {
    pub server: String,
    pub tool: String,
    pub danger: Danger,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// MCP server 连接池：按 serverId 缓存已连接的 server。`ensure_server` 对同一
/// id 的第二次调用是连接复用的快速路径（HashMap 命中直接返回，不重新 spawn
/// 子进程）。
///
/// `#[derive(Clone)]`（Task9b）：全部字段本就是 `Arc<..>`，`clone()` 只是浅拷贝
/// 这几个 `Arc` 指针——克隆出的handle 与原对象共享同一份连接池/待确认登记表/
/// 计数器，不是深拷贝出一个独立的连接池。`mcp_socket::McpSocketListener::start` 需要
/// 一份可以移进长期存活的 `tokio::spawn` 任务里的持有型 handle（`AppState.mcp`
/// 本身只能被借用，见 `session_mgr::open_app_after_acquire`），这个 derive 就是
/// 为了满足那个需要，语义上与该类型文档一贯宣称的“内部已是 Arc 级别并发安全”
/// 完全一致。
#[derive(Clone)]
pub struct McpManager {
    conns: Arc<StdMutex<HashMap<String, Arc<McpConn>>>>,
    /// 真实发生的 spawn 次数（不含 connect-once 复用命中，含崩溃后的重启）。
    /// 主要供测试断言"确实没有重复 spawn"；生产逻辑不依赖这个计数器本身。
    spawn_count: Arc<AtomicUsize>,
    /// per-id 异步单飞门（Important 1）：同一个 id 的并发 `ensure_server` 调用
    /// 共享同一把 `tokio::sync::Mutex`，保证对同一个未连接 id 并发调用时只有
    /// 一个真正 spawn，其余在锁上排队、拿到锁后发现已连接就直接返回，不重复
    /// spawn、不产生孤儿子进程。不同 id 用不同的锁对象，互不阻塞。
    ///
    /// 有意不做"用完即删"：如果在某个等待者还持有旧锁的 `Arc` 时把 map 里的
    /// entry 删掉，另一个并发调用可能会为同一个 id 创建一把*新*锁，两把锁互不
    /// 感知，又会退化回 Important 1 的双 spawn 竞态。MCP server 的 id 基数很小
    /// （用户配置的数量级），让这张表随 id 数量线性常驻是可接受的取舍。
    spawn_gates: Arc<StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    /// P4 T5 专用 install-confirm seam：`confirmId -> PendingInstall`。P6-C 之前
    /// 与 MCP 写确认登记表（当时叫 `pending: HashMap<String, PendingCall>`，进程
    /// 内存、重启即丢）**平行但完全独立**——不同的类型、不同的 map、不共享 resume
    /// 路径，只共享下面这个 `confirm_counter` 生成器（保证同一个 `McpManager` 内
    /// confirm_id 全局唯一，不会因为两套确认体系各自计数而撞出重复 id）。P6-C
    /// 把 MCP 写确认那一半搬到了 `approvals::ApprovalStore`（持久化到
    /// `DataLayout::approvals_dir()`，见该模块文档），`pending_installs` 本身
    /// 未受影响——Maker 安装确认仍是独立的、纯内存态的 seam，理由不变：不纠缠
    /// 已验证过的 P3 MCP 写确认 `call_tool` resume 语义，回归风险最小。执行期
    /// 决策见
    /// `docs/superpowers/plans/2026-07-18-p4-maker-flagship-onboarding.md`
    /// "执行期决策：T5 安装权限确认 seam"一节。登记者：
    /// `maker::handle_maker_request` 的 `__host_maker_install__` 分支
    /// （`register_pending_install`）；唯一消费者：`maker::resolve_install`
    /// （`take_pending_install`，供 Tauri 命令 `maker_respond_install_confirm`
    /// 与测试共用）。
    pending_installs: Arc<StdMutex<HashMap<String, PendingInstall>>>,
    /// **终审 Important 2**：`disconnect(id)` 与 `spawn_crash_monitor` 那个
    /// tokio 任务之间的信号通道——`Child` 句柄在 `ensure_server` 返回之后就
    /// 已经转交给崩溃看护任务持有（见该方法文档），`McpManager` 自身再也拿不
    /// 到它，没法直接 `child.kill()`。每个正在被看护的 serverId 在这里登记
    /// 一个 `Notify`；`disconnect` 触发它，看护任务的 `tokio::select!` 从
    /// `notified()` 分支醒来后自己 kill 掉当前持有的子进程并退出循环（不再
    /// 落回"崩溃后重启"分支，也不再重新插回 `conns`）——区分开了"预期关闭"
    /// 与"崩溃"，补上上面 `spawn_crash_monitor` 文档里记的已知局限。
    disconnect_signals: Arc<StdMutex<HashMap<String, Arc<Notify>>>>,
    /// confirmId 生成器：单调递增计数器，配合固定前缀产出的字符串在本进程内
    /// 保证互不相同（同 `spawn_count` 一样是个简单原子计数器，不需要 UUID 这类
    /// 外部依赖）。P6-C 前供 `pending`/`pending_installs` 两套确认体系共用；
    /// MCP 写确认改用 `approvals::ApprovalStore` 后（自己的 `stg-<now>-<n>` id
    /// 生成器，见该模块），这个计数器只剩 `pending_installs` 一个消费者，但
    /// 仍保留原名/原类型不动——不为了这次搬迁去改一个跟本次改动无关的 id 生成
    /// 细节。
    confirm_counter: Arc<AtomicUsize>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            conns: Arc::new(StdMutex::new(HashMap::new())),
            spawn_count: Arc::new(AtomicUsize::new(0)),
            spawn_gates: Arc::new(StdMutex::new(HashMap::new())),
            disconnect_signals: Arc::new(StdMutex::new(HashMap::new())),
            pending_installs: Arc::new(StdMutex::new(HashMap::new())),
            confirm_counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 已真实 spawn 子进程的次数（不含复用命中）。测试用。
    pub fn spawn_count(&self) -> usize {
        self.spawn_count.load(Ordering::SeqCst)
    }

    /// 某个已连接 server 上真实发生过的 `tools/call` 次数（不含握手）。测试用：
    /// 断言 `host_mcp_call` 的 Denied/PendingConfirm 分支确实没有执行到工具本身
    /// （同 `spawn_count` 的设计意图——给测试一个协议无关、直接的执行证据）。
    pub fn call_count(&self, server_id: &str) -> usize {
        self.conns
            .lock()
            .unwrap()
            .get(server_id)
            .map(|c| c.call_count.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// 某个已连接 server 的子进程 pid（诊断用；未连接返回 `None`）。
    pub fn server_pid(&self, server_id: &str) -> Option<u32> {
        self.conns
            .lock()
            .unwrap()
            .get(server_id)
            .and_then(|c| c.pid)
    }

    /// 确保 `cfg.id` 对应的 server 已连接：已连接（HashMap 命中）→ 直接返回
    /// `Ok(())`，不重新 spawn（connect-once/全局复用）；否则 spawn 子进程
    /// （`cfg.command`/`cfg.args`/`cfg.env`，`tokio::process::Command`），走
    /// stdio JSON-RPC 握手（`initialize` → `tools/list`，LF 分隔一行一个
    /// JSON，帧格式同 P0 `rpc.rs`/`jsonl::FrameBuffer`），并把 `tools/list`
    /// 结果缓存为 `Vec<ToolInfo>`。
    ///
    /// danger 推导见 `parse_tools`：`annotations.readOnlyHint` → `true` 映射
    /// 为 `classify_tool` 的注解实参 `Some("readOnly")`、`false` → `Some("write")`、
    /// 缺失 → `None`，再调用 `classify_tool(tool_name, that_annotation)`。
    ///
    /// server 崩溃（子进程退出）后按 P0 `supervisor::Backoff` 的退避常量尝试
    /// 重新 spawn+握手（best-effort，多次重试仍失败则从连接池移除，下次
    /// `ensure_server` 会重新走 spawn 路径）。
    ///
    /// **connect-once 是单飞的（Important 1）**：并发对同一个未连接 id 调用
    /// `ensure_server` 只会触发一次真正的 spawn。流程：
    /// 1. 快速路径——`conns` 命中直接 `Ok(())`，不碰锁门。
    /// 2. 否则 get-or-create 该 id 专属的异步锁（`spawn_gates`），`.lock().await`
    ///    它。不持有 std `Mutex` 跨 `.await`（拿到 `Arc<AsyncMutex<()>>` 后立即
    ///    释放 std 锁再 `.await`）。
    /// 3. 拿到锁后二次检查 `conns`——等锁期间可能已有另一个并发调用替我们连接
    ///    好了；命中则直接 `Ok(())`，不重复 spawn。
    /// 4. 仍未连接才真正 `spawn_and_handshake` + 插入 + 起崩溃看护。
    ///
    /// 净效果：N 个并发 `ensure_server("X")`（X 未连接）→ 恰好一次 spawn、一个
    /// 连接、零孤儿子进程。
    pub async fn ensure_server(&self, cfg: &ServerConfig) -> Result<(), String> {
        if self.conns.lock().unwrap().contains_key(&cfg.id) {
            return Ok(()); // 已连接：复用，不重新 spawn。
        }

        let gate = {
            let mut gates = self.spawn_gates.lock().unwrap();
            gates
                .entry(cfg.id.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let _permit = gate.lock().await;

        // 二次检查：等锁期间可能已有另一个并发调用者替我们完成了 spawn+insert。
        if self.conns.lock().unwrap().contains_key(&cfg.id) {
            return Ok(());
        }

        let (conn, child) = spawn_and_handshake(cfg).await?;
        self.spawn_count.fetch_add(1, Ordering::SeqCst);
        self.conns
            .lock()
            .unwrap()
            .insert(cfg.id.clone(), Arc::new(conn));

        self.spawn_crash_monitor(cfg.clone(), child);
        Ok(())
    }

    /// server 崩溃后的退避重启看护（仿 P0 `lib.rs::start_main_session`/
    /// `session_mgr.rs::open_app` 的事件循环退避重启模式，退避常量复用
    /// `supervisor::Backoff`）：等子进程退出 → 按退避延迟重新 spawn+握手 →
    /// 成功则把新连接换入 `conns`（保留同一个 serverId）并重置退避、继续看护
    /// 新子进程；退避次数耗尽仍失败则从 `conns` 移除该 id（故障态）。
    /// best-effort：本任务的测试只覆盖握手 happy path，不对崩溃重启路径断言。
    ///
    /// **终审 Important 2**：`tokio::select!` 同时等"子进程自己退出"
    /// （`child.wait()`，走崩溃重启分支）与"被 `disconnect` 显式叫停"
    /// （`notify.notified()`，kill 掉子进程、从 `conns` 移除、退出循环、
    /// **不**重启）——区分开"预期关闭"与"崩溃"，见 `disconnect_signals` 字段
    /// 文档。
    fn spawn_crash_monitor(&self, cfg: ServerConfig, child: Child) {
        let conns = self.conns.clone();
        let spawn_count = self.spawn_count.clone();
        let notify = Arc::new(Notify::new());
        self.disconnect_signals
            .lock()
            .unwrap()
            .insert(cfg.id.clone(), notify.clone());
        let disconnect_signals = self.disconnect_signals.clone();
        tokio::spawn(async move {
            let mut child = child;
            let mut backoff = Backoff::new();
            loop {
                tokio::select! {
                    _ = child.wait() => {}
                    _ = notify.notified() => {
                        let _ = child.kill().await;
                        conns.lock().unwrap().remove(&cfg.id);
                        disconnect_signals.lock().unwrap().remove(&cfg.id);
                        break;
                    }
                }
                match backoff.next_delay() {
                    Some(delay) => {
                        tokio::time::sleep(delay).await;
                        match spawn_and_handshake(&cfg).await {
                            Ok((new_conn, new_child)) => {
                                spawn_count.fetch_add(1, Ordering::SeqCst);
                                conns
                                    .lock()
                                    .unwrap()
                                    .insert(cfg.id.clone(), Arc::new(new_conn));
                                child = new_child;
                                backoff.reset();
                            }
                            Err(_) => continue,
                        }
                    }
                    None => {
                        conns.lock().unwrap().remove(&cfg.id);
                        disconnect_signals.lock().unwrap().remove(&cfg.id);
                        break;
                    }
                }
            }
        });
    }

    /// **终审 Important 2**：主动断开一个 server——从 `conns` 移除（之后
    /// `authorized_tools`/`call_tool`/`server_tools` 立刻看不到它，重新鉴权
    /// 因此对"server 已删"也会 fail-closed 拒绝，见 `respond_staged` 文档）
    /// 并 kill 掉它的子进程（经 `spawn_crash_monitor` 的 `notify` 信号，见该
    /// 方法文档——`Child` 句柄已经转交给那个任务，`McpManager` 自身拿不到）。
    ///
    /// 幂等：对未连接/已断开的 id 调用是 no-op（`disconnect_signals` 里没有
    /// 对应条目，`notify_one` 不会发生，也没有东西可 kill）。生产触发点：
    /// `lib.rs::delete_server` 命令在 `vault::delete_server` 成功后调用，堵上
    /// "用户删掉了 server，但它仍在连接池里活着、暂存调用仍会被执行"这个口子
    /// （见终审报告 Important 2）。
    pub async fn disconnect(&self, id: &str) {
        self.conns.lock().unwrap().remove(id);
        let notify = self.disconnect_signals.lock().unwrap().remove(id);
        if let Some(notify) = notify {
            notify.notify_one();
        }
    }

    /// P3 whole-branch review I-1 修复：批量把已配置的 server 接进本连接池
    /// ——生产环境里**唯一**真正建立 MCP 连接的入口。修复前，`ensure_server`
    /// 只在测试里被调用过，生产代码没有任何调用点：`conns` 永远为空 →
    /// `authorized_tools()` 永远返回 `[]` → 没有任何 app 会被注入
    /// `mcp-bridge`/MCP 工具，配置了 MCP server 在生产环境里完全不起作用。
    ///
    /// 两个生产触发点（`lib.rs`）：
    /// 1. 启动时（`setup`）：读 `vault::list_servers()` 拿到全部已配置 server，
    ///    调用本方法一次性接入，让此前配置过的 server 在应用重启后自动重连。
    /// 2. `put_server` 命令：配置写入 vault 成功后，把这一个新/改配置传进来，
    ///    不需要重启应用就能连上。
    ///
    /// 有意不在这里读 vault：直接吃调用方已经准备好的 `&[ServerConfig]`，
    /// 保持本方法可以脱离 `vault::*` 生产自由函数单测（见
    /// `mcp_manager_it.rs` 的 `connect_servers_*` 测试与 `vault.rs` 关于真实
    /// keychain 在重编译后可能弹交互式授权框、把 `cargo test` 挂死的隔离
    /// 说明）——把 vault 读取放进调用方，测试才能只传 mock server 配置。
    ///
    /// best-effort：单个 `cfg` 的 `ensure_server` 失败（spawn 失败、握手超时、
    /// 协议错误等）只记一条 stderr 诊断日志并跳过，不中止循环、不影响其余
    /// 配置——一个写错命令/暂时连不上的 server 配置不应该拖累其它本可正常
    /// 连接的 server。
    pub async fn connect_servers(&self, configs: &[ServerConfig]) {
        for cfg in configs {
            if let Err(e) = self.ensure_server(cfg).await {
                eprintln!(
                    "MCP server[{}] 连接失败（已跳过，不影响其它 server）：{e}",
                    cfg.id
                );
            }
        }
    }

    /// 某个已连接 server 缓存的工具列表；未连接返回空列表。
    pub fn server_tools(&self, server_id: &str) -> Vec<ToolInfo> {
        self.conns
            .lock()
            .unwrap()
            .get(server_id)
            .map(|c| c.tools.clone())
            .unwrap_or_default()
    }

    /// P6-B：供 `known_tools()`（技能安装门"宿主已知工具名"全集，见
    /// `lib.rs::known_tools` 文档）用——当前**全部**已连接 MCP server 的全部工具
    /// 名，格式化为 `mcp__<server>__<tool>`（与
    /// `capabilities::connectors::ConnectorsCapability::launch` 拼进 `--tools`
    /// 时用的同一命名约定）。有意不做 per-app 授权过滤：这里回答的是"宿主整体
    /// 认不认识这个工具名"，不是"某个 app 能不能看到它"——后者是
    /// `authorized_tools(app_connectors)` 的职责，两者语义不同，不能共用一个
    /// 方法。同样过一遍 `is_safe_name`（原因见该函数文档：第三方 server 自报的
    /// 工具名字节级不受宿主控制）——一个连 `--tools` 白名单都塞不进去的脏名字
    /// 不该被 known_tools 当作"已知"，允许某个技能的 `allowed-tools` 声明它。
    pub fn all_tool_names(&self) -> Vec<String> {
        self.conns
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(server_id, conn)| {
                conn.tools.iter().filter_map(move |t| {
                    let safe = crate::capabilities::connectors::is_safe_name(server_id)
                        && crate::capabilities::connectors::is_safe_name(&t.name);
                    safe.then(|| format!("mcp__{server_id}__{}", t.name))
                })
            })
            .collect()
    }

    /// per-app 授权解析（Task6）：安全关键的可见性隔离层——把"该 app 的清单
    /// 里声明了哪些 connector（category × access）"解析成"该 app 实际能看到
    /// 哪些已连接 MCP server 的哪些工具"。这是白名单式收窄：
    ///
    /// - 对每个已连接 server：它的 `category`（`ensure_server` 连接时记录进
    ///   `McpConn.category`）必须命中 `app_connectors` 中某一条的 `category`，
    ///   否则该 server 的工具整体不可见——category 不在清单里 = 对这个 app
    ///   完全不透明，不会漏出任何工具名。
    /// - 命中后按该条的 `access` 收窄：`Access::Read` **只**放行
    ///   `Danger::Read` 的工具——这是本任务要保证的安全属性，只读授权绝不能
    ///   看见任何 `Danger::Write` 工具；`Access::ReadWrite` 放行该 server 的
    ///   全部工具。
    /// - `app_connectors` 里声明的 category 没有任何已连接 server 匹配 →
    ///   静默贡献零工具，不报错。
    ///
    /// 这只是第一层可见性收窄（"看不见"）；Task7 会在 host 侧对"调用"再加一层
    /// 复核（"看得见也不代表能调"），两层合起来才是完整的授权闭环。
    pub fn authorized_tools(&self, app_connectors: &[ConnectorReq]) -> Vec<AuthedTool> {
        let conns = self.conns.lock().unwrap();
        let mut out = Vec::new();
        for (server_id, conn) in conns.iter() {
            let Some(req) = app_connectors.iter().find(|r| r.category == conn.category) else {
                continue;
            };
            for tool in &conn.tools {
                // P6-C：不直接信 `tool.danger`（那是握手时按 `Trust::Vetted`
                // 算的向后兼容缓存值）——现场用这个连接真实的 `conn.trust`
                // 重新分级，byo server 的读注解在这里被正确忽略。
                let danger =
                    classify_tool_with_trust(&tool.name, tool.annotation.as_deref(), conn.trust);
                let visible = match req.access {
                    Access::Read => danger == Danger::Read,
                    Access::ReadWrite => true,
                };
                if visible {
                    out.push(AuthedTool {
                        server: server_id.clone(),
                        tool: tool.name.clone(),
                        danger,
                        description: tool.description.clone(),
                        input_schema: tool.input_schema.clone(),
                    });
                }
            }
        }
        out
    }

    /// 对一个**已连接**的 server 实际发起一次 `tools/call` JSON-RPC 往返：发送
    /// 请求（复用握手时打开、一直保留的 `stdin`）、在同一个 `rx` 上按 id 匹配
    /// 读取响应（复用 Task5 握手的分帧/超时机制，见 `recv_call_response`），返回
    /// `result` 字段；server 返回 JSON-RPC `error`、server 未连接、或超时，均返回
    /// `Err`。
    ///
    /// 这是"真正执行工具"的唯一入口——`host_mcp_call` 只在通过二次授权复核 +
    /// 危险分级门（`Danger::Read`）之后才会调用它；`Danger::Write` 分支绝不
    /// 调用这个方法（见 `host_mcp_call` 文档）。
    ///
    /// 并发/一致性：见 `McpConn::call_gate` 文档——同一个 server 上的并发调用
    /// 会被串行化，不支持真正并发的 tools/call（本任务范围内的取舍）。
    pub async fn call_tool(
        &self,
        server_id: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let conn = {
            let conns = self.conns.lock().unwrap();
            conns.get(server_id).cloned()
        };
        let Some(conn) = conn else {
            return Err(format!("MCP server[{server_id}] 未连接，无法调用 {tool}"));
        };

        // 串行门：整个"发送 + 等待匹配响应"是一个不可分割的临界区（原因见
        // McpConn::call_gate 文档）。
        let _gate = conn.call_gate.lock().await;
        let id = conn.next_id.fetch_add(1, Ordering::SeqCst);

        {
            let mut stdin = conn.stdin.lock().await;
            send_line(
                &mut stdin,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": { "name": tool, "arguments": args }
                }),
            )
            .await?;
        }
        conn.call_count.fetch_add(1, Ordering::SeqCst);

        let resp = {
            let mut rx = conn.rx.lock().await;
            recv_call_response(&mut rx, id, server_id, "tools/call").await?
        };

        if let Some(err) = resp.get("error") {
            return Err(format!(
                "MCP server[{server_id}] tools/call({tool}) 返回 error：{err}"
            ));
        }

        Ok(resp
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// 登记一条待确认的 Maker 安装（P4 T5，见 `pending_installs` 字段文档）：
    /// mint 一个新 confirm_id（与 P6-C 之前的 MCP 写确认登记表共用过同一个
    /// `confirm_counter`，格式同为 `"confirm-{n}"`）、插入
    /// `pending_installs`、返回该 id。`maker::handle_maker_request` 的
    /// `__host_maker_install__` 分支在 `pkg::load_and_validate` 校验通过
    /// **之后**才调用它——校验失败绝不注册（fail-closed，见该分支文档）。
    pub(crate) fn register_pending_install(&self, draft_dir: PathBuf, trusted: bool) -> String {
        let n = self.confirm_counter.fetch_add(1, Ordering::SeqCst);
        let confirm_id = format!("confirm-{n}");
        self.pending_installs
            .lock()
            .unwrap()
            .insert(confirm_id.clone(), PendingInstall { draft_dir, trusted });
        confirm_id
    }

    /// 取出（并从登记表移除）一条待确认的 Maker 安装；未知/已被消费过的
    /// confirm_id 返回 `None`。`maker::resolve_install` 用它续行（`allow=true`
    /// 时据此调 `install::install_or_upgrade`）或丢弃（`allow=false`）一次
    /// 待确认安装——移除是必须的，防止同一个 confirm_id 被重复消费（重放
    /// 安装两次）。
    pub(crate) fn take_pending_install(&self, confirm_id: &str) -> Option<PendingInstall> {
        self.pending_installs.lock().unwrap().remove(confirm_id)
    }

    /// 只读列出当前所有待确认的 Maker 安装：`(confirm_id, draft_dir)` 快照，
    /// **不移除、不消费**——与 `take_pending_install` 是两个不同的读写语义
    /// （前者只读查询供 P4 T5b 前端确认面轮询展示，后者才是唯一真正的消费
    /// 入口）。`register_pending_install` 只登记了 `draft_dir` 路径，没有缓存
    /// 清单内容（`display_name`/权限人话预览），所以这里只把路径原样交回，
    /// 解析清单是 `maker::list_pending_installs` 的事——`mcp` 模块不反向依赖
    /// `pkg`/`permissions`，同其余方法保持的依赖方向一致。
    pub(crate) fn pending_install_dirs(&self) -> Vec<(String, PathBuf)> {
        self.pending_installs
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| (id.clone(), p.draft_dir.clone()))
            .collect()
    }

    /// `__host_mcp_call__` 路由（Task7，**安全强制点**）：sandboxed app 发起的
    /// 每一次 MCP 工具调用都必须经过这里，不能绕过。三步：
    ///
    /// 1. **二次授权复核（纵深防御）**：不信任"in-pi `mcp-bridge` 扩展只会暴露
    ///    经过授权过滤的工具"这件事本身——那个扩展跑在被沙箱化的 app 侧，可能被
    ///    篡改绕过。host 侧用同一份 `authorized_tools(app_connectors)` 逻辑现场
    ///    重新算一遍 `(server, tool)` 是否在授权集合里；不在 → `Denied`，不论
    ///    请求是怎么"看起来合法"地打到这里的。
    /// 2. **危险分级门**：复用 Step1 顺手带出的 `AuthedTool.danger`（不必再回查
    ///    一次 `server_tools`）——`Danger::Read` 直接 `call_tool` 转发执行并
    ///    返回 `Ok(result)`；`Danger::Write` 分两种情形（P6-C 起改经
    ///    `approvals::ApprovalStore` 持久化，取代此前进程内存的
    ///    `pending`/`always_allow` 两张表——见该模块文档）：
    ///    - 该 `(app_id, server, tool)` 已被一条放行规则覆盖
    ///      （`ApprovalStore::is_allowed`，用户此前在审批中心对同一工具确认时
    ///      勾选过"总是允许"，`add_rule` 落了盘）——跳过 `PendingConfirm`，直接
    ///      `call_tool` 执行并返回 `Ok(result)`，审计 verdict 记为 `"rule"`
    ///      （区别于手动一次性确认的 `"executed"`，便于事后审查哪些是自动放行
    ///      的，见 spec §4 流程4）。
    ///    - 否则登记一条暂存调用（`ApprovalStore::stage`）并返回
    ///      `PendingConfirm(confirmId)`，真正执行留给通知中心/审批中心的
    ///      验收→resume 流程（`notifications::NotificationStore::respond_confirm`/
    ///      `respond_staged`）。
    /// 3. **审计**：各分支都调 `audit::record`，`verdict` 分别是 `"allowed"`
    ///    （只读已执行）/`"denied"`（未授权）/`"pending"`（写操作待确认）/
    ///    `"rule"`（写操作命中放行规则直接执行）；`args` 直接把调用方传入的
    ///    原始 `serde_json::Value` 字符串化后交给 `audit::record`，脱敏由 audit
    ///    自身负责（`audit::redact`），这里不做任何提前脱敏/跳过脱敏的手脚。
    ///    审计写入是 best-effort——失败不影响本函数的返回值（同 P2
    ///    `session_mgr.rs` 对 `audit::record` 的处理方式）。
    ///
    /// 边界情况：`call_tool` 本身失败（server 未连接/崩溃/协议错误）、以及
    /// `ApprovalStore::stage` 落盘失败，都不属于"未授权"，但 `McpCallResult`
    /// 只有 `Ok`/`Denied`/`PendingConfirm` 三个变体（无通用错误变体）——这里
    /// 复用 `Denied(reason)` 承载执行/落盘失败的错误信息，审计 verdict 记为
    /// `"error"` 以便和真正的未授权拒绝（`"denied"`）区分；`stage` 失败时
    /// fail-closed（不放行、不假装暂存成功），与 Global Constraints「`take`
    /// 失败 = 不执行」同一哲学的对称面。
    pub async fn host_mcp_call(
        &self,
        app_id: &str,
        app_connectors: &[ConnectorReq],
        server: &str,
        tool: &str,
        args: serde_json::Value,
        layout: &DataLayout,
    ) -> McpCallResult {
        let authed = self.authorized_tools(app_connectors);
        let Some(matched) = authed.iter().find(|t| t.server == server && t.tool == tool) else {
            let _ = crate::audit::record(layout, app_id, tool, &args.to_string(), "denied");
            return McpCallResult::Denied("unauthorized".to_string());
        };

        match matched.danger {
            Danger::Read => match self.call_tool(server, tool, args.clone()).await {
                Ok(result) => {
                    let _ =
                        crate::audit::record(layout, app_id, tool, &args.to_string(), "allowed");
                    McpCallResult::Ok(result)
                }
                Err(e) => {
                    let _ = crate::audit::record(layout, app_id, tool, &args.to_string(), "error");
                    McpCallResult::Denied(e)
                }
            },
            Danger::Write => {
                let store = crate::approvals::ApprovalStore::new(layout.clone());
                if store.is_allowed(app_id, server, tool) {
                    match self.call_tool(server, tool, args.clone()).await {
                        Ok(result) => {
                            let _ = crate::audit::record(
                                layout,
                                app_id,
                                tool,
                                &args.to_string(),
                                "rule",
                            );
                            McpCallResult::Ok(result)
                        }
                        Err(e) => {
                            let _ = crate::audit::record(
                                layout,
                                app_id,
                                tool,
                                &args.to_string(),
                                "error",
                            );
                            McpCallResult::Denied(e)
                        }
                    }
                } else {
                    match store.stage(
                        app_id,
                        server,
                        tool,
                        args.clone(),
                        crate::approvals::unix_now(),
                    ) {
                        Ok(confirm_id) => {
                            let _ = crate::audit::record(
                                layout,
                                app_id,
                                tool,
                                &args.to_string(),
                                "pending",
                            );
                            McpCallResult::PendingConfirm(confirm_id)
                        }
                        Err(e) => {
                            let _ = crate::audit::record(
                                layout,
                                app_id,
                                tool,
                                &args.to_string(),
                                "error",
                            );
                            McpCallResult::Denied(e)
                        }
                    }
                }
            }
        }
    }
}

/// `host_mcp_call` 的返回结果：`Ok` 已执行完成的 `Danger::Read`（或命中放行
/// 规则的 `Danger::Write`）工具结果；`Denied` 未通过二次授权复核（或——见
/// `host_mcp_call` 文档边界情况——执行期/暂存落盘失败），`String` 是拒绝/失败
/// 原因；`PendingConfirm` 是已经过 `approvals::ApprovalStore::stage` 持久化、
/// 尚未执行的 `Danger::Write` 调用，`String` 是供通知中心/审批中心用来查找/
/// resume 这次调用的 confirmId（即 `StagedCall.id`）。
#[derive(Debug, Clone, PartialEq)]
pub enum McpCallResult {
    Ok(serde_json::Value),
    Denied(String),
    PendingConfirm(String),
}

/// 一条登记在 `McpManager::pending_installs` 里的待确认 Maker 安装（P4 T5，
/// 专用 install-confirm seam，见该字段文档"执行期决策"）：`maker::resolve_install`
/// 消费——`allow=true` 时据此调用
/// `install::install_or_upgrade(&draft_dir, layout, registry, trusted)`，
/// `allow=false` 时直接丢弃、不安装。`trusted` 恒为 `false`
/// （`maker::handle_maker_request` 的 `__host_maker_install__` 分支写死）：
/// Maker subagent 的产出是未受信来源，绝不能借这条 seam 免检 P2 沙盒/受限
/// 模式。字段 `pub`（而非 `pub(crate)`）——`maker` 模块需要构造/读取整个结构体
/// （而不只是同 crate 可见）：`McpManager::register_pending_install` 与
/// `maker::resolve_install` 之间需要把它当作跨模块的普通数据传递。
#[derive(Debug, Clone)]
pub(crate) struct PendingInstall {
    pub draft_dir: PathBuf,
    pub trusted: bool,
}

/// spawn 一个 MCP server 子进程并完成 `initialize` → `tools/list` 握手，返回
/// 缓存好工具列表的 `McpConn` + 子进程句柄（后者交给崩溃看护任务持有，负责
/// `wait()`/退避重启，见 `McpManager::spawn_crash_monitor`）。
async fn spawn_and_handshake(cfg: &ServerConfig) -> Result<(McpConn, Child), String> {
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args);
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 同 P0 rpc.rs：RpcSession/McpManager 被 drop 时兜底杀掉子进程，不留孤儿。
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn MCP server[{}] 失败：{e}", cfg.id))?;
    let pid = child.id();
    let stdin = child.stdin.take().ok_or("MCP server 无 stdin")?;
    let stdout = child.stdout.take().ok_or("MCP server 无 stdout")?;
    let stderr = child.stderr.take();

    // stderr 排空：同 P0 rpc.rs 的理由——管道缓冲区写满会阻塞子进程写 stderr；
    // 这里不做诊断识别，纯读走丢弃。
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
    }

    // stdout 读循环：LF 分帧（`jsonl::FrameBuffer`，与 P0 rpc.rs 同帧格式），
    // 逐行解析为 JSON 转发到 channel；握手在这里 `.recv()` 两次取
    // initialize/tools/list 的响应。
    let (tx, mut rx) = mpsc::channel::<serde_json::Value>(64);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut fb = crate::jsonl::FrameBuffer::default();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for line in fb.push(&chunk[..n]) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                            if tx.send(v).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
    });

    let mut stdin = stdin;
    send_line(
        &mut stdin,
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
    )
    .await?;
    let init_resp = recv_matching_response(&mut rx, &mut child, 1, &cfg.id, "initialize").await?;
    // Minor 3：initialize 响应里若带 `error` 字段，说明 server 自己报告了初始化
    // 失败，不能当成功握手放行——否则后续 tools/list 大概率也不可用，或者
    // 干脆就没有工具，静默吞掉这个 error 会让调用方以为一切正常。
    if let Some(err) = init_resp.get("error") {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(format!(
            "MCP server[{}] initialize 返回 error：{err}",
            cfg.id
        ));
    }

    send_line(
        &mut stdin,
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await?;
    let list_resp = recv_matching_response(&mut rx, &mut child, 2, &cfg.id, "tools/list").await?;
    let tools = parse_tools(&list_resp)?;

    // `rx` 有意不在这里丢弃：握手只消费了 id=1/2，之后 Task7 `call_tool` 的
    // `tools/call` 响应帧还得从同一个 channel 里继续读。丢弃 `rx` 会让上面
    // reader 任务下一次 `tx.send(..).await` 因接收端已关闭而返回 `Err`，
    // 导致该任务提前退出、之后再也读不到这个 server 的任何响应。
    Ok((
        McpConn {
            stdin: AsyncMutex::new(stdin),
            rx: AsyncMutex::new(rx),
            call_gate: AsyncMutex::new(()),
            next_id: AtomicI64::new(3), // 1、2 被握手占用
            call_count: AtomicUsize::new(0),
            tools,
            pid,
            category: cfg.category.clone(),
            trust: cfg.trust,
        },
        child,
    ))
}

async fn send_line(stdin: &mut ChildStdin, value: serde_json::Value) -> Result<(), String> {
    let line = format!("{value}\n");
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("写 MCP server stdin 失败：{e}"))?;
    stdin.flush().await.map_err(|e| format!("flush 失败：{e}"))
}

/// 从响应 channel 里循环取值，直到取到与 `expected_id` 匹配的帧（Minor 3）：
/// 跳过任何没有匹配 `id` 的帧（例如 server 主动推送的、没有 `id` 字段的
/// notification/日志行），而不是"按到达顺序把第 N 条当第 N 个请求的响应"。
/// channel 关闭（子进程退出/`rx` 另一端被 drop）时返回 `None`。不含超时——
/// 超时由调用方（`recv_matching_response`/`recv_call_response`）套一层
/// `tokio::time::timeout` 负责，因为握手和 `tools/call` 在超时后的善后动作不同
/// （前者能 kill 子进程，后者拿不到 `Child` 句柄）。
async fn wait_for_id_match(
    rx: &mut mpsc::Receiver<serde_json::Value>,
    expected_id: i64,
) -> Option<serde_json::Value> {
    loop {
        match rx.recv().await {
            Some(v) => {
                if v.get("id").and_then(|i| i.as_i64()) == Some(expected_id) {
                    return Some(v);
                }
                // id 不匹配（含无 id 的 notification）：不是我们等的这条回复，跳过继续等。
            }
            None => return None,
        }
    }
}

/// 握手专用：从握手 channel 里取出与 `expected_id` 匹配的 JSON-RPC 响应帧
/// （核心匹配逻辑见 `wait_for_id_match`）：
/// - 整体套一层 `HANDSHAKE_TIMEOUT` 超时（Important 2）：超时说明 server 卡死
///   不回应，杀掉子进程（`kill` + `wait` 回收，避免留孤儿/僵尸）后返回 `Err`；
/// - channel 提前关闭（子进程提前退出）也返回 `Err`，但此时子进程已经退出，
///   不需要再 kill。
async fn recv_matching_response(
    rx: &mut mpsc::Receiver<serde_json::Value>,
    child: &mut Child,
    expected_id: i64,
    server_id: &str,
    step: &str,
) -> Result<serde_json::Value, String> {
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, wait_for_id_match(rx, expected_id)).await {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(format!("MCP server[{server_id}] {step} 无响应（提前退出）")),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(format!(
                "MCP server[{server_id}] {step} 握手超时（{HANDSHAKE_TIMEOUT:?}），已杀掉子进程"
            ))
        }
    }
}

/// Task7 `call_tool` 专用：`recv_matching_response` 的姊妹函数，同样复用
/// `wait_for_id_match` 的按 id 匹配逻辑，但握手结束后 `Child` 句柄已经转交给
/// `spawn_crash_monitor` 那个任务持有——这里拿不到 `&mut Child`，超时后无法
/// `kill`，只能返回 `Err`（子进程是否真的卡死留给崩溃看护/下一次调用的超时
/// 自然暴露）。
async fn recv_call_response(
    rx: &mut mpsc::Receiver<serde_json::Value>,
    expected_id: i64,
    server_id: &str,
    step: &str,
) -> Result<serde_json::Value, String> {
    match tokio::time::timeout(TOOL_CALL_TIMEOUT, wait_for_id_match(rx, expected_id)).await {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(format!(
            "MCP server[{server_id}] {step} 无响应（连接已关闭）"
        )),
        Err(_) => Err(format!(
            "MCP server[{server_id}] {step} 超时（{TOOL_CALL_TIMEOUT:?}）"
        )),
    }
}

/// 把 `tools/list` 响应的 `result.tools` 解析为 `Vec<ToolInfo>`：danger 推导
/// 规则见 `McpManager::ensure_server` 文档——`annotations.readOnlyHint` 的
/// `true`/`false`/缺失 分别映射为 `classify_tool` 的注解实参
/// `Some("readOnly")`/`Some("write")`/`None`。`inputSchema` 原样保留（缺失时
/// `serde_json::Value` 的 `Index` 对不存在的 key 返回 `Value::Null`，即
/// `.clone()` 后自然落回 `Null`，不需要额外的 `Option` 处理）。
fn parse_tools(list_resp: &serde_json::Value) -> Result<Vec<ToolInfo>, String> {
    let tools = list_resp["result"]["tools"]
        .as_array()
        .ok_or_else(|| format!("tools/list 响应格式不对（缺 result.tools 数组）：{list_resp}"))?;

    Ok(tools
        .iter()
        .map(|t| {
            let name = t["name"].as_str().unwrap_or_default().to_string();
            let description = t["description"].as_str().unwrap_or_default().to_string();
            let input_schema = t["inputSchema"].clone();
            let annotation = match t["annotations"]["readOnlyHint"].as_bool() {
                Some(true) => Some("readOnly".to_string()),
                Some(false) => Some("write".to_string()),
                None => None,
            };
            let danger = classify_tool_with_trust(&name, annotation.as_deref(), Trust::Vetted);
            ToolInfo {
                name,
                description,
                danger,
                input_schema,
                annotation,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用例 1：read_file（read 前缀）→ Read
    #[test]
    fn test_read_file_without_annotation() {
        assert_eq!(
            classify_tool_with_trust("read_file", None, Trust::Vetted),
            Danger::Read
        );
    }

    /// 测试用例 2：write_file（write 前缀）→ Write
    #[test]
    fn test_write_file_without_annotation() {
        assert_eq!(
            classify_tool_with_trust("write_file", None, Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 3：unknown_op（无前缀匹配）→ Write（保守默认）
    #[test]
    fn test_unknown_op_conservative_default() {
        assert_eq!(
            classify_tool_with_trust("unknown_op", None, Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 4：delete_file + readOnly 注解 → Read（注解优先）
    #[test]
    fn test_annotation_overrides_write_prefix() {
        assert_eq!(
            classify_tool_with_trust("delete_file", Some("readOnly"), Trust::Vetted),
            Danger::Read
        );
    }

    /// 测试用例 5：list_users（list 前缀）→ Read
    #[test]
    fn test_list_prefix_is_read() {
        assert_eq!(
            classify_tool_with_trust("list_users", None, Trust::Vetted),
            Danger::Read
        );
    }

    /// 测试用例 6：大小写不敏感测试：LIST_ITEMS → Read
    #[test]
    fn test_case_insensitive_prefix_matching() {
        assert_eq!(
            classify_tool_with_trust("LIST_ITEMS", None, Trust::Vetted),
            Danger::Read
        );
        assert_eq!(
            classify_tool_with_trust("CREATE_USER", None, Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 7：注解大小写不敏感
    #[test]
    fn test_annotation_case_insensitive() {
        assert_eq!(
            classify_tool_with_trust("unknown_op", Some("READONLY"), Trust::Vetted),
            Danger::Read
        );
        assert_eq!(
            classify_tool_with_trust("unknown_op", Some("Read-Only"), Trust::Vetted),
            Danger::Read
        );
        assert_eq!(
            classify_tool_with_trust("unknown_op", Some("ReAdOnLy"), Trust::Vetted),
            Danger::Read
        );
    }

    /// 测试用例 8：search 前缀 → Read
    #[test]
    fn test_search_prefix_is_read() {
        assert_eq!(
            classify_tool_with_trust("search_documents", None, Trust::Vetted),
            Danger::Read
        );
    }

    /// 测试用例 9：exec 前缀 → Write
    #[test]
    fn test_exec_prefix_is_write() {
        assert_eq!(
            classify_tool_with_trust("exec_command", None, Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 10：fetch 前缀 → Read
    #[test]
    fn test_fetch_prefix_is_read() {
        assert_eq!(
            classify_tool_with_trust("fetch_data", None, Trust::Vetted),
            Danger::Read
        );
    }

    /// 测试用例 11：query 前缀 → Read
    #[test]
    fn test_query_prefix_is_read() {
        assert_eq!(
            classify_tool_with_trust("query_db", None, Trust::Vetted),
            Danger::Read
        );
    }

    /// 测试用例 12：send 前缀 → Write
    #[test]
    fn test_send_prefix_is_write() {
        assert_eq!(
            classify_tool_with_trust("send_email", None, Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 13：remove 前缀 → Write
    #[test]
    fn test_remove_prefix_is_write() {
        assert_eq!(
            classify_tool_with_trust("remove_file", None, Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 14：update 前缀 → Write
    #[test]
    fn test_update_prefix_is_write() {
        assert_eq!(
            classify_tool_with_trust("update_record", None, Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 15：put 前缀 → Write
    #[test]
    fn test_put_prefix_is_write() {
        assert_eq!(
            classify_tool_with_trust("put_object", None, Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 16：前缀边界检查（readFile 应匹配 read 前缀）
    #[test]
    fn test_prefix_boundary_camelcase() {
        // readFile 以 "read" 开头，但紧跟大写字母
        // 需要确认我们的边界检查是否处理驼峰命名
        assert_eq!(
            classify_tool_with_trust("readFile", None, Trust::Vetted),
            Danger::Read
        );
    }

    /// 测试用例 17：write 前缀覆盖的注解测试
    #[test]
    fn test_write_annotation_overrides_read_prefix() {
        assert_eq!(
            classify_tool_with_trust("get_data", Some("write"), Trust::Vetted),
            Danger::Write
        );
    }

    /// 测试用例 18（回归）：readwrite/read-write 注解含 "read" 子串但是写操作，须判 Write。
    /// 防安全欠分类（写工具被 contains("read") 误判为只读而绕过写确认门）。
    #[test]
    fn test_readwrite_annotation_is_write() {
        assert_eq!(
            classify_tool_with_trust("get_data", Some("readwrite"), Trust::Vetted),
            Danger::Write
        );
        assert_eq!(
            classify_tool_with_trust("read_file", Some("read-write"), Trust::Vetted),
            Danger::Write
        );
    }

    // ---- P6-C Task1: 连接器信任分级 byo|vetted ----

    #[test]
    fn byo_readonly_annotation_cannot_downgrade_unknown_tool() {
        assert_eq!(
            classify_tool_with_trust("frobnicate", Some("readOnly"), Trust::Byo),
            Danger::Write
        );
        assert_eq!(
            classify_tool_with_trust("frobnicate", Some("readOnly"), Trust::Vetted),
            Danger::Read
        );
    }

    #[test]
    fn byo_write_annotation_still_escalates_read_prefixed_tool() {
        assert_eq!(
            classify_tool_with_trust("get_thing", Some("destructive"), Trust::Byo),
            Danger::Write
        );
    }

    #[test]
    fn byo_without_annotation_uses_prefix_table() {
        assert_eq!(
            classify_tool_with_trust("list_items", None, Trust::Byo),
            Danger::Read
        );
        assert_eq!(
            classify_tool_with_trust("send_mail", None, Trust::Byo),
            Danger::Write
        );
    }

    #[test]
    fn server_config_without_trust_field_deserializes_as_byo() {
        let c: ServerConfig = serde_json::from_str(
            r#"{"id":"s","category":"c","command":"x","args":[],"env":{},"transport":"stdio"}"#,
        )
        .unwrap();
        assert_eq!(c.trust, Trust::Byo);
    }

    // ---- Task9: parse_tools 保留 inputSchema ----

    #[test]
    fn parse_tools_captures_input_schema() {
        let list_resp = serde_json::json!({
            "result": {
                "tools": [{
                    "name": "read_file",
                    "description": "d",
                    "inputSchema": { "type": "object", "properties": { "path": { "type": "string" } } },
                    "annotations": { "readOnlyHint": true }
                }]
            }
        });
        let tools = parse_tools(&list_resp).expect("解析应成功");
        assert_eq!(
            tools[0].input_schema,
            serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } } })
        );
    }

    #[test]
    fn parse_tools_missing_input_schema_defaults_to_null() {
        let list_resp = serde_json::json!({
            "result": { "tools": [{ "name": "x", "description": "d" }] }
        });
        let tools = parse_tools(&list_resp).expect("解析应成功");
        assert_eq!(tools[0].input_schema, serde_json::Value::Null);
    }
}
