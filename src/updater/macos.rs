use super::{Platform, Update, verify_file};
use anyhow::{Context, Result, ensure};
use semver::Version;
use std::{
    fs::{self, DirBuilder, File},
    io::Read,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

const EXECUTABLE: &str = "Contents/MacOS/pordo";
const BUNDLE_ID: &str = "io.github.l-chris.replayer";
pub(super) const INSTALL_LOCATION: &str = "Pordo must run from a writable application bundle";
const HELPER: &str = include_str!("../../scripts/install-update-macos.sh");

pub(super) fn launch_installer(update: &Update, archive: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let target = exe.ancestors().nth(3).context(INSTALL_LOCATION)?;
    ensure!(target.join(EXECUTABLE) == exe, INSTALL_LOCATION);
    let (_, child) = prepare(update, archive, target, std::process::id())?;
    drop(child); // The helper continues after this process exits.
    let _ = fs::remove_file(archive);
    Ok(())
}

fn prepare(update: &Update, archive: &Path, target: &Path, pid: u32) -> Result<(PathBuf, Child)> {
    ensure!(
        target.extension().is_some_and(|ext| ext == "app")
            && !target.starts_with("/System")
            && !target
                .components()
                .any(|part| part.as_os_str() == "AppTranslocation")
            && target.canonicalize().is_ok_and(|path| path == target),
        INSTALL_LOCATION
    );
    let installed_version = bundle_version(target)?;
    ensure!(
        Version::parse(&update.version)? > Version::parse(&installed_version)?,
        "The update is not newer than the installed application"
    );
    ensure!(
        !other_instance(target, pid)?,
        "Close other windows of this installation before updating"
    );
    let staging = target
        .parent()
        .context(INSTALL_LOCATION)?
        .join(format!(".pordo-update-{}", uuid::Uuid::new_v4()));
    // Stage alongside the application so installation only needs renames.
    DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(|error| {
            eprintln!(
                "Could not stage the update in {}: {error}",
                staging.display()
            );
            anyhow::anyhow!(INSTALL_LOCATION)
        })?;
    match stage_and_launch(update, archive, target, pid, &staging) {
        Ok(child) => Ok((staging, child)),
        Err(error) => {
            let _ = fs::remove_dir_all(staging);
            Err(error)
        }
    }
}

fn stage_and_launch(
    update: &Update,
    archive: &Path,
    target: &Path,
    pid: u32,
    staging: &Path,
) -> Result<Child> {
    let local_zip = staging.join("update.zip");
    fs::copy(archive, &local_zip).context("Could not stage the update ZIP")?;
    verify_file(update, &local_zip)?;
    // ditto preserves the signed bundle's resource metadata and internal symlink.
    checked(
        Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(&local_zip)
            .arg(staging),
    )
    .context("Could not extract the update ZIP")?;
    fs::remove_file(local_zip)?;
    let replacement = staging.join("Pordo.app");
    ensure!(
        replacement
            .canonicalize()
            .is_ok_and(|path| path == replacement),
        "The ZIP does not contain Pordo.app"
    );
    ensure!(
        bundle_version(&replacement)? == update.version,
        "Update bundle version mismatch"
    );
    verify_architecture(&replacement.join(EXECUTABLE), update.platform)?;
    let mut signature = Command::new("/usr/bin/codesign");
    signature.args(["--verify", "--deep", "--strict"]);
    if let Some(team) = signing_team(target)? {
        signature.arg("-R").arg(format!(
            "anchor apple generic and certificate leaf[subject.OU] = \"{team}\""
        ));
    }
    checked(signature.arg(&replacement)).context("Update code signature verification failed")?;
    let log = File::create(staging.join("installer.log"))?;
    Command::new("/bin/bash")
        .args(["-c", HELPER, "pordo-updater"])
        .arg(pid.to_string())
        .arg(target)
        .arg(staging)
        .arg(&update.version)
        .current_dir(target.parent().context(INSTALL_LOCATION)?)
        .env_remove("PORDO_UPDATE_ROLLBACK")
        .env_remove("PORDO_UPDATE_VERSION")
        .env_remove("PORDO_UPDATE_STAGING")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .context("Could not start the macOS update installer")
}

fn checked(command: &mut Command) -> Result<std::process::Output> {
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "{}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

fn bundle_version(app: &Path) -> Result<String> {
    let plist = |key| -> Result<String> {
        let output = checked(
            Command::new("/usr/libexec/PlistBuddy")
                .arg("-c")
                .arg(format!("Print :{key}"))
                .arg(app.join("Contents/Info.plist")),
        )?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    ensure!(
        plist("CFBundleIdentifier")? == BUNDLE_ID && plist("CFBundleExecutable")? == "pordo",
        "The application bundle is not Pordo"
    );
    plist("CFBundleShortVersionString")
}

fn signing_team(app: &Path) -> Result<Option<String>> {
    let output = checked(
        Command::new("/usr/bin/codesign")
            .args(["-d", "--verbose=4"])
            .arg(app),
    )?;
    let description = String::from_utf8_lossy(&output.stderr);
    let team = description
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="));
    match team {
        Some("not set") => Ok(None),
        Some(team)
            if team.len() == 10
                && team
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) =>
        {
            Ok(Some(team.to_owned()))
        }
        _ => anyhow::bail!("Could not verify the installed application's signing identity"),
    }
}

fn verify_architecture(binary: &Path, platform: Platform) -> Result<()> {
    let expected: u32 = match platform {
        Platform::MacArm64 => 0x0100_000c,
        Platform::MacX64 => 0x0100_0007,
        Platform::WindowsX64 => anyhow::bail!("The update is not a macOS package"),
    };
    // Official packages contain a single little-endian 64-bit Mach-O slice.
    let mut header = [0; 8];
    File::open(binary)?.read_exact(&mut header)?;
    ensure!(
        header[..4] == 0xfeed_facfu32.to_le_bytes() && header[4..] == expected.to_le_bytes(),
        "Update executable architecture mismatch"
    );
    Ok(())
}

fn other_instance(target: &Path, parent: u32) -> Result<bool> {
    let output = checked(Command::new("/bin/ps").args(["-ww", "-axo", "pid=,comm="]))?;
    let exe = target.join(EXECUTABLE);
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let Some((pid, command)) = line.trim_start().split_once(char::is_whitespace) else {
            return false;
        };
        pid.parse::<u32>().is_ok_and(|pid| pid != parent) && Path::new(command.trim_start()) == exe
    }))
}

pub(super) fn startup_marker() -> Option<PathBuf> {
    let version = std::env::var("PORDO_UPDATE_VERSION").ok()?;
    if version != env!("CARGO_PKG_VERSION") {
        return None;
    }
    let staging = PathBuf::from(std::env::var_os("PORDO_UPDATE_STAGING")?);
    let installed_app = std::env::current_exe().ok()?;
    let bundle = installed_app.ancestors().nth(3)?;
    if staging.parent()? != bundle.parent()?
        || !staging.file_name()?.to_str()?.starts_with(".pordo-update-")
        || !staging.join("previous.app").is_dir()
    {
        return None;
    }
    Some(staging.join("healthy"))
}

pub(super) fn mark_healthy(marker: &Path) -> Result<()> {
    File::create_new(marker)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::{Asset, REPOSITORY, Updater};
    use std::time::{Duration, Instant};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
            fs::create_dir_all(&base).unwrap();
            let base = base.canonicalize().unwrap();
            let dir = base.join(format!("Pordo updater test {}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn bundle(&self, name: &str, version: &str, healthy: bool) -> PathBuf {
            let app = self.0.join(name);
            let executable = app.join(EXECUTABLE);
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(
                app.join("Contents/Info.plist"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
<key>CFBundleExecutable</key><string>pordo</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#
                ),
            )
            .unwrap();

            let source = self.0.join("app.c");
            fs::write(
                &source,
                format!(
                    r#"
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {{
    if (argc > 1) {{ sleep(30); return 0; }}
    if (getenv("PORDO_UPDATE_ROLLBACK")) {{
        FILE *marker = fopen("rollback-confirmed", "w");
        if (!marker) return 1;
        fclose(marker);
    }}
    const char *stage = getenv("PORDO_UPDATE_STAGING");
    if (stage && {success}) {{
        char path[4096];
        snprintf(path, sizeof(path), "%s/healthy", stage);
        FILE *marker = fopen(path, "w");
        if (!marker) return 1;
        fclose(marker);
        sleep(2);
    }}
    return 0;
}}"#,
                    success = u8::from(healthy)
                ),
            )
            .unwrap();
            let arch = if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "x86_64"
            };
            checked(
                Command::new("/usr/bin/clang")
                    .args(["-arch", arch])
                    .arg(source)
                    .arg("-o")
                    .arg(executable),
            )
            .unwrap();
            checked(
                Command::new("/usr/bin/codesign")
                    .args(["--force", "--sign", "-"])
                    .arg(&app),
            )
            .unwrap();
            app
        }

        fn update(&self, app: &Path) -> (Update, PathBuf) {
            let archive = self.0.join("release.zip");
            checked(
                Command::new("/usr/bin/ditto")
                    .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
                    .arg(app)
                    .arg(&archive),
            )
            .unwrap();
            let version = "0.2.0".to_owned();
            let platform = Platform::current().unwrap();
            let name = platform.asset_name(&version);
            let bytes = fs::read(&archive).unwrap();
            let asset = Asset {
                browser_download_url: format!("{REPOSITORY}/releases/download/v{version}/{name}"),
                name,
                digest: Some(format!("sha256:{}", crate::crypto::sha256_hex(&bytes))),
                size: bytes.len() as u64,
            };
            (
                Update {
                    version,
                    notes: String::new(),
                    asset,
                    platform,
                },
                archive,
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn installer_waits_for_exit_then_commits_or_restores() {
        for healthy in [true, false] {
            let fixture = Fixture::new();
            let installed = fixture.bundle("My Pordo.app", "0.1.0", false);
            let replacement = fixture.bundle("Pordo.app", "0.2.0", healthy);
            let (update, archive) = fixture.update(&replacement);

            let mut current = Command::new(installed.join(EXECUTABLE))
                .arg("wait")
                .spawn()
                .unwrap();
            let staging = prepare(&update, &archive, &installed, current.id());
            assert_eq!(bundle_version(&installed).unwrap(), "0.1.0");
            current.kill().unwrap();
            current.wait().unwrap();
            let (stage, mut installer) = staging.unwrap();
            assert_eq!(installer.wait().unwrap().success(), healthy);
            assert!(!stage.exists());
            assert_eq!(
                bundle_version(&installed).unwrap(),
                if healthy { "0.2.0" } else { "0.1.0" }
            );
            if !healthy {
                let marker = fixture.0.join("rollback-confirmed");
                let deadline = Instant::now() + Duration::from_secs(5);
                while !marker.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                assert!(marker.exists());
            }
        }
    }

    #[test]
    fn invalid_archive_is_rejected_while_the_old_app_is_in_place() {
        let fixture = Fixture::new();
        let installed = fixture.bundle("My Pordo.app", "0.1.0", false);
        let replacement = fixture.bundle("Pordo.app", "0.2.0", true);
        let (mut update, archive) = fixture.update(&replacement);
        update.asset.digest = Some(format!("sha256:{}", "0".repeat(64)));
        let error = prepare(&update, &archive, &installed, std::process::id()).unwrap_err();
        assert!(error.to_string().contains("SHA-256 mismatch"));
        assert_eq!(bundle_version(&installed).unwrap(), "0.1.0");
        assert!(!fs::read_dir(&fixture.0).unwrap().flatten().any(|f| {
            f.file_name()
                .to_string_lossy()
                .starts_with(".pordo-update-")
        }));
    }

    #[test]
    fn health_marker_waits_for_the_first_gui_frame() {
        let fixture = Fixture::new();
        let marker = fixture.0.join("healthy");
        let ctx = eframe::egui::Context::default();
        let mut updater = Updater::new(false, ctx.clone());
        updater.startup_health = Some(marker.clone());
        assert!(!updater.poll());
        assert!(!marker.exists());
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        assert!(!updater.poll());
        assert!(marker.is_file());
    }
}
