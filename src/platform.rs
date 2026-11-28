//! What differs between macOS and Windows: where files live, how other
//! programs are started and inspected, and the parts of being a desktop app
//! that GPUI leaves to the app.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Picks the Mac or the Windows spelling of a keyboard shortcut.
pub const fn keys(mac: &'static str, windows: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        windows
    }
}

pub fn home_dir() -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Where settings and remembered layouts are kept.
pub fn data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Nexus"))
    } else {
        Some(home_dir()?.join("Library/Application Support/Nexus"))
    }
}

/// Where files that can always be rebuilt are kept.
pub fn cache_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Nexus/Cache"))
    } else {
        Some(home_dir()?.join("Library/Caches/nexus"))
    }
}

/// Windows resolves paths to a `\\?\` form that shells and most programs
/// refuse; this is the ordinary spelling of the same path.
pub fn strip_verbatim(path: &str) -> &str {
    match path.strip_prefix(r"\\?\") {
        // `\\?\UNC\server\share` has no drive-letter spelling; leave it.
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest,
        _ => path,
    }
}

/// Resolves symlinks, so one folder always has one spelling.
pub fn canonical(path: &Path) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    if cfg!(windows) {
        if let Some(text) = path.to_str() {
            return Some(PathBuf::from(strip_verbatim(text)));
        }
    }
    Some(path)
}

/// A folder path in a form that every spelling of the same folder shares.
/// Windows ignores case and accepts either slash.
pub fn folder_key(path: &str, windows: bool) -> std::borrow::Cow<'_, str> {
    if windows {
        path.trim_end_matches(['/', '\\'])
            .replace('/', "\\")
            .to_lowercase()
            .into()
    } else {
        path.trim_end_matches('/').into()
    }
}

/// Whether the folder with key `path` is the folder `root` or inside it.
pub fn is_within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with(['/', '\\']))
}

/// The path part of a `file://` URL as a path on this system. Windows
/// writes drives as `/C:/Users/…`.
pub fn local_path(url_path: &str, windows: bool) -> PathBuf {
    let bytes = url_path.as_bytes();
    let has_drive =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    if windows && has_drive {
        PathBuf::from(url_path[1..].replace('/', "\\"))
    } else {
        PathBuf::from(url_path)
    }
}

/// What the menus call showing a file in the system's file manager.
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show in Finder"
    } else if cfg!(windows) {
        "Show in Explorer"
    } else {
        "Show in Files"
    }
}

/// A command for a helper program. On Windows it gets no console window,
/// which would otherwise flash up each time one runs.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Whether a process with this id is running.
#[cfg(unix)]
pub fn process_alive(pid: i32) -> bool {
    // Signal 0 only asks whether the process exists.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// Whether a process with this id is running.
#[cfg(windows)]
pub fn process_alive(pid: i32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid <= 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let known = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        known && code == STILL_ACTIVE as u32
    }
}

/// Waits for a process to exit, giving up after `timeout`.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, timeout: std::time::Duration) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !handle.is_null() {
            WaitForSingleObject(handle, timeout.as_millis() as u32);
            CloseHandle(handle);
        }
    }
}

/// The POSIX locale for a macOS locale such as `en_US`, `en-GB` or
/// `en_US@rg=gbzzzz`. Shells need one that names UTF-8.
#[cfg(any(target_os = "macos", test))]
pub fn posix_locale(apple_locale: &str, exists: impl Fn(&str) -> bool) -> String {
    let base = apple_locale.trim().split('@').next().unwrap_or("");
    let candidate = format!("{}.UTF-8", base.replace('-', "_"));
    if !base.is_empty() && exists(&candidate) {
        candidate
    } else {
        "en_US.UTF-8".to_string()
    }
}

/// An app opened from Finder or the Dock has no locale in its environment,
/// and a shell without one mangles anything that is not ASCII. Terminal
/// apps derive it from the system language; so does this.
#[cfg(target_os = "macos")]
pub fn ensure_locale() {
    let has_locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    if has_locale {
        return;
    }
    let apple_locale = Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let locale = posix_locale(&apple_locale, |name| {
        Path::new("/usr/share/locale").join(name).is_dir()
    });
    std::env::set_var("LANG", locale);
}

/// Windows programs find their own locale.
#[cfg(not(target_os = "macos"))]
pub fn ensure_locale() {}

/// The path named by a `file://` URL, as Finder sends them.
pub fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // An optional host comes before the path.
    let path = &rest[rest.find('/')?..];
    let mut bytes = Vec::with_capacity(path.len());
    let mut input = path.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = (input.next()? as char).to_digit(16)?;
            let low = (input.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    Some(local_path(&String::from_utf8(bytes).ok()?, cfg!(windows)))
}

/// The folder to open for a path handed to the app: a file opens its folder.
pub fn folder_for(path: &Path) -> Option<PathBuf> {
    let path = canonical(path)?;
    if path.is_dir() {
        Some(path)
    } else {
        path.parent().map(PathBuf::from)
    }
}

/// Shows the standard About panel, which reads the name, version and icon
/// from the app bundle.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
pub fn show_about_panel() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    // Menu actions run on the main thread, which AppKit requires.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let nil: *mut Object = std::ptr::null_mut();
        let _: () = msg_send![app, orderFrontStandardAboutPanel: nil];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_come_from_the_system_language() {
        let known = |name: &str| matches!(name, "en_US.UTF-8" | "en_GB.UTF-8" | "ja_JP.UTF-8");
        assert_eq!(posix_locale("en_GB\n", known), "en_GB.UTF-8");
        assert_eq!(posix_locale("ja-JP", known), "ja_JP.UTF-8");
        assert_eq!(posix_locale("en_US@rg=gbzzzz", known), "en_US.UTF-8");
        // Combinations macOS allows but the C library has no data for.
        assert_eq!(posix_locale("en_SG", known), "en_US.UTF-8");
        assert_eq!(posix_locale("", known), "en_US.UTF-8");
    }

    #[test]
    fn windows_paths_lose_their_verbatim_prefix() {
        assert_eq!(strip_verbatim(r"\\?\C:\Users\me"), r"C:\Users\me");
        assert_eq!(strip_verbatim(r"C:\Users\me"), r"C:\Users\me");
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share"),
            r"\\?\UNC\server\share"
        );
        assert_eq!(strip_verbatim("/Users/me"), "/Users/me");
    }

    #[test]
    fn folders_compare_the_way_the_system_does() {
        let key = |path| folder_key(path, true).into_owned();
        assert_eq!(key(r"C:\Users\Me\Proj\"), key("c:/users/me/proj"));
        assert!(is_within(
            &key(r"C:\Users\Me\Proj\src"),
            &key(r"c:\users\me\proj")
        ));
        assert!(!is_within(
            &key(r"C:\Users\Me\Project2"),
            &key(r"C:\Users\Me\Proj")
        ));

        assert_eq!(folder_key("/Users/me/proj/", false), "/Users/me/proj");
        assert_ne!(
            folder_key("/Users/Me", false),
            folder_key("/Users/me", false)
        );
        assert!(is_within("/a/b/c", "/a/b"));
        assert!(!is_within("/a/bc", "/a/b"));
    }

    #[test]
    fn url_paths_name_drives_on_windows_only() {
        assert_eq!(
            local_path("/C:/Users/me/a b.txt", true),
            PathBuf::from(r"C:\Users\me\a b.txt")
        );
        assert_eq!(local_path("/C:/odd", false), PathBuf::from("/C:/odd"));
        assert_eq!(local_path("/tmp/x", true), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn this_process_is_alive() {
        assert!(process_alive(std::process::id() as i32));
        assert!(!process_alive(0));
    }

    #[cfg(not(windows))]
    #[test]
    fn file_urls_become_paths() {
        assert_eq!(
            path_from_file_url("file:///Users/me/My%20Project/"),
            Some(PathBuf::from("/Users/me/My Project/"))
        );
        assert_eq!(
            path_from_file_url("file://localhost/tmp/caf%C3%A9"),
            Some(PathBuf::from("/tmp/café"))
        );
        assert_eq!(path_from_file_url("https://example.com/x"), None);
        assert_eq!(path_from_file_url("file:///tmp/bad%2"), None);
        assert_eq!(path_from_file_url("file:///tmp/bad%zz"), None);
    }
}
