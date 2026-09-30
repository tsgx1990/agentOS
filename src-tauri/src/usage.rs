//! P3 Task18 修复：per-app token 用量 + 真实花费，来源改为 pi 的
//! `get_session_stats` RPC 命令（`rpc::PiEvent::SessionStats`），不再是 Task16
//! 假设的、随 `agent_end` 到达的增量 `usage` 字段——该假设已用真实 pi v0.74.2
//! 证伪（`AgentEndEvent` 没有 `usage` 字段，真实用量只存在于
//! `AgentSession.getSessionStats()`，见 `rpc::PiEvent::SessionStats` 文档的完整
//! 取证记录）。
//!
//! **关键语义变化**：`SessionStats` 是该 pi 会话**当前累计**的 token/花费（不是
//! "这一轮新增了多少"），因此本模块不再"累加"，而是每次收到新的
//! `PiEvent::SessionStats` 就**覆盖**该 app 已记录的值——`set_latest` 是 set 不是
//! add。旧版 Task16 的 `accumulate`（把每次事件的数字加总）在新语义下是错的：
//! 如果真按旧代码对 cumulative 值继续累加，会把同一份累计值反复计入总数，用量
//! 越跑越假。
//!
//! `UsageAccumulator` 仍是 `app_state::AppState::usage` 上唯一共享的实例——
//! `session_mgr.rs`（per-app 事件循环）与 `lib.rs`（主会话事件循环、`app_usage`
//! 命令）都只应该拿这一份共享引用读写，不允许各自新建一份（新建第二份会导致
//! 彼此看不到对方已经写入的最新值）。

use std::collections::HashMap;
use tokio::sync::Mutex;

/// `#[tauri::command] app_usage` 的返回体：该 app（或某个 pi 会话）当前累计的
/// input/output token 数 + pi 真实报告的花费（`SessionStats.cost`）。
///
/// 不再有 `est_cost` 价格常量估算——pi 自己算的 `cost` 就是真实值，没有理由
/// 在宿主这边另起一套占位单价重算一遍粗糙估计值。未曾收到过任何
/// `get_session_stats` 响应的 app（或该会话尚未产生任何 token 用量）返回全零，
/// 这与"真的查过、结果是 0"是同一件事——调用方（`app_usage`/前端 SessionPanel）
/// 不需要区分"没数据"和"数据是零"。
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct UsageResponse {
    pub input: u64,
    pub output: u64,
    pub cost: f64,
}

/// per-app 用量状态：`app_id -> 最近一次 get_session_stats 响应的快照`。
/// 见模块文档："覆盖式 set，不是累加"这条核心语义。
#[derive(Default)]
pub struct UsageAccumulator {
    inner: Mutex<HashMap<String, UsageResponse>>,
}

impl UsageAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用一次 `PiEvent::SessionStats` 的 (input, output, cost) **覆盖**该 app 已
    /// 记录的快照——`SessionStats` 本身就是该会话截至目前的累计值，重复调用
    /// 只应保留"最新一次"，不能像 Task16 的 `accumulate` 那样把多次响应的数字
    /// 加总（那样会把同一份累计值越滚越大，用量会假性翻倍/多倍）。
    pub async fn set_latest(&self, app_id: &str, input: u64, output: u64, cost: f64) {
        let mut guard = self.inner.lock().await;
        guard.insert(
            app_id.to_string(),
            UsageResponse {
                input,
                output,
                cost,
            },
        );
    }

    /// 读取该 app 目前记录的最新用量快照；未曾收到过任何 `get_session_stats`
    /// 响应的 app 返回全零的 `UsageResponse`（不是 `Option`——"没有用量" 和
    /// "用量为零"对调用方而言是同一件事）。
    pub async fn get(&self, app_id: &str) -> UsageResponse {
        self.inner
            .lock()
            .await
            .get(app_id)
            .copied()
            .unwrap_or_default()
    }

    /// `app_usage` 命令的返回体：直接是 `get` 的结果。保留这个方法名（而非让
    /// `lib.rs::app_usage` 直接调 `get`）只是为了不必改 P3 Task16 已接好的调用
    /// 点写法，语义上两者等价。
    pub async fn usage_response(&self, app_id: &str) -> UsageResponse {
        self.get(app_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_latest_overwrites_not_accumulates() {
        // 核心回归钉子：SessionStats 是累计值，第二次调用必须覆盖第一次的记录，
        // 不能变成 (100+250, 50+120) 这种错误的求和。
        let acc = UsageAccumulator::new();
        acc.set_latest("app-a", 100, 50, 0.01).await;
        acc.set_latest("app-a", 250, 120, 0.025).await;
        let resp = acc.get("app-a").await;
        assert_eq!(
            resp,
            UsageResponse {
                input: 250,
                output: 120,
                cost: 0.025
            }
        );
    }

    #[tokio::test]
    async fn tracks_apps_independently() {
        let acc = UsageAccumulator::new();
        acc.set_latest("app-a", 100, 20, 0.01).await;
        acc.set_latest("app-b", 5, 5, 0.001).await;
        assert_eq!(
            acc.get("app-a").await,
            UsageResponse {
                input: 100,
                output: 20,
                cost: 0.01
            }
        );
        assert_eq!(
            acc.get("app-b").await,
            UsageResponse {
                input: 5,
                output: 5,
                cost: 0.001
            }
        );
    }

    #[tokio::test]
    async fn unknown_app_defaults_to_zero_no_spurious_state() {
        let acc = UsageAccumulator::new();
        acc.set_latest("app-a", 100, 20, 0.01).await;
        // 只对 app-a 记过；从未出现过的 app_id 必须仍是全零，不能"看到"别的 app 的值。
        assert_eq!(acc.get("never-seen").await, UsageResponse::default());
    }

    #[tokio::test]
    async fn usage_response_is_an_alias_for_get() {
        let acc = UsageAccumulator::new();
        acc.set_latest("app-a", 1200, 600, 0.03).await;
        let resp = acc.usage_response("app-a").await;
        assert_eq!(
            resp,
            UsageResponse {
                input: 1200,
                output: 600,
                cost: 0.03
            }
        );
    }

    #[tokio::test]
    async fn usage_response_for_unknown_app_is_zero() {
        let acc = UsageAccumulator::new();
        let resp = acc.usage_response("never-seen").await;
        assert_eq!(
            resp,
            UsageResponse {
                input: 0,
                output: 0,
                cost: 0.0
            }
        );
    }
}
