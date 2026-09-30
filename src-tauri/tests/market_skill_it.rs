// P6-B Task 6：市场 skill 条目 + HTTP 索引拉取 + zip 下载安装。
//
// 本机起一个手写的 HTTP/1.0 夹具服务器（`std::net::TcpListener`，不引
// `tiny_http` 等新依赖，仿仓库里其它自建夹具——如 `mock_mcp_server`——的
// 风格），覆盖：
// - `market::fetch_index` 走 `http://` 源真的能拉到并解析索引；
// - `skill_market_install_core` 的 HTTP 分支：正常 zip 装成功、sha256 不符拒装、
//   zip 内含路径穿越条目拒装，且三种情况下都不留半成品目录；
// - 本地（非下载）`kind:"skill"` 条目——直接用仓库真实的
//   `samples/market/index.json` 里新加的那条 `connector-etiquette`，走
//   `install_skill_from_path` 同一条安装门（`skill_market_install_core` 的
//   `download_url == None` 分支）。

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use super_agent_os::market::{self, MarketEntry};
use super_agent_os::paths::DataLayout;
use super_agent_os::skill_market_install_core;
use super_agent_os::skills::{SkillSourceKind, SkillStore};

/// 一条登记的路由：`(path, status, content_type, body, location)`——
/// `location` 非空时额外发一个 `Location` 响应头（终审 I1：重定向逐跳校验
/// 回归测试要用 302 + `Location` 构造重定向链，之前四个既有测试用例都不需要
/// 这个头，统一加个字段、既有调用点补 `None`）。
type Route = (String, u16, &'static str, Vec<u8>, Option<String>);

/// 读到一个空行（请求头结束）或凑够 8 KiB 就停——本夹具只关心请求行里的
/// path，不需要真的解析完整请求。
fn read_request_line(stream: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

fn handle_conn(mut stream: TcpStream, routes: &[Route]) {
    let request_line = read_request_line(&mut stream);
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    let (status, ctype, body, location): (u16, &str, &[u8], Option<&str>) =
        match routes.iter().find(|(p, ..)| *p == path) {
            Some((_, status, ctype, body, location)) => {
                (*status, ctype, body.as_slice(), location.as_deref())
            }
            None => (404, "text/plain", b"not found", None),
        };
    let status_line = match status {
        200 => "200 OK",
        302 => "302 Found",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    let location_header = location
        .map(|l| format!("Location: {l}\r\n"))
        .unwrap_or_default();
    let header = format!(
        "HTTP/1.0 {status_line}\r\nContent-Type: {ctype}\r\n{location_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// 起一个后台线程接受连接的极简 HTTP/1.0 server，返回它的 base URL
/// （`http://127.0.0.1:<port>`）。线程随测试进程退出而结束，不显式关闭
/// （单个测试进程内每个 `#[test]` 各起各的 server，端口用 0 让 OS 分配，互不
/// 冲突）。
fn spawn_mock_server(routes: Vec<Route>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口应成功");
    let addr = listener.local_addr().unwrap();
    let routes = Arc::new(routes);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let routes = routes.clone();
            thread::spawn(move || handle_conn(stream, &routes));
        }
    });
    format!("http://{addr}")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 现场生成一个"正常"技能 zip：一个合法的 `SKILL.md`，name 用 `skill_id`。
fn build_normal_zip(skill_id: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("SKILL.md", options).unwrap();
    let content = format!(
        "---\nname: {skill_id}\ndescription: 供 market_skill_it 集成测试用的技能\n---\n\n从 HTTP 市场装的技能。\n"
    );
    writer.write_all(content.as_bytes()).unwrap();
    writer.finish().unwrap().into_inner()
}

/// 现场生成一个含路径穿越条目（`../evil.txt`）的恶意 zip——`SKILL.md` 本身合法
/// （这样"拒装的原因是路径穿越，不是别的什么"这一点不会被搅浑），另加一条
/// `../evil.txt`。
fn build_path_traversal_zip(skill_id: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("SKILL.md", options).unwrap();
    writer
        .write_all(format!("---\nname: {skill_id}\ndescription: 恶意 zip\n---\n").as_bytes())
        .unwrap();
    writer.start_file("../evil.txt", options).unwrap();
    writer.write_all(b"pwned").unwrap();
    writer.finish().unwrap().into_inner()
}

fn temp_layout() -> (tempfile::TempDir, DataLayout) {
    let tmp = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(tmp.path().to_path_buf());
    (tmp, layout)
}

fn skill_entry(name: &str, download_url: Option<String>, sha256: Option<String>) -> MarketEntry {
    MarketEntry {
        name: name.to_string(),
        display_name: name.to_string(),
        version: "1.0.0".to_string(),
        category: "skill".to_string(),
        icon: None,
        description: String::new(),
        source: "unused-for-http-entries".to_string(),
        permissions: vec![],
        kind: "skill".to_string(),
        download_url,
        sha256,
        size: None,
        author: None,
    }
}

#[test]
fn fetch_index_over_http_returns_parsed_entries() {
    let index_json = r#"{
      "entries": [
        { "name": "@superagent/researcher", "display_name": "研究员", "version": "1.0.0",
          "category": "automation", "source": "researcher" }
      ]
    }"#;
    let base = spawn_mock_server(vec![(
        "/index.json".to_string(),
        200,
        "application/json",
        index_json.as_bytes().to_vec(),
        None,
    )]);

    let raw = market::fetch_index(&format!("{base}/index.json")).expect("HTTP 拉取应成功");
    let entries = market::parse_index(&raw).expect("应能解析拉回来的索引");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "@superagent/researcher");
    assert_eq!(entries[0].kind, "app", "旧条目没有 kind 字段应按 app 处理");
}

// ---- 终审 I1：http_client() 的自定义重定向策略对每一跳都重新过
// check_remote_url，不是只在入口调用一次 ----

/// 正例：回环地址 302 到**另一个**回环地址——`check_remote_url` 本身允许回环
/// http（供本地开发/测试用），这条重定向应该被放行并真的跟到第二个 server，
/// 拿到它的响应体。证明修复没有把"合法的一跳重定向"也一并挡死。
#[test]
fn fetch_index_follows_redirect_to_another_loopback_target() {
    let final_body = r#"{"entries":[]}"#;
    let target = spawn_mock_server(vec![(
        "/final.json".to_string(),
        200,
        "application/json",
        final_body.as_bytes().to_vec(),
        None,
    )]);
    let redirector = spawn_mock_server(vec![(
        "/index.json".to_string(),
        302,
        "text/plain",
        Vec::new(),
        Some(format!("{target}/final.json")),
    )]);

    let raw = market::fetch_index(&format!("{redirector}/index.json"))
        .expect("回环→回环的重定向应被放行并跟随到最终目标");
    assert_eq!(raw, final_body, "应拿到重定向目标 server 的真实响应体");
}

/// 反例（终审 I1 本体）：回环地址 302 到一个公网 http 地址——这条重定向必须在
/// **跟随前**就被拒绝，第二跳压根不应该发生（`example.invalid` 是 RFC 2606
/// 保留的不可解析域名，若修复失效、reqwest 真的去连它，会得到一个 DNS/连接
/// 错误而不是这里断言的"https"策略拒绝错误——两种失败模式的错误文案不同，
/// 断言 `contains("https")` 就是在区分"策略正确拦截"与"策略形同虚设、只是
/// 连接凑巧失败"这两种情况）。
#[test]
fn fetch_index_rejects_redirect_to_public_http_target() {
    let redirector = spawn_mock_server(vec![(
        "/index.json".to_string(),
        302,
        "text/plain",
        Vec::new(),
        Some("http://example.invalid/x".to_string()),
    )]);

    let err = market::fetch_index(&format!("{redirector}/index.json")).unwrap_err();
    assert!(
        err.contains("https"),
        "重定向目标是公网 http，应在跟随前被 check_remote_url 拒绝，错误信息应提到 https，实际：{err}"
    );
}

#[test]
fn skill_market_install_installs_normal_zip_via_http() {
    let zip_bytes = build_normal_zip("http-installed-skill");
    let sha = sha256_hex(&zip_bytes);
    let base = spawn_mock_server(vec![(
        "/skill.zip".to_string(),
        200,
        "application/zip",
        zip_bytes,
        None,
    )]);
    let entry = skill_entry(
        "http-installed-skill",
        Some(format!("{base}/skill.zip")),
        Some(sha.clone()),
    );

    let (_tmp, layout) = temp_layout();
    let installed = skill_market_install_core(
        &entry,
        std::path::Path::new("/nonexistent"),
        &layout,
        &[],
        1000,
    )
    .expect("正常 zip + 正确 sha256 应装成功");
    assert_eq!(installed.meta.id, "http-installed-skill");
    assert!(!installed.trusted, "市场技能条目应 trusted=false");
    assert_eq!(installed.source.kind, SkillSourceKind::Market);
    assert_eq!(
        installed.source.url.as_deref(),
        Some(format!("{base}/skill.zip").as_str())
    );
    assert_eq!(installed.source.sha256.as_deref(), Some(sha.as_str()));

    let store = SkillStore::new(layout.clone());
    assert_eq!(store.list().unwrap().len(), 1);
    // 下载/解包用的临时目录（skills_root/.market-dl-*）不应残留。
    let leftovers: Vec<_> = std::fs::read_dir(layout.skills_root())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".market-dl-"))
        .collect();
    assert!(leftovers.is_empty(), "不应留下市场下载的临时目录");
}

#[test]
fn skill_market_install_rejects_sha256_mismatch_and_leaves_no_residue() {
    let zip_bytes = build_normal_zip("sha-mismatch-skill");
    let base = spawn_mock_server(vec![(
        "/skill.zip".to_string(),
        200,
        "application/zip",
        zip_bytes,
        None,
    )]);
    let wrong_sha = "0".repeat(64);
    let entry = skill_entry(
        "sha-mismatch-skill",
        Some(format!("{base}/skill.zip")),
        Some(wrong_sha),
    );

    let (_tmp, layout) = temp_layout();
    let err = skill_market_install_core(
        &entry,
        std::path::Path::new("/nonexistent"),
        &layout,
        &[],
        1000,
    )
    .expect_err("sha256 不符应拒装");
    assert!(err.contains("sha256"), "实际：{err}");
    assert!(
        !layout.skills_root().exists(),
        "sha256 校验失败连 skills/ 目录都不应创建（校验发生在下载阶段，不到解包/安装那一步）"
    );
}

#[test]
fn skill_market_install_rejects_path_traversal_zip_and_leaves_no_residue() {
    let zip_bytes = build_path_traversal_zip("evil-zip-skill");
    let sha = sha256_hex(&zip_bytes);
    let base = spawn_mock_server(vec![(
        "/skill.zip".to_string(),
        200,
        "application/zip",
        zip_bytes,
        None,
    )]);
    let entry = skill_entry(
        "evil-zip-skill",
        Some(format!("{base}/skill.zip")),
        Some(sha),
    );

    let (_tmp, layout) = temp_layout();
    let err = skill_market_install_core(
        &entry,
        std::path::Path::new("/nonexistent"),
        &layout,
        &[],
        1000,
    )
    .expect_err("含路径穿越条目的 zip 应拒装");
    assert!(err.contains("不安全"), "实际：{err}");
    assert!(
        SkillStore::new(layout.clone()).list().unwrap().is_empty(),
        "拒装不应留下任何已装记录"
    );
    let leftovers: Vec<_> = std::fs::read_dir(layout.skills_root())
        .map(|rd| rd.filter_map(|e| e.ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "拒装不应在 skills/ 下留下任何目录（含临时解包目录）"
    );
}

/// 仓库真实的 `samples/` 目录（`CARGO_MANIFEST_DIR` 是 `src-tauri/`，上一级才是
/// 仓库根），同 `sample_skills_valid.rs::real_samples_dir` 的手法。
fn real_samples_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples")
}

#[test]
fn local_skill_entry_in_real_market_index_installs_via_install_from_dir_path() {
    let index_path = real_samples_dir().join("market").join("index.json");
    let raw = market::fetch_index(index_path.to_str().unwrap()).expect("本地索引应能读到");
    let entries = market::parse_index(&raw).expect("本地索引应能解析");
    let entry = entries
        .iter()
        .find(|e| e.kind == "skill")
        .expect("samples/market/index.json 应至少含一条 kind:\"skill\" 的本地条目");
    assert!(
        entry.download_url.is_none(),
        "本地技能条目不应声明 download_url（不走下载）"
    );

    let (_tmp, layout) = temp_layout();
    let installed = skill_market_install_core(entry, &real_samples_dir(), &layout, &[], 1000)
        .expect("本地 kind:\"skill\" 条目应能直接装成功（不下载）");
    assert!(
        !installed.trusted,
        "市场技能条目应 trusted=false（即便是本地条目）"
    );
    assert_eq!(installed.source.kind, SkillSourceKind::Market);
    assert_eq!(installed.source.url, None);
}
