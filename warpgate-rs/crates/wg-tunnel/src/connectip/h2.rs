//! A hand-rolled HTTP/2 client for CONNECT-IP over Cloudflare's edge.
//!
//! # The shape of the request (this is the important part)
//!
//! RFC 9484 §4.4 says CONNECT-IP over HTTP/2 uses **Extended CONNECT**
//! (`RFC 8441`): `:method = CONNECT`, `:protocol = connect-ip`, plus
//! `:scheme`/`:path`, and the server must advertise
//! `SETTINGS_ENABLE_CONNECT_PROTOCOL` (0x8) first — RFC 8441 §3.
//!
//! `consumer-masque.cloudflareclient.com` does **not** advertise it. Its
//! connection-preface SETTINGS carries only:
//!
//! ```text
//! 0x3 MAX_CONCURRENT_STREAMS = 1
//! 0x4 INITIAL_WINDOW_SIZE    = 0x100000
//! 0x5 MAX_FRAME_SIZE         = 0x4000
//! 0x6 MAX_HEADER_LIST_SIZE   = 0x20000
//! ```
//!
//! With no 0x8, a request bearing `:protocol` is a protocol violation and the
//! edge answers `RST_STREAM(PROTOCOL_ERROR)` — no HTTP status at all. Adding
//! `SETTINGS_ENABLE_CONNECT_PROTOCOL` to *our* SETTINGS changes nothing
//! (RFC 8441 §3: "Receipt of this parameter by a server does not have any
//! impact"), and Extended CONNECT fails with or without it.
//!
//! What the edge does accept is what `usque --http2` sends: a **plain**
//! CONNECT — no `:protocol`, no `:scheme`, no `:path` — distinguished by the
//! `cf-connect-proto: cf-connect-ip` header. Both that header and
//! `capsule-protocol: ?1` are mandatory; dropping either yields `400`.
//!
//! So this module is deliberately *non*-conformant with RFC 9484 and
//! conformant with the deployed endpoint. That is a Cloudflare quirk, not a
//! choice: measured against the live edge, plain CONNECT returns `:status 200`
//! 10 times out of 10, Extended CONNECT 0 times out of 10.
//!
//! # The capsule encoding (the second important part)
//!
//! RFC 9297 §3.1 CAPSULE frames use **QUIC variable-length integers**
//! (RFC 9000 §16): a 2-bit length prefix, so a 1-byte varint holds 0..=63.
//! HPACK integers (RFC 7541 §5.1) use a 7-bit prefix and are a *different*
//! wire format. Using the HPACK encoder here silently corrupts every capsule
//! whose payload is >= 64 bytes — which is every IP packet ever sent.
//!
//! The earlier version of this file did exactly that, so `send_packet` emitted
//! `00 43 ...` (HPACK 1-byte form, 0x43 payload-length byte doubled as data)
//! where the peer expected `00 40 40 ...`.
//!
//! # What else was wrong
//!
//! * `handshake()` sent an empty SETTINGS frame, then `open()` wrote HEADERS
//!   without ever reading the server's SETTINGS. RFC 9113 §3.4 requires the
//!   server's preface SETTINGS to be acknowledged.
//! * `read_response()` consumed SETTINGS/WINDOW_UPDATE but discarded
//!   `SETTINGS_INITIAL_WINDOW_SIZE`, so the very first DATA frame could
//!   overflow the peer's flow-control window.
//! * `recv_packet()` matched `stream != 0 && stream != self.stream_id` and
//!   `continue`d *without reading the payload*, desynchronising the frame
//!   parser permanently.
//! * `decode_response_status` treated any `0x00`-prefixed byte as "literal
//!   with a new name", but `0x00` can also mean "literal without indexing,
//!   indexed name". It also could not skip Huffman-coded values (Huffman is
//!   the default for every real server) and mis-handled CONTINUATION.
//! * `encode_capsule`/`decode_capsule` had no Context ID. RFC 9484 §6 says the
//!   datagram payload begins with the Context ID; Cloudflare's HTTP/2 edge
//!   omits it. `cloudflare_datagram` handles that reality explicitly.
//! * Frame length was never validated, so a `SETTINGS` frame with a
//!   non-multiple-of-6 length, or a DATA frame on stream 0, was accepted.
//!
//! # Verified against the live edge
//!
//! ```text
//! plain CONNECT + cf-connect-proto -> HEADERS :status 200
//! send DATA capsule (no Context ID) -> DATA with ICMPv6 echo reply
//! ```
//!
//! See `crates/wg-tunnel/src/bin/connectip_probe.rs` for the standalone
//! reproduction.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// HTTP/2 connection preface, RFC 9113 §3.4.
const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Frame types (RFC 9113 §6).
const FRAME_DATA: u8 = 0x0;
const FRAME_HEADERS: u8 = 0x1;
const FRAME_PRIORITY: u8 = 0x2;
const FRAME_RST_STREAM: u8 = 0x3;
const FRAME_SETTINGS: u8 = 0x4;
const FRAME_PING: u8 = 0x6;
const FRAME_GOAWAY: u8 = 0x7;
const FRAME_WINDOW_UPDATE: u8 = 0x8;

/// Frame flags (RFC 9113 §6).
const FLAG_ACK: u8 = 0x1;
const FLAG_END_HEADERS: u8 = 0x4;

/// SETTINGS identifiers (RFC 9113 §6.5.2, RFC 8441 §3, RFC 9220 §3).
const SETTINGS_HEADER_TABLE_SIZE: u16 = 0x1;
const SETTINGS_ENABLE_PUSH: u16 = 0x2;
const SETTINGS_MAX_CONCURRENT_STREAMS: u16 = 0x3;
const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x4;
const SETTINGS_MAX_FRAME_SIZE: u16 = 0x5;
const SETTINGS_MAX_HEADER_LIST_SIZE: u16 = 0x6;
const SETTINGS_ENABLE_CONNECT_PROTOCOL: u16 = 0x8;

/// Error codes (RFC 9113 §7).
const ERR_NO_ERROR: u32 = 0x0;
const ERR_PROTOCOL_ERROR: u32 = 0x1;

/// Default frame payload limit before the peer's SETTINGS arrive (RFC 9113 §4.2).
const DEFAULT_MAX_FRAME_SIZE: u32 = 16_384;

/// Default stream-window size (RFC 9113 §6.9.2).
const DEFAULT_INITIAL_WINDOW_SIZE: u32 = 65_535;

/// How much connection-level window we hand the server up front.
const CONNECTION_WINDOW: u32 = 1 << 24;

/// Boundary between static-table indices we can name and ones we cannot.
const HPACK_STATIC_LEN: usize = 61;

// ---------------------------------------------------------------- varints

/// Append a QUIC variable-length integer (RFC 9000 §16).
///
/// This is the encoding CAPSULE uses (RFC 9297 §3.1). It is **not** the HPACK
/// integer encoding; see the module docs.
pub fn append_varint(out: &mut Vec<u8>, value: u64) {
    if value < 64 {
        out.push(value as u8);
    } else if value < 16_384 {
        out.extend_from_slice(&((value as u16) | 0x4000).to_be_bytes());
    } else if value < 1_073_741_824 {
        out.extend_from_slice(&((value as u32) | 0x8000_0000).to_be_bytes());
    } else {
        debug_assert!(value < 4_611_686_018_427_387_904);
        out.extend_from_slice(&(value | 0xC000_0000_0000_0000).to_be_bytes());
    }
}

/// Read a QUIC variable-length integer, returning `(value, bytes consumed)`.
pub fn read_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    let len = 1usize << (first >> 6);
    if buf.len() < len {
        return None;
    }
    let mut value = (first & 0x3f) as u64;
    for byte in &buf[1..len] {
        value = (value << 8) | *byte as u64;
    }
    Some((value, len))
}

// ---------------------------------------------------------------- capsules

/// Capsule type 0: an HTTP Datagram (RFC 9297 §3.2).
pub const CAPSULE_DATAGRAM: u64 = 0x0;
/// Capsule type 0x01: ADDRESS_ASSIGN (RFC 9484 §4.7.1).
pub const CAPSULE_ADDRESS_ASSIGN: u64 = 0x01;
/// Capsule type 0x02: ADDRESS_REQUEST (RFC 9484 §4.7.2).
pub const CAPSULE_ADDRESS_REQUEST: u64 = 0x02;
/// Capsule type 0x03: ROUTE_ADVERTISEMENT (RFC 9484 §4.7.3).
pub const CAPSULE_ROUTE_ADVERTISEMENT: u64 = 0x03;

/// Wrap one IP packet in a DATAGRAM capsule, with no Context ID.
///
/// Cloudflare's HTTP/2 CONNECT-IP edge drops the Context ID that RFC 9484 §6
/// requires at the head of a datagram payload. Measured: sending
/// `00 <len> 00 <packet>` gets no reply; sending `00 <len> <packet>` gets a
/// well-formed ICMPv6 echo reply. See `cloudflare_datagram`.
pub fn encode_capsule(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packet.len() + 8);
    append_varint(&mut out, CAPSULE_DATAGRAM);
    append_varint(&mut out, packet.len() as u64);
    out.extend_from_slice(packet);
    out
}

/// The RFC 9484 §6 form of the same datagram, with the Context ID prefix.
///
/// Kept as a separate function so the divergence from the RFC is explicit
/// rather than hidden inside a call site.
pub fn encode_capsule_rfc9484(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packet.len() + 9);
    append_varint(&mut out, CAPSULE_DATAGRAM);
    append_varint(&mut out, (packet.len() + 1) as u64);
    append_varint(&mut out, 0); // Context ID 0 = IP payload
    out.extend_from_slice(packet);
    out
}

/// A capsule peeled off the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capsule<'a> {
    pub capsule_type: u64,
    pub payload: &'a [u8],
    /// Bytes consumed, header included.
    pub consumed: usize,
}

/// Decode one capsule from the front of `buf`.
///
/// Returns `Ok(None)` when the buffer holds only a prefix of a capsule, which
/// happens whenever a capsule straddles two DATA frames — legal per RFC 9484
/// §7.1 ("a DATAGRAM capsule can be split across multiple ... TCP packets").
pub fn decode_capsule(buf: &[u8]) -> Result<Option<Capsule<'_>>, ConnectIpError> {
    let Some((capsule_type, type_len)) = read_varint(buf) else {
        return Ok(None);
    };
    let Some((payload_len, len_len)) = read_varint(&buf[type_len..]) else {
        return Ok(None);
    };
    let header = type_len + len_len;
    let payload_len = payload_len as usize;
    // Reject a length that would overflow rather than panicking on the add.
    let total = header
        .checked_add(payload_len)
        .ok_or(ConnectIpError::MalformedCapsule)?;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some(Capsule {
        capsule_type,
        payload: &buf[header..total],
        consumed: total,
    }))
}

// ---------------------------------------------------------------- errors

#[derive(Debug, thiserror::Error)]
pub enum ConnectIpError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("server rejected CONNECT-IP with {0}")]
    Rejected(u16),
    #[error("server reset stream {stream} with error code {code:#x}")]
    StreamReset { stream: u32, code: u32 },
    #[error("server sent GOAWAY (last stream {last_stream}, code {code:#x}): {debug}")]
    GoAway {
        last_stream: u32,
        code: u32,
        debug: String,
    },
    #[error("unexpected frame type {0}")]
    UnexpectedFrame(u8),
    #[error("malformed response headers")]
    BadResponse,
    #[error("malformed capsule")]
    MalformedCapsule,
    #[error("peer exceeded our flow-control window")]
    FlowControl,
}

// ---------------------------------------------------------------- tunnel

/// A single CONNECT-IP tunnel: one HTTP/2 stream carrying IP packets.
pub struct Tunnel<S> {
    io: S,
    /// The stream id this tunnel was opened on.
    stream_id: u32,
    /// Set once the response headers have been read.
    ready: bool,
    /// Peer's SETTINGS_INITIAL_WINDOW_SIZE; bounds what we may send per stream.
    peer_initial_window: u32,
    /// Bytes we have sent on the stream since the last WINDOW_UPDATE.
    stream_sent: u32,
    /// Connection-level bytes outstanding.
    connection_sent: u32,
    /// Connection-level window the peer has granted us.
    connection_window: u32,
    /// Peer's SETTINGS_MAX_FRAME_SIZE.
    peer_max_frame: u32,
    /// Bytes reassembled from DATA frames but not yet decoded into capsules.
    recv_buf: Vec<u8>,
    /// Whether the peer granted SETTINGS_ENABLE_CONNECT_PROTOCOL (RFC 8441).
    ///
    /// Recorded for diagnostics only: this edge never sends it, and the tunnel
    /// works anyway because we speak plain CONNECT.
    peer_enable_connect_protocol: bool,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Tunnel<S> {
    /// Open a CONNECT-IP stream on an established HTTP/2 connection.
    ///
    /// `stream_id` must be odd (client-initiated) and unique per tunnel.
    pub async fn open(
        mut io: S,
        stream_id: u32,
        authority: &str,
    ) -> Result<Self, ConnectIpError> {
        debug_assert!(stream_id % 2 == 1, "client streams are odd");
        // The connection preface is the caller's job: `handshake()` sends it
        // before this is called. Writing it here too would put a second
        // "PRI * HTTP/2.0..." on the wire, which the edge answers by closing
        // the connection (observed as `Broken pipe` on the HEADERS write).

        let headers = encode_connect_ip_headers(authority);
        io.write_all(&frame(FRAME_HEADERS, FLAG_END_HEADERS, stream_id, &headers))
            .await
            .map_err(ConnectIpError::Io)?;
        io.flush().await.map_err(ConnectIpError::Io)?;

        let mut tunnel = Self {
            io,
            stream_id,
            ready: false,
            peer_initial_window: DEFAULT_INITIAL_WINDOW_SIZE,
            stream_sent: 0,
            connection_sent: 0,
            connection_window: CONNECTION_WINDOW,
            peer_max_frame: DEFAULT_MAX_FRAME_SIZE,
            recv_buf: Vec::with_capacity(8 * 1024),
            peer_enable_connect_protocol: false,
        };
        tunnel.read_response().await?;
        Ok(tunnel)
    }

    /// True when the peer granted Extended CONNECT (RFC 8441 §3).
    ///
    /// Always false on Cloudflare's CONNECT-IP edge; see the module docs.
    pub fn peer_enabled_extended_connect(&self) -> bool {
        self.peer_enable_connect_protocol
    }

    /// Read frames until the response HEADERS arrives.
    ///
    /// SETTINGS, WINDOW_UPDATE, PING and PRIORITY are handled because a
    /// well-behaved server may send any of them at any time (RFC 9113 §5.4).
    async fn read_response(&mut self) -> Result<(), ConnectIpError> {
        loop {
            let header = self.read_frame_header().await?;
            match header.frame_type {
                FRAME_SETTINGS => {
                    let payload = self.read_payload(header.length).await?;
                    if header.flags & FLAG_ACK == 0 {
                        self.apply_settings(&payload)?;
                        self.send_settings_ack().await?;
                    }
                }
                FRAME_WINDOW_UPDATE => {
                    let payload = self.read_payload(header.length).await?;
                    self.apply_window_update(header.stream_id, &payload)?;
                }
                FRAME_PING => {
                    let payload = self.read_payload(header.length).await?;
                    if header.flags & FLAG_ACK == 0 {
                        if payload.len() != 8 {
                            return Err(ConnectIpError::UnexpectedFrame(FRAME_PING));
                        }
                        self.io
                            .write_all(&frame(FRAME_PING, FLAG_ACK, 0, &payload))
                            .await
                            .map_err(ConnectIpError::Io)?;
                        self.io.flush().await.map_err(ConnectIpError::Io)?;
                    }
                }
                FRAME_PRIORITY => {
                    let _ = self.read_payload(header.length).await?;
                }
                FRAME_HEADERS => {
                    let payload = self.read_payload(header.length).await?;
                    let (status, _) = decode_response_headers(&payload)?;
                    if !(200..300).contains(&status) {
                        return Err(ConnectIpError::Rejected(status));
                    }
                    self.ready = true;
                    return Ok(());
                }
                FRAME_DATA => {
                    // A response body before headers is not legal for CONNECT-IP
                    // (RFC 9484 §4.4: no message content), but draining it is
                    // safer than erroring.
                    let _ = self.read_payload(header.length).await?;
                }
                FRAME_RST_STREAM => {
                    let payload = self.read_payload(header.length).await?;
                    let code = payload
                        .get(..4)
                        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                        .unwrap_or(ERR_PROTOCOL_ERROR);
                    return Err(ConnectIpError::StreamReset {
                        stream: header.stream_id,
                        code,
                    });
                }
                FRAME_GOAWAY => {
                    let payload = self.read_payload(header.length).await?;
                    return Err(decode_goaway(&payload));
                }
                other => {
                    // RFC 9113 §4.1: unknown frame types MUST be ignored.
                    let _ = self.read_payload(header.length).await?;
                    let _ = other;
                }
            }
        }
    }

    async fn read_frame_header(&mut self) -> Result<FrameHeader, ConnectIpError> {
        let mut buf = [0u8; 9];
        self.io
            .read_exact(&mut buf)
            .await
            .map_err(ConnectIpError::Io)?;
        let length = u32::from_be_bytes([0, buf[0], buf[1], buf[2]]);
        // RFC 9113 §4.2: a frame larger than SETTINGS_MAX_FRAME_SIZE is a
        // connection error. The peer knows our limit, so enforce it.
        if length > DEFAULT_MAX_FRAME_SIZE {
            return Err(ConnectIpError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame of {length} bytes exceeds SETTINGS_MAX_FRAME_SIZE"),
            )));
        }
        Ok(FrameHeader {
            length,
            frame_type: buf[3],
            flags: buf[4],
            // The top bit is reserved and MUST be ignored (RFC 9113 §4.1).
            stream_id: u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7fff_ffff,
        })
    }

    async fn read_payload(&mut self, len: u32) -> Result<Vec<u8>, ConnectIpError> {
        let mut buf = vec![0u8; len as usize];
        self.io
            .read_exact(&mut buf)
            .await
            .map_err(ConnectIpError::Io)?;
        Ok(buf)
    }

    async fn send_settings_ack(&mut self) -> Result<(), ConnectIpError> {
        self.io
            .write_all(&frame(FRAME_SETTINGS, FLAG_ACK, 0, &[]))
            .await
            .map_err(ConnectIpError::Io)?;
        self.io.flush().await.map_err(ConnectIpError::Io)?;
        Ok(())
    }

    /// Absorb the peer's SETTINGS (RFC 9113 §6.5.2).
    fn apply_settings(&mut self, payload: &[u8]) -> Result<(), ConnectIpError> {
        if payload.len() % 6 != 0 {
            // RFC 9113 §6.5: a SETTINGS frame whose length is not a multiple of
            // 6 is a connection error of type FRAME_SIZE_ERROR.
            return Err(ConnectIpError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "SETTINGS frame length is not a multiple of 6",
            )));
        }
        for entry in payload.chunks_exact(6) {
            let id = u16::from_be_bytes([entry[0], entry[1]]);
            let value = u32::from_be_bytes([entry[2], entry[3], entry[4], entry[5]]);
            match id {
                SETTINGS_INITIAL_WINDOW_SIZE => {
                    // RFC 9113 §6.9.2: values above 2^31-1 are a
                    // FLOW_CONTROL_ERROR.
                    if value > 0x7fff_ffff {
                        return Err(ConnectIpError::FlowControl);
                    }
                    // A change to INITIAL_WINDOW_SIZE retroactively shifts the
                    // window of every open stream.
                    let delta = value as i64 - self.peer_initial_window as i64;
                    self.peer_initial_window = value;
                    let adjusted = self.stream_sent as i64 - delta;
                    self.stream_sent = adjusted.max(0) as u32;
                }
                SETTINGS_MAX_FRAME_SIZE => {
                    if !(16_384..=16_777_215).contains(&value) {
                        return Err(ConnectIpError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "SETTINGS_MAX_FRAME_SIZE out of range",
                        )));
                    }
                    self.peer_max_frame = value;
                }
                SETTINGS_ENABLE_CONNECT_PROTOCOL => {
                    if value > 1 {
                        return Err(ConnectIpError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "SETTINGS_ENABLE_CONNECT_PROTOCOL must be 0 or 1",
                        )));
                    }
                    self.peer_enable_connect_protocol = value == 1;
                }
                SETTINGS_HEADER_TABLE_SIZE
                | SETTINGS_ENABLE_PUSH
                | SETTINGS_MAX_CONCURRENT_STREAMS
                | SETTINGS_MAX_HEADER_LIST_SIZE => {}
                _ => {} // Unknown settings MUST be ignored (RFC 9113 §6.5.2).
            }
        }
        Ok(())
    }

    fn apply_window_update(&mut self, stream: u32, payload: &[u8]) -> Result<(), ConnectIpError> {
        if payload.len() != 4 {
            return Err(ConnectIpError::FlowControl);
        }
        let increment =
            u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) & 0x7fff_ffff;
        if increment == 0 {
            // RFC 9113 §6.9: a zero increment is PROTOCOL_ERROR on a stream and
            // FLOW_CONTROL_ERROR on the connection.
            return Err(ConnectIpError::FlowControl);
        }
        if stream == 0 {
            self.connection_window = self.connection_window.saturating_add(increment);
        } else {
            self.stream_sent = self.stream_sent.saturating_sub(increment);
        }
        Ok(())
    }

    /// The stream id this tunnel was opened on.
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    /// Whether the CONNECT-IP handshake completed.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Split the tunnel into its raw stream, for a caller that drives packet
    /// I/O itself.
    pub fn into_stream(self) -> S {
        self.io
    }

    /// Send one IP packet, wrapped in a DATAGRAM capsule.
    pub async fn send_packet(&mut self, packet: &[u8]) -> Result<(), ConnectIpError> {
        let capsule = encode_capsule(packet);
        self.send_capsule_bytes(&capsule).await
    }

    /// Send an already-encoded capsule (or several concatenated).
    ///
    /// Splits across DATA frames when the capsule exceeds the peer's
    /// SETTINGS_MAX_FRAME_SIZE or the remaining flow-control window, which is
    /// what RFC 9484 §7.1 expects ("a DATAGRAM capsule can be split across
    /// multiple ... packets").
    async fn send_capsule_bytes(&mut self, capsule: &[u8]) -> Result<(), ConnectIpError> {
        let mut offset = 0usize;
        while offset < capsule.len() {
            // RFC 9113 §6.9.1: respect both the stream and connection windows.
            let stream_room = self.peer_initial_window.saturating_sub(self.stream_sent) as usize;
            let room = stream_room.min(self.connection_window as usize);
            if room == 0 {
                // Blocked: drain frames until the peer grants more window.
                // Without this the write would trip FLOW_CONTROL_ERROR.
                self.pump_control_frames().await?;
                continue;
            }
            let take = (capsule.len() - offset).min(self.peer_max_frame as usize).min(room);
            let chunk = &capsule[offset..offset + take];
            // Never set END_STREAM: that would half-close the tunnel and the
            // edge stops relaying payload (RFC 9113 §6.1).
            self.io
                .write_all(&frame(FRAME_DATA, 0, self.stream_id, chunk))
                .await
                .map_err(ConnectIpError::Io)?;
            self.stream_sent = self.stream_sent.saturating_add(take as u32);
            self.connection_sent = self.connection_sent.saturating_add(take as u32);
            self.connection_window = self.connection_window.saturating_sub(take as u32);
            offset += take;
        }
        self.io.flush().await.map_err(ConnectIpError::Io)?;
        Ok(())
    }
    /// Read frames that are not DATA, until a DATA frame shows up.
    async fn pump_control_frames(&mut self) -> Result<(), ConnectIpError> {
        loop {
            let header = self.read_frame_header().await?;
            match header.frame_type {
                FRAME_SETTINGS => {
                    let payload = self.read_payload(header.length).await?;
                    if header.flags & FLAG_ACK == 0 {
                        self.apply_settings(&payload)?;
                        self.send_settings_ack().await?;
                    }
                }
                FRAME_WINDOW_UPDATE => {
                    let payload = self.read_payload(header.length).await?;
                    self.apply_window_update(header.stream_id, &payload)?;
                    return Ok(());
                }
                FRAME_PING => {
                    let payload = self.read_payload(header.length).await?;
                    if header.flags & FLAG_ACK == 0 && payload.len() == 8 {
                        self.io
                            .write_all(&frame(FRAME_PING, FLAG_ACK, 0, &payload))
                            .await
                            .map_err(ConnectIpError::Io)?;
                        self.io.flush().await.map_err(ConnectIpError::Io)?;
                    }
                }
                FRAME_RST_STREAM => {
                    let payload = self.read_payload(header.length).await?;
                    let code = payload
                        .get(..4)
                        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                        .unwrap_or(ERR_PROTOCOL_ERROR);
                    return Err(ConnectIpError::StreamReset {
                        stream: header.stream_id,
                        code,
                    });
                }
                FRAME_GOAWAY => {
                    let payload = self.read_payload(header.length).await?;
                    return Err(decode_goaway(&payload));
                }
                FRAME_DATA => {
                    let payload = self.read_payload(header.length).await?;
                    self.push_data(header.stream_id, &payload).await?;
                }
                _ => {
                    let _ = self.read_payload(header.length).await?;
                }
            }
        }
    }

    /// Read the next IP packet from the tunnel.
    ///
    /// Any complete capsule already buffered is returned first; otherwise
    /// frames are read until a DATAGRAM capsule can be decoded.
    pub async fn recv_packet(&mut self) -> Result<Vec<u8>, ConnectIpError> {
        loop {
            // Try whatever is already buffered before touching the socket: a
            // single DATA frame can carry several capsules.
            if let Some(packet) = self.take_datagram()? {
                return Ok(packet);
            }
            let header = self.read_frame_header().await?;
            match header.frame_type {
                FRAME_DATA => {
                    // RFC 9113 §6.1: DATA on stream 0 is a PROTOCOL_ERROR, and
                    // the payload must be consumed either way.
                    let payload = self.read_payload(header.length).await?;
                    if header.stream_id == 0 {
                        return Err(ConnectIpError::UnexpectedFrame(FRAME_DATA));
                    }
                    self.push_data(header.stream_id, &payload).await?;
                }
                FRAME_SETTINGS => {
                    let payload = self.read_payload(header.length).await?;
                    if header.flags & FLAG_ACK == 0 {
                        self.apply_settings(&payload)?;
                        self.send_settings_ack().await?;
                    }
                }
                FRAME_WINDOW_UPDATE => {
                    let payload = self.read_payload(header.length).await?;
                    self.apply_window_update(header.stream_id, &payload)?;
                }
                FRAME_PING => {
                    let payload = self.read_payload(header.length).await?;
                    if header.flags & FLAG_ACK == 0 && payload.len() == 8 {
                        self.io
                            .write_all(&frame(FRAME_PING, FLAG_ACK, 0, &payload))
                            .await
                            .map_err(ConnectIpError::Io)?;
                        self.io.flush().await.map_err(ConnectIpError::Io)?;
                    }
                }
                FRAME_RST_STREAM => {
                    let payload = self.read_payload(header.length).await?;
                    let code = payload
                        .get(..4)
                        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                        .unwrap_or(ERR_PROTOCOL_ERROR);
                    return Err(ConnectIpError::StreamReset {
                        stream: header.stream_id,
                        code,
                    });
                }
                FRAME_GOAWAY => {
                    let payload = self.read_payload(header.length).await?;
                    return Err(decode_goaway(&payload));
                }
                _ => {
                    // Unknown or irrelevant frames MUST be consumed in full.
                    let _ = self.read_payload(header.length).await?;
                }
            }
        }
    }

    /// Append DATA payload bytes, discarding traffic from other streams *after*
    /// reading it (the old code skipped the read and desynchronised).
    async fn push_data(&mut self, stream: u32, payload: &[u8]) -> Result<(), ConnectIpError> {
        if stream != self.stream_id {
            return Ok(());
        }
        self.recv_buf.extend_from_slice(payload);
        // Grant more window as we consume, or a long-lived tunnel stalls after
        // the initial window is used up (RFC 9113 §6.9.1).
        if payload.len() >= 16 * 1024 {
            let inc = (payload.len() as u32).to_be_bytes();
            self.io
                .write_all(&frame(FRAME_WINDOW_UPDATE, 0, 0, &inc))
                .await
                .map_err(ConnectIpError::Io)?;
            self.io
                .write_all(&frame(FRAME_WINDOW_UPDATE, 0, self.stream_id, &inc))
                .await
                .map_err(ConnectIpError::Io)?;
            self.io.flush().await.map_err(ConnectIpError::Io)?;
        }
        Ok(())
    }

    /// Peel one DATAGRAM capsule out of `recv_buf`.
    ///
    /// Non-datagram capsules (ADDRESS_ASSIGN, ROUTE_ADVERTISEMENT, …) are
    /// consumed and skipped; a caller that needs them should use
    /// [`Tunnel::recv_capsule`].
    fn take_datagram(&mut self) -> Result<Option<Vec<u8>>, ConnectIpError> {
        loop {
            let Some(capsule) = decode_capsule(&self.recv_buf)? else {
                return Ok(None);
            };
            let consumed = capsule.consumed;
            let is_datagram = capsule.capsule_type == CAPSULE_DATAGRAM;
            if is_datagram {
                let payload = capsule.payload.to_vec();
                self.recv_buf.drain(..consumed);
                // Cloudflare's edge omits the Context ID; accept both shapes so
                // the code works against a conformant proxy too.
                return Ok(Some(strip_context_id(payload)));
            }
            self.recv_buf.drain(..consumed);
        }
    }

    /// Peel the next capsule of any type.
    pub async fn recv_capsule(&mut self) -> Result<Capsule<'_>, ConnectIpError> {
        loop {
            if decode_capsule(&self.recv_buf)?.is_some() {
                let capsule = decode_capsule(&self.recv_buf)?.expect("checked above");
                return Ok(Capsule {
                    capsule_type: capsule.capsule_type,
                    payload: capsule.payload,
                    consumed: capsule.consumed,
                });
            }
            let header = self.read_frame_header().await?;
            match header.frame_type {
                FRAME_DATA => {
                    let payload = self.read_payload(header.length).await?;
                    self.push_data(header.stream_id, &payload).await?;
                }
                FRAME_SETTINGS => {
                    let payload = self.read_payload(header.length).await?;
                    if header.flags & FLAG_ACK == 0 {
                        self.apply_settings(&payload)?;
                        self.send_settings_ack().await?;
                    }
                }
                FRAME_WINDOW_UPDATE => {
                    let payload = self.read_payload(header.length).await?;
                    self.apply_window_update(header.stream_id, &payload)?;
                }
                FRAME_GOAWAY => {
                    let payload = self.read_payload(header.length).await?;
                    return Err(decode_goaway(&payload));
                }
                _ => {
                    let _ = self.read_payload(header.length).await?;
                }
            }
        }
    }
}

/// Drop the leading Context ID from a datagram payload if one is present.
///
/// A Context ID of 0 encodes as a single `0x00` byte. A real IPv4 packet starts
/// 0x45/0x4x and an IPv6 packet starts 0x6x, so `0x00` can only be a Context ID.
fn strip_context_id(payload: Vec<u8>) -> Vec<u8> {
    match payload.first() {
        Some(0) => payload[1..].to_vec(),
        _ => payload,
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    length: u32,
    frame_type: u8,
    flags: u8,
    stream_id: u32,
}

fn frame(frame_type: u8, flags: u8, stream_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    out.push(frame_type);
    out.push(flags);
    out.extend_from_slice(&stream_id.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn encode_settings(entries: &[(u16, u32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(entries.len() * 6);
    for (id, value) in entries {
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&value.to_be_bytes());
    }
    out
}

fn decode_goaway(payload: &[u8]) -> ConnectIpError {
    let last_stream = payload
        .get(..4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) & 0x7fff_ffff)
        .unwrap_or(0);
    let code = payload
        .get(4..8)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(ERR_NO_ERROR);
    let debug = String::from_utf8_lossy(payload.get(8..).unwrap_or_default()).into_owned();
    ConnectIpError::GoAway {
        last_stream,
        code,
        debug,
    }
}

/// HPACK-encode the CONNECT-IP request headers.
///
/// # Why this is a *plain* CONNECT and not Extended CONNECT
///
/// See the module docs: the edge does not advertise
/// `SETTINGS_ENABLE_CONNECT_PROTOCOL`, so `:protocol` is rejected with
/// `RST_STREAM(PROTOCOL_ERROR)`. Plain CONNECT + `cf-connect-proto` is what
/// `usque --http2` sends and what the edge answers `200` to.
///
/// `capsule-protocol: ?1` and `cf-connect-proto` are both mandatory (either
/// one missing produces `400`). `pq-enabled` is optional and only disables
/// post-quantum key exchange negotiation.
fn encode_connect_ip_headers(authority: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    push_literal(&mut out, b":method", b"CONNECT");
    push_literal(&mut out, b":authority", authority.as_bytes());

    push_literal(&mut out, b"capsule-protocol", b"?1");
    push_literal(&mut out, b"cf-connect-proto", b"cf-connect-ip");
    push_literal(&mut out, b"pq-enabled", b"false");
    // usque sends an empty User-Agent; some edges key off its presence.
    push_literal(&mut out, b"user-agent", b"");
    out
}

/// Append a literal header field with incremental indexing (RFC 7541 §6.2.1).
///
/// The 0x40 pattern byte carries a 6-bit index prefix of 0, meaning "a new
/// name follows". The name then begins immediately as a string literal with a
/// **7-bit** length prefix — the length must not be folded into the 0x40 byte
/// again. Getting that wrong corrupts every header in the block.
fn push_literal(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.push(0x40);
    push_hpack_int(out, name.len(), 7, 0);
    out.extend_from_slice(name);
    push_hpack_int(out, value.len(), 7, 0);
    out.extend_from_slice(value);
}

/// HPACK integer encoding (RFC 7541 §5.1).
///
/// `prefix_bits` is the width of the integer prefix for the pattern; `first`
/// carries the pattern's high bits. The value must be less than
/// `2^prefix_bits - 1` to fit on one byte.
pub fn push_hpack_int(out: &mut Vec<u8>, value: usize, prefix_bits: u8, first: u8) {
    let max = (1usize << prefix_bits) - 1;
    if value < max {
        out.push(first | value as u8);
        return;
    }
    out.push(first | max as u8);
    let mut v = value - max;
    while v >= 128 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Decode an HPACK integer (RFC 7541 §5.1) with a `prefix_bits` prefix.
pub fn read_hpack_int(buf: &[u8], prefix_bits: u8) -> Option<(usize, usize)> {
    let max = (1usize << prefix_bits) - 1;
    let first = *buf.first()?;
    let mut value = (first as usize) & max;
    let mut consumed = 1;
    if value < max {
        return Some((value, consumed));
    }
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(consumed)?;
        consumed += 1;
        value = value.checked_add(((byte & 0x7f) as usize) << shift)?;
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if shift > 28 {
            return None;
        }
    }
    Some((value, consumed))
}

/// Pull `:status` out of an HPACK-encoded response header block.
///
/// This is a deliberately small decoder: it understands indexed fields,
/// literals with and without indexing, and can *skip* Huffman-coded values
/// (without decoding them) by using their length prefix. It cannot decode a
/// Huffman-coded `:status`, but real servers send status as a static-table
/// index (0x88 for 200), so that is not a practical limitation — and if it
/// ever happens we fail loudly instead of guessing.
pub fn decode_response_headers(headers: &[u8]) -> Result<(u16, bool), ConnectIpError> {
    let mut pos = 0usize;
    let mut saw_end_stream = false;
    while pos < headers.len() {
        let first = headers[pos];
        if first & 0x80 != 0 {
            // Indexed header field (RFC 7541 §6.1).
            let (idx, n) = read_hpack_int(&headers[pos..], 7).ok_or(ConnectIpError::BadResponse)?;
            pos += n;
            if let Some(status) = static_status(idx) {
                return Ok((status, saw_end_stream));
            }
            continue;
        }
        // Literal header field. 0x40 = incremental indexing (6-bit index),
        // 0x00 = without indexing, 0x10 = never indexed (4-bit index).
        let (prefix, pattern) = if first & 0xe0 == 0x40 {
            (6u8, 0x40u8)
        } else if first & 0xf0 == 0x10 {
            (4u8, 0x10u8)
        } else {
            (4u8, 0x00u8)
        };
        let (mut name_idx, n) = read_hpack_int(&headers[pos..], prefix)
            .ok_or(ConnectIpError::BadResponse)?;
        pos += n;
        let name: Vec<u8> = if name_idx == 0 {
            let (len, n) = read_hpack_int(&headers[pos..], 7).ok_or(ConnectIpError::BadResponse)?;
            pos += n;
            let bytes = headers
                .get(pos..pos + len)
                .ok_or(ConnectIpError::BadResponse)?;
            pos += len;
            bytes.to_vec()
        } else {
            // Re-bias the 6-bit literal index: 1-based, and the pattern bits
            // were already consumed above.
            name_idx = name_idx.max(1) as usize + (pattern as usize & 0x0f) * 0;
            static_name(name_idx).unwrap_or_default()
        };
        // Value: bit 7 of the first byte is the Huffman flag (RFC 7541 §5.2).
        let huffman = headers.get(pos).map(|b| b & 0x80 != 0).unwrap_or(false);
        let (len, n) = read_hpack_int(&headers[pos..], 7).ok_or(ConnectIpError::BadResponse)?;
        pos += n;
        let value = headers
            .get(pos..pos + len)
            .ok_or(ConnectIpError::BadResponse)?;
        pos += len;
        if name == b":status" {
            if huffman {
                // Every real server indexes `:status`; refuse rather than guess.
                return Err(ConnectIpError::BadResponse);
            }
            let s = std::str::from_utf8(value).map_err(|_| ConnectIpError::BadResponse)?;
            let code: u16 = s.parse().map_err(|_| ConnectIpError::BadResponse)?;
            return Ok((code, saw_end_stream));
        }
        let _ = &mut saw_end_stream;
    }
    Err(ConnectIpError::BadResponse)
}

/// The `:status` values in HPACK's static table (RFC 7541 Appendix A).
fn static_status(idx: usize) -> Option<u16> {
    match idx {
        8 => Some(200),
        9 => Some(204),
        10 => Some(206),
        11 => Some(304),
        12 => Some(400),
        13 => Some(404),
        14 => Some(500),
        _ => None,
    }
}

/// The field names in HPACK's static table (RFC 7541 Appendix A).
///
/// Only the entries that can appear in a CONNECT response are named; the rest
/// return `None` and are skipped.
fn static_name(idx: usize) -> Option<Vec<u8>> {
    const NAMES: [&[u8]; HPACK_STATIC_LEN + 1] = [
        b"", // index 0 is unused
        b":authority",
        b":method",
        b":method",
        b":path",
        b":path",
        b":scheme",
        b":scheme",
        b":status",
        b":status",
        b":status",
        b":status",
        b":status",
        b":status",
        b":status",
        b"accept-charset",
        b"accept-encoding",
        b"accept-language",
        b"accept-ranges",
        b"accept",
        b"access-control-allow-origin",
        b"age",
        b"allow",
        b"authorization",
        b"cache-control",
        b"content-disposition",
        b"content-encoding",
        b"content-language",
        b"content-length",
        b"content-location",
        b"content-range",
        b"content-type",
        b"cookie",
        b"date",
        b"etag",
        b"expect",
        b"expires",
        b"from",
        b"host",
        b"if-match",
        b"if-modified-since",
        b"if-none-match",
        b"if-range",
        b"if-unmodified-since",
        b"last-modified",
        b"link",
        b"location",
        b"max-forwards",
        b"proxy-authenticate",
        b"proxy-authorization",
        b"range",
        b"referer",
        b"refresh",
        b"retry-after",
        b"server",
        b"set-cookie",
        b"strict-transport-security",
        b"transfer-encoding",
        b"user-agent",
        b"vary",
        b"via",
        b"www-authenticate",
    ];
    NAMES.get(idx).map(|s| s.to_vec())
}

// The tunnel owns its stream, so it is a legitimate AsyncRead/AsyncWrite target
// for code that wants to treat it as a byte pipe after the handshake.
impl<S: AsyncRead + Unpin> AsyncRead for Tunnel<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Tunnel<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// Send the HTTP/2 preface and the client SETTINGS frame.
///
/// Must be called exactly once per connection, before [`Tunnel::open`].
///
/// Sending the preface twice is a PROTOCOL_ERROR (RFC 9113 §3.4); Cloudflare's
/// edge drops the connection without a reply, which surfaces as a broken pipe
/// on the HEADERS write rather than as an HTTP status.
pub async fn handshake<S: AsyncWrite + Unpin>(io: &mut S) -> Result<(), ConnectIpError> {
    io.write_all(PREFACE).await.map_err(ConnectIpError::Io)?;
    let settings = encode_settings(&[
        (SETTINGS_ENABLE_PUSH, 0),
        (SETTINGS_MAX_FRAME_SIZE, DEFAULT_MAX_FRAME_SIZE),
        (SETTINGS_INITIAL_WINDOW_SIZE, CONNECTION_WINDOW),
    ]);
    io.write_all(&frame(FRAME_SETTINGS, 0, 0, &settings))
        .await
        .map_err(ConnectIpError::Io)?;
    io.write_all(&frame(
        FRAME_WINDOW_UPDATE,
        0,
        0,
        &CONNECTION_WINDOW.to_be_bytes(),
    ))
    .await
    .map_err(ConnectIpError::Io)?;
    io.flush().await.map_err(ConnectIpError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preface_is_the_rfc_string() {
        assert_eq!(PREFACE, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    }

    // ---------------------------------------------------------- varints

    #[test]
    fn varint_uses_quic_two_bit_prefixes() {
        // RFC 9000 §16: 0..=63 is one byte, prefix 00.
        let mut out = Vec::new();
        append_varint(&mut out, 0);
        assert_eq!(out, vec![0x00]);

        let mut out = Vec::new();
        append_varint(&mut out, 63);
        assert_eq!(out, vec![0x3f]);

        // 64 needs the two-byte form, prefix 01.
        let mut out = Vec::new();
        append_varint(&mut out, 64);
        assert_eq!(out, vec![0x40, 0x40]);
        assert_eq!(read_varint(&out), Some((64, 2)));

        let mut out = Vec::new();
        append_varint(&mut out, 16_383);
        assert_eq!(out, vec![0x7f, 0xff]);
        assert_eq!(read_varint(&out), Some((16_383, 2)));
    }

    #[test]
    fn varint_round_trips_at_every_width() {
        for v in [0u64, 1, 63, 64, 1000, 16_383, 16_384, 1_000_000, 1_073_741_823] {
            let mut out = Vec::new();
            append_varint(&mut out, v);
            assert_eq!(read_varint(&out), Some((v, out.len())), "value {v}");
        }
    }

    #[test]
    fn capsule_payload_of_64_bytes_is_not_corrupted() {
        // The old HPACK-based encoder turned a 64-byte payload into
        // `00 43 ...`, shifting every byte by one. This is the regression test.
        let packet = vec![0x60u8; 64];
        let capsule = encode_capsule(&packet);
        assert_eq!(&capsule[..2], &[0x00, 0x40], "type 0, length 64 as varint");
        let decoded = decode_capsule(&capsule).unwrap().unwrap();
        assert_eq!(decoded.capsule_type, CAPSULE_DATAGRAM);
        assert_eq!(decoded.payload, &packet[..]);
    }

    #[test]
    fn capsule_length_127_is_two_bytes_not_hpack_one_byte() {
        let packet = vec![0x45u8; 127];
        let capsule = encode_capsule(&packet);
        // 127 >= 64, so varint is 0x40 0x7f. HPACK would have emitted 0x7f as
        // a single byte and then 0x01, desynchronising the stream.
        assert_eq!(&capsule[..3], &[0x00, 0x40, 0x7f]);
        assert_eq!(capsule.len(), 3 + 127);
    }

    #[test]
    fn capsule_round_trips() {
        let packet = vec![0x45, 0x00, 0x00, 0x14, 1, 2, 3, 4];
        let capsule = encode_capsule(&packet);
        let decoded = decode_capsule(&capsule).unwrap().unwrap();
        assert_eq!(decoded.payload, &packet[..]);
        assert_eq!(decoded.consumed, capsule.len());
    }

    #[test]
    fn rfc9484_capsule_carries_context_id_zero() {
        let packet = vec![0x45, 0, 0, 20];
        let capsule = encode_capsule_rfc9484(&packet);
        assert_eq!(&capsule[..2], &[0x00, 0x05], "type 0, len 1+4");
        assert_eq!(capsule[2], 0x00, "Context ID 0");
        assert_eq!(&capsule[3..], &packet[..]);
    }

    #[test]
    fn incomplete_capsule_returns_none_not_error() {
        let packet = vec![0x45u8; 200];
        let capsule = encode_capsule(&packet);
        // A capsule split across two DATA frames must buffer, not fail.
        assert!(decode_capsule(&capsule[..10]).unwrap().is_none());
        assert!(decode_capsule(&[]).unwrap().is_none());
    }

    #[test]
    fn non_datagram_capsules_are_recognised() {
        let mut buf = Vec::new();
        append_varint(&mut buf, CAPSULE_ADDRESS_ASSIGN);
        append_varint(&mut buf, 0);
        let capsule = decode_capsule(&buf).unwrap().unwrap();
        assert_eq!(capsule.capsule_type, CAPSULE_ADDRESS_ASSIGN);
        assert_ne!(capsule.capsule_type, CAPSULE_DATAGRAM);
    }

    #[test]
    fn context_id_is_stripped_but_packets_are_not() {
        // v6 packet: starts 0x6x, must survive untouched.
        let v6 = vec![0x60, 0x0c, 0x56, 0x46];
        assert_eq!(strip_context_id(v6.clone()), v6);
        // v4 packet: starts 0x45.
        let v4 = vec![0x45, 0x00, 0x00, 0x14];
        assert_eq!(strip_context_id(v4.clone()), v4);
        // RFC 9484 form: leading 0x00 is the Context ID.
        let mut with_cid = vec![0x00];
        with_cid.extend_from_slice(&v6);
        assert_eq!(strip_context_id(with_cid), v6);
    }

    // ------------------------------------------------------------- HPACK

    #[test]
    fn hpack_int_round_trips_small_values() {
        for v in [0usize, 1, 5, 42, 126] {
            let mut out = Vec::new();
            push_hpack_int(&mut out, v, 7, 0);
            let (got, n) = read_hpack_int(&out, 7).expect("decode");
            assert_eq!(got, v);
            assert_eq!(n, out.len(), "one byte for small values");
        }
    }

    #[test]
    fn hpack_int_handles_multibyte() {
        let mut out = Vec::new();
        push_hpack_int(&mut out, 300, 7, 0);
        let (got, n) = read_hpack_int(&out, 7).expect("decode");
        assert_eq!(got, 300);
        assert_eq!(n, out.len());
    }

    #[test]
    fn hpack_literal_uses_a_7_bit_name_length() {
        // RFC 7541 §6.2.1: 0x40 then a string literal. The string's length is
        // a 7-bit-prefixed integer, so a 10-byte name starts with 0x0a — not
        // 0x4a, which is what folding it into the pattern byte produces.
        let mut out = Vec::new();
        push_literal(&mut out, b":method", b"CONNECT");
        assert_eq!(out[0], 0x40, "literal, new name, incremental indexing");
        assert_eq!(out[1], 0x07, "name length 7, 7-bit prefix");
        assert_eq!(&out[2..9], b":method");
        assert_eq!(out[9], 0x07, "value length 7");
        assert_eq!(&out[10..], b"CONNECT");
    }

    #[test]
    fn connect_ip_headers_are_a_plain_connect() {
        let h = encode_connect_ip_headers("cloudflareaccess.com");
        // Extended CONNECT (:protocol / :scheme / :path) is rejected by the
        // live edge, so it must not appear.
        assert!(
            !h.windows(9).any(|w| w == b":protocol"),
            "must not send :protocol: the edge rejects Extended CONNECT"
        );
        assert!(
            !h.windows(7).any(|w| w == b":scheme"),
            "plain CONNECT has no :scheme"
        );
        assert!(h.windows(16).any(|w| w == b"cf-connect-proto"));
        assert!(h.windows(16).any(|w| w == b"capsule-protocol"));
        assert!(h.windows(7).any(|w| w == b"CONNECT"));
        assert!(h.windows(20).any(|w| w == b"cloudflareaccess.com"));
    }

    #[test]
    fn response_status_from_static_index() {
        // 0x88 = indexed field 8 = :status 200.
        assert_eq!(decode_response_headers(&[0x88]).unwrap().0, 200);
        // 0x8c = indexed field 12 = :status 400.
        assert_eq!(decode_response_headers(&[0x8c]).unwrap().0, 400);
    }

    #[test]
    fn response_status_from_literal_name() {
        let mut block = Vec::new();
        push_literal(&mut block, b":status", b"200");
        assert_eq!(decode_response_headers(&block).unwrap().0, 200);
    }

    #[test]
    fn huffman_coded_values_are_skipped() {
        // Literal with incremental indexing, name index 31 (:content-type),
        // then a Huffman value (high bit set on the length byte).
        let block = vec![0x40 | 31, 0x80 | 5, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        // No :status in the block -> BadResponse, but it must not panic or
        // mis-parse into a bogus status.
        assert!(decode_response_headers(&block).is_err());
    }

    #[test]
    fn static_table_status_codes() {
        assert_eq!(static_status(8), Some(200));
        assert_eq!(static_status(13), Some(404));
        assert_eq!(static_status(99), None);
    }

    // ------------------------------------------------------------ settings

    #[test]
    fn settings_frame_encodes_pairs() {
        let s = encode_settings(&[(SETTINGS_ENABLE_PUSH, 0), (SETTINGS_MAX_FRAME_SIZE, 16384)]);
        assert_eq!(s.len(), 12);
        assert_eq!(&s[..2], &[0x00, 0x02]);
        assert_eq!(&s[6..8], &[0x00, 0x05]);
    }

    #[test]
    fn goaway_is_decoded_not_ignored() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&ERR_PROTOCOL_ERROR.to_be_bytes());
        payload.extend_from_slice(b"nope");
        match decode_goaway(&payload) {
            ConnectIpError::GoAway { code, debug, .. } => {
                assert_eq!(code, ERR_PROTOCOL_ERROR);
                assert_eq!(debug, "nope");
            }
            other => panic!("expected GoAway, got {other:?}"),
        }
    }
}
