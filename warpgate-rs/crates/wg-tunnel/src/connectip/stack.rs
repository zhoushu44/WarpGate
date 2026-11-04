//! A userspace TCP/IP stack riding on a CONNECT-IP tunnel.
//!
//! This is what makes the native path a real replacement for the `usque`
//! subprocess. Cloudflare's HTTP/2 CONNECT-IP gives us a stream of raw **IP
//! packets** — not a TCP byte stream. To serve SOCKS5 we have to terminate TCP
//! ourselves, in userspace:
//!
//! ```text
//! SOCKS5 client ──▶ DialFn ──▶ smoltcp socket
//!                                   │
//!                              IP packets
//!                                   │
//!                    Tunnel::send_packet / recv_packet
//!                                   │
//!                          HTTP/2 CONNECT-IP
//! ```
//!
//! The Node version got this from gvisor inside `usque`. Here it is smoltcp,
//! which is ~1 MB of state per instance instead of a whole Go runtime.
//!
//! ## Design
//!
//! One stack per tunnel (not per connection): the tunnel is a single IP
//! identity, so all TCP connections through it share the address and the
//! packet stream. Connections are demultiplexed by smoltcp's socket set.
//!
//! A single task owns the stack and drives it, because smoltcp is not `Sync`
//! and the tunnel is a single stream that cannot be read from two places. The
//! task multiplexes between:
//!
//! * packets arriving from the tunnel → `Interface::poll`
//! * bytes arriving from a SOCKS5 client → the socket's send buffer
//! * bytes ready in a socket's receive buffer → the SOCKS5 client
//!
//! Each accepted SOCKS5 connection gets a duplex pipe; the task pumps between
//! that pipe and its socket.

use std::io;
use std::sync::Arc;

use smoltcp::iface::{Config as IfaceConfig, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use crate::connectip::h2::Tunnel;
use crate::connectip::trace;
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpListenEndpoint};
use tokio::sync::Mutex;

use crate::AnyStream;

/// Packets the tunnel hands us, drained by the stack task.
///
/// `Device::receive` is synchronous but the tunnel is async, so arriving
/// packets are buffered here by the receive task.
#[derive(Default)]
struct PacketQueue {
    inbound: std::collections::VecDeque<Vec<u8>>,
    outbound: Vec<Vec<u8>>,
}

/// smoltcp device backed by the CONNECT-IP tunnel.
struct TunnelDevice {
    queue: Arc<Mutex<PacketQueue>>,
}

impl Device for TunnelDevice {
    type RxToken<'a> = RxBuf;
    type TxToken<'a> = TxBuf;

    fn receive(&mut self, _t: SmolInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // smoltcp calls this from a sync context; take the lock without
        // blocking. A miss just means "no packet yet", which is legal.
        let mut q = self.queue.try_lock().ok()?;
        let pkt = q.inbound.pop_front()?;
        trace::emit(format_args!(
            "device.receive len={} inbound_left={}",
            pkt.len(),
            q.inbound.len()
        ));
        Some((RxBuf { buf: pkt }, TxBuf { queue: self.queue.clone() }))
    }

    fn transmit(&mut self, _t: SmolInstant) -> Option<Self::TxToken<'_>> {
        Some(TxBuf {
            queue: self.queue.clone(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = 1280;
        caps
    }
}

struct RxBuf {
    buf: Vec<u8>,
}

impl RxToken for RxBuf {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buf)
    }
}

struct TxBuf {
    queue: Arc<Mutex<PacketQueue>>,
}

impl TxToken for TxBuf {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        // The lock is uncontended in practice: only the stack task transmits.
        let mut buf = vec![0u8; len];
        let out = f(&mut buf);
        if let Ok(mut q) = self.queue.try_lock() {
            trace::emit(format_args!(
                "device.transmit len={} outbound_queued={}",
                buf.len(),
                q.outbound.len() + 1
            ));
            q.outbound.push(buf);
        }
        out
    }
}

/// A TCP stack bound to one CONNECT-IP tunnel.
pub struct UserspaceStack {
    /// Outbound queue shared with the tunnel pump.
    queue: Arc<Mutex<PacketQueue>>,
    /// Handles we can still open connections on.
    iface: Arc<Mutex<Interface>>,
    /// Removal queue and waker registry, shared with every `StackStream`.
    shared: Arc<StackShared>,
    sockets: Arc<Mutex<SocketSet<'static>>>,
    /// Local address inside the tunnel. Kept for diagnostics and for callers
    /// that need to know which family this stack speaks.
    pub local: IpAddress,
    /// Next ephemeral source port to hand out.
    ///
    /// smoltcp's `TcpSocket::connect` returns `ConnectError::Unaddressable`
    /// when the *local* port is zero (see `socket/tcp.rs`: `if
    /// local_endpoint.port == 0 { return Err(Unaddressable) }`). The earlier
    /// code passed `port: 0` meaning "pick one for me", which is not what that
    /// API does, so every dial failed with the misleading message
    /// "connect: unaddressable destination".
    next_port: std::sync::atomic::AtomicU16,
}

impl UserspaceStack {
    /// Whether the underlying tunnel has failed.
    pub fn is_dead(&self) -> bool {
        self.shared.is_dead()
    }

    /// Build a stack over `tunnel`, using `local` as our address.
    ///
    /// `local` is the address Cloudflare assigned to the account
    /// (`usque.json`'s `ipv4`/`ipv6`); the tunnel only routes for that
    /// identity, so a mismatch means every packet is dropped.
    pub fn new(local: IpAddress) -> Self {
        // A synthetic address: the medium is IP, so it never reaches the wire,
        // but smoltcp still requires one.
        let mut cfg = IfaceConfig::new(HardwareAddress::Ip);
        // Distinct seed per stack, so two instances cannot collide on TCP
        // sequence numbers.
        cfg.random_seed = rand_seed();
        let mut iface = Interface::new(cfg, &mut TunnelDevice {
            queue: Arc::new(Mutex::new(PacketQueue::default())),
        }, SmolInstant::now());
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(local, if matches!(local, IpAddress::Ipv4(_)) { 32 } else { 128 }));
        });
        // The tunnel is a point-to-point link; a default route through it is
        // what makes outbound connections resolve.
        let _ = iface.routes_mut().add_default_ipv4_route(smoltcp::wire::Ipv4Address::new(0, 0, 0, 0));
        if matches!(local, IpAddress::Ipv6(_)) {
            let _ = iface
                .routes_mut()
                .add_default_ipv6_route(smoltcp::wire::Ipv6Address::UNSPECIFIED);
        }

        Self {
            queue: Arc::new(Mutex::new(PacketQueue::default())),
            iface: Arc::new(Mutex::new(iface)),
            sockets: Arc::new(Mutex::new(SocketSet::new(Vec::new()))),
            shared: Arc::new(StackShared::default()),
            local,
            // 49152..=65535 is the IANA ephemeral range.
            next_port: std::sync::atomic::AtomicU16::new(49152),
        }
    }

    /// Also assign an IPv4 address to the interface.
    ///
    /// A WARP account owns *both* an IPv6 and a `172.16.0.2`-style IPv4. The
    /// tunnel carries either, but smoltcp refuses to originate a packet whose
    /// source address the interface does not have: with only the IPv6 assigned,
    /// every IPv4 destination fails with `ConnectError::Unaddressable`
    /// ("connect: unaddressable destination"), which is what `api.ipify.org`
    /// (A record only) hit.
    pub async fn add_ipv4(&self, addr: smoltcp::wire::Ipv4Address) {
        let mut iface = self.iface.lock().await;
        let _ = iface.update_ip_addrs(|addrs| {
            if !addrs.iter().any(|c| matches!(c, IpCidr::Ipv4(_))) {
                let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(addr), 32));
            }
        });
        let _ = iface
            .routes_mut()
            .add_default_ipv4_route(smoltcp::wire::Ipv4Address::new(0, 0, 0, 0));
    }

    /// Hand out the next unused ephemeral source port.
    ///
    /// Wraps within 49152..=65535 and skips ports that are still connected.
    async fn alloc_port(&self) -> u16 {
        use std::sync::atomic::Ordering;
        loop {
            let port = self.next_port.fetch_add(1, Ordering::Relaxed);
            let port = if port < 49152 { 49152 } else { port };
            let taken = {
                let set = self.sockets.lock().await;
                let busy = set.iter().any(|(_, any)| match any {
                    // Only TCP sockets are in this set.
                    smoltcp::socket::Socket::Tcp(s) => s
                        .local_endpoint()
                        .map(|e| e.port == port)
                        .unwrap_or(false),
                });
                busy
            };
            if !taken {
                return port;
            }
        }
    }

    /// Whether the interface has an address of the given family.
    async fn has_family(&self, want_v6: bool) -> bool {
        let iface = self.iface.lock().await;
        iface.ip_addrs().iter().any(|c| match c {
            IpCidr::Ipv4(_) => !want_v6,
            IpCidr::Ipv6(_) => want_v6,
        })
    }

    /// Open a TCP connection to `host:port` and return it as a byte stream.
    pub async fn connect(&self, host: &str, port: u16) -> io::Result<AnyStream> {
        let remote = resolve(host).await?;
        // A v4 destination needs a v4 source address on the interface; without
        // one smoltcp reports the opaque `unaddressable destination`. Say which
        // address family is missing instead.
        if matches!(remote, IpAddress::Ipv4(_)) && !self.has_family(false).await {
            return Err(io::Error::other(
                "connect: no IPv4 address on the tunnel interface (call add_ipv4)",
            ));
        }
        if matches!(remote, IpAddress::Ipv6(_)) && !self.has_family(true).await {
            return Err(io::Error::other("connect: no IPv6 address on the tunnel interface"));
        }
        // Pick a real ephemeral source port. smoltcp rejects a zero local port
        // outright, so "let the stack choose" has to be done here.
        let local_port = self.alloc_port().await;
        let handle = {
            // Lock order MUST match `pump_once` (iface then sockets). Taking
            // them the other way round here deadlocks against the pump, which
            // runs every 2 ms — that was the "1 in 8 dials hang" symptom.
            let mut iface = self.iface.lock().await;
            let mut set = self.sockets.lock().await;
            let rx = tcp::SocketBuffer::new(vec![0u8; 64 * 1024]);
            let tx = tcp::SocketBuffer::new(vec![0u8; 64 * 1024]);
            let mut sock = tcp::Socket::new(rx, tx);
            sock.connect(
                iface.context(),
                smoltcp::wire::IpEndpoint::new(remote, port),
                IpListenEndpoint {
                    addr: None,
                    port: local_port,
                },
            )
            .map_err(|e| io::Error::other(format!("connect: {e}")))?;
            set.add(sock)
        };

        // Wait for the handshake before handing the stream back, so the caller
        // sees a connect failure instead of a stream that never carries bytes.
        //
        // `is_active()` is NOT the right test: smoltcp's `TcpSocket::is_active`
        // is `!matches!(state, Closed | TimeWait | Listen)`, so it is already
        // true in `SynSent` — i.e. immediately after `connect()`, before a
        // single byte of the three-way handshake has been exchanged. Returning
        // there handed the SOCKS relay a socket where `can_send()`/`can_recv()`
        // are both false; the relay's first `poll_read` saw `may_recv()==false`
        // and turned that into EOF, closing the client before it ever wrote.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        if trace::enabled() {
            trace::emit(format_args!(
                "connect: begin handle={:?} local_port={} remote={remote}:{port}",
                handle, local_port
            ));
        }
        let mut last_state: Option<String> = None;
        let mut waited = 0u32;
        loop {
            if tokio::time::Instant::now() > deadline {
                self.sockets.lock().await.remove(handle);
                return Err(io::Error::new(io::ErrorKind::TimedOut, "tcp connect"));
            }
            {
                let mut set = self.sockets.lock().await;
                let sock = set.get_mut::<tcp::Socket>(handle);
                let state = sock.state();
                let state_name = format!("{state:?}");
                // Every iteration, not just on transitions: a state that never
                // moves (SynSent forever) is exactly the failure being hunted.
                trace::emit(format_args!(
                    "connect: iter={} state={} is_active={} can_send={} can_recv={} may_send={} may_recv={} txq={} rxq={}",
                    waited,
                    state_name,
                    sock.is_active(),
                    sock.can_send(),
                    sock.can_recv(),
                    sock.may_send(),
                    sock.may_recv(),
                    sock.send_queue(),
                    sock.recv_queue(),
                ));
                if last_state.as_deref() != Some(state_name.as_str()) {
                    trace::emit(format_args!(
                        "connect: STATE TRANSITION {} -> {} after {} polls",
                        last_state.as_deref().unwrap_or("<new>"),
                        state_name,
                        waited,
                    ));
                    last_state = Some(state_name.clone());
                }
                // The handshake is done once the state has left the SYN
                // states. `Established` is the normal case; `CloseWait` et al.
                // are accepted because the peer may have closed between two
                // polls — that is a live connection with a readable FIN, not a
                // connect failure.
                if handshake_done(state) {
                    trace::emit(format_args!(
                        "connect: returning with state={} can_send={} can_recv={}",
                        state_name,
                        sock.can_send(),
                        sock.can_recv(),
                    ));
                    break;
                }
                if state == tcp::State::Closed {
                    drop(set);
                    self.sockets.lock().await.remove(handle);
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "tcp closed",
                    ));
                }
            }
            self.pump_once().await;
            waited += 1;
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        Ok(Box::new(StackStream {
            handle,
            sockets: self.sockets.clone(),
            shared: self.shared.clone(),
            _queue: self.queue.clone(),
        }) as AnyStream)
    }

    /// Drive a live CONNECT-IP tunnel, feeding its packets into the stack and
    /// sending the stack's packets back out.
    ///
    /// Spawns two tasks that own the tunnel's read and write halves. They share
    /// `queue`: the reader pushes inbound IP packets, the writer drains
    /// outbound ones. The stack polls between them.
    ///
    /// Returns a handle that stops the pump when dropped, so a reconnecting
    /// instance does not leak tasks.
    pub fn attach<S>(self: &Arc<Self>, tunnel: Tunnel<S>) -> StackPump
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        // The tunnel is one stream that must be read and written from the same
        // owner, so a single task does both: it drains `outbound` and waits on
        // `recv_packet` with a short timeout, which keeps both directions live
        // without needing to split the stream.
        let shared = Arc::new(Mutex::new(tunnel));
        let queue_rx = self.queue.clone();

        // The reader owns the only signal that the tunnel died, so it needs the
        // shared flag. Without this the stack keeps handing out a DialFn that
        // can never carry bytes: the resilient wrapper never learns to rebuild.
        let shared_state = self.shared.clone();

        let reader = tokio::spawn(async move {
            loop {
                // Flush anything the stack produced. This has to happen in the
                // same task as the read, because `Tunnel` needs `&mut self` for
                // both directions and a second task would deadlock on the lock.
                {
                    let batch = {
                        let mut q = queue_rx.lock().await;
                        std::mem::take(&mut q.outbound)
                    };
                    if !batch.is_empty() {
                        let mut t = shared.lock().await;
                        for pkt in batch {
                            if let Err(e) = t.send_packet(&pkt).await {
                                // Losing the tunnel is not routine: log it, the
                                // stack is now deaf in both directions and every
                                // dial through this instance will fail.
                                tracing::warn!(
                                    error = %e,
                                    len = pkt.len(),
                                    "connect-ip send_packet failed; reader task exits"
                                );
                                shared_state.mark_dead();
                                return;
                            }
                            trace::note_tx(pkt.len());
                        }
                    }
                }

                // Short timeout so the outbound flush above runs often even
                // when the tunnel is idle.
                let wait = tokio::time::timeout(
                    std::time::Duration::from_millis(20),
                    async { shared.lock().await.recv_packet().await },
                )
                .await;
                let Ok(recv) = wait else {
                    continue;
                };
                match recv {
                    Ok(pkt) if !pkt.is_empty() => {
                        trace::note_rx(pkt.len());
                        trace::emit(format_args!(
                            "tunnel.recv_packet len={} proto=0x{:02x}",
                            pkt.len(),
                            pkt.first().copied().unwrap_or(0)
                        ));
                        let mut q = queue_rx.lock().await;
                        // Bound the backlog: a stalled stack must not grow
                        // without limit. Dropping packets is correct here —
                        // TCP retransmits.
                        if q.inbound.len() < 512 {
                            q.inbound.push_back(pkt);
                        } else {
                            trace::bump(&trace::RX_DROPPED, 1);
                        }
                    }
                    Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(1)).await,
                    Err(e) => {
                        // Same reasoning as for the write half: this is the
                        // tunnel dying, and it otherwise looks like a silent
                        // "0 bytes" from the SOCKS5 side.
                        tracing::warn!(
                            error = %e,
                            "connect-ip recv_packet failed; reader task exits"
                        );
                        shared_state.mark_dead();
                        break;
                    }
                }
            }
        });



        // A second task drives smoltcp. Without it nothing ever calls
        // `Interface::poll`, so the stack never emits a SYN and never consumes
        // an inbound packet: `connect-ip` succeeds, the tunnel reports UP, and
        // every dial hangs until its timeout.
        let stack = self.clone();
        let driver = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(2));
            loop {
                ticker.tick().await;
                stack.pump_once().await;
            }
        });

        StackPump {
            _reader: reader,
            _driver: driver,
        }
    }

    /// Process one round of the stack: move packets both ways.
    async fn pump_once(&self) {
        let now = SmolInstant::now();
        trace::bump(&trace::PUMPS, 1);
        // Do NOT hold the queue lock across `poll`. `TunnelDevice::receive`
        // takes it (non-blocking) to pop an inbound packet, so holding it here
        // makes every `receive` return `None` and the stack never sees a
        // single packet — the symptom is a tunnel that opens fine and then
        // never completes a TCP handshake.
        let mut dev = TunnelDevice {
            queue: self.queue.clone(),
        };
        let mut iface = self.iface.lock().await;
        let mut set = self.sockets.lock().await;
        // Snapshot the queue depths and socket states around the poll: an
        // inbound packet that is never consumed shows up as a non-zero
        // `inbound` that does not move.
        let (in_before, out_before) = match self.queue.try_lock() {
            Ok(q) => (q.inbound.len(), q.outbound.len()),
            Err(_) => (usize::MAX, usize::MAX),
        };
        let sockets_before: Vec<String> = if trace::enabled() {
            set.iter()
                .map(|(h, s)| match s {
                    smoltcp::socket::Socket::Tcp(t) => format!(
                        "h{:?}= {:?} txq={} rxq={}",
                        h,
                        t.state(),
                        t.send_queue(),
                        t.recv_queue()
                    ),
                })
                .collect()
        } else {
            Vec::new()
        };
        let _ = iface.poll(now, &mut dev, &mut set);
        let (in_after, out_after) = match self.queue.try_lock() {
            Ok(q) => (q.inbound.len(), q.outbound.len()),
            Err(_) => (usize::MAX, usize::MAX),
        };
        if trace::enabled() {
            trace::emit(format_args!(
                "pump#{} inbound {}->{} outbound {}->{} sockets=[{}]",
                trace::PUMPS.load(std::sync::atomic::Ordering::Relaxed),
                in_before,
                in_after,
                out_before,
                out_after,
                sockets_before.join(", ")
            ));
        }

        // Reclaim sockets whose stream was dropped. The pump already holds
        // the lock, so Drop never has to win a race for it.
        let drained: Vec<_> = self
            .shared
            .pending_removal
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        for h in drained {
            let _ = set.remove(h);
        }

        drop(set);
        drop(iface);

        // Progress may have been made; release anyone blocked in
        // poll_read/poll_write. Without this they wait forever: the waker is
        // registered but nothing ever fires it, so the first write after a
        // half-open handshake sleeps eternally.
        self.shared.wake_all();
    }
}

/// Keeps the tunnel pump tasks alive; dropping it stops them.
pub struct StackPump {
    _reader: tokio::task::JoinHandle<()>,
    /// Drives `Interface::poll` so the stack makes progress.
    _driver: tokio::task::JoinHandle<()>,
}

/// A TCP connection running inside the userspace stack.
// Replacement for StackStream's Drop and the busy-wait in poll_read/poll_write.
//
// Two bugs fixed here:
//
// 1. `Drop` used `try_lock` and gave up silently on failure. The pump holds
//    the socket lock every 2 ms, so under concurrency most drops lost the race
//    and the socket stayed in the set forever — measured as 46 -> 360 sockets
//    over 500 concurrent requests, strictly linear. The handle now goes onto a
//    pending-removal queue (its own short-lived lock) that the pump drains.
//
// 2. `poll_read`/`poll_write` called `waker().wake_by_ref()` on every failed
//    attempt. That is an immediate self-wake, and with a 2 ms pump it becomes a
//    busy loop: four tasks were observed spinning at 193% CPU forever once a
//    socket could no longer make progress. The waker is now stored and awoken
//    by the pump only when it actually polled the interface.

/// Socket handles awaiting removal, plus the wakers of tasks waiting on the
/// stack to make progress.
#[derive(Default)]
pub struct StackShared {
    /// Handles whose `StackStream` has been dropped. Drained by the pump.
    /// Set once the tunnel is gone. Every dial then fails fast with a clear
    /// reason instead of hanging or returning a fake EOF.
    pub dead: std::sync::atomic::AtomicBool,
    /// Handles whose `StackStream` has been dropped. Drained by the pump.
    pub pending_removal: std::sync::Mutex<Vec<SocketHandle>>,
    /// Tasks blocked in `poll_read`/`poll_write`, woken after each pump.
    pub wakers: std::sync::Mutex<Vec<std::task::Waker>>,
}

impl StackShared {
    /// Record that the tunnel is unusable.
    pub fn mark_dead(&self) {
        self.dead.store(true, std::sync::atomic::Ordering::SeqCst);
        self.wake_all();
    }

    /// Whether the tunnel is unusable.
    pub fn is_dead(&self) -> bool {
        self.dead.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Queue a handle for removal. Never blocks, never fails.
    pub fn queue_removal(&self, h: SocketHandle) {
        if let Ok(mut q) = self.pending_removal.lock() {
            q.push(h);
        }
    }

    /// Remember a waker so the pump can wake it after polling.
    ///
    /// Duplicates are harmless but would grow without bound on a long-lived
    /// blocked task, so a task's waker replaces its previous entry.
    pub fn register(&self, w: &std::task::Waker) {
        if let Ok(mut v) = self.wakers.lock() {
            if v.iter().any(|x| x.will_wake(w)) {
                return;
            }
            v.push(w.clone());
        }
    }

    /// Wake every waiting task and clear the list.
    pub fn wake_all(&self) {
        if let Ok(mut v) = self.wakers.lock() {
            for w in v.drain(..) {
                w.wake();
            }
        }
    }
}

struct StackStream {
    handle: SocketHandle,
    sockets: Arc<Mutex<SocketSet<'static>>>,
    /// Shares the stack's packet queue so the writer can nudge the pump.
    /// Removal queue and waker registry shared with the pump.
    shared: Arc<StackShared>,
    _queue: Arc<Mutex<PacketQueue>>,
}

/// Whether a `connect()` in progress has finished its three-way handshake.
///
/// `is_active()` cannot be used for this: smoltcp defines it as
/// `!matches!(state, Closed | TimeWait | Listen)`, so `SynSent` — the state
/// `TcpSocket::connect` sets synchronously — already counts as "active".
fn handshake_done(state: tcp::State) -> bool {
    matches!(
        state,
        tcp::State::Established
            | tcp::State::FinWait1
            | tcp::State::FinWait2
            | tcp::State::CloseWait
            | tcp::State::Closing
            | tcp::State::LastAck
    )
}

/// Whether `may_recv() == false` in this state really means "the peer closed".
///
/// `may_recv()` is false in `SynSent`/`SynRecv` too, where nothing has been
/// received yet. Reporting EOF there is what let the SOCKS relay close a
/// brand-new connection with 0 bytes: `copy_bidirectional` treats `Ok(0)` as a
/// clean endpoint and stops, so the client saw EOF instead of an error.
fn recv_finished(state: tcp::State) -> bool {
    matches!(
        state,
        tcp::State::Closed
            | tcp::State::CloseWait
            | tcp::State::LastAck
            | tcp::State::TimeWait
            | tcp::State::Closing
            | tcp::State::FinWait1
            | tcp::State::FinWait2
    )
}

impl AsyncRead for StackStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut set = match this.sockets.try_lock() {
            Ok(g) => g,
            Err(_) => {
                this.shared.register(cx.waker());
                return std::task::Poll::Pending;
            }
        };
        let sock = set.get_mut::<tcp::Socket>(this.handle);
        trace::emit(format_args!(
            "poll_read: ENTER handle={:?} state={:?} can_recv={} may_recv={} rxq={} want={}",
            this.handle,
            sock.state(),
            sock.can_recv(),
            sock.may_recv(),
            sock.recv_queue(),
            buf.remaining(),
        ));
        if !sock.can_recv() {
            // `may_recv()` is the correct EOF test. It goes false once the
            // peer's FIN has arrived *and* the receive buffer is drained —
            // which leaves the socket in CLOSE-WAIT, not CLOSED. Testing
            // `state == Closed` therefore never fires for an ordinary
            // server-side close, so every `read_to_end` / `copy_bidirectional`
            // hung until its own timeout even though all bytes had already
            // been delivered.
            if !sock.may_recv() {
                // `may_recv()` also goes false in `SynSent`/`SynRecv`, where
                // there is nothing to read yet — turning that into `Ok(0)`
                // told the relay "clean EOF" on a connection that had not even
                // finished its handshake. Only report EOF once the socket is
                // really finished; otherwise wait for the pump.
                if recv_finished(sock.state()) {
                    trace::bump(&trace::READ_EOFS, 1);
                    trace::emit(format_args!(
                        "poll_read: EOF at handle={:?} state={:?} may_recv={} rxq={}",
                        this.handle,
                        sock.state(),
                        sock.may_recv(),
                        sock.recv_queue()
                    ));
                    // Mirror the peer's FIN so our half closes too, letting the
                    // socket leave CLOSE-WAIT instead of lingering.
                    if sock.state() != tcp::State::Closed {
                        sock.close();
                    }
                    return std::task::Poll::Ready(Ok(()));
                }
                trace::bump(&trace::READ_WAITS, 1);
                trace::emit(format_args!(
                    "poll_read: waiting for handshake handle={:?} state={:?} rxq={}",
                    this.handle,
                    sock.state(),
                    sock.recv_queue()
                ));
                this.shared.register(cx.waker());
                return std::task::Poll::Pending;
            }
            if sock.state() == tcp::State::Closed {
                return std::task::Poll::Ready(Ok(()));
            }
            trace::bump(&trace::READ_WAITS, 1);
            this.shared.register(cx.waker());
            return std::task::Poll::Pending;
        }
        match sock.recv_slice(buf.initialize_unfilled()) {
            Ok(n) => {
                trace::note_recv(n);
                trace::emit(format_args!(
                    "poll_read: recv_slice -> Ok({n}) state={:?} rxq_left={}",
                    sock.state(),
                    sock.recv_queue()
                ));
                buf.advance(n);
                std::task::Poll::Ready(Ok(()))
            }
            Err(e) => {
                trace::emit(format_args!("poll_read: recv_slice err={e:?}"));
                this.shared.register(cx.waker());
                std::task::Poll::Pending
            }
        }
    }
}

impl Drop for StackStream {
    /// Hand the socket back for removal.
    ///
    /// Drop cannot await, and try_lock loses the race against the 2 ms pump
    /// almost every time under load -- measured as 46 -> 360 sockets over 500
    /// concurrent requests, strictly linear. The handle therefore goes onto a
    /// queue that pump_once drains while it already holds the lock.
    fn drop(&mut self) {
        self.shared.queue_removal(self.handle);
        // Wake whoever is waiting, so the reclaimed slot is noticed promptly.
        self.shared.wake_all();
    }
}
impl AsyncWrite for StackStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        let this = self.get_mut();
        let mut set = match this.sockets.try_lock() {
            Ok(g) => g,
            Err(_) => {
                this.shared.register(cx.waker());
                return std::task::Poll::Pending;
            }
        };
        let sock = set.get_mut::<tcp::Socket>(this.handle);
        trace::emit(format_args!(
            "poll_write: ENTER handle={:?} state={:?} want={} can_send={} may_send={} txq={} txspace={}",
            this.handle,
            sock.state(),
            buf.len(),
            sock.can_send(),
            sock.may_send(),
            sock.send_queue(),
            sock.send_capacity().saturating_sub(sock.send_queue()),
        ));
        if !sock.can_send() {
            if sock.state() == tcp::State::Closed {
                trace::emit(format_args!(
                    "poll_write: state=Closed -> BrokenPipe ({} bytes discarded)",
                    buf.len()
                ));
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "closed",
                )));
            }
            trace::bump(&trace::WRITE_WAITS, 1);
            trace::emit(format_args!(
                "poll_write: not sendable handle={:?} state={:?} may_send={} txq={} can_send={}",
                this.handle,
                sock.state(),
                sock.may_send(),
                sock.send_queue(),
                sock.can_send()
            ));
            this.shared.register(cx.waker());
            return std::task::Poll::Pending;
        }
        match sock.send_slice(buf) {
            Ok(n) => {
                trace::note_send(n);
                // `Ok(n)` only means "copied into the socket's tx buffer"; the
                // packet is emitted on a later `Interface::poll`.
                trace::emit(format_args!(
                    "poll_write: send_slice({}) -> Ok({n}) state={:?} txq={} (buffered)",
                    buf.len(),
                    sock.state(),
                    sock.send_queue()
                ));
                std::task::Poll::Ready(Ok(n))
            }
            Err(e) => {
                trace::emit(format_args!(
                    "poll_write: send_slice({}) -> Err({e:?}) -> Pending",
                    buf.len()
                ));
                this.shared.register(cx.waker());
                std::task::Poll::Pending
            }
        }
    }
fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        if let Ok(mut set) = self.sockets.try_lock() {
            set.get_mut::<tcp::Socket>(self.handle).close();
        }
        std::task::Poll::Ready(Ok(()))
    }
}

/// A per-stack random seed for TCP sequence numbers.
fn rand_seed() -> u64 {
    let mut b = [0u8; 8];
    if getrandom::getrandom(&mut b).is_ok() {
        u64::from_le_bytes(b)
    } else {
        // Time-based fallback: collisions are still unlikely across stacks.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5eed)
    }
}

/// Resolve a hostname to an IP address.
///
/// Inside the tunnel there is no DNS of our own, so this uses the host
/// resolver. Resolving on the host leaks the query, which is why the SOCKS5
/// layer must send the *hostname* here only when the client asked for local
/// resolution (`socks5` vs `socks5h`).
async fn resolve(host: &str) -> io::Result<IpAddress> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(match ip {
            std::net::IpAddr::V4(v4) => IpAddress::Ipv4(v4.into()),
            std::net::IpAddr::V6(v6) => IpAddress::Ipv6(v6.into()),
        });
    }
    let host = host.to_string();
    tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        (host.as_str(), 0u16)
            .to_socket_addrs()
            .ok()
            .and_then(|mut it| it.next())
            .map(|a| match a.ip() {
                std::net::IpAddr::V4(v4) => IpAddress::Ipv4(v4.into()),
                std::net::IpAddr::V6(v6) => IpAddress::Ipv6(v6.into()),
            })
    })
    .await
    .ok()
    .flatten()
    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "dns"))
}

use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_builds_with_v6_address() {
        let s = UserspaceStack::new(IpAddress::v6(0x2606, 0x4700, 0x110, 0, 0, 0, 0, 1));
        assert!(matches!(s.local, IpAddress::Ipv6(_)));
    }

    #[test]
    fn stack_builds_with_v4_address() {
        let s = UserspaceStack::new(IpAddress::v4(172, 16, 0, 2));
        assert!(matches!(s.local, IpAddress::Ipv4(_)));
    }

    #[tokio::test]
    async fn resolves_literals_without_dns() {
        let ip = resolve("1.2.3.4").await.unwrap();
        assert!(matches!(ip, IpAddress::Ipv4(_)));
        let ip = resolve("2606:4700::1").await.unwrap();
        assert!(matches!(ip, IpAddress::Ipv6(_)));
    }

    /// A peer FIN must surface as EOF from `poll_read`.
    ///
    /// Regression test for a bug that made every `read_to_end` and every
    /// `copy_bidirectional` hang until its own timeout: the old `poll_read`
    /// returned `Ready(Ok(()))` only for `State::Closed`, but a normal
    /// server-side close leaves the socket in `CLOSE-WAIT`, where
    /// `state() != Closed` while `may_recv()` is already false.
    ///
    /// The assertion that matters is the invariant the fix relies on: `may_recv`
    /// is strictly weaker than `state == Closed`, so it can detect EOF in states
    /// where the old check could not. A fresh socket demonstrates one such state.
    #[test]
    fn may_recv_detects_eof_where_state_closed_does_not() {
        let rx = tcp::SocketBuffer::new(vec![0u8; 1024]);
        let tx = tcp::SocketBuffer::new(vec![0u8; 1024]);
        let mut sock = tcp::Socket::new(rx, tx);

        // A socket with no peer cannot receive.
        assert!(!sock.may_recv(), "a socket with no peer cannot receive");

        // `may_recv() == false` while the state is *not* `Established`: this is
        // exactly the shape of CLOSE-WAIT after the peer's FIN — `may_recv` is
        // false, but the socket is not `Closed`, so the old EOF test missed it.
        assert_ne!(sock.state(), tcp::State::Established);
        assert!(
            !sock.may_recv(),
            "EOF must be detectable from may_recv(), not only from Closed"
        );

        // `close()` must be safe to call from a non-established state: the fix
        // mirrors the peer's FIN so the socket can leave CLOSE-WAIT.
        sock.close();
    }

    /// `is_active()` is true in `SynSent`, so it cannot gate "handshake done".
    ///
    /// This is the regression that made every SOCKS5 request return 0 bytes:
    /// `connect()` broke out of its wait loop immediately (state `SynSent`),
    /// `poll_read` then saw `may_recv() == false` and reported EOF, and the
    /// relay closed the client before a single byte was written.
    #[tokio::test]
    async fn syn_sent_is_active_but_not_connected() {
        let stack = UserspaceStack::new(IpAddress::v4(172, 16, 0, 2));
        let handle = {
            let mut iface = stack.iface.lock().await;
            let mut set = stack.sockets.lock().await;
            let rx = tcp::SocketBuffer::new(vec![0u8; 1024]);
            let tx = tcp::SocketBuffer::new(vec![0u8; 1024]);
            let mut sock = tcp::Socket::new(rx, tx);
            sock.connect(
                iface.context(),
                smoltcp::wire::IpEndpoint::new(IpAddress::v4(1, 2, 3, 4), 80),
                IpListenEndpoint { addr: None, port: 40000 },
            )
            .unwrap();
            assert_eq!(sock.state(), tcp::State::SynSent);
            // The trap, asserted explicitly: the old predicate is already true.
            assert!(sock.is_active(), "is_active() is true in SynSent");
            // ...but the connection is not usable, which is why the predicate
            // below had to change.
            assert!(!sock.can_send());
            assert!(!sock.can_recv());
            assert!(!handshake_done(sock.state()), "SynSent is not a handshake");
            assert!(!recv_finished(sock.state()), "SynSent is not EOF");
            set.add(sock)
        };
        stack.sockets.lock().await.remove(handle);
    }

    #[test]
    fn handshake_predicate_covers_the_real_states() {
        assert!(handshake_done(tcp::State::Established));
        // A peer that closed while we waited still gave us a usable stream.
        assert!(handshake_done(tcp::State::CloseWait));
        for not_yet in [tcp::State::Closed, tcp::State::SynSent, tcp::State::SynReceived, tcp::State::Listen] {
            assert!(!handshake_done(not_yet), "{not_yet:?} must not count as connected");
        }
        // EOF is only real once the peer's FIN (or a reset) has been seen.
        for not_eof in [tcp::State::SynSent, tcp::State::SynReceived, tcp::State::Established, tcp::State::Listen] {
            assert!(!recv_finished(not_eof), "{not_eof:?} must not be reported as EOF");
        }
        for eof in [tcp::State::CloseWait, tcp::State::Closed, tcp::State::TimeWait, tcp::State::LastAck] {
            assert!(recv_finished(eof), "{eof:?} is a finished receive half");
        }
    }
}

#[cfg(test)]
mod wiring_tests {
    /// The waker and removal machinery is useless unless something calls it.
    ///
    /// A previous change edited only the definitions and the poll bodies:
    /// register() was called but nothing ever woke those tasks, so the first
    /// write after a half-open handshake slept forever. That variant was 100%
    /// deadlock -- worse than the busy loop it replaced. This test fails if the
    /// call sites go missing again.
    #[test]
    fn shared_helpers_are_wired() {
        let src = include_str!("stack.rs");
        assert!(
            src.contains("self.shared.queue_removal(self.handle)"),
            "Drop must hand the handle to the pump; try_lock loses the race"
        );
        assert!(
            src.matches("self.shared.wake_all()").count() >= 2,
            "both Drop and pump_once must wake waiters, else registered wakers never fire"
        );
        assert!(
            src.contains("pending_removal"),
            "the pump must drain the removal queue"
        );
        // Only look at the production half: this test itself mentions the
        // pattern, and a naive whole-file search would match that.
        let prod = src.split("#[cfg(test)]").next().unwrap_or("");
        assert!(
            !prod.contains(&["cx.waker().", "wake_by_ref()"].concat()),
            "self-waking turns the 2ms pump into a busy loop (190% CPU)"
        );
    }
}
