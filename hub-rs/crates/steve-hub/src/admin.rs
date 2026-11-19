//! Same-user management over a private Unix socket, independent of public HTTP policy.
pub(crate) mod client;
mod paths;
mod routes;

pub use paths::socket_path;
use std::{io, path::Path, time::Duration};
use tokio::net::{UnixListener, UnixStream};

pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// Bound synchronously during installation, then polled as an ordinary Hub job.
pub(crate) struct Server {
    listener: std::os::unix::net::UnixListener,
    cleanup: paths::Cleanup,
    store: steve_store::Store,
}
pub(crate) fn bind(home: &Path, store: steve_store::Store) -> anyhow::Result<Server> {
    let (listener, cleanup) = paths::bind(home)?;
    Ok(Server {
        listener,
        cleanup,
        store,
    })
}
impl Server {
    pub(crate) async fn run(self) -> anyhow::Result<()> {
        let Self {
            listener,
            cleanup,
            store,
        } = self;
        let _cleanup = cleanup;
        let listener = UnixListener::from_std(listener)?;
        axum::serve(PeerListener(listener), routes::router(store, DEADLINE)).await?;
        Ok(())
    }
}
fn uid_allowed(peer: u32, expected: u32) -> bool {
    peer == expected
}
pub(crate) fn euid() -> u32 {
    // SAFETY: geteuid has no pointers or side effects.
    unsafe { libc::geteuid() }
}
struct PeerListener(UnixListener);
impl axum::serve::Listener for PeerListener {
    type Io = UnixStream;
    type Addr = tokio::net::unix::SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = axum::serve::Listener::accept(&mut self.0).await;
            if stream
                .peer_cred()
                .is_ok_and(|cred| uid_allowed(cred.uid(), euid()))
            {
                return (stream, addr);
            }
            tracing::warn!("management socket rejected a peer with unknown or different uid");
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_the_effective_uid_is_accepted_including_when_effective_uid_is_root() {
        assert!(super::uid_allowed(501, 501));
        assert!(!super::uid_allowed(502, 501));
        assert!(!super::uid_allowed(0, 501));
        assert!(super::uid_allowed(0, 0));
        assert!(!super::uid_allowed(501, 0));
    }
}
