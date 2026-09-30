use crate::paths::DataLayout;

/// 读出该应用整份状态 JSON（`state/<app_id>.json`），文件不存在/损坏/内容非法时
/// 一律视为空对象，保证 `get` 在应用从未 `set` 过时也能安全返回 `None` 而非报错。
fn read_all(layout: &DataLayout, app_id: &str) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(layout.state_path(app_id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 读取应用状态中的单个 key；文件缺失或 key 不存在都返回 `None`。
pub fn get(layout: &DataLayout, app_id: &str, key: &str) -> Option<serde_json::Value> {
    read_all(layout, app_id).get(key).cloned()
}

// 原子写：先写临时文件 `.json.tmp` 再 `rename` 到目标路径，避免进程中途崩溃/断电
// 留下半写文件。P1 单用户、pi 单会话工具调用串行 → 并发写冲突下 last-writer-wins
// 可接受（spec §5 注），P2 若引入多写者再考虑加锁/CAS。
pub fn set(
    layout: &DataLayout,
    app_id: &str,
    key: &str,
    val: &serde_json::Value,
) -> Result<(), String> {
    let mut map = read_all(layout, app_id);
    map.insert(key.to_string(), val.clone());
    let path = layout.state_path(app_id);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string(&map).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::DataLayout;
    use tempfile::tempdir;
    #[test]
    fn set_then_get_roundtrip() {
        let d = tempdir().unwrap();
        let layout = DataLayout::new(d.path().to_path_buf());
        layout.ensure_app("x").unwrap();
        set(&layout, "x", "items", &serde_json::json!(["买牛奶"])).unwrap();
        assert_eq!(get(&layout, "x", "items").unwrap()[0], "买牛奶");
        assert!(get(&layout, "x", "missing").is_none());
    }
}
