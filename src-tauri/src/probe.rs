//! 连通性测试：让 pi 自己发一条极短请求，而不是在宿主里重写各家 HTTP 协议。
//!
//! 环境变量与 models.json 用的是 `model_overrides::model_launch` 的同一份产物，
//! 所以「测试通过 ⇔ 应用里真能用」。代价是每次测试消耗几十 token，界面要明说。
//!
//! # 不进沙盒的理由
//! 探测进程无工具（`--no-tools`）、无扩展、无技能、提示词固定，不执行任何应用代码，
//! 也不读写应用数据；它只在一次性的临时 agent home 里运行，结束即删。
//!
//! # 实测的 `pi --mode json -p` 事件形状（pi 0.84.4，对自建 401 / 关闭端口的夹具实测）
//! stdout 每行一个 JSON 事件，顺序为 `session`、`agent_start`、`turn_start`、
//! `message_start/end`（user）、`message_start/end`（assistant）、`turn_end`、
//! `agent_end`、`agent_settled`。失败时 assistant 的 `message_end` 形如：
//! `{"type":"message_end","message":{"role":"assistant","content":[],"provider":"…",
//! "model":"…","stopReason":"error","errorMessage":"401: {…}"}}`；
//! 网络不通时 `errorMessage` 是 `"Connection error."`（不含 ENOTFOUND 之类字样）。
//! 与计划假设一致，另有两点差异必须处理：
//! 1. 失败后 pi 默认做 agent 级自动重试（2s/4s/8s，网络错误会连发三轮 `message_end`），
//!    所以取**最后一个** assistant `message_end`；探测的 agent home 里写
//!    `settings.json` 的 `retry.enabled=false`，从源头关掉重试，不让探测白等 14 秒。
//! 2. stdin 若是打开的管道，`-p` 会一直等 stdin 的 EOF 而挂住，必须把 stdin 接到 null。
//!
//! 任何异常退出路径上，临时 agent home 都由 [`TempHome`] 的 `Drop` 删除；超时由
//! `kill_on_drop(true)` 在 future 被丢弃时杀掉子进程。
use crate::model_overrides::{model_launch, EffectiveModel, ModelLaunch, ModelSource};
use crate::paths::DataLayout;
use crate::providers::{self, ProvidersStore};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tauri::Manager;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    Ok,
    NoKey,
    InvalidKey,
    NotFound,
    RateLimited,
    Network,
    Timeout,
    Other,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProbeReport {
    pub ok: bool,
    pub kind: ProbeKind,
    pub latency_ms: u64,
    pub provider: String,
    pub model: String,
    /// 中文人话。
    pub message: String,
    /// pi 原始错误：先 `audit::redact` 再截断到 300 字符。
    pub detail: String,
}

const DETAIL_MAX_CHARS: usize = 300;
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const STALE_PROBE_AGE: Duration = Duration::from_secs(3600);
static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 清理 `dir` 下修改时间早于 `older_than` 的子目录（进程被强杀后的残留）；任何失败都忽略。
fn cleanup_stale_probes(dir: &Path, older_than: Duration) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > older_than);
        if stale && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

pub fn probe_argv(provider: &str, model: &str) -> Vec<String> {
    [
        "--mode",
        "json",
        "-p",
        "--no-session",
        "--no-tools",
        "--no-extensions",
        "--no-skills",
        "--offline",
        "--provider",
        provider,
        "--model",
        model,
        "Reply with the single word OK.",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 逐行解析 stdout 的 JSON 事件，取最后一个 `type=="message_end"` 且 `message.role=="assistant"`
/// 的事件：`stopReason` 为 `stop`/`length`/`toolUse` → `Ok(())`；`error` → `Err(errorMessage 或 "未知错误")`；
/// 其余（含 `aborted`、缺失）→ `Err(可读说明)`；
/// 一个都没有 → `Err("未收到模型回复")`。非 JSON 行忽略。
pub fn parse_probe_output(stdout: &str) -> Result<(), String> {
    let last = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .rfind(|v| v["type"] == "message_end" && v["message"]["role"] == "assistant");
    let Some(ev) = last else {
        return Err("未收到模型回复".to_string());
    };
    let msg = &ev["message"];
    // 白名单：只有正常结束的几种算成功，其余（含 aborted、缺失）都算失败。
    match msg["stopReason"].as_str() {
        Some("stop" | "length" | "toolUse") => Ok(()),
        Some("error") => Err(msg["errorMessage"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("未知错误")
            .to_string()),
        Some("aborted") => Err("请求被中断（aborted）".to_string()),
        Some(other) => Err(format!("模型未正常结束（stopReason={other}）")),
        None => Err("模型未正常结束（缺少 stopReason）".to_string()),
    }
}

/// `m` 里是否有独立的三位状态码 `code`（前后不是数字），避免把端口号、id 里的数字误认为状态码。
fn has_code(m: &str, code: &str) -> bool {
    m.match_indices(code).any(|(i, _)| {
        let before = m[..i].chars().next_back();
        let after = m[i + code.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

/// 错误文本 → 类别。网络类先判（`ECONNREFUSED 127.0.0.1:4010` 里的数字不能被当成状态码）。
pub fn classify(msg: &str) -> ProbeKind {
    let m = msg.to_lowercase();
    let any = |ks: &[&str]| ks.iter().any(|k| m.contains(k));
    if any(&["no api key", "missing api key", "api key is required"]) {
        return ProbeKind::NoKey;
    }
    if any(&[
        "enotfound",
        "econnrefused",
        "econnreset",
        "getaddrinfo",
        "fetch failed",
        "certificate",
        "connection error",
        "connection refused",
    ]) {
        return ProbeKind::Network;
    }
    if has_code(&m, "401")
        || has_code(&m, "403")
        || any(&[
            "invalid api key",
            "incorrect api key",
            "authentication",
            "unauthorized",
        ])
    {
        return ProbeKind::InvalidKey;
    }
    if has_code(&m, "404") || any(&["not found", "model_not_found", "does not exist"]) {
        return ProbeKind::NotFound;
    }
    if has_code(&m, "429") || any(&["rate limit", "rate_limit", "quota", "insufficient balance"]) {
        return ProbeKind::RateLimited;
    }
    ProbeKind::Other
}

pub fn message_for(kind: ProbeKind, is_custom: bool) -> String {
    match kind {
        ProbeKind::Ok => "连接正常，模型能正常回复",
        ProbeKind::InvalidKey => "密钥无效或没有权限，请检查是否复制完整",
        ProbeKind::NotFound if is_custom => {
            "地址或模型 id 不对：检查 base_url 是否带 /v1、模型 id 是否拼写正确"
        }
        ProbeKind::NotFound => "模型 id 不存在或该账号无权使用",
        ProbeKind::Network => "连不上服务：检查网络或代理",
        ProbeKind::RateLimited => "被限流或余额不足",
        ProbeKind::Timeout => "30 秒内没有响应",
        ProbeKind::NoKey => "还没有填写密钥",
        ProbeKind::Other => "测试没有通过，详见下方原始错误",
    }
    .to_string()
}

/// 先把本次注入的环境变量值（长度 ≥ 8）整串替换为 `***`（纵深防御：上游错误体可能原样回显
/// 低熵密钥，`audit::redact` 认不出），再脱敏、截断到 `DETAIL_MAX_CHARS` 个字符。
fn make_detail(raw: &str, secrets: &[(String, String)]) -> String {
    let mut s = raw.to_string();
    for (_, v) in secrets {
        if v.len() >= 8 {
            s = s.replace(v.as_str(), "***");
        }
    }
    crate::audit::redact(&s)
        .chars()
        .take(DETAIL_MAX_CHARS)
        .collect()
}

/// 一次性 agent home：创建时建目录，`Drop`（含 panic、超时丢弃 future）时整个删除。
struct TempHome(PathBuf);

impl TempHome {
    fn create(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self(dir.to_path_buf()))
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn report(
    kind: ProbeKind,
    started: Instant,
    provider: &str,
    model: &str,
    is_custom: bool,
    detail: &str,
    secrets: &[(String, String)],
) -> ProbeReport {
    ProbeReport {
        ok: kind == ProbeKind::Ok,
        kind,
        latency_ms: started.elapsed().as_millis() as u64,
        provider: provider.to_string(),
        model: model.to_string(),
        message: message_for(kind, is_custom),
        detail: make_detail(detail, secrets),
    }
}

/// 末尾 `n` 行。
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// 在一次性 agent home（`agent_home`，结束后整体删除）里起 pi 发一条极短请求。
pub async fn run_probe(
    pi_bin: &Path,
    agent_home: &Path,
    provider: &str,
    model: &str,
    launch: &ModelLaunch,
    timeout: Duration,
) -> ProbeReport {
    let started = Instant::now();
    let is_custom = providers::native(provider).is_none();
    let fail = |kind, detail: &str| {
        report(
            kind,
            started,
            provider,
            model,
            is_custom,
            detail,
            &launch.env,
        )
    };

    let _home = match TempHome::create(agent_home) {
        Ok(h) => h,
        Err(e) => return fail(ProbeKind::Other, &format!("无法创建临时目录：{e}")),
    };
    // 探测不重试：重试只会让失败的探测白等 2+4+8 秒。
    let mut files = vec![(
        "settings.json",
        serde_json::json!({ "retry": { "enabled": false } }),
    )];
    if let Some(mj) = &launch.models_json {
        files.push(("models.json", mj.clone()));
    }
    for (name, value) in files {
        if let Err(e) = std::fs::write(agent_home.join(name), value.to_string()) {
            return fail(ProbeKind::Other, &format!("无法写入 {name}：{e}"));
        }
    }

    let mut cmd = tokio::process::Command::new(pi_bin);
    cmd.args(probe_argv(provider, model))
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .env("PI_CODING_AGENT_DIR", agent_home)
        .env("PI_TELEMETRY", "0")
        // 在临时目录里跑，不读取任何项目上下文文件。
        .current_dir(agent_home)
        // stdin 必须是 null：打开的管道会让 `-p` 一直等 EOF。
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(ProbeKind::Other, &format!("无法启动 pi：{e}")),
    };

    // 超时后 future 被丢弃，child 随之 drop，kill_on_drop 杀掉进程。
    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => return fail(ProbeKind::Timeout, ""),
        Ok(Err(e)) => return fail(ProbeKind::Other, &format!("等待 pi 结束失败：{e}")),
        Ok(Ok(o)) => o,
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    match parse_probe_output(&stdout) {
        Ok(()) => fail(ProbeKind::Ok, ""),
        Err(err) => {
            // 没有 assistant 消息且进程异常退出：stderr 末尾才是真因。
            let detail = if !out.status.success() && err == "未收到模型回复" {
                let t = tail_lines(&stderr, 20);
                if t.trim().is_empty() {
                    err
                } else {
                    t
                }
            } else {
                err
            };
            fail(classify(&detail), &detail)
        }
    }
}

#[tauri::command]
pub async fn test_provider(
    app: tauri::AppHandle,
    provider: String,
    model: Option<String>,
) -> Result<ProbeReport, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
    cleanup_stale_probes(&root.join("probe"), STALE_PROBE_AGE);
    let layout = DataLayout::new(root.clone());
    let custom = ProvidersStore::new(layout.providers_path()).list()?;
    if !providers::is_known(&provider, &custom) {
        return Err(format!("未知的 provider：{provider}"));
    }
    let model = match model.filter(|m| !m.trim().is_empty()) {
        Some(m) => m,
        None => match providers::native(&provider) {
            Some(n) => n.presets[0].to_string(),
            None => custom
                .iter()
                .find(|c| c.id == provider)
                .and_then(|c| c.models.first().cloned())
                .ok_or_else(|| "该 provider 没有可用的模型".to_string())?,
        },
    };
    let is_custom = providers::native(&provider).is_none();
    if crate::secrets::read_key(&provider).is_none() {
        return Ok(report(
            ProbeKind::NoKey,
            Instant::now(),
            &provider,
            &model,
            is_custom,
            "",
            &[],
        ));
    }
    let eff = EffectiveModel {
        provider: Some(provider.clone()),
        model: Some(model.clone()),
        source: ModelSource::App,
    };
    let launch = model_launch(&eff, &custom, crate::secrets::read_key, true);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // 进程内计数后缀：同一毫秒的并发探测也不会撞名。
    let seq = PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let home = root
        .join("probe")
        .join(format!("{stamp}-{}-{seq}", std::process::id()));
    Ok(run_probe(
        &crate::pi_bin::resolve_pi_bin(),
        &home,
        &provider,
        &model,
        &launch,
        PROBE_TIMEOUT,
    )
    .await)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK_STREAM: &str = r#"{"type":"session","version":3}
{"type":"message_end","message":{"role":"user","content":[]}}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"OK"}],"stopReason":"stop"}}
{"type":"agent_settled"}"#;

    #[test]
    fn probe_argv_disables_tools_extensions_skills_and_session() {
        let a = probe_argv("custom-x", "m1");
        for f in [
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--offline",
            "-p",
        ] {
            assert!(a.iter().any(|x| x == f), "缺 {f}: {a:?}");
        }
        let pos = |f: &str| a.iter().position(|x| x == f).unwrap();
        assert_eq!(a[pos("--provider") + 1], "custom-x");
        assert_eq!(a[pos("--model") + 1], "m1");
        assert_eq!(a[pos("--mode") + 1], "json");
    }

    #[test]
    fn parse_probe_output_ok_on_normal_assistant_message() {
        assert_eq!(parse_probe_output(OK_STREAM), Ok(()));
    }

    #[test]
    fn parse_probe_output_err_on_stop_reason_error() {
        let s = r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"401: bad"}}"#;
        assert_eq!(parse_probe_output(s), Err("401: bad".to_string()));
        let s = r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error"}}"#;
        assert_eq!(parse_probe_output(s), Err("未知错误".to_string()));
    }

    #[test]
    fn parse_probe_output_takes_last_assistant_message_after_retries() {
        let s = format!(
            "{}\n{}",
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"Connection error."}}"#,
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop"}}"#
        );
        assert_eq!(parse_probe_output(&s), Ok(()));
    }

    #[test]
    fn parse_probe_output_err_when_no_assistant_message() {
        assert_eq!(
            parse_probe_output("not json\n{\"type\":\"agent_start\"}"),
            Err("未收到模型回复".to_string())
        );
        assert_eq!(parse_probe_output(""), Err("未收到模型回复".to_string()));
    }

    #[test]
    fn classify_maps_401_to_invalid_key() {
        assert_eq!(classify("401: Incorrect API key"), ProbeKind::InvalidKey);
        assert_eq!(classify("403 Forbidden"), ProbeKind::InvalidKey);
        assert_eq!(classify("Authentication failed"), ProbeKind::InvalidKey);
    }

    #[test]
    fn classify_maps_404_to_not_found() {
        assert_eq!(classify("404 page not found"), ProbeKind::NotFound);
        assert_eq!(classify("model_not_found"), ProbeKind::NotFound);
        assert_eq!(classify("The model does not exist"), ProbeKind::NotFound);
    }

    #[test]
    fn classify_maps_enotfound_to_network() {
        assert_eq!(
            classify("getaddrinfo ENOTFOUND api.x.com"),
            ProbeKind::Network
        );
        assert_eq!(classify("Connection error."), ProbeKind::Network);
        // 端口号里的 401 不是状态码。
        assert_eq!(classify("ECONNREFUSED 127.0.0.1:4010"), ProbeKind::Network);
        assert_eq!(
            classify("connect ECONNREFUSED 127.0.0.1:401"),
            ProbeKind::Network
        );
    }

    #[test]
    fn classify_maps_429_to_rate_limited() {
        assert_eq!(classify("429 Too Many Requests"), ProbeKind::RateLimited);
        assert_eq!(classify("insufficient balance"), ProbeKind::RateLimited);
        assert_eq!(classify("something odd"), ProbeKind::Other);
        assert_eq!(classify("No API key for provider x"), ProbeKind::NoKey);
    }

    #[test]
    fn parse_probe_output_stop_reason_whitelist() {
        let line = |r: &str| {
            format!(
                r#"{{"type":"message_end","message":{{"role":"assistant","stopReason":"{r}"}}}}"#
            )
        };
        for ok in ["stop", "length", "toolUse"] {
            assert_eq!(parse_probe_output(&line(ok)), Ok(()), "{ok}");
        }
        let e = parse_probe_output(&line("aborted")).unwrap_err();
        assert!(e.contains("中断"), "{e}");
        assert!(parse_probe_output(&line("weird"))
            .unwrap_err()
            .contains("weird"));
        let none = r#"{"type":"message_end","message":{"role":"assistant"}}"#;
        assert!(parse_probe_output(none).is_err());
    }

    #[test]
    fn detail_masks_injected_env_values_even_if_low_entropy() {
        let env = vec![
            (
                "SUPERAGENT_KEY_CUSTOM_X".to_string(),
                "hunter2-hunter2".to_string(),
            ),
            ("SHORT".to_string(), "abc".to_string()),
        ];
        let d = make_detail("401 bad key hunter2-hunter2 and abc", &env);
        assert!(!d.contains("hunter2"), "{d}");
        assert!(d.contains("abc"), "短值不替换：{d}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_probe_masks_env_value_echoed_on_stderr() {
        let t = tempfile::tempdir().unwrap();
        let pi = script(
            t.path(),
            "echo \"ECONNREFUSED token=$SUPERAGENT_KEY_CUSTOM_X\" >&2\nexit 1",
        );
        let launch = ModelLaunch {
            env: vec![("SUPERAGENT_KEY_CUSTOM_X".into(), "plainsecret99".into())],
            ..Default::default()
        };
        let r = run_probe(
            &pi,
            &t.path().join("h"),
            "custom-x",
            "m",
            &launch,
            Duration::from_secs(10),
        )
        .await;
        assert!(!r.detail.contains("plainsecret99"), "{}", r.detail);
    }

    #[test]
    fn cleanup_stale_probes_respects_age_threshold() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("old");
        let fresh = t.path().join("fresh");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(old.join("models.json"), "{}").unwrap();
        // 阈值很大（1 小时）：都保留；睡 50ms 后阈值 10ms：都算过期。
        cleanup_stale_probes(t.path(), Duration::from_secs(3600));
        assert!(old.exists() && fresh.exists());
        std::thread::sleep(Duration::from_millis(50));
        cleanup_stale_probes(t.path(), Duration::from_millis(10));
        assert!(!old.exists() && !fresh.exists());
        // 目录不存在也不报错。
        cleanup_stale_probes(&t.path().join("nope"), Duration::ZERO);
    }

    #[test]
    fn message_for_not_found_on_custom_mentions_base_url() {
        assert!(message_for(ProbeKind::NotFound, true).contains("base_url"));
        assert!(!message_for(ProbeKind::NotFound, false).contains("base_url"));
    }

    #[test]
    fn detail_is_redacted_and_truncated() {
        let d = make_detail("failed api_key=sk-test-fake-not-a-real-key", &[]);
        assert!(d.contains("***"), "{d}");
        assert!(!d.contains("sk-test-fake-not-a-real-key"), "{d}");
        let long = make_detail(&"x ".repeat(500), &[]);
        assert!(long.chars().count() <= 300);
    }

    // ---- run_probe：用脚本冒充 pi，验证清理与超时 ----

    #[cfg(unix)]
    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("fake-pi.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_probe_ok_removes_temp_home_and_writes_models_json() {
        let t = tempfile::tempdir().unwrap();
        let ok_line =
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop"}}"#;
        let pi = script(
            t.path(),
            &format!(
                "cp \"$PI_CODING_AGENT_DIR/models.json\" \"{}/seen.json\"\n\
                 cp \"$PI_CODING_AGENT_DIR/settings.json\" \"{}/seen-settings.json\"\n\
                 echo '{ok_line}'",
                t.path().display(),
                t.path().display()
            ),
        );
        let home = t.path().join("probe/1-2");
        let launch = ModelLaunch {
            models_json: Some(serde_json::json!({"providers": {}})),
            ..Default::default()
        };
        let r = run_probe(
            &pi,
            &home,
            "custom-x",
            "m",
            &launch,
            Duration::from_secs(10),
        )
        .await;
        assert!(r.ok, "{r:?}");
        assert_eq!(r.kind, ProbeKind::Ok);
        assert!(!home.exists(), "临时 agent home 应已删除");
        assert!(t.path().join("seen.json").exists());
        let s = std::fs::read_to_string(t.path().join("seen-settings.json")).unwrap();
        assert!(s.contains("\"enabled\":false"), "{s}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_probe_timeout_kills_child_and_removes_home() {
        let t = tempfile::tempdir().unwrap();
        // 不依赖 pidfile：假 pi 直接 exec 一个带唯一参数的 sleep，事后按命令行找残留。
        // 机器很忙时脚本可能还没跑到 exec 就被超时杀掉，这种情况同样应通过。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let secs = format!("7{}{:03}", std::process::id(), nanos % 1000);
        let pi = script(t.path(), &format!("exec sleep {secs}"));
        let home = t.path().join("h");
        let r = run_probe(
            &pi,
            &home,
            "anthropic",
            "m",
            &ModelLaunch::default(),
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(r.kind, ProbeKind::Timeout, "{r:?}");
        assert!(!home.exists());
        // 轮询（约 5 秒）：不应再有该 sleep 进程；僵尸（状态以 Z 开头）视为已被杀。
        let needle = format!("sleep {secs}");
        let mut leftover = true;
        for _ in 0..50 {
            let out = std::process::Command::new("ps")
                .args(["-ax", "-o", "stat=,command="])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            leftover = text.lines().any(|l| {
                let l = l.trim_start();
                !l.starts_with('Z') && l.trim_end().ends_with(&needle)
            });
            if !leftover {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!leftover, "超时后子进程应已被杀掉：sleep {secs} 仍在运行");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_probe_nonzero_exit_uses_stderr_and_redacts_key() {
        let t = tempfile::tempdir().unwrap();
        let pi = script(
            t.path(),
            "echo 'ECONNREFUSED while using api_key=sk-test-fake-not-a-real-key' >&2\nexit 1",
        );
        let home = t.path().join("h");
        let r = run_probe(
            &pi,
            &home,
            "openai",
            "m",
            &ModelLaunch::default(),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(r.kind, ProbeKind::Network, "{r:?}");
        assert!(
            !r.detail.contains("sk-test-fake-not-a-real-key"),
            "{}",
            r.detail
        );
        assert!(!home.exists());
    }

    #[tokio::test]
    async fn run_probe_reports_other_when_binary_missing() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("h");
        let r = run_probe(
            Path::new("/nonexistent/pi"),
            &home,
            "openai",
            "m",
            &ModelLaunch::default(),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(r.kind, ProbeKind::Other);
        assert!(!home.exists());
    }
}
