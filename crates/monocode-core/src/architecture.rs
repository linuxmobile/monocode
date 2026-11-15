//! Guards for the crate boundaries described in `docs/native/migration.md`.
//!
//! `monocode-core` is UI-independent and must stay that way. A stray
//! `use gpui::…` compiles here when the GPUI workspace happens to be in the
//! dependency graph, so the boundary is asserted against the source instead.

/// Every dependency core declares must be UI-independent.
///
/// `crates.io` is not available to a test, so this checks the manifest text for
/// the crates that would drag a windowing toolkit into the service layer.
#[test]
fn core_has_no_gui_dependency() {
    let manifest = include_str!("../Cargo.toml");
    for forbidden in ["gpui", "tauri", "winit", "wry", "gtk"] {
        // The workspace section may name the native crate in a comment; only
        // real dependency lines count.
        let declared = manifest
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#') && !line.starts_with("//"))
            .any(|line| {
                let key = line.split(['=', ' ']).next().unwrap_or_default();
                key == forbidden || key.starts_with(&format!("{forbidden}-"))
            });
        assert!(
            !declared,
            "monocode-core must not depend on `{forbidden}`: keep UI work in monocode-gpui",
        );
    }
}

/// `log` is the one facade shared by core and the app, so core has to declare
/// it rather than borrow a transitive copy.
#[test]
fn core_declares_the_log_facade() {
    let manifest = include_str!("../Cargo.toml");
    assert!(
        manifest
            .lines()
            .map(str::trim)
            .any(|line| line.starts_with("log ") || line.starts_with("log=")),
        "monocode-core logs through the `log` crate and must list it as a dependency",
    );
}
