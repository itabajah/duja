//! The kernel uevent listener: a `NETLINK_KOBJECT_UEVENT` socket, a `poll`, and
//! a self-pipe to end it.
//!
//! # Why netlink directly and not libudev
//!
//! `libudev` is the usual way to watch for device changes, and it is a C library
//! with a system dependency, a build-time `pkg-config` probe, and a
//! `-sys` crate. It exists to *interpret* uevents — matching rules, resolving
//! parents, applying udev's database — and Duja needs none of that. It needs to
//! know that a `drm` connector changed. That message is broadcast on a netlink
//! multicast group any unprivileged process may join, and reading it costs one
//! socket. ADR-0022 records the decision and reverses the crate docs' older
//! plan of a "udev `drm` monitor".
//!
//! # No `unsafe`
//!
//! Every syscall here goes through `rustix`'s safe wrappers, including the
//! netlink address type. That matters because `duja-platform` confines `unsafe`
//! to its Windows and macOS `sys` modules, and a hand-rolled `sockaddr_nl`
//! through `libc` would have put four unsafe blocks in the Linux backend for no
//! capability the safe path lacks.

use std::os::fd::{AsFd, OwnedFd};

use crossbeam_channel::Sender;
use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use rustix::net::netlink::{KOBJECT_UEVENT, SocketAddrNetlink};
use rustix::net::{AddressFamily, RecvFlags, SocketFlags, SocketType, bind, recv, socket_with};
use rustix::pipe::{PipeFlags, pipe_with};

use crate::PlatformEvent;
use crate::linux_events::classify_uevent;

/// The multicast group the **kernel** broadcasts uevents on.
///
/// Group 1 is the kernel's own; udev re-broadcasts its processed events on
/// group 2, which Duja deliberately does not join — that one only exists if
/// udev is running, and its messages carry udev's own framing.
const KERNEL_GROUP: u32 = 1;

/// Let the kernel assign this socket's port id rather than claiming one.
///
/// A netlink socket bound to an explicit non-zero pid collides with any other
/// socket in the system that picked the same number, and the conventional
/// choice — the process id — collides as soon as a process opens two. Zero means
/// "assign me one", which cannot collide.
const AUTO_PORT_ID: u32 = 0;

/// Receive buffer for one uevent datagram.
///
/// The kernel caps a uevent at `UEVENT_BUFFER_SIZE` (2048 bytes) plus its
/// environment, and `libudev` uses 8 KiB for the same job. A datagram longer
/// than this is truncated rather than split, which for Duja means a possible
/// missed `SUBSYSTEM=` line and therefore a missed re-enumeration — not a
/// corrupt read.
const BUFFER_LEN: usize = 8192;

/// A running uevent listener.
///
/// Holds the write end of the self-pipe; dropping or explicitly stopping it
/// wakes the listener's `poll` and ends the thread.
pub(crate) struct UeventListener {
    stop: Option<OwnedFd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl UeventListener {
    /// Open the socket and start the listener thread.
    ///
    /// # Errors
    ///
    /// The `rustix` error from `socket`, `bind` or `pipe`. `EPERM` on the bind is
    /// the one worth recognising: it means this kernel does **not** allow an
    /// unprivileged process onto the multicast group, which ADR-0022 records as
    /// an assumption rather than an observation.
    pub(crate) fn spawn(tx: Sender<PlatformEvent>) -> Result<Self, Errno> {
        let socket = socket_with(
            AddressFamily::NETLINK,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC,
            Some(KOBJECT_UEVENT),
        )?;
        bind(&socket, &SocketAddrNetlink::new(AUTO_PORT_ID, KERNEL_GROUP))?;
        let (stop_read, stop_write) = pipe_with(PipeFlags::CLOEXEC)?;

        let thread = std::thread::Builder::new()
            .name("duja-uevent".to_owned())
            .spawn(move || listen(&socket, &stop_read, &tx))
            .map_err(|_| Errno::NOMEM)?;

        Ok(UeventListener {
            stop: Some(stop_write),
            thread: Some(thread),
        })
    }

    /// Wake the listener and join its thread. Idempotent.
    pub(crate) fn shutdown(&mut self) {
        // Closing the write end makes the read end poll-ready at EOF, which is
        // what the loop below watches for. Nothing is ever written to this pipe;
        // the close *is* the message, so there is no partial-write case.
        self.stop = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for UeventListener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The listener loop: wait on the socket and the stop pipe, forward what
/// matters, exit when the pipe closes or the receiver goes away.
fn listen(socket: &OwnedFd, stop: &OwnedFd, tx: &Sender<PlatformEvent>) {
    let mut buffer = [0u8; BUFFER_LEN];
    // Borrowed once, outside the loop: `PollFd::new` takes a reference, so
    // building these inline would borrow a temporary that dies before `poll`
    // sees it.
    let socket_fd = socket.as_fd();
    let stop_fd = stop.as_fd();
    loop {
        let mut fds = [
            PollFd::new(&socket_fd, PollFlags::IN),
            PollFd::new(&stop_fd, PollFlags::IN),
        ];
        // No timeout: this thread has nothing to do on a tick, and the stop pipe
        // is what ends it. A timeout would only add wakeups to a process whose
        // idle-wakeup budget is a stated product property.
        match poll(&mut fds, None) {
            Ok(_) => {}
            // A signal interrupting the wait is not an error; every other one is
            // unrecoverable for this socket, so the thread ends rather than
            // spinning on it.
            Err(Errno::INTR) => continue,
            Err(_) => return,
        }
        let (socket_ready, stop_ready) = match (fds.first(), fds.get(1)) {
            (Some(socket_fd), Some(stop_fd)) => (socket_fd.revents(), stop_fd.revents()),
            _ => return,
        };
        // The stop pipe is checked first and unconditionally: at shutdown the
        // socket may also be ready, and draining it would delay the join for as
        // long as the kernel keeps producing events.
        if !(stop_ready & (PollFlags::IN | PollFlags::HUP | PollFlags::ERR)).is_empty() {
            return;
        }
        // `POLLERR` is reported in `revents` whether or not it was requested, and
        // a netlink socket raises it whenever `sk_err` is set — which stays set
        // until a `recv` consumes it. Testing `IN` alone would `continue` past a
        // socket in the error state with an empty queue, `poll` would return
        // immediately again, and the thread would spin a core in a process whose
        // idle-wakeup budget is a stated product property. So the error bits are
        // read as readiness: the `recv` below is what clears them.
        if (socket_ready & (PollFlags::IN | PollFlags::ERR | PollFlags::HUP)).is_empty() {
            continue;
        }
        let received = match recv(socket, &mut buffer[..], RecvFlags::empty()) {
            // `Buffer::Output` for a slice is the length rustix has already
            // clamped to the buffer; the raw syscall count beside it can exceed
            // it under `MSG_TRUNC`, which is not requested here but is not worth
            // depending on.
            Ok((len, _)) => len,
            Err(Errno::INTR | Errno::AGAIN) => continue,
            // The kernel dropped broadcasts because this socket's receive buffer
            // was full — `netlink_overrun` sets `sk_err = ENOBUFS`. Recoverable
            // and *expected*: `SO_RCVBUF` is the default ~208 KiB (libudev raises
            // its own to 128 MiB via `SO_RCVBUFFORCE` for this reason), and
            // plugging a dock produces exactly the burst that overflows it. So
            // the one event Duja cares about is the one most likely to be lost,
            // and the answer is to assume it was: re-enumerating on a burst that
            // changed nothing costs one debounced pass, where treating this as
            // fatal would kill hot-plug for the rest of the session with no log
            // and no supervision (a clean `return` is not a panic).
            Err(Errno::NOBUFS) => {
                if tx.send(PlatformEvent::DisplaysChanged).is_err() {
                    return;
                }
                continue;
            }
            Err(_) => return,
        };
        let Some(datagram) = buffer.get(..received) else {
            continue;
        };
        if let Some(event) = classify_uevent(datagram) {
            // A closed receiver means the consumer is gone; there is nobody left
            // to tell, so stop rather than burn a thread forwarding into a void.
            if tx.send(event).is_err() {
                return;
            }
        }
    }
}
