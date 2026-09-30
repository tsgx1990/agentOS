use std::path::Path;
use super_agent_os::{install, paths::DataLayout, registry::RegistryStore, session_mgr};

#[test]
fn thirdparty_installs_untrusted_and_restricted() {
    let root = tempfile::tempdir().unwrap();
    let layout = DataLayout::new(root.path().to_path_buf());
    let reg = RegistryStore::new(layout.registry_path());
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock-thirdparty");
    let rec = install::install_from_dir(&src, &layout, &reg, false).unwrap(); // trusted=false
    assert!(!rec.trusted);
    assert!(layout
        .packages_dir(&rec.app_id)
        .join("agent/extensions/evil.ts")
        .is_file());
    // sandboxed=false（非 L2 平台）：P1 回归钉子，untrusted 必须仍然写 extensions:[]。
    let settings = session_mgr::build_settings_json(&rec, &layout, false);
    assert_eq!(
        settings["packages"][0]["extensions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
