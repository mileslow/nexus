//! Inspects the processes running in a terminal: which one is in the
//! foreground, where it is working, and whether it is an agent CLI.

use crate::sessions::Agent;

#[cfg(unix)]
mod imp {
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::os::fd::{AsRawFd, RawFd};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use alacritty_terminal::tty::Pty;

    /// Watches what is running in one terminal.
    pub struct Watch {
        /// Our own handle on the PTY, used only to ask for its foreground process.
        fd: RawFd,
        shell: i32,
    }

    impl Watch {
        pub fn new(pty: &Pty) -> Self {
            let pty_fd = pty.file().as_raw_fd();
            unsafe {
                // Without this every later terminal's shell would inherit this
                // PTY's master side, keeping it open after its own tab is closed.
                let flags = libc::fcntl(pty_fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(pty_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
                Self {
                    fd: libc::fcntl(pty_fd, libc::F_DUPFD_CLOEXEC, 0),
                    shell: pty.child().id() as i32,
                }
            }
        }

        pub fn shell_pid(&self) -> i32 {
            self.shell
        }

        /// The process group currently reading from the terminal.
        pub fn foreground(&self) -> i32 {
            let pid = unsafe { libc::tcgetpgrp(self.fd) };
            if pid > 0 {
                pid
            } else {
                self.shell
            }
        }

        /// Whether the shell itself has the terminal, rather than a command
        /// it is waiting for.
        pub fn shell_in_front(&self) -> bool {
            let group = unsafe { libc::tcgetpgrp(self.fd) };
            group <= 0 || group == self.shell
        }

        /// Hangs up the shell, which hangs up its own jobs.
        pub fn hang_up(&self) {
            unsafe {
                libc::kill(self.shell, libc::SIGHUP);
            }
        }

        /// Hangs up the shell once it is between commands, or after `limit`.
        ///
        /// A shell hung up while it waits for a command takes that command
        /// down before it can tidy up. Startup files run such commands:
        /// `pyenv rehash` cut off this way leaves its lock file behind, and
        /// every shell started in the next minute then waits on that lock
        /// before it shows a prompt.
        pub fn hang_up_between_commands(&self, limit: Duration) {
            let deadline = Instant::now() + limit;
            while !self.shell_in_front() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            self.hang_up();
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.fd);
            }
        }
    }

    pub fn process_name(pid: i32) -> Option<String> {
        let mut buf = [0u8; 256];
        let len =
            unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
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
    pub(super) fn parse_procargs(buf: &[u8]) -> Option<Vec<String>> {
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
        let len =
            unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, buf.len() as u32) };
        (len > 0).then(|| PathBuf::from(String::from_utf8_lossy(&buf[..len as usize]).into_owned()))
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use alacritty_terminal::tty::Pty;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            process: HANDLE,
            class: u32,
            information: *mut c_void,
            length: u32,
            written: *mut u32,
        ) -> i32;
    }

    /// An open process, closed again when dropped.
    struct Process(HANDLE);

    impl Process {
        fn open(pid: i32, access: u32) -> Option<Self> {
            if pid <= 0 {
                return None;
            }
            let handle = unsafe { OpenProcess(access, 0, pid as u32) };
            (!handle.is_null()).then_some(Self(handle))
        }

        fn read<T: Copy>(&self, address: usize) -> Option<T> {
            let mut value = std::mem::MaybeUninit::<T>::uninit();
            let mut read = 0usize;
            let ok = unsafe {
                ReadProcessMemory(
                    self.0,
                    address as *const c_void,
                    value.as_mut_ptr() as *mut c_void,
                    std::mem::size_of::<T>(),
                    &mut read,
                )
            };
            (ok != 0 && read == std::mem::size_of::<T>()).then(|| unsafe { value.assume_init() })
        }

        fn read_utf16(&self, address: usize, bytes: usize) -> Option<String> {
            if address == 0 || bytes == 0 || bytes > 64 * 1024 {
                return None;
            }
            let mut buffer = vec![0u16; bytes / 2];
            let mut read = 0usize;
            let ok = unsafe {
                ReadProcessMemory(
                    self.0,
                    address as *const c_void,
                    buffer.as_mut_ptr() as *mut c_void,
                    buffer.len() * 2,
                    &mut read,
                )
            };
            (ok != 0 && read == buffer.len() * 2).then(|| String::from_utf16_lossy(&buffer))
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Where a string sits in a 64-bit process's start-up parameters.
    #[derive(Clone, Copy)]
    enum Parameter {
        CurrentDirectory = 0x38,
        CommandLine = 0x70,
    }

    /// Reads one of the strings Windows keeps for every process. There is no
    /// call for another process's folder or arguments, so this follows the
    /// documented pointers from the process's environment block.
    #[cfg(target_pointer_width = "64")]
    fn parameter(pid: i32, which: Parameter) -> Option<String> {
        const PROCESS_BASIC_INFORMATION: u32 = 0;
        const PEB_PROCESS_PARAMETERS: usize = 0x20;

        let process = Process::open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
        // `[exit status, PEB address, affinity, priority, pid, parent pid]`.
        let mut basic = [0usize; 6];
        let status = unsafe {
            NtQueryInformationProcess(
                process.0,
                PROCESS_BASIC_INFORMATION,
                basic.as_mut_ptr() as *mut c_void,
                std::mem::size_of_val(&basic) as u32,
                std::ptr::null_mut(),
            )
        };
        if status < 0 || basic[1] == 0 {
            return None;
        }
        let parameters: usize = process.read(basic[1] + PEB_PROCESS_PARAMETERS)?;
        // A counted string: `u16` byte length, `u16` capacity, then a pointer.
        let length: u16 = process.read(parameters + which as usize)?;
        let buffer: usize = process.read(parameters + which as usize + 8)?;
        process.read_utf16(buffer, length as usize)
    }

    #[cfg(not(target_pointer_width = "64"))]
    fn parameter(_pid: i32, _which: Parameter) -> Option<String> {
        None
    }

    struct Entry {
        pid: i32,
        parent: i32,
        name: String,
    }

    /// Every running process with its parent. Listing them is the costly
    /// part of a poll, and every terminal polls on the same tick, so one
    /// listing serves them all.
    fn processes() -> Arc<Vec<Entry>> {
        const FRESH_FOR: Duration = Duration::from_millis(250);
        static LISTING: Mutex<Option<(Instant, Arc<Vec<Entry>>)>> = Mutex::new(None);
        let mut listing = LISTING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*listing {
            Some((taken, processes)) if taken.elapsed() < FRESH_FOR => processes.clone(),
            _ => {
                let processes = Arc::new(list_processes());
                *listing = Some((Instant::now(), processes.clone()));
                processes
            }
        }
    }

    fn list_processes() -> Vec<Entry> {
        let mut found = Vec::new();
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return found;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                let length = entry
                    .szExeFile
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(entry.szExeFile.len());
                found.push(Entry {
                    pid: entry.th32ProcessID as i32,
                    parent: entry.th32ParentProcessID as i32,
                    name: super::program_name(&String::from_utf16_lossy(
                        &entry.szExeFile[..length],
                    )),
                });
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        found
    }

    /// When a process started, in the system's own ticks.
    fn started(pid: i32) -> Option<u64> {
        let process = Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut times = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let [created, exited, kernel, user] = &mut times;
        let ok = unsafe { GetProcessTimes(process.0, created, exited, kernel, user) };
        (ok != 0).then(|| (created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64)
    }

    /// Watches what is running in one terminal.
    pub struct Watch {
        shell: i32,
    }

    impl Watch {
        pub fn new(pty: &Pty) -> Self {
            Self {
                shell: pty.child_watcher().pid().map_or(0, |pid| pid.get() as i32),
            }
        }

        pub fn shell_pid(&self) -> i32 {
            self.shell
        }

        /// What the shell is running. Windows has no notion of a terminal's
        /// foreground process, so this is an agent anywhere among the
        /// shell's first few generations of children, else its newest child,
        /// else the shell.
        pub fn foreground(&self) -> i32 {
            let processes = processes();
            let shell_started = started(self.shell);
            let mut generation = vec![self.shell];
            let mut newest_child: Option<(u64, i32)> = None;
            for depth in 0..3 {
                let mut next = Vec::new();
                for entry in processes.iter() {
                    if !generation.contains(&entry.parent)
                        || entry.pid == entry.parent
                        || entry.name == "conhost"
                    {
                        continue;
                    }
                    if depth == 0 {
                        // Process ids are reused, so something older than
                        // the shell only looks like its child.
                        let Some(started) = started(entry.pid) else {
                            continue;
                        };
                        if shell_started.is_some_and(|shell| started < shell) {
                            continue;
                        }
                        if newest_child.is_none_or(|(newest, _)| started > newest) {
                            newest_child = Some((started, entry.pid));
                        }
                    }
                    if super::detect_agent(&entry.name, entry.pid).is_some() {
                        return entry.pid;
                    }
                    next.push(entry.pid);
                }
                if next.is_empty() {
                    break;
                }
                generation = next;
            }
            newest_child.map_or(self.shell, |(_, pid)| pid)
        }

        /// Closing the terminal ends what runs in it; there is nothing to
        /// ask first, and nothing to wait for.
        pub fn hang_up_between_commands(&self, _limit: Duration) {}
    }

    pub fn process_path(pid: i32) -> Option<PathBuf> {
        let process = Process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let ok =
            unsafe { QueryFullProcessImageNameW(process.0, 0, buffer.as_mut_ptr(), &mut length) };
        (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])))
    }

    pub fn process_name(pid: i32) -> Option<String> {
        let path = process_path(pid)?;
        Some(super::program_name(&path.file_name()?.to_string_lossy()))
    }

    pub fn process_cwd(pid: i32) -> Option<PathBuf> {
        let folder = parameter(pid, Parameter::CurrentDirectory)?;
        // Windows keeps a trailing separator on it.
        let trimmed = folder.trim_end_matches('\\');
        Some(PathBuf::from(if trimmed.ends_with(':') {
            format!("{trimmed}\\")
        } else {
            trimmed.to_string()
        }))
    }

    pub fn process_args(pid: i32) -> Option<Vec<String>> {
        let line = parameter(pid, Parameter::CommandLine)?;
        Some(super::split_command_line(&line))
    }
}

pub use imp::{process_args, process_cwd, process_name, process_path, Watch};

/// A program's name as shown in tabs: `Claude.EXE` is `claude`.
#[cfg(any(windows, test))]
fn program_name(file_name: &str) -> String {
    let name = file_name.to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// Splits a Windows command line into arguments. Quotes group words; the
/// rarer backslash rules do not matter for recognizing a program.
#[cfg(any(windows, test))]
fn split_command_line(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
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
    let tail: Vec<&str> = path.rsplit(['/', '\\']).take(3).collect();
    tail.iter().find_map(|component| classify(component))
}

/// Recognizes agent CLIs. Some install as a versioned binary (so the process
/// is named like `2.1.3`), others run through a JS runtime; both are found by
/// their executable path or script argument.
pub fn detect_agent(name: &str, pid: i32) -> Option<Agent> {
    if let Some(agent) = classify(name) {
        return Some(agent);
    }
    if matches!(
        name,
        "zsh" | "bash" | "fish" | "sh" | "nu" | "pwsh" | "powershell" | "cmd" | ""
    ) {
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

    #[cfg(unix)]
    #[test]
    fn parses_procargs_layout() {
        use imp::parse_procargs;
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

    /// Starts a shell on a PTY the way a tab does, runs `command` in it, and
    /// waits for that command to take the terminal.
    #[cfg(unix)]
    fn shell_running(command: &str) -> (alacritty_terminal::tty::Pty, Watch) {
        use std::io::Write as _;
        use std::time::{Duration, Instant};

        use alacritty_terminal::event::WindowSize;
        use alacritty_terminal::tty;

        let options = tty::Options {
            shell: Some(tty::Shell::new("/bin/sh".into(), vec!["-i".into()])),
            working_directory: None,
            drain_on_exit: false,
            // Keeps the startup files of whoever runs the tests out of it.
            env: [("ENV".to_string(), String::new())].into(),
        };
        let size = WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: 8,
            cell_height: 16,
        };
        let pty = tty::new(&options, size, 0).expect("a PTY");
        let watch = Watch::new(&pty);
        // Read what the shell prints, as a tab does: a shell cannot finish
        // exiting while its output is waiting to be read.
        let mut output = pty.file().try_clone().expect("a second handle");
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buffer = [0u8; 4096];
            loop {
                match output.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        let mut input = pty.file();
        input
            .write_all(format!("{command}\n").as_bytes())
            .expect("typing into the shell");
        let typed = Instant::now();
        while watch.shell_in_front() {
            assert!(typed.elapsed() < Duration::from_secs(10), "{command} never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        (pty, watch)
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_is_hung_up_between_commands() {
        use std::time::{Duration, Instant};

        let (pty, watch) = shell_running("sleep 1");
        let asked = Instant::now();
        watch.hang_up_between_commands(Duration::from_secs(10));
        // The shell was left alone until its command had finished.
        let waited = asked.elapsed();
        assert!(waited > Duration::from_millis(300), "hung up after {waited:?}");
        assert!(waited < Duration::from_secs(5), "hung up after {waited:?}");
        assert!(watch.shell_in_front());
        // Dropping the PTY waits for the shell, which the hang-up has ended.
        drop(pty);
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_stays_busy_is_hung_up_anyway() {
        use std::time::{Duration, Instant};

        let (pty, watch) = shell_running("sleep 30");
        let asked = Instant::now();
        watch.hang_up_between_commands(Duration::from_millis(200));
        let waited = asked.elapsed();
        assert!(waited >= Duration::from_millis(200), "hung up after {waited:?}");
        drop(pty);
        // Neither the wait nor the shell lasted as long as the command.
        assert!(asked.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn reads_own_process() {
        let pid = std::process::id() as i32;
        assert!(process_name(pid).is_some());
        assert_eq!(
            process_cwd(pid).and_then(|path| path.canonicalize().ok()),
            std::env::current_dir()
                .and_then(|path| path.canonicalize())
                .ok()
        );
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
        assert_eq!(
            classify_path(r"C:\Users\me\.local\bin\claude.exe"),
            Some(Agent::Claude)
        );
        assert_eq!(
            classify_path(
                r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js"
            ),
            Some(Agent::Codex)
        );
        assert_eq!(process_path(std::process::id() as i32).is_some(), true);
    }

    #[test]
    fn windows_programs_and_command_lines() {
        assert_eq!(program_name("Claude.EXE"), "claude");
        assert_eq!(program_name("pwsh.exe"), "pwsh");
        assert_eq!(program_name("node"), "node");
        assert_eq!(
            split_command_line(
                r#""C:\Program Files\nodejs\node.exe"  C:\npm\claude\cli.js --resume "a b""#
            ),
            vec![
                r"C:\Program Files\nodejs\node.exe",
                r"C:\npm\claude\cli.js",
                "--resume",
                "a b"
            ]
        );
        assert_eq!(split_command_line(r#"tool """#), vec!["tool", ""]);
        assert!(split_command_line("   ").is_empty());
    }

    #[test]
    fn shells_are_not_agents() {
        assert_eq!(detect_agent("pwsh", 0), None);
        assert_eq!(detect_agent("powershell", 0), None);
    }
}
