pub const SLOT_COUNT: usize = 12;

pub fn scheme_name(slot: usize) -> String {
    format!("sagent{slot}")
}

pub struct SlotPool {
    slots: Vec<Option<String>>,
}

impl SlotPool {
    /// 创建容量为 `n` 的槽位池，初始全部为空闲（`None`）。
    pub fn new(n: usize) -> Self {
        Self {
            slots: vec![None; n],
        }
    }

    /// 为 `app_id` 分配一个槽位：若已占用某槽位则幂等返回该槽位；
    /// 否则占用第一个空闲槽位并返回；若已无空闲槽位则返回 `None`。
    pub fn assign(&mut self, app_id: &str) -> Option<usize> {
        if let Some(s) = self.slot_for_app(app_id) {
            return Some(s);
        }
        let free = self.slots.iter().position(|s| s.is_none())?;
        self.slots[free] = Some(app_id.to_string());
        Some(free)
    }

    /// 释放该 `app_id` 占用的槽位（若有），使其可被重新分配。
    pub fn release_app(&mut self, app_id: &str) {
        for s in self.slots.iter_mut() {
            if s.as_deref() == Some(app_id) {
                *s = None;
            }
        }
    }

    /// 查询指定槽位当前占用者的 app_id（若该槽位空闲或越界则为 `None`）。
    pub fn app_for_slot(&self, slot: usize) -> Option<String> {
        self.slots.get(slot).and_then(|s| s.clone())
    }

    /// 反查 `app_id` 当前占用的槽位编号（若未分配则为 `None`）。
    pub fn slot_for_app(&self, app_id: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.as_deref() == Some(app_id))
    }
}

/// 校验单个域名 token 可安全放进 CSP connect-src：
/// 拒绝纯通配 "*"（等于放开一切）、以及含 CSP 元字符/空白（防注入新指令）。
/// 允许子域通配如 "*.example.com"、带端口/scheme 的正常 host-source。
fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d != "*"
        && !d.contains(|c: char| c.is_whitespace() || matches!(c, ';' | '\'' | '"' | ','))
}

/// 每应用 CSP：核心是 connect-src 白名单（封死 H5 自外联）。应用在 sandboxed
/// iframe 内、单应用独立 origin，故其自有 inline 脚本/样式放行（'unsafe-inline'）
/// 不跨应用扩大风险；关键的 exfil 防护落在 connect-src。
///
/// 这是本应用外联白名单的唯一技术性把关点：无论上游（安装包/配置）存了什么，
/// 这里都必须在拼接前过滤掉危险 token（纯通配 "*" 或含 CSP 元字符的注入尝试），
/// 否则整条防线形同虚设。
///
/// 注意：不设 `frame-ancestors`。应用需要被宿主窗口以
/// `<iframe src="sagent{slot}://localhost/...">` 内嵌渲染，而宿主的顶层 origin
/// 与 `sagent{slot}://localhost` 不同；若加上 `frame-ancestors 'self'` 会导致
/// 宿主自己都无法内嵌该应用（空白 iframe）。这个桌面场景里没有外部网站能内嵌
/// 自定义 scheme 文档，且每个应用自身的 `default-src 'self'` 已阻止它加载/内嵌
/// 其它应用的 scheme，所以去掉 `frame-ancestors` 不引入跨应用或点击劫持风险。
pub fn csp_header(domains: &[String]) -> String {
    let safe: Vec<&str> = domains
        .iter()
        .map(|d| d.as_str())
        .filter(|d| valid_domain(d))
        .collect();
    let connect = if safe.is_empty() {
        "connect-src 'self';".to_string()
    } else {
        format!("connect-src 'self' {};", safe.join(" "))
    };
    format!(
        "default-src 'self'; script-src 'self' 'unsafe-inline'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data:; {connect} \
base-uri 'none'; form-action 'none';"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_is_idempotent_and_bounded() {
        let mut p = SlotPool::new(2);
        let s0 = p.assign("a").unwrap();
        assert_eq!(p.assign("a"), Some(s0)); // 幂等复用
        let _s1 = p.assign("b").unwrap();
        assert_eq!(p.assign("c"), None); // 满
        p.release_app("a");
        assert!(p.assign("c").is_some()); // 释放后可用
    }

    #[test]
    fn scheme_and_lookup() {
        let mut p = SlotPool::new(3);
        let s = p.assign("app-x").unwrap();
        assert_eq!(p.app_for_slot(s), Some("app-x".to_string()));
        assert_eq!(p.slot_for_app("app-x"), Some(s));
        assert_eq!(scheme_name(0), "sagent0");
    }

    #[test]
    fn csp_includes_connect_src_domains() {
        let h = csp_header(&["api.amap.com".into(), "*.booking.com".into()]);
        assert!(h.contains("connect-src 'self' api.amap.com *.booking.com"));
        assert!(h.contains("default-src 'self'"));
        // 受限第三方无域名 → 仅 'self'
        let h2 = csp_header(&[]);
        assert!(h2.contains("connect-src 'self';"));
    }

    #[test]
    fn csp_rejects_wildcard_and_injection() {
        let h = csp_header(&[
            "*".into(),
            "evil.com; frame-src x".into(),
            "api.ok.com".into(),
            "*.ok.com".into(),
        ]);
        assert!(h.contains("connect-src 'self' api.ok.com *.ok.com;"));
        assert!(!h.contains("frame-src evil"));
        assert!(!h.contains(" *;"));
    }
}
