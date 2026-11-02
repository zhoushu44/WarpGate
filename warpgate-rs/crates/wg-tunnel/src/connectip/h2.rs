//! A hand-rolled HTTP/2 client for CONNECT-IP.
//!
//! # Why not use `hyper`
//!
//! Cloudflare's CONNECT-IP is an *Extended* CONNECT: it requires the
//! `:protocol = connect-ip` pseudo-header. hyper 1.x has no public API to emit
//! one — `hyper::ext::Protocol` only reaches the transport for *upgrades*, not
//! for a plain CONNECT — so a request built the obvious way goes out as an
//! ordinary CONNECT and the server answers `unspecific protocol error`.
//!
//! Since only a handful of frames are needed (preface, SETTINGS, one HEADERS,
//! then a byte pipe), writing them directly is smaller and far easier to verify
//! than fighting a general-purpose client.
//!
//! # What this module is
//!
//! Just enough HTTP/2 to open one CONNECT-IP tunnel: the preface, SETTINGS, the
//! request HEADERS frame, and the response. Everything after the response is the
//! caller's business.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// HTTP/2 connection preface, RFC 9113 §3.4.
const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Frame types used here.
const FRAME_SETTINGS: u8 = 0x4;
const FRAME_HEADERS: u8 = 0x1;
const FRAME_DATA: u8 = 0x0;
const FRAME_WINDOW_UPDATE: u8 = 0x8;

/// Flags.
const FLAG_END_HEADERS: u8 = 0x4;
const FLAG_ACK: u8 = 0x1;

/// A single CONNECT-IP tunnel: one HTTP/2 stream carrying IP packets.
pub struct Tunnel<S> {
    io: S,
    /// The stream id this tunnel was opened on.
    stream_id: u32,
    /// Set once the response headers have been read.
    ready: bool,
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

        let headers = encode_connect_ip_headers(authority);
        let mut frame = Vec::with_capacity(9 + headers.len());
        frame.extend_from_slice(&(headers.len() as u32).to_be_bytes()[1..].to_vec());
        frame.push(FRAME_HEADERS);
        frame.push(FLAG_END_HEADERS);
        frame.extend_from_slice(&stream_id.to_be_bytes());
        frame.extend_from_slice(&headers);
        io.write_all(&frame)
            .await
            .map_err(ConnectIpError::Io)?;
        io.flush().await.map_err(ConnectIpError::Io)?;

        let mut t = Self {
            io,
            stream_id,
            ready: false,
        };
        t.read_response().await?;
        Ok(t)
    }

    /// Read the response HEADERS frame.
    ///
    /// Any SETTINGS or WINDOW_UPDATE frames that arrive first are consumed and
    /// ignored — a well-behaved server is allowed to send them at any time.
    async fn read_response(&mut self) -> Result<(), ConnectIpError> {
        loop {
            let header = self.read_frame_header().await?;
            match header.1 {
                FRAME_SETTINGS => {
                    // Always ACK a SETTINGS frame, even one we did not ask for.
                    let _ = self.read_exact_payload(header.2).await?;
                    self.send_settings_ack().await?;
                }
                FRAME_WINDOW_UPDATE => {
                    let _ = self.read_exact_payload(header.2).await?;
                }
                FRAME_HEADERS => {
                    let payload = self.read_exact_payload(header.2).await?;
                    let status = decode_response_status(&payload)?;
                    if status != 200 {
                        return Err(ConnectIpError::Rejected(status));
                    }
                    self.ready = true;
                    return Ok(());
                }
                FRAME_DATA => {
                    // Response body before headers: not expected, but harmless.
                    let _ = self.read_exact_payload(header.2).await?;
                }
                other => {
                    return Err(ConnectIpError::UnexpectedFrame(other));
                }
            }
        }
    }

    async fn read_frame_header(&mut self) -> Result<(u32, u8, u32), ConnectIpError> {
        let mut buf = [0u8; 9];
        self.io
            .read_exact(&mut buf)
            .await
            .map_err(ConnectIpError::Io)?;
        let len = u32::from_be_bytes([0, buf[0], buf[1], buf[2]]);
        let ftype = buf[3];
        let stream = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]);
        Ok((len, ftype, stream))
    }

    async fn read_exact_payload(&mut self, len: u32) -> Result<Vec<u8>, ConnectIpError> {
        let mut buf = vec![0u8; len as usize];
        self.io
            .read_exact(&mut buf)
            .await
            .map_err(ConnectIpError::Io)?;
        Ok(buf)
    }

    async fn send_settings_ack(&mut self) -> Result<(), ConnectIpError> {
        let mut frame = Vec::with_capacity(9);
        frame.extend_from_slice(&0u32.to_be_bytes()[1..].to_vec());
        frame.push(FRAME_SETTINGS);
        frame.push(FLAG_ACK);
        frame.extend_from_slice(&0u32.to_be_bytes());
        self.io.write_all(&frame).await.map_err(ConnectIpError::Io)?;
        self.io.flush().await.map_err(ConnectIpError::Io)?;
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

    /// Send one IP packet, wrapped in a DATAGRAM capsule.
    ///
    /// Capsule framing (RFC 9297 §3.1): varint type, varint length, then the
    /// payload. Type 0 is the HTTP Datagram capsule, whose payload is the IP
    /// packet directly.
    pub async fn send_packet(&mut self, packet: &[u8]) -> Result<(), ConnectIpError> {
        let capsule = encode_capsule(packet);
        let mut frame = Vec::with_capacity(9 + capsule.len());
        frame.extend_from_slice(&(capsule.len() as u32).to_be_bytes()[1..].to_vec());
        frame.push(FRAME_DATA);
        frame.push(0);
        frame.extend_from_slice(&self.stream_id.to_be_bytes());
        frame.extend_from_slice(&capsule);
        self.io.write_all(&frame).await.map_err(ConnectIpError::Io)?;
        self.io.flush().await.map_err(ConnectIpError::Io)?;
        Ok(())
    }

    /// Read the next IP packet from the tunnel.
    ///
    /// Skips non-DATA frames and non-datagram capsules.
    pub async fn recv_packet(&mut self) -> Result<Vec<u8>, ConnectIpError> {
        loop {
            let (_len, ftype, stream) = self.read_frame_header().await?;
            if stream != 0 && stream != self.stream_id {
                // Another stream's traffic; ignore it (no other streams exist).
                continue;
            }
            match ftype {
                FRAME_DATA => {
                    let payload = self.read_exact_payload(_len).await?;
                    if let Some(packet) = decode_capsule(&payload) {
                        return Ok(packet);
                    }
                }
                FRAME_SETTINGS => {
                    let _ = self.read_exact_payload(_len).await?;
                    self.send_settings_ack().await?;
                }
                FRAME_WINDOW_UPDATE => {
                    let _ = self.read_exact_payload(_len).await?;
                }
                _ => {
                    let _ = self.read_exact_payload(_len).await?;
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectIpError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("server rejected CONNECT-IP with {0}")]
    Rejected(u16),
    #[error("unexpected frame type {0}")]
    UnexpectedFrame(u8),
    #[error("malformed response headers")]
    BadResponse,
}

/// HPACK-encode the CONNECT-IP request headers.
///
/// Every field is emitted as a literal with incremental indexing (0x40) and a
/// plain integer length, so no Huffman table is needed. Pseudo-headers come
/// first, as HTTP/2 requires.
fn encode_connect_ip_headers(authority: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    // Extended CONNECT: the `:protocol` pseudo-header is what makes this
    // CONNECT-IP rather than a plain CONNECT. Cloudflare rejects the latter.
    push_literal(&mut out, b":method", b"CONNECT");
    push_literal(&mut out, b":scheme", b"https");
    push_literal(&mut out, b":authority", authority.as_bytes());
    push_literal(&mut out, b":path", b"/");
    push_literal(&mut out, b":protocol", b"connect-ip");

    push_literal(&mut out, b"capsule-protocol", b"?1");
    push_literal(&mut out, b"cf-connect-proto", b"cf-connect-ip");
    push_literal(&mut out, b"pq-enabled", b"false");
    // usque sends an empty User-Agent; some edges key off its presence.
    push_literal(&mut out, b"user-agent", b"");
    out
}

/// Append a literal header field with incremental indexing.
fn push_literal(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.push(0x40);
    push_hpack_int(out, name.len(), 7, 0);
    out.extend_from_slice(name);
    push_hpack_int(out, value.len(), 7, 0);
    out.extend_from_slice(value);
}

/// HPACK integer encoding (RFC 7541 §5.1), 7-bit prefix.
///
/// `first_byte` supplies any high bits the caller needs (e.g. the 0x40 literal
/// marker); this fills the low 7 bits.
fn push_hpack_int(out: &mut Vec<u8>, value: usize, _prefix_bits: u8, first_byte: u8) {
    const MAX: usize = 127;
    if value < MAX {
        out.push(first_byte | value as u8);
        return;
    }
    out.push(first_byte | MAX as u8);
    let mut v = value - MAX;
    while v >= 128 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Wrap an IP packet in a DATAGRAM capsule.
fn encode_capsule(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packet.len() + 8);
    push_hpack_int(&mut out, 0, 7, 0); // capsule type 0 = HTTP Datagram
    push_hpack_int(&mut out, packet.len(), 7, 0);
    out.extend_from_slice(packet);
    out
}

/// Extract the IP packet from a DATAGRAM capsule, if that is what it is.
fn decode_capsule(buf: &[u8]) -> Option<Vec<u8>> {
    let (ctype, n1) = read_hpack_int(buf)?;
    if ctype != 0 {
        return None; // not a datagram capsule
    }
    let (len, n2) = read_hpack_int(&buf[n1..])?;
    let start = n1 + n2;
    buf.get(start..start + len).map(|s| s.to_vec())
}

/// Decode an HPACK integer with a 7-bit prefix, returning (value, bytes read).
///
/// Only the 7-bit form is needed: every integer this module emits uses it.
fn read_hpack_int(buf: &[u8]) -> Option<(usize, usize)> {
    const MAX: usize = 127;
    let first = *buf.first()?;
    let mut value = (first & 0x7f) as usize;
    let mut consumed = 1;
    if value == MAX {
        let mut shift = 0u32;
        loop {
            let b = *buf.get(consumed)?;
            consumed += 1;
            value += ((b & 0x7f) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                break;
            }
            if shift > 28 {
                return None;
            }
        }
    }
    Some((value, consumed))
}

/// Pull `:status` out of an HPACK-encoded header block.
fn decode_response_status(headers: &[u8]) -> Result<u16, ConnectIpError> {
    let mut pos = 0;
    while pos < headers.len() {
        let first = headers[pos];
        if first & 0x80 != 0 {
            // Indexed header field: a static-table reference.
            // Only the entries we care about are decoded; anything else is
            // skipped by re-deriving its length from the table.
            let idx = (first & 0x7f) as usize;
            pos += 1;
            if let Some(status) = static_status(idx) {
                return Ok(status);
            }
            continue;
        }
        // Literal with incremental indexing (0x40) or without (0x00).
        let (name_len, n) = read_hpack_int(&headers[pos..]).ok_or(ConnectIpError::BadResponse)?;
        pos += n;
        let name = headers
            .get(pos..pos + name_len)
            .ok_or(ConnectIpError::BadResponse)?;
        pos += name_len;
        let (value_len, n) = read_hpack_int(&headers[pos..]).ok_or(ConnectIpError::BadResponse)?;
        pos += n;
        let value = headers
            .get(pos..pos + value_len)
            .ok_or(ConnectIpError::BadResponse)?;
        pos += value_len;
        if name == b":status" {
            let s = std::str::from_utf8(value).map_err(|_| ConnectIpError::BadResponse)?;
            return s.parse().map_err(|_| ConnectIpError::BadResponse);
        }
    }
    Err(ConnectIpError::BadResponse)
}

/// The `:status` values in HPACK's static table (RFC 7541 Appendix A).
fn static_status(idx: usize) -> Option<u16> {
    match idx {
        8 => Some(200),
        9 => Some(206),
        10 => Some(204),
        11 => Some(304),
        12 => Some(400),
        13 => Some(404),
        14 => Some(500),
        _ => None,
    }
}

/// Send the HTTP/2 preface and an empty SETTINGS frame.
pub async fn handshake<S: AsyncWrite + Unpin>(io: &mut S) -> Result<(), ConnectIpError> {
    io.write_all(PREFACE).await.map_err(ConnectIpError::Io)?;
    let mut settings = Vec::with_capacity(9);
    settings.extend_from_slice(&0u32.to_be_bytes()[1..].to_vec());
    settings.push(FRAME_SETTINGS);
    settings.push(0);
    settings.extend_from_slice(&0u32.to_be_bytes());
    io.write_all(&settings).await.map_err(ConnectIpError::Io)?;
    io.flush().await.map_err(ConnectIpError::Io)?;
    Ok(())
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
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preface_is_the_rfc_string() {
        assert_eq!(PREFACE, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    }

    #[test]
    fn hpack_int_round_trips_small_values() {
        for v in [0usize, 1, 5, 42, 126] {
            let mut out = Vec::new();
            push_hpack_int(&mut out, v, 7, 0);
            let (got, n) = read_hpack_int(&out).expect("decode");
            assert_eq!(got, v);
            assert_eq!(n, out.len(), "one byte for small values");
        }
    }

    #[test]
    fn hpack_int_handles_multibyte() {
        let mut out = Vec::new();
        push_hpack_int(&mut out, 300, 7, 0);
        let (got, n) = read_hpack_int(&out).expect("decode");
        assert_eq!(got, 300);
        assert_eq!(n, out.len());
    }

    #[test]
    fn capsule_round_trips() {
        let packet = vec![0x45, 0x00, 0x00, 0x14, 1, 2, 3, 4];
        let capsule = encode_capsule(&packet);
        let decoded = decode_capsule(&capsule).expect("decode");
        assert_eq!(decoded, packet);
    }

    #[test]
    fn non_datagram_capsules_are_skipped() {
        // Type 1 (control capsule) with an empty payload.
        let mut buf = Vec::new();
        push_hpack_int(&mut buf, 1, 7, 0);
        push_hpack_int(&mut buf, 0, 7, 0);
        assert!(decode_capsule(&buf).is_none());
    }

    #[test]
    fn connect_ip_headers_include_the_protocol_pseudo_header() {
        let h = encode_connect_ip_headers("cloudflareaccess.com");
        // The literal `:protocol` name must appear verbatim.
        let needle = b":protocol";
        assert!(
            h.windows(needle.len()).any(|w| w == needle),
            "Extended CONNECT requires :protocol"
        );
        assert!(h.windows(10).any(|w| w == b"connect-ip"));
    }

    #[test]
    fn static_table_status_codes() {
        assert_eq!(static_status(8), Some(200));
        assert_eq!(static_status(13), Some(404));
        assert_eq!(static_status(99), None);
    }
}
