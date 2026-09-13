//! Unix-domain control socket for `hyprforge-settings`.
//!
//! # Why this exists
//!
//! The tray spawns `hyprforge-settings --screen <name>` fresh on every
//! click (`hyprforge-tray/src/bin/trayd.rs`'s `spawn_settings`). Without
//! this, clicking "Network settings…" then "Bluetooth settings…" opens
//! two windows, wherever the compositor happens to put them. The owner's
//! decision: one Settings window, ever — if one is already running, a
//! second invocation hands its request to it and exits instead of
//! opening its own.
//!
//! `singleton.rs` is what tells a second invocation that one is already
//! running (an `flock`, not a PID file or a `pgrep`, per CLAUDE.md). This
//! module is how the second invocation then gets its `--screen` request
//! *to* the first, over a socket, modelled directly on
//! `hyprforge-clipboard`'s `ipc.rs` (itself modelled on
//! `notif-ipc`) so this workspace keeps one socket convention rather than
//! growing a third.
//!
//! # Protocol
//!
//! One JSON object per line, request -> response:
//!
//! | Request                              | Response                       |
//! |---------------------------------------|--------------------------------|
//! | `{"cmd":"show-screen","screen":"…"}`  | `{"ok":true}`                  |
//! | `{"cmd":"status"}`                    | `{"ok":true,"status":{…}}`     |
//! | unknown / malformed                   | `{"ok":false,"error":"…"}`     |
//!
//! `show-screen` names a screen the same way `--screen` does (see
//! `screen_from_cli` in `main.rs`) — an unrecognised name is refused with
//! an error response, exactly like an unrecognised `--screen` argument is
//! refused at the command line, rather than silently falling back to
//! whatever screen happened to be open.
//!
//! `status` is cheap and makes the socket debuggable independent of any
//! screen change — see `StatusInfo`.
//!
//! The connection stays open after a response; a client may send
//! multiple requests before closing. Malformed requests do **not** close
//! the connection.
//!
//! # What a request actually does, and where
//!
//! This module never touches the running [`App`](crate::App)'s state
//! directly — it has no idea `Screen` or `Message` exist. Parsing a
//! request line and deciding what to answer is the pure
//! [`handle_line`], tested below with no socket at all. The async server
//! ([`run_at`]) only does I/O: read a line, call `handle_line`, write the
//! response, and — for a validated `show-screen` — forward the raw
//! screen name to whatever's listening on the channel it was given.
//! `main.rs`'s subscription is the other end of that channel: it is the
//! one place that turns a screen name back into a `Screen` (through the
//! same `screen_from_cli` the command line already uses) and decides to
//! focus the window. That keeps one source of truth for "which names are
//! screens" instead of this module growing a second copy of `Screen`.
//!
//! # Socket path
//!
//! `$XDG_RUNTIME_DIR/hyprforge-settings.sock` — the same convention as
//! `notif.sock` and `clipd.sock`. Any stale socket at that path is
//! removed before binding; the socket is removed again on clean exit.
//!
//! # A client cannot tie up anything
//!
//! A connection that sends nothing at all is bounded by [`IDLE_TIMEOUT`]
//! rather than left to `read_line` forever, and each connection runs as
//! its own task so one slow or silent client cannot block another.
//! [`CLIENT_TIMEOUT`] bounds the other direction: a second invocation
//! must never hang waiting on a wedged first one — CLAUDE.md is explicit
//! that nothing here waits on another process without a bound.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::UnboundedSender;

/// How long a connection may sit idle (no complete request line) before
/// this process gives up on it and closes it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a second invocation waits for the running instance to answer
/// before giving up. Short: the user is waiting on this as a menu click,
/// and a tray click that hung for even a second would look broken.
pub const CLIENT_TIMEOUT: Duration = Duration::from_millis(500);

/// Errors setting up the IPC listener. Distinct from a request failing —
/// these are startup problems, not something a client asked for.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("$XDG_RUNTIME_DIR is not set; cannot determine the settings control socket path")]
    NoRuntimeDir,
    #[error("failed to bind settings control socket at {path}: {source}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// `$XDG_RUNTIME_DIR/hyprforge-settings.sock`.
pub fn socket_path() -> Result<PathBuf, IpcError> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(IpcError::NoRuntimeDir)?;
    Ok(PathBuf::from(runtime_dir).join("hyprforge-settings.sock"))
}

// ── Wire protocol ────────────────────────────────────────────────────────

/// Incoming request, tagged on `"cmd"` in kebab-case — the same shape
/// `notif-ipc::protocol::Request` and `hyprforge-clipboard::ipc::Request`
/// use.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    /// Switch the running window to this screen (and focus it). `screen`
    /// is the same string `--screen` accepts, validated the same way.
    ShowScreen { screen: String },
    /// No screen change — just prove there is a process on the other end
    /// (and, per `main.rs`, still focus the window). This is what a bare
    /// second `hyprforge-settings` invocation with no `--screen` sends.
    Status,
}

/// `status` snapshot. Deliberately minimal: this module has no access to
/// the running app's actual screen (see the module doc's "what a
/// request actually does, and where"), so this only confirms there is a
/// live process holding the socket — which is exactly what a caller
/// asking `status` wants to know.
#[derive(Debug, Serialize, PartialEq)]
pub struct StatusInfo {
    pub running: bool,
}

/// A response, serialised as one JSON object with `"ok"` first.
#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Response {
    Ok,
    Err { ok: bool, error: String },
    Status { ok: bool, status: StatusInfo },
}

/// Every response as JSON, with `"ok"` always present and, for
/// [`Response::Ok`], nothing else — `{"ok":true}` exactly, matching
/// `notif-ipc`'s `OkResponse`. Handled by hand rather than deriving
/// `Serialize` for a bare unit variant, which would serialise as `null`.
fn to_json(response: &Response) -> String {
    match response {
        Response::Ok => r#"{"ok":true}"#.to_string(),
        other => serde_json::to_string(other)
            .unwrap_or_else(|_| r#"{"ok":false,"error":"internal serialization error"}"#.into()),
    }
}

fn err(message: impl Into<String>) -> Response {
    Response::Err { ok: false, error: message.into() }
}

// ── The pure layer ──────────────────────────────────────────────────────

/// Parses one request line and decides what to answer, and — for a
/// validated `show-screen` — which screen name to hand upstream. No I/O
/// happens in here, and it does not know what a `Screen` or a `Message`
/// is: `is_known_screen` is the same validity check `--screen` uses
/// (`crate::screen_name_is_known`, wrapping `screen_from_cli`), passed in
/// so this stays testable with a fake screen list and so there is one
/// source of truth for "which names are screens" rather than two.
///
/// Returns `(response_json, screen_to_show)`. `screen_to_show` is
/// `Some(name)` only when the request was `show-screen` and `name` named
/// a real screen — never for `status`, and never for a rejected name.
pub fn handle_line(line: &str, is_known_screen: fn(&str) -> bool) -> (String, Option<String>) {
    let request: Result<Request, _> = serde_json::from_str(line);
    match request {
        Err(e) => (to_json(&err(format!("malformed request: {e}"))), None),
        Ok(Request::Status) => {
            (to_json(&Response::Status { ok: true, status: StatusInfo { running: true } }), None)
        }
        Ok(Request::ShowScreen { screen }) => {
            if is_known_screen(&screen) {
                (to_json(&Response::Ok), Some(screen))
            } else {
                (to_json(&err(format!("no screen called {screen:?}"))), None)
            }
        }
    }
}

// ── The blocking client ──────────────────────────────────────────────────
//
// The second invocation is a short-lived process with no other use for
// an async runtime — "connect, write one line, read one line" needs
// nothing more than a blocking socket with a deadline on it, the same
// choice `hyprforge-clipboard::ipc`'s client makes for the same reason.

/// Everything that can go wrong asking the running instance to show a
/// screen. Kept distinct from [`IpcError`] (the server's own startup
/// errors): a service that is not running is its own state with its own
/// message, per CLAUDE.md, never folded into an ordinary failure — and
/// never cached, since the caller here runs once and exits regardless.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("$XDG_RUNTIME_DIR is not set")]
    NoRuntimeDir,
    /// Almost always means no `hyprforge-settings` is running — a
    /// missing socket file behaves the same as a refused connection.
    #[error("couldn't reach a running hyprforge-settings: {0}")]
    Unreachable(String),
    #[error("hyprforge-settings did not respond in time")]
    Timeout,
    #[error("hyprforge-settings sent a response this client could not understand")]
    Malformed,
    /// Understood the request and refused it — an unknown screen name.
    #[error("{0}")]
    Refused(String),
}

fn client_socket_error(error: std::io::Error) -> ClientError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ClientError::Timeout,
        _ => ClientError::Unreachable(error.to_string()),
    }
}

/// Sends one request line to `path` and interprets the one response line
/// that comes back. No I/O happens beyond that single round trip.
fn request_at(path: &Path, request: &Request, timeout: Duration) -> Result<(), ClientError> {
    use std::io::{BufRead, Write};

    let mut stream = std::os::unix::net::UnixStream::connect(path).map_err(client_socket_error)?;
    // Both directions get the same bound: a write can block too, on a
    // kernel socket buffer that never drains because nothing on the
    // other end is reading.
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let line = serde_json::to_string(request).expect("Request always serialises");
    stream.write_all(format!("{line}\n").as_bytes()).map_err(client_socket_error)?;

    let mut reader = std::io::BufReader::new(stream);
    let mut response_line = String::new();
    let read = reader.read_line(&mut response_line).map_err(client_socket_error)?;
    if read == 0 {
        return Err(ClientError::Unreachable("connection closed with no response".to_string()));
    }

    let value: serde_json::Value =
        serde_json::from_str(response_line.trim()).map_err(|_| ClientError::Malformed)?;
    match value.get("ok").and_then(|v| v.as_bool()) {
        Some(true) => Ok(()),
        Some(false) => {
            let message =
                value.get("error").and_then(|v| v.as_str()).unwrap_or("request refused").to_string();
            Err(ClientError::Refused(message))
        }
        None => Err(ClientError::Malformed),
    }
}

/// Asks the running `hyprforge-settings` at the default socket path to
/// switch to `screen` (and focus itself). `screen` is passed through
/// unvalidated on this side — the server is what decides whether it
/// names a real screen, the same way it decides for `--screen`.
pub fn request_show_screen(screen: &str) -> Result<(), ClientError> {
    let path = socket_path().map_err(|_| ClientError::NoRuntimeDir)?;
    request_show_screen_at(&path, screen, CLIENT_TIMEOUT)
}

/// [`request_show_screen`] against an explicit socket `path` and
/// `timeout` — the seam the tests below use instead of
/// `$XDG_RUNTIME_DIR`'s real socket.
pub fn request_show_screen_at(
    path: &Path,
    screen: &str,
    timeout: Duration,
) -> Result<(), ClientError> {
    request_at(path, &Request::ShowScreen { screen: screen.to_string() }, timeout)
}

/// Asks the running `hyprforge-settings` to focus itself, with no screen
/// change — what a bare second invocation (no `--screen`) sends.
pub fn request_focus() -> Result<(), ClientError> {
    let path = socket_path().map_err(|_| ClientError::NoRuntimeDir)?;
    request_focus_at(&path, CLIENT_TIMEOUT)
}

/// [`request_focus`] against an explicit socket `path` and `timeout`.
pub fn request_focus_at(path: &Path, timeout: Duration) -> Result<(), ClientError> {
    request_at(path, &Request::Status, timeout)
}

// ── The socket layer ────────────────────────────────────────────────────

/// Runs the control socket at `$XDG_RUNTIME_DIR/hyprforge-settings.sock`
/// until this returns. Removes a stale socket file before binding and
/// removes its own socket file again on return. Every validated
/// `show-screen` request's screen name is sent on `screens`; a `status`
/// request sends nothing on the channel (nothing to show), so `main.rs`'s
/// subscription must still treat *any* incoming connection as a reason to
/// focus the window, not only one that carried a screen name.
///
/// Splitting "was there a connection at all" from "which screen" needs a
/// richer channel item than `String` alone, hence [`Signal`].
pub async fn run(screens: UnboundedSender<Signal>) -> Result<(), IpcError> {
    run_at(&socket_path()?, screens).await
}

/// A control-socket event, forwarded upstream for `main.rs`'s
/// subscription to turn into a `Message`. Kept to two variants rather
/// than reusing `Option<String>` so a reader doesn't have to remember
/// which meaning `None` carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// A `show-screen` request named a real screen — switch to it (and
    /// focus).
    ShowScreen(String),
    /// A connection was made but asked for no screen change (`status`,
    /// or a `show-screen` naming an unknown screen) — focus only.
    FocusOnly,
}

/// [`run`] against an explicit `path` — the seam tests use to avoid
/// `$XDG_RUNTIME_DIR` and the real socket entirely.
pub async fn run_at(path: &Path, screens: UnboundedSender<Signal>) -> Result<(), IpcError> {
    /// Removes the socket file when dropped, on a clean return or a
    /// cancelled task alike — the same reasoning as `notif-ipc`'s and
    /// `hyprforge-clipboard::ipc`'s `SocketGuard`.
    struct SocketGuard(PathBuf);
    impl Drop for SocketGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).map_err(|source| IpcError::Bind { path: path.to_path_buf(), source })?;
    let _guard = SocketGuard(path.to_path_buf());

    tracing::info!(path = %path.display(), "listening for settings control connections");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let screens = screens.clone();
                tokio::spawn(async move {
                    handle_connection(stream, screens).await;
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept error on settings control socket");
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, screens: UnboundedSender<Signal>) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let read = tokio::time::timeout(IDLE_TIMEOUT, reader.read_line(&mut line)).await;
        let n = match read {
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return, // read error: client gone
            Err(_) => {
                tracing::debug!("settings control connection idle too long; closing");
                return;
            }
        };
        if n == 0 {
            return; // client closed the connection
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let (response_json, screen) = handle_line(trimmed, crate::screen_name_is_known);
        // Any connection at all is a reason to focus — see `Signal`'s
        // doc. A malformed line still counts: something tried to reach
        // this process, even if it said nothing understandable.
        let signal = match screen {
            Some(name) => Signal::ShowScreen(name),
            None => Signal::FocusOnly,
        };
        let _ = screens.send(signal);

        if writer.write_all(format!("{response_json}\n").as_bytes()).await.is_err() {
            return; // client gone
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn known(name: &str) -> bool {
        matches!(name, "network" | "bluetooth")
    }

    #[test]
    fn a_known_screen_is_accepted_and_handed_upstream() {
        let (json, screen) = handle_line(r#"{"cmd":"show-screen","screen":"network"}"#, known);
        assert_eq!(json, r#"{"ok":true}"#);
        assert_eq!(screen, Some("network".to_string()));
    }

    #[test]
    fn an_unknown_screen_is_refused_and_nothing_is_handed_upstream() {
        let (json, screen) = handle_line(r#"{"cmd":"show-screen","screen":"bogus"}"#, known);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["ok"], false);
        assert!(value["error"].as_str().unwrap().contains("bogus"));
        assert_eq!(screen, None);
    }

    #[test]
    fn status_reports_running_and_asks_for_no_screen_change() {
        let (json, screen) = handle_line(r#"{"cmd":"status"}"#, known);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["status"]["running"], true);
        assert_eq!(screen, None);
    }

    #[test]
    fn malformed_json_is_refused_without_closing_the_conversation() {
        let (json, screen) = handle_line("not json at all", known);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["ok"], false);
        assert!(value["error"].as_str().is_some());
        assert_eq!(screen, None);
    }

    #[test]
    fn an_unknown_command_tag_is_refused() {
        let (json, screen) = handle_line(r#"{"cmd":"frobnicate"}"#, known);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(screen, None);
    }

    // ── The socket layer, against a real (throwaway) socket ────────────

    async fn do_cmd(path: &Path, request: &str) -> String {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let stream = UnixStream::connect(path).await.expect("connect to control socket");
        let (reader, mut writer) = stream.into_split();
        writer.write_all(format!("{request}\n").as_bytes()).await.expect("write request");
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read response");
        line.trim().to_string()
    }

    #[tokio::test]
    async fn the_server_answers_show_screen_and_forwards_it_upstream() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let socket_path = tmpdir.path().join("settings-test.sock");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let path_for_server = socket_path.clone();
        tokio::spawn(async move {
            let _ = run_at(&path_for_server, tx).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = do_cmd(&socket_path, r#"{"cmd":"show-screen","screen":"network"}"#).await;
        assert_eq!(resp, r#"{"ok":true}"#);
        assert_eq!(rx.recv().await, Some(Signal::ShowScreen("network".to_string())));
    }

    #[tokio::test]
    async fn a_status_request_focuses_without_naming_a_screen() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let socket_path = tmpdir.path().join("settings-test.sock");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let path_for_server = socket_path.clone();
        tokio::spawn(async move {
            let _ = run_at(&path_for_server, tx).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = do_cmd(&socket_path, r#"{"cmd":"status"}"#).await;
        let value: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(rx.recv().await, Some(Signal::FocusOnly));
    }

    #[tokio::test]
    async fn a_stale_socket_file_does_not_block_binding() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let socket_path = tmpdir.path().join("settings-test.sock");
        // A leftover regular file where the socket should be — as if a
        // previous run left one behind (or something else created it).
        std::fs::write(&socket_path, b"not a socket").unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let path_for_server = socket_path.clone();
        let handle = tokio::spawn(async move { run_at(&path_for_server, tx).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!handle.is_finished(), "run_at should still be serving, not stuck on a bind error");
        handle.abort();
    }

    #[test]
    fn the_blocking_client_reports_unreachable_when_nobody_is_listening() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let socket_path = tmpdir.path().join("nobody-here.sock");
        let result = request_show_screen_at(&socket_path, "network", Duration::from_millis(100));
        assert!(matches!(result, Err(ClientError::Unreachable(_))));
    }

    #[tokio::test]
    async fn the_blocking_client_is_refused_for_an_unknown_screen() {
        let tmpdir = tempfile::TempDir::new().unwrap();
        let socket_path = tmpdir.path().join("settings-test.sock");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let path_for_server = socket_path.clone();
        tokio::spawn(async move {
            let _ = run_at(&path_for_server, tx).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let path_for_client = socket_path.clone();
        let result = tokio::task::spawn_blocking(move || {
            request_show_screen_at(&path_for_client, "bogus", CLIENT_TIMEOUT)
        })
        .await
        .unwrap();
        assert!(matches!(result, Err(ClientError::Refused(_))));
    }
}
