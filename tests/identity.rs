//! The program reports one version everywhere: its package version, which the
//! packaged executable's name (engine-iso-<version>) also carries.
use serde_json::Value;
use std::process::Command;

fn run(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_engine-iso"))
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn version_is_the_package_version_everywhere() {
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(run(&["--version"]).trim(), format!("engine-iso {version}"));
    let hello: Value = serde_json::from_str(&run(&["--describe", "--threads", "1"])).unwrap();
    assert_eq!(hello["engine"], "engine-iso");
    assert_eq!(hello["version"], version);
    let capabilities: Value = serde_json::from_str(&run(&["--capabilities"])).unwrap();
    assert_eq!(capabilities["implementation"]["version"], version);
    assert_eq!(iso_rust::engine::version(), version);
    // Once reported, the version cannot change.
    assert!(iso_rust::engine::set_version("0.0.0-other").is_err());
}
