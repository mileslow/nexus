//! One running copy of Nexus per user on Windows.
//!
//! macOS already works this way: opening the app again, or opening a folder
//! with it, reaches the copy that is running. Windows starts a second copy
//! instead, and two copies would overwrite each other's remembered windows.
//! So the first copy listens on a local socket and writes its port and a
//! secret beside the state file; a later copy hands over what it was asked
//! to open and exits.

use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
const ACCEPTED: &str = "ok";

pub enum Claim {
    /// This is the only copy; requests from later copies arrive here.
    Primary(Server),
    /// Another copy is running and has taken the request.
    HandedOff,
}

pub struct Server {
    listener: TcpListener,
    secret: String,
}

fn secret() -> String {
    // Std's hasher keys are random per instance.
    let part = || {
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

/// What the running copy wrote: its port, its secret and its process id.
fn read_lock(lock: &Path) -> Option<(u16, String, u32)> {
    let text = std::fs::read_to_string(lock).ok()?;
    let mut parts = text.split_whitespace();
    let port = parts.next()?.parse().ok()?;
    let secret = parts.next()?.to_string();
    let pid = parts.next()?.parse().ok()?;
    Some((port, secret, pid))
}

/// Lets the running copy bring its window in front of this one, which
/// Windows otherwise refuses to a program the user is not using.
fn allow_foreground(_pid: u32) {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn AllowSetForegroundWindow(process_id: u32) -> i32;
        }
        unsafe {
            AllowSetForegroundWindow(_pid);
        }
    }
}

fn hand_off(lock: &Path, request: &str) -> Option<()> {
    let (port, secret, pid) = read_lock(lock)?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
    allow_foreground(pid);
    stream
        .write_all(format!("{secret}\n{request}\n").as_bytes())
        .ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    (reply.trim() == ACCEPTED).then_some(())
}

/// Hands `request` (one line) to the copy named by the lock file, or
/// becomes that copy when there is none.
pub fn claim(lock: &Path, request: &str) -> std::io::Result<Claim> {
    if hand_off(lock, request).is_some() {
        return Ok(Claim::HandedOff);
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let secret = secret();
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let port = listener.local_addr()?.port();
    std::fs::write(lock, format!("{port} {secret} {}\n", std::process::id()))?;
    Ok(Claim::Primary(Server { listener, secret }))
}

impl Server {
    /// Calls `on_request` with each request later copies hand over, for as
    /// long as the app runs.
    pub fn serve(self, on_request: impl Fn(String) + Send + 'static) {
        std::thread::spawn(move || {
            for stream in self.listener.incoming() {
                let Ok(stream) = stream else { continue };
                if let Some(request) = self.read_request(stream) {
                    on_request(request);
                }
            }
        });
    }

    fn read_request(&self, stream: TcpStream) -> Option<String> {
        stream.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut secret = String::new();
        reader.read_line(&mut secret).ok()?;
        if secret.trim_end() != self.secret {
            return None;
        }
        let mut request = String::new();
        reader.read_line(&mut request).ok()?;
        let mut stream = stream;
        stream.write_all(format!("{ACCEPTED}\n").as_bytes()).ok()?;
        Some(request.trim_end_matches(['\r', '\n']).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn lock_file(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nexus-instance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("instance")
    }

    #[test]
    fn a_second_copy_hands_its_request_to_the_first() {
        let lock = lock_file("handoff");
        let Claim::Primary(server) = claim(&lock, "").unwrap() else {
            panic!("the first copy should be the primary");
        };
        let (tx, rx) = mpsc::channel();
        server.serve(move |request| tx.send(request).unwrap());

        assert!(matches!(
            claim(&lock, r"C:\Users\me\My Project").unwrap(),
            Claim::HandedOff
        ));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            r"C:\Users\me\My Project"
        );
        assert!(matches!(claim(&lock, "").unwrap(), Claim::HandedOff));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "");
        let _ = std::fs::remove_dir_all(lock.parent().unwrap());
    }

    #[test]
    fn a_stale_lock_is_taken_over() {
        let lock = lock_file("stale");
        // A port nothing listens on any more.
        let port = {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.local_addr().unwrap().port()
        };
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, format!("{port} abc 1\n")).unwrap();
        assert!(matches!(claim(&lock, "x").unwrap(), Claim::Primary(_)));
        let _ = std::fs::remove_dir_all(lock.parent().unwrap());
    }

    #[test]
    fn requests_without_the_secret_are_ignored() {
        let lock = lock_file("secret");
        let Claim::Primary(server) = claim(&lock, "").unwrap() else {
            panic!("the first copy should be the primary");
        };
        let (port, secret, _) = read_lock(&lock).unwrap();
        let (tx, rx) = mpsc::channel();
        server.serve(move |request| tx.send(request).unwrap());

        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.write_all(b"guess\nC:\\evil\n").unwrap();
        let mut reply = String::new();
        let _ = BufReader::new(stream).read_line(&mut reply);
        assert_ne!(reply.trim(), ACCEPTED);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        assert_eq!(secret.len(), 32);
        let _ = std::fs::remove_dir_all(lock.parent().unwrap());
    }
}
