//! Self-update. Looks for a newer release on GitHub, downloads it in the
//! background, and swaps it in when the user chooses to restart.
//!
//! Every release publishes the app and a small manifest with its version
//! and checksum: `Nexus.zip` and `latest.json` for the Mac, `Nexus.exe` and
//! `latest-windows.json` for Windows. The work is done by tools that ship
//! with the system: `curl` on both, `shasum`, `ditto`, `plutil` and
//! `codesign` on a Mac, `certutil` on Windows.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context as _, Result};
use gpui::{div, prelude::*, px, App, Context, Entity, Global, Hsla, SharedString, Task, Window};
use serde::Deserialize;

use crate::assets::icon;
use crate::platform;
use crate::theme;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_URL: &str = "https://github.com/mileslow/nexus/releases/latest";
const MANIFEST_URL: &str = if cfg!(windows) {
    "https://github.com/mileslow/nexus/releases/latest/download/latest-windows.json"
} else {
    "https://github.com/mileslow/nexus/releases/latest/download/latest.json"
};
const CURL: &str = if cfg!(windows) {
    "curl.exe"
} else {
    "/usr/bin/curl"
};
/// Names the process a freshly updated copy waits for before starting.
#[cfg(windows)]
const WAIT_FOR_PID: &str = "NEXUS_WAIT_FOR_PID";

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(8);
/// Often enough that a release shows up in a running copy within minutes.
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
/// How long "Up to date" and error messages stay in the title bar.
const NOTICE_FOR: Duration = Duration::from_secs(6);

/// The `latest.json` published with every release.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
}

/// `1.2.3` or `v1.2.3` as numbers. Anything else is not a version.
pub fn parse_version(text: &str) -> Option<[u64; 3]> {
    let text = text.trim();
    let text = text.strip_prefix('v').unwrap_or(text);
    let mut parts = text.split('.');
    let version = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    parts.next().is_none().then_some(version)
}

/// True when `candidate` is a later version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// The `.app` bundle an executable lives in, if it lives in one.
#[cfg(any(target_os = "macos", test))]
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let bundled = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    bundled.then(|| bundle.to_path_buf())
}

/// True for a program built by `cargo`, which is not an installed copy.
#[cfg(any(windows, test))]
pub fn is_cargo_build(exe: &Path) -> bool {
    exe.parent()
        .and_then(Path::file_name)
        .is_some_and(|folder| folder == "debug" || folder == "release" || folder == "deps")
}

/// What an update replaces: the app bundle on a Mac, `Nexus.exe` on
/// Windows. A build run straight from `cargo` has neither.
fn installed() -> Option<PathBuf> {
    let exe = platform::canonical(&std::env::current_exe().ok()?)?;
    #[cfg(target_os = "macos")]
    return bundle_of(&exe);
    #[cfg(windows)]
    return (!is_cargo_build(&exe)).then_some(exe);
    #[cfg(not(any(target_os = "macos", windows)))]
    return None;
}

/// Runs a tool and returns what it printed, or an error naming what failed.
fn run(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = platform::command(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run {program}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.lines().last().unwrap_or("").trim().to_string();
        bail!("{program} failed: {message}");
    }
    Ok(output.stdout)
}

fn fetch_manifest(url: &str) -> Result<Manifest> {
    let body = run(CURL, &["-fsSL", "--max-time", "20", url])
        .context("could not reach the release server")?;
    serde_json::from_slice(&body).context("the release information was not understood")
}

fn download(url: &str, to: &Path) -> Result<()> {
    let to = to.to_string_lossy();
    run(CURL, &["-fsSL", "--max-time", "900", "-o", &to, url]).context("the download failed")?;
    Ok(())
}

#[cfg(not(windows))]
fn sha256_of(path: &Path) -> Result<String> {
    let path = path.to_string_lossy();
    let output = run("/usr/bin/shasum", &["-a", "256", &path])?;
    let text = String::from_utf8_lossy(&output);
    text.split_whitespace()
        .next()
        .map(str::to_lowercase)
        .ok_or_else(|| anyhow!("shasum printed nothing"))
}

#[cfg(windows)]
fn sha256_of(path: &Path) -> Result<String> {
    let path = path.to_string_lossy();
    let output = run("certutil.exe", &["-hashfile", &path, "SHA256"])?;
    hash_from_certutil(&String::from_utf8_lossy(&output))
        .ok_or_else(|| anyhow!("certutil printed no checksum"))
}

/// The checksum in what `certutil -hashfile` prints: the one line that is
/// 64 hex digits. Older systems put spaces between the bytes.
#[cfg(any(windows, test))]
fn hash_from_certutil(output: &str) -> Option<String> {
    output
        .lines()
        .map(|line| line.replace(' ', "").trim().to_lowercase())
        .find(|line| line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(target_os = "macos")]
fn bundle_version(app: &Path) -> Result<String> {
    let plist = app.join("Contents/Info.plist");
    let output = run(
        "/usr/bin/plutil",
        &[
            "-extract",
            "CFBundleShortVersionString",
            "raw",
            "-o",
            "-",
            &plist.to_string_lossy(),
        ],
    )?;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}

/// Downloads the release into `dir`, checks it, and returns the unpacked app.
#[cfg(target_os = "macos")]
fn stage(manifest: &Manifest, dir: &Path) -> Result<PathBuf> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir)?;
    let zip = dir.join("Nexus.zip");
    download(&manifest.url, &zip)?;
    ensure!(
        sha256_of(&zip)? == manifest.sha256.trim().to_lowercase(),
        "the download did not match its checksum"
    );

    let unpacked = dir.join("unpacked");
    run(
        "/usr/bin/ditto",
        &[
            "-x",
            "-k",
            &zip.to_string_lossy(),
            &unpacked.to_string_lossy(),
        ],
    )?;
    let _ = fs::remove_file(&zip);
    let app = unpacked.join("Nexus.app");
    ensure!(
        app.join("Contents/MacOS/nexus").is_file(),
        "the download did not contain Nexus.app"
    );
    run(
        "/usr/bin/codesign",
        &["--verify", "--deep", "--strict", &app.to_string_lossy()],
    )
    .context("the downloaded app is damaged")?;
    // A release stamped with the wrong version would be offered forever.
    let version = bundle_version(&app)?;
    ensure!(
        version == manifest.version,
        "the download is version {version}, not {}",
        manifest.version
    );
    Ok(app)
}

/// Downloads the release into `dir`, checks it, and returns the new program.
#[cfg(windows)]
fn stage(manifest: &Manifest, dir: &Path) -> Result<PathBuf> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir)?;
    let exe = dir.join("Nexus.exe");
    download(&manifest.url, &exe)?;
    ensure!(
        sha256_of(&exe)? == manifest.sha256.trim().to_lowercase(),
        "the download did not match its checksum"
    );
    // Asking for its version shows that it runs, and a release stamped with
    // the wrong version would be offered forever.
    let output =
        run(&exe.to_string_lossy(), &["--version"]).context("the downloaded app does not run")?;
    let output = String::from_utf8_lossy(&output);
    let version = output.split_whitespace().last().unwrap_or("");
    ensure!(
        version == manifest.version,
        "the download is version {version}, not {}",
        manifest.version
    );
    Ok(exe)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn stage(_manifest: &Manifest, _dir: &Path) -> Result<PathBuf> {
    bail!("this system cannot update itself")
}

fn remove(path: &Path) {
    let _ = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
}

/// Moves an app into place, copying when it has to cross to another volume.
fn move_into_place(from: &Path, to: &Path) -> Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    #[cfg(not(windows))]
    run(
        "/usr/bin/ditto",
        &[&from.to_string_lossy(), &to.to_string_lossy()],
    )?;
    #[cfg(windows)]
    fs::copy(from, to)?;
    remove(from);
    Ok(())
}

/// Where the app being replaced waits until the new one is in place.
fn backup_of(installed: &Path) -> Option<PathBuf> {
    let name = installed.file_name()?.to_string_lossy();
    Some(installed.with_file_name(format!(".{name}.previous")))
}

/// Puts the app at `staged` where `installed` is. The old app stays beside
/// it until the new one is in place, and comes back if anything goes wrong.
pub fn swap(staged: &Path, installed: &Path) -> Result<()> {
    let backup = backup_of(installed).ok_or_else(|| anyhow!("the app has no name"))?;
    remove(&backup);
    fs::rename(installed, &backup).with_context(|| {
        format!(
            "could not replace {} (is its folder writable?)",
            installed.display()
        )
    })?;
    if let Err(error) = move_into_place(staged, installed) {
        remove(installed);
        let _ = fs::rename(&backup, installed);
        return Err(error.context("could not put the new version in place"));
    }
    // Windows keeps a running program's file until it exits; that copy is
    // cleared away the next time the app starts.
    remove(&backup);
    Ok(())
}

/// Opens `bundle` once this process has exited.
#[cfg(unix)]
fn relaunch_after_exit(bundle: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    let script = r#"while kill -0 "$0" 2>/dev/null; do sleep 0.1; done; open "$1""#;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(std::process::id().to_string())
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("could not schedule the restart")?;
    Ok(())
}

/// Starts the new program, which waits for this one to exit first.
#[cfg(windows)]
fn relaunch_after_exit(exe: &Path) -> Result<()> {
    Command::new(exe)
        .env(WAIT_FOR_PID, std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not schedule the restart")?;
    Ok(())
}

/// Called first thing at launch. A copy started by an update waits for the
/// copy it replaces to exit, so the two never run side by side.
pub fn wait_for_previous() {
    #[cfg(windows)]
    if let Some(pid) = std::env::var(WAIT_FOR_PID)
        .ok()
        .and_then(|pid| pid.parse().ok())
    {
        std::env::remove_var(WAIT_FOR_PID);
        platform::wait_for_exit(pid, Duration::from_secs(15));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    /// Downloaded and checked; installed when the user restarts.
    Ready {
        version: String,
        staged: PathBuf,
    },
    /// A newer release exists but this build cannot replace itself.
    Available(String),
    Failed(String),
}

pub struct Updater {
    status: Status,
    /// What an update replaces. Builds run straight from `cargo` have
    /// nothing to replace, and only point at the release page.
    installed: Option<PathBuf>,
    manifest_url: String,
    work: Option<Task<()>>,
    notice: Option<Task<()>>,
}

struct GlobalUpdater(Entity<Updater>);

impl Global for GlobalUpdater {}

impl Updater {
    /// Creates the app-wide updater and starts checking in the background.
    pub fn init(cx: &mut App) {
        let installed = installed();
        // What is left of the copy the last update replaced.
        if let Some(backup) = installed.as_deref().and_then(backup_of) {
            remove(&backup);
        }
        // `NEXUS_UPDATE_MANIFEST` points at a different `latest.json`, for
        // trying a release before publishing it.
        let manifest_url =
            std::env::var("NEXUS_UPDATE_MANIFEST").unwrap_or_else(|_| MANIFEST_URL.to_string());
        let automatic = installed.is_some() && std::env::var_os("NEXUS_NO_UPDATE_CHECK").is_none();
        let updater = cx.new(|_| Updater {
            status: Status::Idle,
            installed,
            manifest_url,
            work: None,
            notice: None,
        });
        if automatic {
            let weak = updater.downgrade();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(FIRST_CHECK_AFTER).await;
                loop {
                    if weak.update(cx, |this, cx| this.check(false, cx)).is_err() {
                        break;
                    }
                    cx.background_executor().timer(CHECK_EVERY).await;
                }
            })
            .detach();
        }
        cx.set_global(GlobalUpdater(updater));
    }

    pub fn global(cx: &App) -> Entity<Updater> {
        cx.global::<GlobalUpdater>().0.clone()
    }

    fn set_status(&mut self, status: Status, cx: &mut Context<Self>) {
        self.notice = None;
        // Passing messages clear themselves.
        if matches!(status, Status::UpToDate | Status::Failed(_)) {
            self.notice = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(NOTICE_FOR).await;
                let _ = this.update(cx, |this, cx| {
                    this.status = Status::Idle;
                    cx.notify();
                });
            }));
        }
        self.status = status;
        cx.notify();
    }

    /// Looks for a newer release. `manual` means the user asked, so the
    /// outcome is shown even when there is nothing new or the check fails.
    pub fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if matches!(
            self.status,
            Status::Checking | Status::Downloading(_) | Status::Ready { .. }
        ) {
            return;
        }
        if manual {
            self.set_status(Status::Checking, cx);
        }
        let url = self.manifest_url.clone();
        let can_install = self.installed.is_some();
        self.work = Some(cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_spawn(async move { fetch_manifest(&url) })
                .await;
            let manifest = match fetched {
                Ok(manifest) if is_newer(&manifest.version, VERSION) => manifest,
                Ok(_) => {
                    let _ = this.update(cx, |this, cx| {
                        let status = if manual {
                            Status::UpToDate
                        } else {
                            Status::Idle
                        };
                        this.set_status(status, cx);
                    });
                    return;
                }
                Err(error) => {
                    // Being offline is not worth a message unless asked.
                    let _ = this.update(cx, |this, cx| {
                        let status = if manual {
                            Status::Failed(format!("{error:#}"))
                        } else {
                            Status::Idle
                        };
                        this.set_status(status, cx);
                    });
                    return;
                }
            };

            let version = manifest.version.clone();
            if !can_install {
                let _ = this.update(cx, |this, cx| {
                    this.set_status(Status::Available(version), cx)
                });
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.set_status(Status::Downloading(version.clone()), cx)
            });
            let staged = cx
                .background_spawn(async move {
                    let dir = platform::cache_dir()
                        .ok_or_else(|| anyhow!("no cache folder"))?
                        .join("update");
                    stage(&manifest, &dir)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let status = match staged {
                    Ok(staged) => Status::Ready { version, staged },
                    Err(error) if manual => Status::Failed(format!("{error:#}")),
                    Err(_) => Status::Idle,
                };
                this.set_status(status, cx);
            });
        }));
    }

    /// Installs the downloaded release and restarts into it.
    fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        let (Status::Ready { staged, .. }, Some(installed)) =
            (self.status.clone(), self.installed.clone())
        else {
            return;
        };
        match swap(&staged, &installed).and_then(|()| relaunch_after_exit(&installed)) {
            Ok(()) => cx.quit(),
            Err(error) => self.set_status(Status::Failed(format!("{error:#}")), cx),
        }
    }

    fn clicked(&mut self, cx: &mut Context<Self>) {
        match &self.status {
            Status::Ready { .. } => self.restart_to_update(cx),
            Status::Available(_) | Status::Failed(_) => cx.open_url(RELEASES_URL),
            _ => {}
        }
    }
}

/// The title-bar pill that reports update progress. Empty when there is
/// nothing to say.
pub struct UpdateBadge {
    updater: Entity<Updater>,
}

impl UpdateBadge {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let updater = Updater::global(cx);
        cx.observe(&updater, |_, _, cx| cx.notify()).detach();
        Self { updater }
    }
}

impl Render for UpdateBadge {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, glyph, actionable, color): (String, &'static str, bool, Hsla) =
            match &self.updater.read(cx).status {
                Status::Idle => return div().hidden().into_any_element(),
                Status::Checking => (
                    "Checking for updates…".into(),
                    "refresh",
                    false,
                    theme::text_muted().into(),
                ),
                Status::UpToDate => (
                    format!("Nexus {VERSION} is up to date"),
                    "check",
                    false,
                    theme::text_muted().into(),
                ),
                Status::Downloading(version) => (
                    format!("Downloading {version}…"),
                    "arrow-down",
                    false,
                    theme::text_muted().into(),
                ),
                Status::Ready { version, .. } => (
                    format!("Restart to update to {version}"),
                    "refresh",
                    true,
                    theme::accent().into(),
                ),
                Status::Available(version) => (
                    format!("Nexus {version} is available"),
                    "arrow-down",
                    true,
                    theme::accent().into(),
                ),
                Status::Failed(message) => (
                    format!("Update failed: {message}"),
                    "refresh",
                    true,
                    theme::red().into(),
                ),
            };
        let updater = self.updater.clone();
        div()
            .id("update-badge")
            .occlude()
            .flex_none()
            .max_w(px(420.))
            .h(px(22.))
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(11.))
            .border_1()
            .border_color(color.opacity(0.45))
            .bg(color.opacity(0.12))
            .text_color(color)
            .child(icon(glyph, px(12.), color))
            .child(div().min_w_0().truncate().child(SharedString::from(label)))
            .when(actionable, |badge| {
                badge
                    .cursor_pointer()
                    .hover(|style| style.bg(color.opacity(0.22)))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        updater.update(cx, |updater, cx| updater.clicked(cx));
                    })
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert_eq!(parse_version("v1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse_version("0.10.0\n"), Some([0, 10, 0]));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.2.x"), None);

        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("nonsense", "0.2.0"), "never update to garbage");
    }

    #[test]
    fn finds_the_bundle_an_executable_lives_in() {
        assert_eq!(
            bundle_of(Path::new("/Applications/Nexus.app/Contents/MacOS/nexus")),
            Some(PathBuf::from("/Applications/Nexus.app"))
        );
        assert_eq!(bundle_of(Path::new("/work/target/release/nexus")), None);
        assert_eq!(bundle_of(Path::new("/x/MacOS/Contents/MacOS/nexus")), None);
    }

    #[test]
    fn cargo_builds_are_not_installed_copies() {
        assert!(is_cargo_build(Path::new("/work/target/debug/nexus.exe")));
        assert!(is_cargo_build(Path::new("/work/target/release/nexus.exe")));
        assert!(!is_cargo_build(Path::new("/Programs/Nexus/Nexus.exe")));
    }

    #[test]
    fn reads_the_checksum_certutil_prints() {
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let modern = format!(
            "SHA256 hash of Nexus.exe:\r\n{hash}\r\nCertUtil: -hashfile command completed successfully.\r\n"
        );
        assert_eq!(hash_from_certutil(&modern).as_deref(), Some(hash));
        let spaced: String = hash
            .as_bytes()
            .chunks(2)
            .map(|pair| std::str::from_utf8(pair).unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        let older = format!(
            "SHA256 hash of file Nexus.exe:\n{}\nCertUtil: done\n",
            spaced.to_uppercase()
        );
        assert_eq!(hash_from_certutil(&older).as_deref(), Some(hash));
        assert_eq!(
            hash_from_certutil("CertUtil: -hashfile command FAILED"),
            None
        );
    }

    #[test]
    fn swapping_a_single_program_works_like_swapping_an_app() {
        let dir = scratch("swap-file");
        let installed = dir.join("Programs/Nexus.exe");
        let staged = dir.join("cache/Nexus.exe");
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&installed, "old").unwrap();
        fs::write(&staged, "new").unwrap();

        swap(&staged, &installed).unwrap();

        assert_eq!(fs::read_to_string(&installed).unwrap(), "new");
        assert!(!staged.exists());
        assert!(!backup_of(&installed).unwrap().exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_a_release_manifest() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"version": "0.3.0", "url": "https://example.com/Nexus.zip",
                "sha256": "abc", "notes": "ignored"}"#,
        )
        .unwrap();
        assert_eq!(manifest.version, "0.3.0");
        assert_eq!(manifest.sha256, "abc");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_app(path: &Path, marker: &str) {
        fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
        fs::write(path.join("Contents/MacOS/nexus"), marker).unwrap();
    }

    #[test]
    fn swapping_replaces_the_app_and_leaves_nothing_behind() {
        let dir = scratch("swap");
        let bundle = dir.join("Applications/Nexus.app");
        let staged = dir.join("cache/Nexus.app");
        fake_app(&bundle, "old");
        fake_app(&staged, "new");

        swap(&staged, &bundle).unwrap();

        let binary = fs::read_to_string(bundle.join("Contents/MacOS/nexus")).unwrap();
        assert_eq!(binary, "new");
        assert!(!staged.exists());
        let left: Vec<_> = fs::read_dir(dir.join("Applications"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, vec!["Nexus.app"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_swap_keeps_the_old_app() {
        let dir = scratch("swap-fail");
        let bundle = dir.join("Applications/Nexus.app");
        fake_app(&bundle, "old");

        let missing = dir.join("cache/Nexus.app");
        assert!(swap(&missing, &bundle).is_err());

        let binary = fs::read_to_string(bundle.join("Contents/MacOS/nexus")).unwrap();
        assert_eq!(binary, "old");
        let _ = fs::remove_dir_all(dir);
    }
}
