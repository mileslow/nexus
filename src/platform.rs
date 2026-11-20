//! The parts of being a Mac app that GPUI leaves to the app: a locale for
//! shells started from the Dock, folders opened from Finder, and the About
//! panel.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The POSIX locale for a macOS locale such as `en_US`, `en-GB` or
/// `en_US@rg=gbzzzz`. Shells need one that names UTF-8.
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
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

/// The folder to open for a path handed to the app: a file opens its folder.
pub fn folder_for(path: &Path) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    if path.is_dir() {
        Some(path)
    } else {
        path.parent().map(PathBuf::from)
    }
}

/// Shows the standard About panel, which reads the name, version and icon
/// from the app bundle.
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
