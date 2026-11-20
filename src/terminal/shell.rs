//! Which shell a terminal runs, and how to hand it a command.

use std::path::Path;

/// The families of shell, which differ in how they take a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `zsh`, `bash`, `fish` and the like.
    Posix,
    PowerShell,
    Cmd,
}

impl Kind {
    pub fn of(program: &str) -> Self {
        let name = Path::new(program)
            .file_stem()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match name.as_str() {
            "pwsh" | "powershell" => Kind::PowerShell,
            "cmd" => Kind::Cmd,
            _ => Kind::Posix,
        }
    }
}

/// The shell to start: the user's own on a Mac, PowerShell on Windows
/// (the newer `pwsh` when it is installed). `NEXUS_SHELL` overrides both.
pub fn program() -> String {
    let named = |name: &str| std::env::var(name).ok().filter(|shell| !shell.is_empty());
    if let Some(shell) = named("NEXUS_SHELL") {
        return shell;
    }
    if cfg!(windows) {
        let on_path = std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|folder| folder.join("pwsh.exe").is_file())
        });
        if on_path {
            "pwsh.exe"
        } else {
            "powershell.exe"
        }
        .to_string()
    } else {
        named("SHELL").unwrap_or_else(|| "/bin/zsh".to_string())
    }
}

/// Arguments that start `program` interactively. With a command, the shell
/// runs it first and is still there when it exits.
pub fn arguments(program: &str, command: Option<&str>) -> Vec<String> {
    let args: Vec<&str> = match (Kind::of(program), command) {
        // A login shell loads the user's PATH, so agent CLIs resolve.
        (Kind::Posix, None) => vec!["-l"],
        (Kind::Posix, Some(command)) => {
            let stay = format!("{command}; exec {} -l -i", quote(program, Kind::Posix));
            return vec!["-l".into(), "-i".into(), "-c".into(), stay];
        }
        (Kind::PowerShell, None) => vec!["-NoLogo"],
        (Kind::PowerShell, Some(command)) => vec!["-NoLogo", "-NoExit", "-Command", command],
        (Kind::Cmd, None) => vec![],
        (Kind::Cmd, Some(command)) => vec!["/K", command],
    };
    args.into_iter().map(str::to_string).collect()
}

/// Quotes `text` so the shell reads it as one word.
pub fn quote(text: &str, kind: Kind) -> String {
    let plain = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '-' | '_' | '.' | '/' | ':' | '@')
            || (c == '\\' && kind != Kind::Posix)
    };
    if !text.is_empty() && text.chars().all(plain) {
        return text.to_string();
    }
    match kind {
        Kind::Posix => format!("'{}'", text.replace('\'', r"'\''")),
        Kind::PowerShell => format!("'{}'", text.replace('\'', "''")),
        Kind::Cmd => format!("\"{text}\""),
    }
}

/// Quotes `text` for the shell terminals start.
pub fn quote_for_default_shell(text: &str) -> String {
    let kind = if cfg!(windows) {
        Kind::of(&program())
    } else {
        Kind::Posix
    };
    quote(text, kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_are_recognized_by_program_name() {
        assert_eq!(Kind::of("/bin/zsh"), Kind::Posix);
        assert_eq!(Kind::of("pwsh.exe"), Kind::PowerShell);
        assert_eq!(Kind::of("PowerShell.exe"), Kind::PowerShell);
        assert_eq!(Kind::of("cmd.exe"), Kind::Cmd);
        assert_eq!(Kind::of("bash.exe"), Kind::Posix);
    }

    #[test]
    fn a_command_leaves_a_shell_behind() {
        assert_eq!(arguments("/bin/zsh", None), vec!["-l"]);
        assert_eq!(
            arguments("/bin/zsh", Some("claude")),
            vec!["-l", "-i", "-c", "claude; exec /bin/zsh -l -i"]
        );
        assert_eq!(arguments("pwsh.exe", None), vec!["-NoLogo"]);
        assert_eq!(
            arguments("powershell.exe", Some("claude --resume abc")),
            vec!["-NoLogo", "-NoExit", "-Command", "claude --resume abc"]
        );
        assert_eq!(arguments("cmd.exe", Some("codex")), vec!["/K", "codex"]);
    }

    #[test]
    fn quoting_follows_the_shell() {
        assert_eq!(quote("src/main.rs", Kind::Posix), "src/main.rs");
        assert_eq!(quote("it's; rm", Kind::Posix), r"'it'\''s; rm'");
        assert_eq!(
            quote(r"C:\src\main.rs", Kind::PowerShell),
            r"C:\src\main.rs"
        );
        assert_eq!(
            quote(r"C:\My Files\it's", Kind::PowerShell),
            r"'C:\My Files\it''s'"
        );
        assert_eq!(quote(r"C:\My Files", Kind::Cmd), r#""C:\My Files""#);
        assert_eq!(quote("", Kind::Posix), "''");
    }
}
