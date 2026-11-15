use super::{Download, Installation, Installed, PackageKind, Update};
#[cfg(test)]
use std::path::Component;

use std::path::Path;
use std::{fs, process::Command};

pub(super) fn install(
    update: &Update,
    download: Download,
    installation: &Installation,
) -> Result<Installed, String> {
    match installation.kind {
        PackageKind::Appimage => install_appimage(&download.path, &installation.path),

        PackageKind::Deb | PackageKind::Rpm => {
            let extension = installation.kind.suffix();
            let package = download
                ._directory
                .path()
                .join(format!("update.{extension}"));
            fs::rename(&download.path, &package).map_err(|e| e.to_string())?;
            let (command, argument) = if installation.kind == PackageKind::Deb {
                ("/usr/bin/dpkg", "-i")
            } else {
                ("/usr/bin/rpm", "-U")
            };
            success(
                Command::new("pkexec")
                    .arg(command)
                    .arg(argument)
                    .arg("--")
                    .arg(package),
                "The package update was cancelled or failed",
            )?;
            Ok(Installed::Relaunch(installation.path.clone()))
        }
        _ => {
            let _ = (update, download);
            Err("This installation cannot install native updates".into())
        }
    }
}

fn success(command: &mut Command, message: &str) -> Result<(), String> {
    let output = command.output().map_err(|e| format!("{message}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{message}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Prepare first, then swap on the same filesystem, retaining the original if
/// the second rename fails. A failed rollback leaves its backup for recovery.
fn replace(prepared: &Path, target: &Path, backup: &Path) -> Result<(), String> {
    fs::rename(target, backup).map_err(|e| e.to_string())?;
    if let Err(error) = fs::rename(prepared, target) {
        return match fs::rename(backup, target) {
            Ok(()) => Err(format!(
                "Could not install update; the previous version was restored: {error}"
            )),
            Err(rollback) => Err(format!(
                "Could not install update: {error}. The previous version is at {}: {rollback}",
                backup.display()
            )),
        };
    }
    Ok(())
}

fn install_appimage(package: &Path, target: &Path) -> Result<Installed, String> {
    use std::io::Read;
    let mut header = [0u8; 11];
    fs::File::open(package)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(|e| e.to_string())?;
    if &header[..4] != b"\x7fELF" || &header[8..11] != b"AI\x02" {
        return Err("The update is not a type-2 AppImage".into());
    }
    let parent = target
        .parent()
        .ok_or("Cannot locate the AppImage directory")?;
    let stage = tempfile::Builder::new()
        .prefix(".monocode-update-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let prepared = stage.path().join("new.AppImage");
    fs::copy(package, &prepared).map_err(|e| e.to_string())?;
    fs::set_permissions(
        &prepared,
        fs::metadata(target)
            .map_err(|e| e.to_string())?
            .permissions(),
    )
    .map_err(|e| e.to_string())?;
    let backup = stage.path().join("previous.AppImage");
    let result = replace(&prepared, target, &backup);
    if result.is_err() && backup.exists() {
        let _ = stage.keep();
    }
    result?;
    Ok(Installed::Relaunch(target.to_path_buf()))
}

#[cfg(test)]
fn extract_app(package: &Path, directory: &Path) -> Result<(), String> {
    let file = fs::File::open(package).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut expanded = 0u64;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let mut components = path.components();
        if components.next() != Some(Component::Normal("Monocode Native.app".as_ref()))
            || components.any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("The update archive contains an invalid app path".into());
        }
        let kind = entry.header().entry_type();
        // Native bundles currently contain only files and directories. Refuse
        // archive links/devices rather than allowing writes outside the stage.
        if !kind.is_file() && !kind.is_dir() {
            return Err("The update archive contains an unsupported entry".into());
        }
        expanded = expanded.saturating_add(entry.size());
        if expanded > 2 * 1024 * 1024 * 1024 {
            return Err("Expanded update exceeds its size limit".into());
        }
        if !entry.unpack_in(directory).map_err(|e| e.to_string())? {
            return Err("The update archive escaped its staging directory".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_archives_preserve_files_and_refuse_other_roots_and_links() {
        fn archive(path: &Path, name: &str, symlink: bool) {
            let file = fs::File::create(path).unwrap();
            let gzip = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut archive = tar::Builder::new(gzip);
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            if symlink {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_link_name("../../outside").unwrap();
                header.set_size(0);
                header.set_cksum();
                archive.append_data(&mut header, name, &b""[..]).unwrap();
            } else {
                header.set_size(5);
                header.set_cksum();
                archive
                    .append_data(&mut header, name, &b"hello"[..])
                    .unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap();
        }
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package.tar.gz");
        let stage = temp.path().join("stage");
        fs::create_dir(&stage).unwrap();
        archive(
            &package,
            "Monocode Native.app/Contents/MacOS/monocode-gpui",
            false,
        );
        extract_app(&package, &stage).unwrap();
        assert_eq!(
            fs::read(stage.join("Monocode Native.app/Contents/MacOS/monocode-gpui")).unwrap(),
            b"hello"
        );
        archive(&package, "Another.app/Contents/file", false);
        assert!(extract_app(&package, &stage).is_err());
        archive(&package, "Monocode Native.app/escape", true);
        assert!(extract_app(&package, &stage).is_err());
        assert!(!temp.path().join("outside").exists());
    }
    #[test]
    fn failed_swap_restores_the_previous_executable() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("app");
        let backup = temp.path().join("backup");
        fs::write(&target, "old version").unwrap();
        assert!(replace(&temp.path().join("missing"), &target, &backup).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "old version");
        assert!(!backup.exists());
    }
    #[test]
    fn successful_swap_preserves_a_backup_until_the_caller_commits() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("app");
        let prepared = temp.path().join("new");
        let backup = temp.path().join("backup");
        fs::write(&target, "old").unwrap();
        fs::write(&prepared, "new").unwrap();
        replace(&prepared, &target, &backup).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "new");
        assert_eq!(fs::read_to_string(backup).unwrap(), "old");
    }
}
