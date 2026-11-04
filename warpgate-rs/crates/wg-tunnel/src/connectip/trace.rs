//! Env-gated diagnostics for the CONNECT-IP data plane.
//!
//! Set `WG_STACK_TRACE=1` to print a per-poll / per-packet trace to stderr.
//! The counters are always accumulated (relaxed atomics, effectively free), so
//! a caller can print a summary at the end even with line tracing off.
//!
//! This exists because "tunnel is up but zero bytes arrive" has three very
//! different causes that look identical from the SOCKS5 socket:
//!
//! * the SYN never leaves the stack (`pump_once` never runs),
//! * the SYN leaves but the handshake never completes (`connect` returned too
//!   early / the reply never arrives), or
//! * the handshake completes but the relay stops polling (`poll_read` EOF).
//!
//! Counting at each layer (`send_slice`/`recv_slice`, queue depths, tunnel
//! `send_packet`/`recv_packet`) tells those apart in one run.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();

/// Whether line tracing is on (`WG_STACK_TRACE=1`).
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| match std::env::var("WG_STACK_TRACE").as_deref() {
        Ok("1") | Ok("true") | Ok("yes") => true,
        _ => false,
    })
}

/// Print one trace line (immediate flush, so a hang still shows the tail).
pub fn emit(args: std::fmt::Arguments<'_>) {
    if enabled() {
        eprintln!("[stack] {args}");
    }
}

// ---------------------------------------------------------------- counters

/// `pump_once` invocations.
pub static PUMPS: AtomicU64 = AtomicU64::new(0);
/// Packets handed to `Tunnel::send_packet`.
pub static TX_PACKETS: AtomicU64 = AtomicU64::new(0);
pub static TX_BYTES: AtomicU64 = AtomicU64::new(0);
/// Packets returned by `Tunnel::recv_packet`.
pub static RX_PACKETS: AtomicU64 = AtomicU64::new(0);
pub static RX_BYTES: AtomicU64 = AtomicU64::new(0);
/// Inbound packets dropped because the stack backlog was full.
pub static RX_DROPPED: AtomicU64 = AtomicU64::new(0);
/// `TcpSocket::send_slice` / `recv_slice` calls and byte counts.
pub static SEND_CALLS: AtomicU64 = AtomicU64::new(0);
pub static SEND_BYTES: AtomicU64 = AtomicU64::new(0);
pub static RECV_CALLS: AtomicU64 = AtomicU64::new(0);
pub static RECV_BYTES: AtomicU64 = AtomicU64::new(0);
/// `poll_read` calls that returned EOF.
pub static READ_EOFS: AtomicU64 = AtomicU64::new(0);
/// `poll_read` calls that returned Pending because the socket is not usable yet.
pub static READ_WAITS: AtomicU64 = AtomicU64::new(0);
pub static WRITE_WAITS: AtomicU64 = AtomicU64::new(0);

pub fn bump(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Ordering::Relaxed);
}

pub fn note_tx(len: usize) {
    bump(&TX_PACKETS, 1);
    bump(&TX_BYTES, len as u64);
}
pub fn note_rx(len: usize) {
    bump(&RX_PACKETS, 1);
    bump(&RX_BYTES, len as u64);
}
pub fn note_send(len: usize) {
    bump(&SEND_CALLS, 1);
    bump(&SEND_BYTES, len as u64);
}
pub fn note_recv(len: usize) {
    bump(&RECV_CALLS, 1);
    bump(&RECV_BYTES, len as u64);
}

fn g(c: &AtomicU64) -> u64 {
    c.load(Ordering::Relaxed)
}

/// One-line counter summary, for the repro binary.
pub fn summary() -> String {
    format!(
        "pumps={} tx_pkts={} tx_bytes={} rx_pkts={} rx_bytes={} rx_dropped={} \
         send_calls={} send_bytes={} recv_calls={} recv_bytes={} read_eofs={} read_waits={} write_waits={}",
        g(&PUMPS),
        g(&TX_PACKETS),
        g(&TX_BYTES),
        g(&RX_PACKETS),
        g(&RX_BYTES),
        g(&RX_DROPPED),
        g(&SEND_CALLS),
        g(&SEND_BYTES),
        g(&RECV_CALLS),
        g(&RECV_BYTES),
        g(&READ_EOFS),
        g(&READ_WAITS),
        g(&WRITE_WAITS),
    )
}
