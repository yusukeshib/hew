//! Per-process session registry + IPC socket.
//!
//! A running TUI advertises itself under a registry directory
//! (`$XDG_RUNTIME_DIR/hew/` or `/tmp/hew-$UID/`) as a pair of files:
//!
//!   `<id>.sock`  a Unix-domain socket the `hew comment` client connects to
//!   `<id>.json`  small metadata (pid, cwd, name, files) for discovery
//!
//! The socket listener runs on its own thread and never touches the comment
//! store directly: it forwards each request to the main loop over an `mpsc`
//! channel and waits for the reply. That keeps the running TUI process the
//! single writer of the in-memory store (no shared lock in the render path).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::Permissions;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

/// Cap on a single request line, so a flood can't exhaust memory.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// How long the listener waits for a client to send its request line.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// A request decoded from the wire and handed to the main loop.
pub enum IpcRequest {
    /// Dump the current review store as JSON.
    List,
}

/// One IPC request plus the channel the main loop replies on.
pub struct IpcMessage {
    pub req: IpcRequest,
    pub reply: Sender<String>,
}

/// Discovery metadata persisted next to each session socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub pid: u32,
    #[serde(default)]
    pub name: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub files: Vec<String>,
}

/// A registered session. Removes its socket + metadata files on drop.
pub struct Session {
    sock_path: PathBuf,
    meta_path: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock_path);
        let _ = std::fs::remove_file(&self.meta_path);
    }
}

/// The registry directory, created if missing.
pub fn registry_dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(base) => PathBuf::from(base).join("hew"),
        // SAFETY: getuid only reads the calling process's real uid.
        None => std::env::temp_dir().join(format!("hew-{}", unsafe { libc::getuid() })),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Keep the registry private: the sockets here expose live review data, so
    // other users must not be able to traverse or read it.
    std::fs::set_permissions(&dir, Permissions::from_mode(0o700))
        .with_context(|| format!("chmod 0700 {}", dir.display()))?;
    Ok(dir)
}

/// True when a process with `pid` is still alive.
fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 performs only an existence/permission check.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    // EPERM means the process exists but we lack permission to signal it —
    // still alive, so don't sweep it as stale.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// A short, filesystem-safe session id.
fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// Reject names that aren't safe as a single path component, so `--name` can't
/// escape the registry directory or produce an unbindable socket path.
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Remove socket+metadata pairs whose owning process is gone.
fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let dead = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Meta>(&text) {
                Ok(meta) => !pid_alive(meta.pid),
                Err(_) => true, // unparseable metadata is stale
            },
            Err(_) => continue,
        };
        if dead {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("sock"));
        }
    }
}

/// A live session discovered in the registry, with its socket path.
pub struct Found {
    pub meta: Meta,
    pub sock: PathBuf,
}

/// List the live sessions, sweeping stale entries first.
pub fn discover() -> Result<Vec<Found>> {
    let dir = registry_dir()?;
    sweep_stale(&dir);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<Meta>(&text) else {
            continue;
        };
        let sock = path.with_extension("sock");
        if sock.exists() {
            out.push(Found { meta, sock });
        }
    }
    Ok(out)
}

/// Register a session and start its socket listener. Returns the session handle
/// (drop = deregister) and the receiver the main loop drains for requests.
pub fn start(name: Option<String>, files: Vec<String>) -> Result<(Session, Receiver<IpcMessage>)> {
    let dir = registry_dir()?;
    sweep_stale(&dir);
    if let Some(n) = &name {
        if !valid_id(n) {
            anyhow::bail!(
                "invalid --name '{n}': use only letters, digits, '-', '_', '.' (≤64 chars)"
            );
        }
    }
    let id = name.clone().unwrap_or_else(short_id);
    let sock_path = dir.join(format!("{id}.sock"));
    let meta_path = dir.join(format!("{id}.json"));
    // Clear any leftover socket of the same name before binding.
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding {}", sock_path.display()))?;
    // Restrict the socket to the owner so other users can't connect and read
    // the live comment store.
    std::fs::set_permissions(&sock_path, Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", sock_path.display()))?;

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let meta = Meta {
        id,
        pid: std::process::id(),
        name,
        cwd,
        files,
    };
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)
        .with_context(|| format!("writing {}", meta_path.display()))?;

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || serve(listener, tx));

    Ok((
        Session {
            sock_path,
            meta_path,
        },
        rx,
    ))
}

/// Accept connections and forward each request to the main loop. Runs on a
/// daemon thread that exits with the process, so per-connection errors are
/// simply skipped.
fn serve(listener: UnixListener, tx: Sender<IpcMessage>) {
    for stream in listener.incoming().flatten() {
        let _ = handle_conn(stream, &tx);
    }
}

/// Read one request line, ask the main loop, write the reply back.
fn handle_conn(mut stream: UnixStream, tx: &Sender<IpcMessage>) -> Result<()> {
    // A silent or oversized client must not wedge the (serial) accept loop.
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&stream).take(MAX_REQUEST_BYTES);
        reader.read_line(&mut line)?;
    }
    let resp = match parse_wire(line.trim()) {
        Some(req) => {
            let (rtx, rrx) = mpsc::channel();
            tx.send(IpcMessage { req, reply: rtx })
                .map_err(|_| anyhow::anyhow!("main loop gone"))?;
            rrx.recv()
                .unwrap_or_else(|_| "{\"error\":\"no reply\"}".into())
        }
        None => "{\"error\":\"unknown request\"}".into(),
    };
    writeln!(stream, "{resp}")?;
    Ok(())
}

/// Decode a wire request line (`{"cmd":"list"}`) into an [`IpcRequest`].
fn parse_wire(line: &str) -> Option<IpcRequest> {
    #[derive(Deserialize)]
    #[serde(tag = "cmd", rename_all = "lowercase")]
    enum Wire {
        List,
    }
    match serde_json::from_str::<Wire>(line).ok()? {
        Wire::List => Some(IpcRequest::List),
    }
}

// ---- client side (`hew comment`) ------------------------------------------

/// Connect to a session socket, send one wire request, return the reply.
pub fn query(sock: &Path, wire: &str) -> Result<String> {
    let mut stream =
        UnixStream::connect(sock).with_context(|| format!("connecting {}", sock.display()))?;
    writeln!(stream, "{wire}")?;
    stream.shutdown(std::net::Shutdown::Write).ok();
    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    Ok(resp.trim().to_string())
}

/// Resolve which session a client command targets.
///
/// Order: explicit `--session` (id or name) → the only live session → error
/// listing the candidates.
pub fn resolve_target(selector: Option<&str>) -> Result<Found> {
    let mut sessions = discover()?;
    if let Some(sel) = selector {
        if let Some(pos) = sessions
            .iter()
            .position(|f| f.meta.id == sel || f.meta.name.as_deref() == Some(sel))
        {
            return Ok(sessions.swap_remove(pos));
        }
        anyhow::bail!("no hew session matching '{sel}' (id or name)");
    }
    match sessions.len() {
        0 => anyhow::bail!("no running hew session"),
        1 => Ok(sessions.swap_remove(0)),
        _ => {
            let list = sessions
                .iter()
                .map(|f| {
                    format!(
                        "  {}  ({}, {} files)",
                        f.meta.id,
                        f.meta.cwd,
                        f.meta.files.len()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("multiple hew sessions; pass --session <id|name>:\n{list}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restore a process-global env var on drop so tests don't leak state.
    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &Path) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, val);
            EnvGuard { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn rejects_unsafe_names() {
        assert!(valid_id("pr-42"));
        assert!(valid_id("feature_x.v2"));
        assert!(!valid_id(""));
        assert!(!valid_id("."));
        assert!(!valid_id(".."));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("../escape"));
        assert!(!valid_id("has space"));
    }

    #[test]
    fn socket_roundtrip_discovery_and_cleanup() {
        // Isolate the registry to a unique temp dir for this process.
        let tmp = std::env::temp_dir().join(format!("hew-ipc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let _env = EnvGuard::set("XDG_RUNTIME_DIR", &tmp);

        let (session, rx) = start(Some("unit".into()), vec!["a.rs".into()]).unwrap();

        // Discovery and single-session auto-resolution both find it.
        assert!(discover()
            .unwrap()
            .iter()
            .any(|f| f.meta.name.as_deref() == Some("unit")));
        let target = resolve_target(None).unwrap();
        assert_eq!(target.meta.name.as_deref(), Some("unit"));
        assert_eq!(target.meta.files, vec!["a.rs".to_string()]);

        // A stand-in main loop answers exactly one request.
        let main = std::thread::spawn(move || {
            if let Ok(msg) = rx.recv() {
                assert!(matches!(msg.req, IpcRequest::List));
                let _ = msg.reply.send("{\"threads\":[]}".into());
            }
        });
        let resp = query(&target.sock, "{\"cmd\":\"list\"}").unwrap();
        assert_eq!(resp, "{\"threads\":[]}");
        main.join().unwrap();

        // Unknown selector is an error.
        assert!(resolve_target(Some("nope")).is_err());

        // Dropping the session deregisters it.
        drop(session);
        assert!(!discover()
            .unwrap()
            .iter()
            .any(|f| f.meta.name.as_deref() == Some("unit")));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
