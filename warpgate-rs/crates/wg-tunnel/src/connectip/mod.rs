//! CONNECT-IP over HTTP/2 — the data plane the Node version actually runs.
//!
//! `usque socks --http2` (which is what `config.masque_http2: true` selects)
//! does three things: a userspace network stack, a CONNECT-IP session over
//! HTTP/2, and a SOCKS5 front end. This module is the middle piece.
//!
//! * [`h2`] — a hand-rolled HTTP/2 client. The edge does **not** implement
//!   RFC 9484's Extended CONNECT: it wants a *plain* `CONNECT` carrying
//!   `cf-connect-proto: cf-connect-ip` and a `:authority` that includes the
//!   `:443` port. Sending `:protocol`, `:scheme` or `:path` gets the request
//!   reset. Only a handful of frames are needed, so writing them directly is
//!   both smaller and verifiable.
//! * [`probe`] — drives [`h2`] against the real endpoint, which is the gate for
//!   deciding whether to invest in a userspace network stack.

pub mod h2;
pub mod native;
pub mod stack;
pub mod probe;
pub use h2::Tunnel as H2Tunnel;
pub use h2::handshake as h2_handshake;
pub use native::NativeTunnel;
pub use probe::probe as probe_connect_ip;

/// SNI for the HTTP/2 CONNECT-IP endpoint (usque `ConnectSNI`).
pub const CONNECT_SNI: &str = "consumer-masque.cloudflareclient.com";

/// The CONNECT-IP URI usque dials (a URI template with no variables).
pub const CONNECT_URI: &str = "https://cloudflareaccess.com";

/// The `:authority` to send with the CONNECT-IP request.
///
/// This **must** carry the `:443` port. The edge resolves the tunnel by the
/// authority's port and answers `400` to a bare host: measured against the
/// live endpoint, `cloudflareaccess.com:443` → `200`, `cloudflareaccess.com`
/// → `400` (consistently, across many fresh connections). usque gets this for
/// free: Go's HTTP/2 client fills `:authority` from `req.Host`, which
/// `connect-ip-go` sets to `authorityFromURL(u)` — and that helper appends
/// `:443` when the URL carries no explicit port.
///
/// Any port works (a bogus `:8443` is accepted too); the field only has to be
/// present and non-empty.
pub const CONNECT_AUTHORITY: &str = "cloudflareaccess.com:443";

/// The capsule protocol header value.
pub const CAPSULE_PROTOCOL: &str = "?1";


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_usque() {
        assert_eq!(CONNECT_SNI, "consumer-masque.cloudflareclient.com");
        assert_eq!(CONNECT_URI, "https://cloudflareaccess.com");
        assert_eq!(CAPSULE_PROTOCOL, "?1");
    }

    /// The authority must include the port: a bare host is answered with 400.
    #[test]
    fn connect_authority_carries_the_port() {
        assert_eq!(CONNECT_AUTHORITY, "cloudflareaccess.com:443");
        assert!(CONNECT_AUTHORITY.ends_with(":443"));
    }
}
