//! Inspects the process running in the foreground of a PTY (macOS).

use std::ffi::{c_char, c_int, c_void, CStr};
use std::os::fd::RawFd;
use std::path::PathBuf;

use crate::sessions::Agent;

/// The process group currently reading from the terminal.
pub fn foreground_pid(pty_fd: RawFd) -> Option<i32> {
    let pid = unsafe { libc::tcgetpgrp(pty_fd) };
    (pid > 0).then_some(pid)
}

pub fn process_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    (len > 0).then(|| String::from_utf8_lossy(&buf[..len as usize]).into_owned())
}

pub fn process_cwd(pid: i32) -> Option<PathBuf> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as c_int;
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut c_void,
            size,
        )
    };
    if written != size {
        return None;
    }
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const c_char) };
    let path = path.to_string_lossy();
    (!path.is_empty()).then(|| PathBuf::from(path.into_owned()))
}

/// Reads a process's argv through `KERN_PROCARGS2`.
pub fn process_args(pid: i32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok != 0 || size < 8 {
        return None;
    }
    let mut buf = vec![0u8; size];
    let ok = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok != 0 || size < 8 {
        return None;
    }
    buf.truncate(size);
    parse_procargs(&buf)
}

/// Layout: `argc: i32`, executable path, NUL padding, then argv strings.
fn parse_procargs(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if argc <= 0 {
        return None;
    }
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let rest = &rest[exec_end..];
    let args_start = rest.iter().position(|&b| b != 0)?;
    Some(
        rest[args_start..]
            .split(|&b| b == 0)
            .take(argc as usize)
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// The executable a process is running.
pub fn process_path(pid: i32) -> Option<PathBuf> {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
    (len > 0).then(|| PathBuf::from(String::from_utf8_lossy(&buf[..len as usize]).into_owned()))
}

fn classify(text: &str) -> Option<Agent> {
    let text = text.to_ascii_lowercase();
    if text.contains("claude") {
        Some(Agent::Claude)
    } else if text.contains("codex") {
        Some(Agent::Codex)
    } else {
        None
    }
}

/// Looks only at the last few path components, so a project folder that
/// happens to be named after an agent does not count.
fn classify_path(path: &str) -> Option<Agent> {
    let tail: Vec<&str> = path.rsplit('/').take(3).collect();
    tail.iter().find_map(|component| classify(component))
}

/// Recognizes agent CLIs. Some install as a versioned binary (so the process
/// is named like `2.1.3`), others run through a JS runtime; both are found by
/// their executable path or script argument.
pub fn detect_agent(name: &str, pid: i32) -> Option<Agent> {
    if let Some(agent) = classify(name) {
        return Some(agent);
    }
    if matches!(name, "zsh" | "bash" | "fish" | "sh" | "nu" | "") {
        return None;
    }
    if matches!(name, "node" | "bun" | "deno") {
        let args = process_args(pid)?;
        return args
            .iter()
            .skip(1)
            .take(2)
            .find_map(|arg| classify_path(arg));
    }
    process_path(pid).and_then(|path| classify_path(&path.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_procargs_layout() {
        let mut buf = Vec::new();
        buf.extend(2i32.to_ne_bytes());
        buf.extend(b"/usr/bin/node\0\0\0\0");
        buf.extend(b"node\0/opt/bin/codex\0HOME=/x\0");
        assert_eq!(
            parse_procargs(&buf),
            Some(vec!["node".to_string(), "/opt/bin/codex".to_string()])
        );
        assert_eq!(parse_procargs(&[0, 0]), None);
    }

    #[test]
    fn reads_own_process() {
        let pid = std::process::id() as i32;
        assert!(process_name(pid).is_some());
        assert_eq!(process_cwd(pid), std::env::current_dir().ok());
        assert!(!process_args(pid).unwrap().is_empty());
    }

    #[test]
    fn detects_agents_by_name() {
        assert_eq!(detect_agent("claude", 0), Some(Agent::Claude));
        assert_eq!(detect_agent("codex", 0), Some(Agent::Codex));
        assert_eq!(detect_agent("zsh", 0), None);
    }

    #[test]
    fn detects_agents_by_install_path() {
        assert_eq!(
            classify_path("/Users/me/.local/share/claude/versions/2.1.328"),
            Some(Agent::Claude)
        );
        assert_eq!(
            classify_path("/opt/node/lib/node_modules/@openai/codex/bin/codex.js"),
            Some(Agent::Codex)
        );
        // A project directory named after an agent is not an agent.
        assert_eq!(
            classify_path("/Users/me/claude-notes/target/debug/server"),
            None
        );
        assert_eq!(process_path(std::process::id() as i32).is_some(), true);
    }
}
