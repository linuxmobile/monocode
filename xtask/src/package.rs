//! Turns a prebuilt executable into Linux packages.
//!
//! This never builds and never touches release paths: it takes the binary it is
//! given and writes files under the output directory. That keeps packaging
//! reproducible from an already-built artifact and impossible to run by accident
//! during development.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The licence files that must ship with the app, mapped to their final names.
const NOTICES: [(&str, &str); 7] = [
    ("LICENSE", "LICENSE"),
    ("NOTICE", "NOTICE"),
    ("docs/native/LICENSE-GPUI-APACHE", "LICENSE-GPUI-APACHE"),
    ("docs/native/LICENSE-MERMAID-MIT", "LICENSE-MERMAID-MIT"),
    (
        "crates/monocode-gpui/assets/hugeicons.LICENSE",
        "hugeicons.LICENSE",
    ),
    (
        "crates/monocode-gpui/assets/dark-plus.LICENSE",
        "dark-plus.LICENSE",
    ),
    (
        "crates/monocode-gpui/assets/native-editor.LICENSE.txt",
        "native-editor.LICENSE.txt",
    ),
];

/// A parsed preview version such as `0.16.0-preview.1`.
pub struct Preview {
    pub base: String,
    pub sequence: String,
}

/// Parses a preview version, refusing anything Cargo or a package manager
/// would read differently.
pub fn preview_version(version: &str) -> Result<Preview, String> {
    let (base, sequence) = version
        .split_once("-preview.")
        .ok_or_else(|| "Expected a native preview version such as 0.16.0-preview.1".to_string())?;
    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        // A leading zero would make the component mean something else in some
        // parsers, so it is rejected rather than silently accepted.
        || parts
            .iter()
            .any(|part| part.len() > 1 && part.starts_with('0'))
    {
        return Err("Expected a native preview version such as 0.16.0-preview.1".into());
    }
    if sequence.is_empty()
        || !sequence.bytes().all(|byte| byte.is_ascii_digit())
        || (sequence.len() > 1 && sequence.starts_with('0'))
    {
        return Err("Expected a native preview version such as 0.16.0-preview.1".into());
    }
    // Debian and RPM each store these components in a fixed-width field.
    for part in parts.iter().copied().chain(std::iter::once(sequence)) {
        if part.parse::<u32>().map(|n| n > 65535).unwrap_or(true) {
            return Err(format!(
                "Expected a native preview version such as 0.16.0-preview.1, got {version}"
            ));
        }
    }
    Ok(Preview {
        base: base.to_string(),
        sequence: sequence.to_string(),
    })
}

/// Copies a file, failing loudly rather than shipping a package with a hole.
fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::copy(from, to).map_err(|e| format!("copy {}: {e}", from.display()))?;
    Ok(())
}

fn write_file(path: &Path, contents: &str, mode: Option<u32>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Every packaged tree needs the same licences beside the executable.
fn notices(root: &Path, directory: &Path) -> Result<(), String> {
    for (source, name) in NOTICES {
        copy(&root.join(source), &directory.join(name))?;
    }
    copy(
        &root.join("crates/monocode-gpui/assets/material/LICENSE"),
        &directory.join("material-icons.LICENSE"),
    )
}

/// Lays out the files both the deb and the AppImage share.
fn linux_tree(root: &Path, directory: &Path, binary: &Path, kind: &str) -> Result<String, String> {
    let app = directory.join("usr/lib/monocode-native");
    copy(binary, &app.join("monocode-gpui"))?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            app.join("monocode-gpui"),
            std::fs::Permissions::from_mode(0o755),
        )
        .map_err(|e| format!("chmod the executable: {e}"))?;
    }
    // The packaged app reads this to know it is an install, not a dev build.
    write_file(
        &app.join("native-install.json"),
        &serde_json::json!({ "app_id": "dev.usemono.gpui", "kind": kind }).to_string(),
        None,
    )?;
    notices(root, &app.join("licenses"))?;
    write_file(
        &directory.join("usr/bin/monocode-native"),
        "#!/bin/sh\nexec /usr/lib/monocode-native/monocode-gpui \"$@\"\n",
        Some(0o755),
    )?;
    let desktop = "[Desktop Entry]\nType=Application\nName=Monocode Native\nExec=monocode-native %F\nIcon=monocode-native\nCategories=Development;\nTerminal=false\nStartupWMClass=dev.usemono.gpui\n";
    write_file(
        &directory.join("usr/share/applications/monocode-native.desktop"),
        desktop,
        None,
    )?;
    copy(
        &root.join("assets/icons/monocode.png"),
        &directory.join("usr/share/icons/hicolor/256x256/apps/monocode-native.png"),
    )?;
    Ok(desktop.to_string())
}

fn run(command: &str, arguments: &[String], env: Option<&[(&str, &str)]>) -> Result<(), String> {
    let mut invocation = Command::new(command);
    invocation.args(arguments);
    if let Some(env) = env {
        for (key, value) in env {
            invocation.env(key, value);
        }
    }
    let status = invocation
        .status()
        .map_err(|e| format!("Could not run {command}: {e}"))?;
    if !status.success() {
        return Err(format!("{command} failed ({status})"));
    }
    Ok(())
}

/// Builds the requested package and writes an artifacts index beside it.
pub fn package_native(
    root: &Path,
    kind: &str,
    binary: &Path,
    out: &Path,
    arch: &str,
) -> Result<(), String> {
    // The version comes from the environment so a caller cannot package a
    // binary whose version disagrees with the release by typo.
    let version = std::env::var("MONOCODE_NATIVE_VERSION")
        .map_err(|_| "MONOCODE_NATIVE_VERSION is required".to_string())?;
    let parsed = preview_version(&version)?;
    // A binary of the wrong version would ship as a mismatch nobody notices
    // until an update fails.
    let reported = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| format!("Could not run {}: {e}", binary.display()))?;
    let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    if reported != format!("Monocode Native {version}") {
        return Err(format!(
            "The executable reports {reported:?}, which does not match {version}"
        ));
    }
    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let work = out.join("work");
    std::fs::create_dir_all(&work).map_err(|e| format!("create {}: {e}", work.display()))?;
    let mut artifacts = Vec::new();

    match kind {
        "linux" => {
            let deb_root = work.join("deb");
            linux_tree(root, &deb_root, binary, "deb")?;
            // `~` sorts before any release of the same base version, which is
            // what makes a preview upgradeable to the final build.
            write_file(
                &deb_root.join("DEBIAN/control"),
                &format!(
                    "Package: monocode-native\nVersion: {}\nArchitecture: amd64\nMaintainer: Monocode maintainers\nSection: devel\nPriority: optional\nDepends: libasound2, libfontconfig1, libfreetype6, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libx11-xcb1, libxcb1, libxcb-shape0, libxcb-xfixes0, libxcb-randr0, libegl1, libvulkan1, libdbus-1-3, pkexec | policykit-1\nDescription: Monocode Native experimental preview\n",
                    version.replace("-preview.", "~preview.")
                ),
                None,
            )?;
            let deb = out.join(format!("monocode-native_{version}_amd64.deb"));
            run(
                "dpkg-deb",
                &[
                    "--build".into(),
                    // Package files belong to root regardless of who built them.
                    "--root-owner-group".into(),
                    deb_root.to_string_lossy().into_owned(),
                    deb.to_string_lossy().into_owned(),
                ],
                None,
            )?;
            artifacts.push(serde_json::json!({
                "filename": deb.file_name().unwrap().to_string_lossy(),
                "target": "linux-x86_64-deb",
            }));

            let appdir = work.join("MonocodeNative.AppDir");
            linux_tree(root, &appdir, binary, "appimage")?;
            // An AppImage runs from an unpredictable path, so it uses the
            // bundled name rather than the system wrapper.
            write_file(
                &appdir.join("monocode-native.desktop"),
                "[Desktop Entry]\nType=Application\nName=Monocode Native\nExec=monocode-gpui %F\nIcon=monocode-native\nCategories=Development;\nTerminal=false\nStartupWMClass=dev.usemono.gpui\n",
                None,
            )?;
            copy(
                &root.join("assets/icons/monocode.png"),
                &appdir.join("monocode-native.png"),
            )?;
            write_file(
                &appdir.join("AppRun"),
                "#!/bin/sh\nexec \"$APPDIR/usr/lib/monocode-native/monocode-gpui\" \"$@\"\n",
                Some(0o755),
            )?;
            let appimage = out.join(format!("MonocodeNative_{version}_amd64.AppImage"));
            let tool = std::env::var("APPIMAGETOOL").unwrap_or_else(|_| "appimagetool".into());
            run(
                &tool,
                &[
                    appdir.to_string_lossy().into_owned(),
                    appimage.to_string_lossy().into_owned(),
                ],
                Some(&[("ARCH", "x86_64"), ("APPIMAGE_EXTRACT_AND_RUN", "1")]),
            )?;
            artifacts.push(serde_json::json!({
                "filename": appimage.file_name().unwrap().to_string_lossy(),
                "target": "linux-x86_64-appimage",
            }));
        }
        "rpm" => {
            let top = work.join("rpmbuild");
            for name in ["BUILD", "RPMS", "SOURCES", "SPECS", "SRPMS"] {
                std::fs::create_dir_all(top.join(name))
                    .map_err(|e| format!("create {}: {e}", top.join(name).display()))?;
            }
            linux_tree(root, &top.join("SOURCES/tree"), binary, "rpm")?;
            let spec = top.join("SPECS/monocode-native.spec");
            write_file(
                &spec,
                &format!(
                    "Name: monocode-native\nVersion: {}\nRelease: 0.preview.{}{{?dist}}\nSummary: Monocode Native experimental preview\nLicense: MIT AND Apache-2.0\nBuildArch: x86_64\nRequires: polkit\n%description\nMonocode Native experimental preview.\n%install\nmkdir -p %{{buildroot}}\ncp -a %{{_sourcedir}}/tree/. %{{buildroot}}/\n%files\n/usr/bin/monocode-native\n/usr/lib/monocode-native\n/usr/share/applications/monocode-native.desktop\n/usr/share/icons/hicolor/256x256/apps/monocode-native.png\n",
                    parsed.base, parsed.sequence
                ),
                None,
            )?;
            run(
                "rpmbuild",
                &[
                    "--define".into(),
                    format!("_topdir {}", top.display()),
                    // A stray build-id symlink would appear as an extra file.
                    "--define".into(),
                    "_build_id_links none".into(),
                    "-bb".into(),
                    spec.to_string_lossy().into_owned(),
                ],
                None,
            )?;
            let directory = top.join("RPMS/x86_64");
            let mut produced: Vec<PathBuf> = std::fs::read_dir(&directory)
                .map_err(|e| format!("read {}: {e}", directory.display()))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    name.ends_with(".rpm")
                        && !name.contains("debuginfo")
                        && !name.contains("debugsource")
                })
                .collect();
            if produced.len() != 1 {
                return Err(format!(
                    "Expected exactly one rpm, found {}",
                    produced.len()
                ));
            }
            let rpm = produced.pop().unwrap();
            let name = rpm.file_name().unwrap().to_string_lossy().into_owned();
            copy(&rpm, &out.join(&name))?;
            artifacts.push(serde_json::json!({
                "filename": name,
                "target": "linux-x86_64-rpm",
            }));
        }
        other => return Err(format!("Unknown native package kind: {other}")),
    }

    let index = out.join(format!("artifacts-{kind}-{arch}.json"));
    let text = serde_json::to_string_pretty(&artifacts).map_err(|e| e.to_string())?;
    std::fs::write(&index, text).map_err(|e| format!("write {}: {e}", index.display()))?;
    for artifact in &artifacts {
        println!("{}", artifact["filename"]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preview_version_is_split_into_its_parts() {
        let parsed = preview_version("0.16.0-preview.3").unwrap();
        assert_eq!(parsed.base, "0.16.0");
        assert_eq!(parsed.sequence, "3");
    }

    #[test]
    fn a_release_version_is_not_a_preview() {
        // Packaging a non-preview would write a channel the updater cannot read.
        assert!(preview_version("0.16.0").is_err());
        assert!(preview_version("0.16.0-preview").is_err());
        assert!(preview_version("0.16.0-preview.1.2").is_err());
    }

    #[test]
    fn components_that_do_not_fit_a_package_field_are_rejected() {
        // 65536 overflows the field Debian and RPM use.
        assert!(preview_version("0.16.0-preview.65536").is_err());
        assert!(preview_version("65536.0.0-preview.1").is_err());
        assert!(preview_version("0.16.0-preview.0").is_ok());
        assert!(preview_version("0.16.0-preview.65535").is_ok());
    }

    #[test]
    fn non_numeric_or_padded_components_are_rejected() {
        for version in [
            "0.16.x-preview.1",
            "0.16.0-preview.x",
            "0.-1.0-preview.1",
            "00.16.0-preview.1",
            "0.16.0-preview.01",
            "-preview.1",
            "",
        ] {
            assert!(
                preview_version(version).is_err(),
                "{version:?} was accepted"
            );
        }
    }
}
