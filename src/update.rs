//! Self-update. Looks for a newer release on GitHub, downloads it in the
//! background, and swaps the app bundle when the user chooses to restart.
//!
//! Every release publishes `Nexus.zip` (the app) and `latest.json` (its
//! version and checksum). The work is done by tools that ship with macOS:
//! `curl`, `shasum`, `ditto`, `plutil` and `codesign`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context as _, Result};
use gpui::{div, prelude::*, px, App, Context, Entity, Global, Hsla, SharedString, Task, Window};
use serde::Deserialize;

use crate::assets::icon;
use crate::links::home_dir;
use crate::theme;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_URL: &str = "https://github.com/mileslow/nexus/releases/latest";
const MANIFEST_URL: &str = "https://github.com/mileslow/nexus/releases/latest/download/latest.json";

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(8);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
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
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let bundled = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    bundled.then(|| bundle.to_path_buf())
}

fn running_bundle() -> Option<PathBuf> {
    bundle_of(&std::env::current_exe().ok()?.canonicalize().ok()?)
}

/// Runs a tool and returns what it printed, or an error naming what failed.
fn run(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(program)
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
    let body = run("/usr/bin/curl", &["-fsSL", "--max-time", "20", url])
        .context("could not reach the release server")?;
    serde_json::from_slice(&body).context("the release information was not understood")
}

fn sha256_of(path: &Path) -> Result<String> {
    let path = path.to_string_lossy();
    let output = run("/usr/bin/shasum", &["-a", "256", &path])?;
    let text = String::from_utf8_lossy(&output);
    text.split_whitespace()
        .next()
        .map(str::to_lowercase)
        .ok_or_else(|| anyhow!("shasum printed nothing"))
}

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
fn stage(manifest: &Manifest, dir: &Path) -> Result<PathBuf> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir)?;
    let zip = dir.join("Nexus.zip");
    run(
        "/usr/bin/curl",
        &[
            "-fsSL",
            "--max-time",
            "900",
            "-o",
            &zip.to_string_lossy(),
            &manifest.url,
        ],
    )
    .context("the download failed")?;
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

/// Moves a folder, copying when it has to cross to another volume.
fn move_dir(from: &Path, to: &Path) -> Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    run(
        "/usr/bin/ditto",
        &[&from.to_string_lossy(), &to.to_string_lossy()],
    )?;
    let _ = fs::remove_dir_all(from);
    Ok(())
}

/// Puts the app at `staged` where `bundle` is. The old app stays beside it
/// until the new one is in place, and comes back if anything goes wrong.
pub fn swap(staged: &Path, bundle: &Path) -> Result<()> {
    let name = bundle
        .file_name()
        .ok_or_else(|| anyhow!("the app has no name"))?
        .to_string_lossy();
    let backup = bundle.with_file_name(format!(".{name}.previous"));
    let _ = fs::remove_dir_all(&backup);
    fs::rename(bundle, &backup).with_context(|| {
        format!(
            "could not replace {} (is its folder writable?)",
            bundle.display()
        )
    })?;
    if let Err(error) = move_dir(staged, bundle) {
        let _ = fs::remove_dir_all(bundle);
        let _ = fs::rename(&backup, bundle);
        return Err(error.context("could not put the new version in place"));
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(())
}

/// Opens `bundle` once this process has exited.
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
    /// The app bundle this process runs from. Builds run straight from
    /// `cargo` have none, and only point at the release page.
    bundle: Option<PathBuf>,
    manifest_url: String,
    work: Option<Task<()>>,
    notice: Option<Task<()>>,
}

struct GlobalUpdater(Entity<Updater>);

impl Global for GlobalUpdater {}

impl Updater {
    /// Creates the app-wide updater and starts checking in the background.
    pub fn init(cx: &mut App) {
        let bundle = running_bundle();
        // `NEXUS_UPDATE_MANIFEST` points at a different `latest.json`, for
        // trying a release before publishing it.
        let manifest_url =
            std::env::var("NEXUS_UPDATE_MANIFEST").unwrap_or_else(|_| MANIFEST_URL.to_string());
        let automatic = bundle.is_some() && std::env::var_os("NEXUS_NO_UPDATE_CHECK").is_none();
        let updater = cx.new(|_| Updater {
            status: Status::Idle,
            bundle,
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
        let can_install = self.bundle.is_some();
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
                    let dir = home_dir()
                        .ok_or_else(|| anyhow!("no home folder"))?
                        .join("Library/Caches/nexus/update");
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
        let (Status::Ready { staged, .. }, Some(bundle)) =
            (self.status.clone(), self.bundle.clone())
        else {
            return;
        };
        match swap(&staged, &bundle).and_then(|()| relaunch_after_exit(&bundle)) {
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
