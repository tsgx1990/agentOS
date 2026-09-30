// BYOK 错误分治。注意：跨进程全局限流桶（同 key 共享配额）属 P2，此处只做分类 + 提示。
#[derive(Debug, PartialEq)]
pub enum ByokVerdict {
    AuthFailed,
    RateLimited,
    Transient,
    None,
}

pub fn classify_error(msg: &str) -> ByokVerdict {
    let m = msg.to_lowercase();
    if m.is_empty() {
        return ByokVerdict::None;
    }
    if m.contains("401") || m.contains("authentication") || m.contains("invalid api key") {
        return ByokVerdict::AuthFailed;
    }
    if m.contains("429")
        || m.contains("rate_limit")
        || m.contains("rate limit")
        || m.contains("overloaded")
    {
        return ByokVerdict::RateLimited;
    }
    if m.contains("500") || m.contains("502") || m.contains("503") || m.contains("error") {
        return ByokVerdict::Transient;
    }
    ByokVerdict::None
}

pub fn frontend_payload(v: &ByokVerdict) -> Option<(&'static str, &'static str)> {
    match v {
        ByokVerdict::AuthFailed => Some(("auth", "API Key 无效或已过期，请重新配置")),
        ByokVerdict::RateLimited => Some(("rate_limit", "触发限流，正在自动重试……")),
        ByokVerdict::Transient => Some(("transient", "网络波动，正在重试……")),
        ByokVerdict::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_auth_failure() {
        assert!(matches!(
            classify_error("401 authentication_error"),
            ByokVerdict::AuthFailed
        ));
        assert!(matches!(
            classify_error("Invalid API key"),
            ByokVerdict::AuthFailed
        ));
    }

    #[test]
    fn classifies_rate_limit() {
        assert!(matches!(
            classify_error("429 rate_limit"),
            ByokVerdict::RateLimited
        ));
        assert!(matches!(
            classify_error("overloaded_error"),
            ByokVerdict::RateLimited
        ));
    }

    #[test]
    fn classifies_transient_and_none() {
        assert!(matches!(
            classify_error("500 internal error"),
            ByokVerdict::Transient
        ));
        assert!(matches!(classify_error(""), ByokVerdict::None));
    }

    #[test]
    fn auth_payload_is_reconfigure_not_retry() {
        let (kind, _msg) = frontend_payload(&ByokVerdict::AuthFailed).unwrap();
        assert_eq!(kind, "auth");
        assert!(frontend_payload(&ByokVerdict::None).is_none());
    }
}
