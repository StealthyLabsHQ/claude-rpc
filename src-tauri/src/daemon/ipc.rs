// Discord IPC transport: framing, handshake, activity pushes, and the
// per-platform connection (named pipe on Windows, unix socket elsewhere).

use serde_json::{json, Value};
use std::io::{Read, Write};

#[cfg(windows)]
use std::{
    fs::{File, OpenOptions},
    thread,
    time::Duration,
};
#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::PathBuf, time::Duration};

#[cfg(windows)]
use windows::Win32::{Foundation::HANDLE, System::Pipes::PeekNamedPipe};

#[cfg(windows)]
use super::now_ms;
use super::sanitize_discord_user;

const IPC_READ_TIMEOUT_MS: u64 = 5_000;
pub(crate) struct DiscordIpc {
    connection: IpcConnection,
    pub(crate) username: Option<String>,
    nonce: u64,
}

enum IpcConnection {
    #[cfg(windows)]
    File(File),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for IpcConnection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::File(file) => file.read(buf),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
        }
    }
}

impl Write for IpcConnection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::File(file) => file.write(buf),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::File(file) => file.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

impl IpcConnection {
    /// Block until at least `needed` bytes can be read without the following
    /// `read_exact` blocking, or fail after `IPC_READ_TIMEOUT_MS`. This bounds the
    /// single daemon thread (and Quit's `handle.join`) so a wedged Discord that
    /// accepts the SET_ACTIVITY write but never answers can no longer hang it
    /// forever — a timeout/closed-pipe surfaces as `Err`, and the run loop drops
    /// the connection and reconnects on the next tick. On Unix the stream's own
    /// read timeout already enforces this, so it is a no-op there.
    fn await_readable(&self, needed: usize) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::File(file) => wait_pipe_readable(file, needed),
            #[cfg(unix)]
            Self::Unix(_) => {
                let _ = needed;
                Ok(())
            }
        }
    }
}

// Windows named pipes opened as a plain blocking `File` cannot use
// `set_read_timeout`. Poll `PeekNamedPipe` (non-destructive) until enough bytes
// are buffered that the subsequent `read_exact` returns immediately, bailing out
// on a closed pipe or after the timeout budget.
#[cfg(windows)]
fn wait_pipe_readable(file: &File, needed: usize) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    if needed == 0 {
        return Ok(());
    }
    let handle = HANDLE(file.as_raw_handle());
    let deadline = now_ms().saturating_add(IPC_READ_TIMEOUT_MS);
    loop {
        let mut available: u32 = 0;
        unsafe { PeekNamedPipe(handle, None, 0, None, Some(&mut available), None) }.map_err(
            |_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "discord ipc pipe closed"),
        )?;
        if available as usize >= needed {
            return Ok(());
        }
        if now_ms() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "discord ipc read timed out",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

impl DiscordIpc {
    pub(crate) fn connect(client_id: &str) -> std::io::Result<Self> {
        let mut client = Self {
            connection: connect_discord_ipc()?,
            username: None,
            nonce: 0,
        };
        client.send_frame(0, &json!({ "v": 1, "client_id": client_id }))?;
        let ready = client.read_frame()?;
        client.username = ready
            .get("data")
            .and_then(|data| data.get("user"))
            .and_then(|user| user.get("username"))
            .and_then(Value::as_str)
            .map(|value| sanitize_discord_user(value).unwrap_or_else(|| value.to_string()));
        Ok(client)
    }

    pub(crate) fn set_activity(&mut self, activity: Value) -> std::io::Result<()> {
        let nonce = self.next_nonce();
        self.send_frame(
            1,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": { "pid": std::process::id(), "activity": activity },
                "nonce": nonce,
            }),
        )?;
        self.read_response(&nonce)
    }

    pub(crate) fn clear_activity(&mut self) -> std::io::Result<()> {
        let nonce = self.next_nonce();
        self.send_frame(
            1,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": { "pid": std::process::id() },
                "nonce": nonce,
            }),
        )?;
        self.read_response(&nonce)
    }

    fn read_response(&mut self, nonce: &str) -> std::io::Result<()> {
        for _ in 0..4 {
            let frame = self.read_frame()?;
            if frame.get("nonce").and_then(Value::as_str) == Some(nonce) {
                if frame.get("evt").and_then(Value::as_str) == Some("ERROR") {
                    return Err(std::io::Error::other("discord rpc error"));
                }
                return Ok(());
            }
        }
        // No frame carried our nonce within the budget: treat the push as
        // unconfirmed rather than silently successful, so the run loop drops the
        // connection and reconnects instead of caching a presence that may never
        // have landed. With no event subscriptions the response is normally the
        // very first frame, so this only fires on a genuinely wedged/desynced peer.
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "discord rpc: no matching response",
        ))
    }

    fn next_nonce(&mut self) -> String {
        self.nonce += 1;
        format!("claude-rpc-{}-{}", std::process::id(), self.nonce)
    }

    fn send_frame(&mut self, opcode: u32, payload: &Value) -> std::io::Result<()> {
        let data = serde_json::to_vec(payload)?;
        self.connection.write_all(&opcode.to_le_bytes())?;
        self.connection
            .write_all(&(data.len() as u32).to_le_bytes())?;
        self.connection.write_all(&data)?;
        self.connection.flush()
    }

    fn read_frame(&mut self) -> std::io::Result<Value> {
        // Bound the loop: a peer that only ever sends PING (opcode 3) must not
        // be able to keep this thread spinning forever.
        for _ in 0..16 {
            let mut header = [0u8; 8];
            self.connection.await_readable(header.len())?;
            self.connection.read_exact(&mut header)?;
            let opcode = u32::from_le_bytes(header[0..4].try_into().unwrap());
            let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            if len > 1024 * 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "discord ipc frame too large",
                ));
            }
            let mut payload = vec![0u8; len];
            self.connection.await_readable(len)?;
            self.connection.read_exact(&mut payload)?;
            let value: Value = serde_json::from_slice(&payload)?;
            match opcode {
                1 => return Ok(value),
                2 => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "discord closed ipc",
                    ));
                }
                3 => {
                    let _ = self.send_frame(4, &value);
                }
                4 => {}
                _ => {}
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "discord ipc: too many control frames",
        ))
    }
}

#[cfg(windows)]
fn connect_discord_ipc() -> std::io::Result<IpcConnection> {
    for id in 0..10 {
        let path = format!(r"\\?\pipe\discord-ipc-{id}");
        if let Ok(candidate) = OpenOptions::new().read(true).write(true).open(path) {
            return Ok(IpcConnection::File(candidate));
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "discord ipc",
    ))
}

#[cfg(unix)]
fn connect_discord_ipc() -> std::io::Result<IpcConnection> {
    for base in discord_ipc_roots() {
        for id in 0..10 {
            let path = base.join(format!("discord-ipc-{id}"));
            if let Ok(stream) = UnixStream::connect(path) {
                // Bound blocking reads/writes so a wedged Discord can't hang the
                // single daemon thread (and Quit's handle.join) forever; a timeout
                // surfaces as Err and the run loop reconnects on the next tick.
                let _ = stream.set_read_timeout(Some(Duration::from_millis(IPC_READ_TIMEOUT_MS)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(IPC_READ_TIMEOUT_MS)));
                return Ok(IpcConnection::Unix(stream));
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "discord ipc",
    ))
}

#[cfg(unix)]
fn discord_ipc_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for name in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        if let Some(path) = std::env::var_os(name).map(PathBuf::from) {
            push_unique_path(&mut roots, path);
        }
    }
    for path in ["/tmp", "/var/tmp", "/usr/tmp"] {
        push_unique_path(&mut roots, PathBuf::from(path));
    }
    roots
}

#[cfg(unix)]
fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}
