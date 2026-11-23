//! The operator's commands (`mh3sp announce`, `mh3sp stop`) reach the running server through a socket that is on no
//! port and in no file: a Linux abstract Unix socket, reached by its name alone ([`NAME`]). There is nothing to mount
//! or expose, it exists in the network namespace of the process that holds it (so, in a container, only for what runs
//! in it: `docker compose exec mh3sp mh3sp stop 5min`), and it goes with the process.
//!
//! An abstract socket has no permissions, and with host networking every process of the host shares the namespace:
//! the server answers only its own user and root, read from the caller's credentials ([`allowed`]), and the commands
//! talk only to a listener of that same kind, read from the listener's: a process of another user that took the name
//! while the server was down gets nothing said to it, and can't pass for it.
//!
//! Linux only: elsewhere the commands say so, and the chat's `/announce` and `/stop` remain. A second server in the
//! same namespace finds the name taken and runs without the socket (a warning).

use std::sync::Arc;

use anyhow::Result;
#[cfg(target_os = "linux")]
use anyhow::{Context, bail};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::Shared;

/// The socket's name: fixed, so that a command finds the server without reading its configuration.
pub const NAME: &str = "mh3sp";

/// Whether the user at the other end of the socket is the server's own, or root: what a caller must be to command the
/// server, and what the server must be for a command to trust it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn allowed(peer: u32, server: u32) -> bool {
    peer == server || peer == 0
}

/// What a command prints where the socket doesn't exist.
#[cfg(not(target_os = "linux"))]
const NEEDS_LINUX: &str = "this command needs Linux (the server's commands go through an abstract Unix socket): use the chat's /announce and /stop";

/// Binds the socket by its name.
#[cfg(target_os = "linux")]
fn bind(name: &str) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    let listener = UnixListener::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes())?)?;
    listener.set_nonblocking(true)?;
    tokio::net::UnixListener::from_std(listener)
}

/// Binds the socket and serves it until `shutdown`, in `tasks`. A name already taken, or no abstract sockets, is a
/// warning: the server runs, only its two commands can't reach it.
#[cfg(target_os = "linux")]
pub(crate) fn start(
    name: &str,
    shared: &Arc<Shared>,
    shutdown: &CancellationToken,
    tasks: &mut JoinSet<()>,
) {
    match bind(name) {
        Ok(listener) => {
            tracing::info!(role = "control", name, "listening");
            tasks.spawn(serve::run(listener, shared.clone(), shutdown.clone()));
        }
        Err(e) => tracing::warn!(
            error = %e,
            name,
            "the commands' socket is not available (another server runs here?): `mh3sp announce` and `mh3sp stop` won't reach this one"
        ),
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn start(
    _name: &str,
    _shared: &Arc<Shared>,
    _shutdown: &CancellationToken,
    _tasks: &mut JoinSet<()>,
) {
}

#[cfg(target_os = "linux")]
mod serve {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::extract::{DefaultBodyLimit, State};
    use axum::http::StatusCode;
    use axum::response::Response;
    use axum::routing::post;
    use tokio::net::{UnixListener, UnixStream};
    use tokio_util::sync::CancellationToken;
    use tracing::{error, info, warn};

    use crate::Shared;
    use crate::public::text;

    /// Whether the user behind a connection (its uid) may command the server.
    pub(super) type Allow = Box<dyn Fn(u32) -> bool + Send + Sync>;

    /// The listener axum serves: connections whose caller isn't allowed are closed before any request is read.
    pub(super) struct Owner {
        pub(super) inner: UnixListener,
        pub(super) allow: Allow,
    }

    impl axum::serve::Listener for Owner {
        type Io = UnixStream;
        type Addr = tokio::net::unix::SocketAddr;

        async fn accept(&mut self) -> (Self::Io, Self::Addr) {
            loop {
                match self.inner.accept().await {
                    Ok((stream, addr)) => match stream.peer_cred() {
                        Ok(caller) if (self.allow)(caller.uid()) => {
                            return (stream, addr);
                        }
                        Ok(caller) => {
                            warn!(
                                uid = caller.uid(),
                                "a caller of the commands' socket was refused"
                            );
                        }
                        Err(e) => {
                            warn!(error = %e, "the caller of the commands' socket is unknown: refused")
                        }
                    },
                    // A failed accept (descriptors used up): wait, don't spin.
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        }

        fn local_addr(&self) -> std::io::Result<Self::Addr> {
            self.inner.local_addr()
        }
    }

    pub(super) async fn run(
        listener: UnixListener,
        shared: Arc<Shared>,
        shutdown: CancellationToken,
    ) {
        let uid = rustix::process::getuid().as_raw();
        let listener = Owner {
            inner: listener,
            allow: Box::new(move |caller| super::allowed(caller, uid)),
        };
        let app = Router::new()
            .route("/announce", post(announce))
            .route("/stop", post(stop))
            // An announcement is a few lines of chat.
            .layer(DefaultBodyLimit::max(2048))
            .with_state(shared);
        let stopped = async move { shutdown.cancelled().await };
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(stopped)
            .await
        {
            error!(error = %e, "the commands' socket stopped");
        }
    }

    async fn announce(State(shared): State<Arc<Shared>>, body: String) -> Response {
        match body.trim() {
            "" => text(StatusCode::BAD_REQUEST, "no text\n"),
            message => {
                let players = crate::game::announce(&shared, message);
                info!(players, text = %message, "announcement");
                text(StatusCode::OK, format!("sent to {players} players\n"))
            }
        }
    }

    async fn stop(State(shared): State<Arc<Shared>>, body: String) -> Response {
        match body.trim() {
            "cancel" => match crate::stop::cancel(&shared) {
                crate::stop::Cancelled::Lifted => text(StatusCode::OK, "stop cancelled\n"),
                refused => text(StatusCode::CONFLICT, format!("{}\n", refused.said())),
            },
            delay => match crate::stop::parse(delay) {
                Some(delay) => {
                    crate::stop::begin(&shared, delay);
                    let when = crate::stop::human(delay);
                    text(StatusCode::OK, format!("stopping in {when}\n"))
                }
                None => text(
                    StatusCode::BAD_REQUEST,
                    "a delay like 5min, 1h30m, 30s, or cancel\n",
                ),
            },
        }
    }
}

/// `mh3sp announce "<text>"`: an announcement to every player of the running server; what it answered.
pub fn announce(name: &str, text: &str) -> Result<String> {
    command(name, "announce", text)
}

/// `mh3sp stop <delay|cancel>`: stopping the running server with warning; what it answered.
pub fn stop(name: &str, delay: &str) -> Result<String> {
    command(name, "stop", delay)
}

#[cfg(target_os = "linux")]
fn command(name: &str, command: &str, body: &str) -> Result<String> {
    let me = rustix::process::getuid().as_raw();
    exchange(name, command, body, |server| allowed(server, me))
}

/// One request to the server listening on `name`, and what it answered. Nothing is sent before the listener's user is
/// known to be one `trusted` (the server's own, or root): the name is free to anyone sharing the namespace. The whole
/// exchange is bounded, in time and in size, since what answers is a process this one doesn't control until then.
#[cfg(target_os = "linux")]
fn exchange(
    name: &str,
    command: &str,
    body: &str,
    trusted: impl Fn(u32) -> bool,
) -> Result<String> {
    use std::io::{ErrorKind, Read, Write};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixStream};
    use std::time::{Duration, Instant};

    const TIMEOUT: Duration = Duration::from_secs(5);
    /// The answer is a line; more than this is not the server's.
    const MAX_ANSWER: usize = 16 * 1024;
    // What the server does to a caller it refuses: it closes, which the caller may see as an end, a reset or a broken
    // pipe, depending on whether its request had been sent.
    let closed =
        |e: &std::io::Error| matches!(e.kind(), ErrorKind::ConnectionReset | ErrorKind::BrokenPipe);

    let addr = SocketAddr::from_abstract_name(name.as_bytes())?;
    let mut stream = UnixStream::connect_addr(&addr).with_context(|| {
        format!("no running server answers on the commands' socket ({name}): is mh3sp running here, as this user?")
    })?;
    let listener = rustix::net::sockopt::socket_peercred(&stream)
        .context("the user of the commands' socket's listener is unknown")?;
    if !trusted(listener.uid.as_raw()) {
        bail!(
            "what listens on the commands' socket ({name}) is not this server's user (nor root): nothing was sent to it"
        );
    }
    let deadline = Instant::now() + TIMEOUT;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let request = format!(
        "POST /{command} HTTP/1.0\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    if let Err(e) = stream.write_all(request.as_bytes())
        && !closed(&e)
    {
        return Err(e.into());
    }
    let mut response = Vec::new();
    let mut buf = [0; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("the server did not answer in {} s", TIMEOUT.as_secs());
        }
        stream.set_read_timeout(Some(left))?;
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&buf[..n]);
                if response.len() > MAX_ANSWER {
                    bail!("the answer on the commands' socket is too long to be the server's");
                }
            }
            Err(e) if closed(&e) => break,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                bail!("the server did not answer in {} s", TIMEOUT.as_secs())
            }
            Err(e) => return Err(e.into()),
        }
    }
    // A caller the server refuses is closed with no answer.
    if response.is_empty() {
        bail!(
            "the server closed the connection: this user may not command it (its own user, or root)"
        );
    }
    let response = String::from_utf8_lossy(&response);
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    match head.split_whitespace().nth(1) {
        Some("200") => Ok(body.trim().to_owned()),
        _ => bail!("refused: {}", body.trim()),
    }
}

#[cfg(not(target_os = "linux"))]
fn command(_name: &str, _command: &str, _body: &str) -> Result<String> {
    anyhow::bail!(NEEDS_LINUX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's own user and root, nobody else: an abstract socket has no permissions of its own.
    #[test]
    fn only_the_servers_user_and_root_may_command_it() {
        assert!(allowed(1000, 1000), "the server's own user");
        assert!(allowed(0, 1000), "root");
        assert!(!allowed(1001, 1000), "another user");
        assert!(!allowed(1000, 0), "a user, when the server runs as root");
        assert!(allowed(0, 0));
    }

    /// A name of its own for each test: they run side by side in one network namespace.
    #[cfg(target_os = "linux")]
    fn unique(what: &str) -> String {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!(
            "mh3sp-test-{what}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    /// The wiring, not the rule (that is `allowed`, tested above, and root would pass any uid test): a listener whose
    /// check says no closes the connection before reading a byte, never hands it to the server, and the command says
    /// why. With a check that says yes, the same connection is handed over: so the refusal is the check's.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_listener_refuses_a_caller_it_does_not_allow() {
        use axum::serve::Listener;

        let name = unique("refuse");
        let mut owner = serve::Owner {
            inner: bind(&name).unwrap(),
            allow: Box::new(|_| false),
        };
        let accepting = tokio::spawn(async move {
            owner.accept().await;
        });
        let caller = name.clone();
        let told = tokio::task::spawn_blocking(move || exchange(&caller, "stop", "5min", |_| true))
            .await
            .unwrap();
        let told = format!("{:#}", told.unwrap_err());
        assert!(told.contains("may not command it"), "{told}");
        assert!(!accepting.is_finished(), "a refused caller was handed over");
        accepting.abort();

        let name = unique("allow");
        let mut owner = serve::Owner {
            inner: bind(&name).unwrap(),
            allow: Box::new(|_| true),
        };
        let caller = name.clone();
        let client =
            tokio::task::spawn_blocking(move || exchange(&caller, "stop", "5min", |_| true));
        let (stream, _) = tokio::time::timeout(std::time::Duration::from_secs(5), owner.accept())
            .await
            .expect("an allowed caller is handed over");
        // Nobody serves it here: closing it ends the command, which says no more than that.
        drop(stream);
        assert!(client.await.unwrap().is_err());
    }

    /// The other way round: the commands send nothing to a listener that isn't the server's user (nor root). The
    /// listener here answers anyone, and reads what the command sent: nothing.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_command_sends_nothing_to_a_listener_that_is_not_the_servers() {
        use axum::serve::Listener;
        use tokio::io::AsyncReadExt;

        let name = unique("impostor");
        let mut impostor = serve::Owner {
            inner: bind(&name).unwrap(),
            allow: Box::new(|_| true),
        };
        let caller = name.clone();
        let command =
            tokio::task::spawn_blocking(move || exchange(&caller, "announce", "secret", |_| false));
        let (mut stream, _) = impostor.accept().await;
        let mut heard = Vec::new();
        stream.read_to_end(&mut heard).await.unwrap();
        assert!(heard.is_empty(), "{}", String::from_utf8_lossy(&heard));
        let told = format!("{:#}", command.await.unwrap().unwrap_err());
        assert!(told.contains("is not this server's user"), "{told}");
    }

    /// What answers is bounded in size and in time: a listener that never stops talking, or never answers, doesn't hang
    /// the command.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_command_does_not_wait_for_ever_on_what_answers() {
        use axum::serve::Listener;
        use tokio::io::AsyncWriteExt;

        let name = unique("endless");
        let mut talker = serve::Owner {
            inner: bind(&name).unwrap(),
            allow: Box::new(|_| true),
        };
        let caller = name.clone();
        let command =
            tokio::task::spawn_blocking(move || exchange(&caller, "stop", "5min", |_| true));
        let (mut stream, _) = talker.accept().await;
        let chunk = [b'x'; 4096];
        // More than the cap, with no end: the command gives up at the cap.
        while stream.write_all(&chunk).await.is_ok() {}
        let told = format!("{:#}", command.await.unwrap().unwrap_err());
        assert!(told.contains("too long"), "{told}");
    }
}
