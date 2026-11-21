//! One physical browser connection (a websocket, or the pipe the fork hands the daemon)
//! carrying many virtual CDP connections.
//!
//! Each virtual connection is a browser session of its own (`Target.attachToBrowserTarget`).
//! A message a virtual connection sends gets an id unique on the physical connection and, when
//! it names no session, its browser session. Every message the browser sends goes to the
//! virtual connection that owns its session, and to no other: a message whose owner is not
//! known goes nowhere. To its user a virtual connection looks like a connection of its own to
//! the browser: its browser session's id is taken off what it receives, as a top-level
//! connection sees no session id there.

#[cfg(test)]
pub(crate) mod fake;
mod mux;
mod transport;

use std::os::fd::OwnedFd;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::cdp::QUEUED_MAX_MESSAGE;
use mux::{Handle, Inner};

/// A physical connection to the browser. Cheap to clone; all clones share it.
#[derive(Clone)]
pub struct BrowserLink {
    inner: Arc<Inner>,
}

impl BrowserLink {
    /// Connects to the browser's DevTools websocket. A message over 256 MiB ends only the
    /// virtual connection it belongs to, never the link.
    pub async fn websocket(url: &str) -> Result<BrowserLink> {
        Ok(Self::start(
            transport::websocket(url, QUEUED_MAX_MESSAGE).await?,
        ))
    }

    /// The daemon's end of a DevTools pipe: reads `read`, writes `write` (fork mode: fds 3
    /// and 4). Messages are JSON, each ended by a NUL; one over 256 MiB is skipped and ends
    /// only the virtual connection it belongs to. Needs a tokio runtime.
    pub fn pipe(read: OwnedFd, write: OwnedFd) -> Result<BrowserLink> {
        Ok(Self::start(transport::pipe(
            read,
            write,
            QUEUED_MAX_MESSAGE,
        )?))
    }

    fn start(transport: transport::Transport) -> BrowserLink {
        BrowserLink {
            inner: Inner::start(transport),
        }
    }

    /// A new virtual connection: a new browser session (`Target.attachToBrowserTarget`).
    pub async fn open(&self) -> Result<VirtualConn> {
        let (handle, rx) = self.inner.open().await?;
        Ok(VirtualConn {
            sender: VirtualSender {
                shared: Arc::new(handle),
            },
            receiver: VirtualReceiver { rx },
        })
    }

    /// How many virtual connections are open now.
    pub fn virtual_connections(&self) -> usize {
        self.inner.virtual_connections()
    }

    /// A link whose physical connection has already ended (for tests of its users).
    #[cfg(test)]
    pub(crate) fn ended_for_test() -> BrowserLink {
        let (_, incoming) = mpsc::unbounded_channel();
        let (outgoing, _) = mpsc::unbounded_channel();
        Self::start(transport::Transport { incoming, outgoing })
    }

    /// Resolves once the physical connection has ended.
    pub async fn closed(&self) {
        self.inner.closed().await
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

/// A virtual connection. It closes when every `VirtualSender` of it is dropped: its browser
/// session and every session attached through it are detached.
pub struct VirtualConn {
    sender: VirtualSender,
    receiver: VirtualReceiver,
}

impl VirtualConn {
    pub fn split(self) -> (VirtualSender, VirtualReceiver) {
        (self.sender, self.receiver)
    }
}

/// Sends CDP messages on a virtual connection. Cheap to clone.
#[derive(Clone)]
pub struct VirtualSender {
    shared: Arc<Handle>,
}

impl VirtualSender {
    /// Sends one CDP message (a JSON object with an `id`). A message naming a session this
    /// virtual connection does not own is not sent; its error reply comes back on the
    /// receiver instead. Fails once the virtual or the physical connection has ended.
    pub fn send(&self, text: String) -> Result<()> {
        self.shared.send(text)
    }
}

/// Receives the messages of a virtual connection. Dropping it while senders live closes the
/// virtual connection when the next message for it arrives.
pub struct VirtualReceiver {
    rx: mpsc::UnboundedReceiver<String>,
}

impl VirtualReceiver {
    /// The next message; `None` once the virtual or the physical connection has ended.
    pub async fn recv(&mut self) -> Option<String> {
        self.rx.recv().await
    }
}
