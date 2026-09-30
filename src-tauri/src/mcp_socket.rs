// 宿主侧 MCP unix socket 监听器（Task9b）：Task8 在 `hosttools/mcp_transport.ts`
// 里实现了 CLIENT 端（in-pi `mcp-bridge` 扩展经 `SUPERAGENT_MCP_SOCKET` 连接这个
// socket），P6-A 的 `CapabilityRegistry::launch`（经 `session_mgr::assemble_launch_plan`
// 拼进最终启动计划）算出了每个 app 的 `socket_path`（`DataLayout::mcp_socket_path`）
// 并经 env 注入给该 app 的 pi 子进程——但当时都还没有真正在这个路径上
// `bind`/`listen` 的宿主端。本模块补上这一半：起一个 `tokio::net::UnixListener`，
// 收 `mcp_transport.ts` 那种"连一次、写一行 JSON 请求、读一行 JSON 响应、关连接"
// 的调用，解析出 `method`/`params` 后转发给 `CapabilityRegistry::dispatch`（P6-A）。
//
// ## 线协议/分发（必须与 `mcp_transport.ts` 字节对齐，见该文件头部注释）
//
// 客户端每次调用新开一条连接，写一行 `{"method":..., "params":{...}}\n`，读
// 一行响应后关闭连接。`process_request` 自己不再判断"这个 method 属于哪类
// 能力、这个 app 有没有资格调"——那套判断现在统一收在
// `CapabilityRegistry::dispatch`（P6-A，见 `capability.rs`）：认领了该
// `method` 的能力若未被这个 app 的 `Permissions`/`CallerIdentity` 声明，回
// `{"ok":false,"error":"unauthorized: 该应用未声明能力 <key>"}`；没有能力认领
// 这个 `method`，回 `{"ok":false,"error":"未知的宿主方法 <method>"}`；声明了
// 就路由给对应 `Capability::handle`，返回值原样当整条响应写回 client——各能力
// 自己的返回形状不统一套用同一套编码（`__host_mcp_call__` 走
// `{result:...}`/`{error:...}`，maker/notify 等走 `{ok, ...}`），`process_request`
// 不再关心这些差异。
//
// `__host_mcp_call__` 由 `capabilities::connectors::ConnectorsCapability`
// 认领，其 `handle` 转发给 Task7 的安全强制点 `McpManager::host_mcp_call`，再
// 用本模块的 `encode_result` 把 `McpCallResult` 编码成 `mcp_transport.ts` 已经
// 能解码的形状：
// - `Ok(value)` → `{"result": value}`；
// - `Denied(msg)` → `{"error": msg}`（客户端据此 reject，`mcp_bridge.ts` 的
//   `execute()` 把这个原因包一层"MCP 工具 X 调用失败：msg"再抛出，模型看到的
//   是一次工具调用失败，符合"未授权/执行出错就是硬失败"的语义）；
// - `PendingConfirm(confirmId)` → `{"result": {"pending_confirm": true,
//   "confirm_id": confirmId, "message": "此写操作已暂存，等待用户在审批中心
//   批准；批准后执行结果会以宿主消息送回本会话。请不要重试同一操作，继续处理
//   其它工作。"}}`（P6-C：文案改为如实说明"待批"，不再暗示"已提交"这种听起来
//   像已经生效的措辞——见
//   `docs/superpowers/specs/2026-09-02-p6c-approval-trust-design.md` §8 裁决2）
//   ——刻意落在 `result` 分支而不是 `error` 分支：这不是失败，`mcp_bridge.ts` 的
//   `execute()` 会把这个对象原样当工具执行结果返回给模型（`JSON.stringify` 进
//   `content[0].text`），模型能读到这句如实说明"待批、结果会经宿主消息回送、
//   不要重试"的话并据此继续处理其它工作，而不是把一次待确认的写操作误判成
//   报错，也不会误以为它已经执行成功。这一形状与 `Ok(read_file)`
//   的形状（`{content:[...], isError:false}`，来自真实 MCP server 的
//   `tools/call` 结果）明显不同，调用方可以按需要区分（本任务的测试也断言了
//   这一点），但严格来说 `mcp_transport.ts`/`mcp_bridge.ts` 当前都不需要显式
//   分支判断——两者都只是"resolve 出来的值原样转述给模型"，`pending_confirm`
//   字段本身就是说给模型/用户听的，不需要代码逻辑分支。
//   `mcp_transport.ts`/`mcp_bridge.ts` 均未改动——现有解码规则已经足以承载这个
//   形状，不需要为此调整那两个文件。
//
// ## app 身份/权限绝不来自线上请求
//
// socket 是 per-app 的（`DataLayout::mcp_socket_path` 按 app_id 分路径）：一个
// app 的身份（`CallerIdentity{app_id,trusted,depth}`）与清单权限
// （`Permissions`）由"它连的是哪个 socket"决定，
// `McpSocketListener::start_with_identity` 创建时就把两者捕获死了。请求体里
// 即使塞了 `app_id`/`trusted`/权限等字段（例如被篡改的 in-pi 扩展试图冒充
// 另一个 app 或自我提权），`process_request` 解析请求时压根不读这些字段——
// `CapabilityRegistry::dispatch` 拿到的永远是监听器绑定时那份 `identity`/
// `perms`，不是线上任何声称的值。这是纵深防御的最后一环：即使某个
// `Capability::handle` 内部的二次授权复核本身没被绕过，如果这里误信了线上
// 自称的身份，整条防线也会从"这个 socket 属于哪个 app、它声明了什么权限"
// 这个环节被击穿。
use crate::capability::{CallCtx, CallerIdentity, CapabilityRegistry};
use crate::mcp::{McpCallResult, McpManager};
use crate::paths::DataLayout;
use crate::permissions::{ConnectorReq, Permissions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

/// 单次请求体的硬上限（P3 review：DoS 加固）：MCP 工具调用的 `args` 里可能
/// 内联一份文件的完整内容（例如 `write_file` 把新内容整份塞进参数），所以不
/// 能设得太小；但也必须是一个有限值——`handle_conn` 用 `read_line` 读一整行
/// JSON 请求，若不对这次读取设上限，一个恶意/异常客户端发一行不带 `\n` 的
/// 巨量字节流、且连接不关闭，会让处理这条连接的缓冲区无限增长下去；宿主
/// 进程是所有 app 共享的单一进程，这不只是这个 app 自己的问题，而是整个
/// 宿主的内存耗尽。16 MiB 对绝大多数"内联文件内容"级别的参数已经足够宽裕，
/// 明显更大的文件本就不适合整份塞进单次 JSON-RPC 请求。
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// 单次请求"读到完整一行"的超时上限（P3 review：DoS 加固）。合法客户端
/// （`mcp_transport.ts` 的 `hostMcpCall`）连接后立刻整行写出请求 JSON——走的
/// 是本机 unix socket，没有网络延迟，正常情况下应在毫秒级完成；5s 对系统
/// 繁忙/调度抖动已经相当宽裕。设这个上限是为了防一个只连接不发送数据（或
/// 只发一半就再也不发）的恶意/异常客户端，把处理它的 tokio 任务永久挂起——
/// `read_line` 本身不带超时语义，不包一层 `tokio::time::timeout` 就会一直
/// `.await` 下去，即使不占内存也占着一个任务/文件描述符不放。
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// accept 出错时的退避步长与封顶步数（P3 review：DoS 加固）：连续出错时，
/// 退避时长按 `ACCEPT_ERROR_BACKOFF_STEP * min(连续出错次数,
/// ACCEPT_ERROR_BACKOFF_MAX_STEPS)` 线性增长。避免一次瞬时的 fd 耗尽风暴
/// （例如宿主进程整体 fd 用尽导致 `accept()` 返回 `EMFILE`）让 accept 循环
/// 疯狂自旋——忙等本身还会消耗 CPU/调度，让 fd 耗尽更难恢复；但也不会无限
/// 拉长等待——瞬时故障消退后循环很快就会恢复正常 accept。
const ACCEPT_ERROR_BACKOFF_STEP: Duration = Duration::from_millis(50);
const ACCEPT_ERROR_BACKOFF_MAX_STEPS: u32 = 20;

/// 一个已启动的、per-app 的 MCP socket 监听器：持有 accept 循环任务的句柄 +
/// 它 bind 的 socket 路径（供 `stop()` 清理）。`start()` 之后即已在监听，无需
/// 再调用别的方法启动。
pub struct McpSocketListener {
    socket_path: PathBuf,
    handle: JoinHandle<()>,
}

impl McpSocketListener {
    /// 在 `socket_path` 上起一个 unix socket 监听器，绑定到 `app_id` +
    /// `connectors`（即"这个 socket 属于哪个 app、这个 app 声明了哪些
    /// connector"）——这两者从此固定，不会因为线上请求的内容而改变（见模块
    /// 文档"app 身份/权限绝不来自线上请求"）。
    ///
    /// - 若 `socket_path` 已存在一个遗留文件（例如上次异常退出没清理干净），
    ///   先删除它再 `bind`——否则 `UnixListener::bind` 会因地址已被占用而失败
    ///   （`AddrInUse`）。
    /// - 父目录若不存在则先创建（`DataLayout::mcp_socket_path` 形如
    ///   `<root>/mcp/<app_id>/mcp.sock`，`mcp/<app_id>/` 这一层通常还没有人
    ///   建过）。
    /// - 每个 accepted 连接都单独起一个 `tokio::spawn` 处理（Task8 的客户端是
    ///   连接一次、调一次、就关闭——短连接，天然支持并发多个 in-flight 调用，
    ///   互不阻塞）。
    ///
    /// 向后兼容的构造：不带 P5 互联总线上下文（`hosttools_dir=None`、`depth=0`）、
    /// 不带完整身份/清单权限——只绑定 `connectors`（其余权限字段全落默认值，
    /// `trusted=false`）。供不关心这些的既有 MCP/Maker 测试与旧调用点原样使用。
    pub fn start(
        manager: McpManager,
        layout: DataLayout,
        app_id: String,
        connectors: Vec<ConnectorReq>,
        socket_path: PathBuf,
    ) -> std::io::Result<Self> {
        Self::start_with(manager, layout, app_id, connectors, socket_path, None, 0)
    }

    /// 完整构造（P5 互联总线版）：额外绑定
    /// - `hosttools_dir`：`handle_call_agent` 拉起被调方会话时需要（`Some` 才启用
    ///   `__host_call_agent__`；`None` 则该分支返回"未启用"错误）。
    /// - `depth`：这个监听器代表的调用深度（前台交互会话=0；被调方调用域监听器=
    ///   调用方 depth + 1）。深度绑定在监听器上、**绝不**取自线上请求（P5 §1.4）。
    ///
    /// P6-A：本函数现在是 `start_with_identity` 的薄封装——`identity` 只填
    /// `app_id`/`depth`（`trusted` 恒为 `false`），`perms` 只填 `connectors`
    /// （其余权限字段落默认值——`agents.call`/`system.notifications` 等一律
    /// 声明为空/关闭），`registry` 用进程内建的 `capabilities::builtin()`。
    /// 这与本函数改造前"只认 connectors"的既有行为完全一致：既有调用点
    /// （`session_mgr.rs` 两处、8 个测试文件）传的从来就只是 connectors，不
    /// 传其余权限字段，此处补的默认值与它们此前隐式依赖的空值分毫不差。
    pub fn start_with(
        manager: McpManager,
        layout: DataLayout,
        app_id: String,
        connectors: Vec<ConnectorReq>,
        socket_path: PathBuf,
        hosttools_dir: Option<PathBuf>,
        depth: u32,
    ) -> std::io::Result<Self> {
        let perms = Permissions {
            connectors,
            ..Default::default()
        };
        Self::start_with_identity(
            manager,
            layout,
            CallerIdentity {
                app_id,
                trusted: false,
                depth,
            },
            perms,
            socket_path,
            hosttools_dir,
            Arc::new(crate::capabilities::builtin()),
        )
    }

    /// P6-A 完整构造：绑定调用者完整身份（`CallerIdentity`，含 `trusted`）与
    /// 完整清单权限（`Permissions`，不只 `connectors`），以及本次要使用的
    /// `CapabilityRegistry`。这三者一旦 bind 就固定死在这个监听器上，线上请求
    /// 从此再也无法通过任何字段影响"我是谁、我声明了什么、按哪张能力表分发"
    /// （见模块文档"app 身份/权限绝不来自线上请求"）。
    ///
    /// `registry` 用 `Arc` 传入（而非监听器自己 `builtin()` 一份）：
    /// `notifications` 等能力内部持有跨连接共享状态（限速窗口），
    /// `session_mgr::open_app_after_acquire` 需要传 `AppState.capabilities`
    /// 这唯一一份进程级实例，若每个监听器各自新建一份注册表，同一 app 换个
    /// 监听器（比如重开）限速窗口就会被重置——调用方对这一点有充分控制权。
    pub fn start_with_identity(
        manager: McpManager,
        layout: DataLayout,
        identity: CallerIdentity,
        perms: Permissions,
        socket_path: PathBuf,
        hosttools_dir: Option<PathBuf>,
        registry: Arc<CapabilityRegistry>,
    ) -> std::io::Result<Self> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 清理遗留的 stale socket 文件：不检查是否"真的还有人在监听"，只要
        // 文件存在就先删——这条路径本来就是 per-app 专属、由本模块独占管理，
        // 不存在"文件属于别的合法监听者"的情况。
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path)?;
        let identity = Arc::new(identity);
        let perms = Arc::new(perms);
        let layout = Arc::new(layout);
        let hosttools_dir = Arc::new(hosttools_dir);

        let handle = tokio::spawn(async move {
            let mut consecutive_accept_errors: u32 = 0;
            loop {
                let (stream, _addr) = match listener.accept().await {
                    Ok(pair) => {
                        consecutive_accept_errors = 0;
                        pair
                    }
                    // accept 本身失败：不再永久 `break` 杀死这个 app 的 accept
                    // 循环（P3 review：修复前，一次瞬时的 fd 耗尽 `EMFILE` 会让
                    // 这个 app 的 MCP 从此再也连不上，直到整个 app 重启）。这里
                    // 只记录 + 退避一小段时间再继续；真正让循环结束的机制是
                    // `stop()` 的 `handle.abort()`（见下）——teardown 走的是任务
                    // 取消，不依赖 `accept()` 返回错误，所以不需要在这里区分
                    // "哪些 accept 错误才是真正致命的"，一律当瞬时错误处理、
                    // 退避后继续即可。
                    Err(e) => {
                        consecutive_accept_errors = consecutive_accept_errors.saturating_add(1);
                        eprintln!(
                            "mcp_socket: accept 出错（连续第 {consecutive_accept_errors} 次）：{e}，退避后继续监听"
                        );
                        let steps = consecutive_accept_errors.min(ACCEPT_ERROR_BACKOFF_MAX_STEPS);
                        tokio::time::sleep(ACCEPT_ERROR_BACKOFF_STEP * steps).await;
                        continue;
                    }
                };
                let manager = manager.clone();
                let layout = layout.clone();
                let identity = identity.clone();
                let perms = perms.clone();
                let hosttools_dir = hosttools_dir.clone();
                let registry = registry.clone();
                tokio::spawn(async move {
                    handle_conn(
                        stream,
                        &manager,
                        &registry,
                        &identity,
                        &perms,
                        &layout,
                        hosttools_dir.as_deref(),
                    )
                    .await;
                });
            }
        });

        Ok(Self {
            socket_path,
            handle,
        })
    }

    /// 停止监听：中止 accept 循环任务并删除 socket 文件。已在处理中的连接
    /// （各自独立的 `tokio::spawn` 任务）不受 `abort()` 影响，会自然跑完；
    /// 这里不等待它们，`open_app`/`close_app` 的生命周期粒度不需要这层同步。
    ///
    /// P3 review 提到可以顺手追踪这些per-连接任务的 `JoinHandle`，让 `stop()`
    /// 也能 `abort`/`await` 到它们、做到完全同步的 teardown——这里选择不做：
    /// 这些任务现在已经有了硬上限（读取阶段封顶在 `REQUEST_READ_TIMEOUT`，
    /// `host_mcp_call` 内部的握手/工具调用也各自有 `mcp.rs::HANDSHAKE_TIMEOUT`/
    /// `TOOL_CALL_TIMEOUT` 兜底），生命周期本就有限，不追踪也不会真的失控；
    /// 真要追踪又会引出新问题——"已跑完的任务何时从追踪集合里摘除"，不摘除
    /// 就是另一个无界增长点（这个模块这次要解决的正是无界增长），对当前收益
    /// 不成比例。授权语义完全不受影响：身份自始至终来自监听器绑定时的
    /// `identity`/`perms`，与这些任务是否被追踪无关。
    pub async fn stop(self) {
        self.handle.abort();
        // `abort()` 后 `.await` 该 handle 会得到 `Err`（JoinError::is_cancelled）
        // ——预期结果，不是真正的任务 panic，丢弃即可。
        let _ = self.handle.await;
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// 处理一条已 accept 的连接：读一行 JSON 请求（有大小上限 + 读超时，见
/// `MAX_REQUEST_BYTES`/`REQUEST_READ_TIMEOUT`）、转发给 `process_request`、写
/// 一行编码后的 JSON 响应、（因为 `stream` 在函数结束时被 drop）关闭连接——
/// 严格对应 `mcp_transport.ts::hostMcpCall` 的"一次调用一条连接"协议。
///
/// 任何读取/解析失败、超过大小上限、或读超时都静默返回（不 panic、不重试、
/// 不写任何响应）：`mcp_transport.ts` 那侧会因为连接被直接关闭而在
/// `"close"`/`"error"` 事件上拿到失败，属于"传输层故障"而非本模块要编码的
/// 三个 `McpCallResult` 分支之一。这个函数运行在独立的 `tokio::spawn` 任务
/// 里，提前 return 只丢弃这一条连接——既不影响 accept 循环，也不影响其它
/// 并发连接（P3 review：DoS 加固——恶意/异常的单条连接不能拖垮宿主或其它
/// app 的调用）。
async fn handle_conn(
    stream: UnixStream,
    manager: &McpManager,
    registry: &CapabilityRegistry,
    identity: &CallerIdentity,
    perms: &Permissions,
    layout: &DataLayout,
    hosttools_dir: Option<&Path>,
) {
    let (read_half, mut write_half) = stream.into_split();
    let reader = BufReader::new(read_half);
    // `.take(MAX_REQUEST_BYTES)`：一旦读满这个上限，`Take` 会表现得像遇到了
    // EOF（不再向底层 socket 要更多字节），`read_line` 因此会带着已读到的、
    // 不完整（没有 `\n` 结尾）的内容返回，而不是无限读下去——见
    // `MAX_REQUEST_BYTES` 处注释。外层再套一层 `REQUEST_READ_TIMEOUT`：大小
    // 和时间两个上限任一触发，这次读取都会尽快结束，不会无限期 `.await`。
    let mut limited = reader.take(MAX_REQUEST_BYTES as u64);
    let mut line = String::new();
    let read_outcome =
        tokio::time::timeout(REQUEST_READ_TIMEOUT, limited.read_line(&mut line)).await;
    match read_outcome {
        // 读超时：客户端连上了但迟迟凑不出完整一行，主动放弃这条连接。
        Err(_elapsed) => return,
        // 连接未写入任何完整行就关闭/出错。
        Ok(Ok(0)) | Ok(Err(_)) => return,
        Ok(Ok(_)) => {}
    }
    // 读满 `MAX_REQUEST_BYTES` 仍未凑出一行（没有 `\n` 结尾）：视为超大帧，
    // 直接丢弃这条连接，不尝试当 JSON 解析（反正也读不全，解析必然失败）。
    if !line.ends_with('\n') && line.len() >= MAX_REQUEST_BYTES {
        return;
    }

    let response = process_request(
        line.trim(),
        registry,
        identity,
        perms,
        manager,
        layout,
        hosttools_dir,
    )
    .await;
    let out = format!("{response}\n");
    let _ = write_half.write_all(out.as_bytes()).await;
}

/// 解析一行请求 JSON、交给 `CapabilityRegistry::dispatch` 按 `method` 找到
/// 认领它的能力并执行、把返回值原样当整条响应写回 client。请求体里的
/// `method`/`params` 取自线上 JSON；其余字段（`app_id`/`connectors`/权限等）
/// 一律忽略不读——身份/权限来自函数参数 `identity`/`perms`（监听器创建时
/// 绑定的），不是请求体（见模块文档"app 身份/权限绝不来自线上请求"）。
///
/// **拒绝也审计**（HANDOFF 已知坑：此前"被拒绝的 MCP/工具调用不产生审计
/// 记录，只审计确实执行了的"）：`dispatch` 因为"该能力未被这个 app 声明"而
/// 拒绝时，返回值形如 `{"ok":false,"error":"unauthorized: ..."}`——这里认
/// `error` 以 `"unauthorized"` 开头为准，写一条 `verdict="denied"` 的审计
/// 记录（`crate::audit::record`，best-effort，写失败不影响本次调用的响应）；
/// 落盘的 `args` 截断到至多 4096 字符——这条分支的 `params` 完全来自未经任何
/// 能力校验的 wire 内容，不截断的话一个没声明任何能力的 app 可以在每次必被拒
/// 的请求里塞任意大小的 `params`，把宿主共享的审计目录当放大器写爆。
/// 能力内部自身的成功/失败审计（例如 `connectors` 能力内 `host_mcp_call` 的
/// 二次授权复核）由各能力自己负责，不在这里重复处理——那些失败不是"未声明
/// 该能力"，是"声明了但这次具体调用被业务规则拒绝"，语义不同。
async fn process_request(
    line: &str,
    registry: &CapabilityRegistry,
    identity: &CallerIdentity,
    perms: &Permissions,
    manager: &McpManager,
    layout: &DataLayout,
    hosttools_dir: Option<&Path>,
) -> serde_json::Value {
    let parsed: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({ "error": format!("请求不是合法 JSON：{e}") }),
    };
    let method = parsed
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let params = parsed
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let ctx = CallCtx {
        layout,
        mcp: manager,
        hosttools_dir,
    };
    let out = registry
        .dispatch(&method, params.clone(), identity, perms, &ctx)
        .await;
    if out.get("ok") == Some(&serde_json::Value::Bool(false))
        && out
            .get("error")
            .and_then(|e| e.as_str())
            .map(|e| e.starts_with("unauthorized"))
            .unwrap_or(false)
    {
        // M-2（code review）：截断到至多 4096 字符再落审计——这条分支覆盖的是
        // "该应用未声明能力"的拒绝，此时 `params` 完全来自未经任何能力信任的
        // wire 内容（还没有任何 `Capability::handle` 校验/拒绝过它），一个没
        // 声明任何能力的 app 可以在每次被拒的请求里塞进任意大小的 `params`
        // （例如 `__host_notify__` 的 `body` 字段），驱动宿主为每次被拒请求都
        // 写一条几 MiB 的审计记录——审计目录是宿主共享资源，不该被一个连能力
        // 都没声明的调用方牵着写爆。截断只影响落盘的 `args` 字段，不影响返回
        // 给 client 的 `out`（拒绝原因本就与 `params` 内容无关）。
        let args: String = params.to_string().chars().take(4096).collect();
        let _ = crate::audit::record(layout, &identity.app_id, &method, &args, "denied");
    }
    out
}

/// 把 `McpCallResult` 编码成 `mcp_transport.ts` 能解码的 JSON 形状，见模块
/// 文档"线协议"一节。供 `capabilities::connectors::ConnectorsCapability`
/// 调用——`__host_mcp_call__` 的编码规则住在这里，本模块仍然拥有它，只是
/// 调用点从"手写的 `process_request` mcp 分支"变成了那个能力的 `handle`。
pub(crate) fn encode_result(result: McpCallResult) -> serde_json::Value {
    match result {
        McpCallResult::Ok(value) => serde_json::json!({ "result": value }),
        McpCallResult::Denied(reason) => serde_json::json!({ "error": reason }),
        McpCallResult::PendingConfirm(confirm_id) => serde_json::json!({
            "result": {
                "pending_confirm": true,
                "confirm_id": confirm_id,
                "message": "此写操作已暂存，等待用户在审批中心批准；批准后执行结果会以宿主消息送回本会话。请不要重试同一操作，继续处理其它工作。",
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // encode_result 是本模块最容易在集成测试里断言不到细节（比如"Denied 编码
    // 后 error 字段的值就是原始 reason，不做任何包装"）的一小块纯函数逻辑，
    // 单独钉一遍；端到端的 socket 往返覆盖见 tests/mcp_socket_it.rs。

    #[test]
    fn encode_ok_wraps_value_under_result_key() {
        let v = serde_json::json!({ "content": [{ "type": "text", "text": "hi" }] });
        let encoded = encode_result(McpCallResult::Ok(v.clone()));
        assert_eq!(encoded, serde_json::json!({ "result": v }));
    }

    #[test]
    fn encode_denied_wraps_reason_under_error_key_verbatim() {
        let encoded = encode_result(McpCallResult::Denied("unauthorized".to_string()));
        assert_eq!(encoded, serde_json::json!({ "error": "unauthorized" }));
    }

    #[test]
    fn encode_pending_confirm_is_result_shaped_not_error_and_carries_confirm_id() {
        let encoded = encode_result(McpCallResult::PendingConfirm("confirm-7".to_string()));
        assert!(encoded.get("error").is_none());
        let result = encoded
            .get("result")
            .expect("PendingConfirm 应落在 result 里");
        assert_eq!(result["pending_confirm"], serde_json::json!(true));
        assert_eq!(result["confirm_id"], serde_json::json!("confirm-7"));
        // P6-C 裁决2：回执如实说"待批"，不模拟成功——文案须同时含"暂存"（不是
        // "已提交"这种听起来像已生效的措辞）与"审批中心"（模型能据此转告用户
        // 去哪里处理），不再断言笼统的"确认"二字。
        let message = result["message"].as_str().unwrap();
        assert!(
            message.contains("暂存"),
            "message 应如实说明「待批」：{message}"
        );
        assert!(
            message.contains("审批中心"),
            "message 应指向审批中心：{message}"
        );
    }

    #[test]
    fn encode_pending_confirm_shape_differs_from_ok_shape() {
        let ok = encode_result(McpCallResult::Ok(serde_json::json!({ "content": [] })));
        let pending = encode_result(McpCallResult::PendingConfirm("c1".to_string()));
        assert_ne!(ok, pending);
    }
}
