//! CONNECT-IP over HTTP/2 — the data plane the Node version actually runs.
//!
//! `usque socks --http2` (which is what `config.masque_http2: true` selects)
//! does three things: a userspace network stack, a CONNECT-IP session over
//! HTTP/2, and a SOCKS5 front end. This module is the middle piece.
//!
//! * [`h2`] — a hand-rolled HTTP/2 client. hyper has no way to emit the
//!   `:protocol = connect-ip` pseudo-header that Extended CONNECT requires, and
//!   Cloudflare answers `unspecific protocol error` without it. Only a handful
//!   of frames are needed, so writing them directly is both smaller and
//!   verifiable.
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
}
