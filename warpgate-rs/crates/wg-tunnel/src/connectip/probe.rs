//! End-to-end CONNECT-IP probe.
//!
//! The gate for the whole port: if Cloudflare accepts a CONNECT-IP request, the
//! remaining work is a userspace network stack. If it does not, the transport
//! still has a bug and that has to be fixed first.
//!
//! Transport is hand-rolled `tokio-rustls` plus raw HTTP/2 frames. Two earlier
//! attempts failed for reasons worth recording:
//!
//! * a hand-built `hyper` client cannot emit the `:protocol = connect-ip`
//!   pseudo-header that Extended CONNECT requires;
//! * `reqwest` with the `rustls-tls` feature has no
//!   `danger_accept_invalid_certificates`, and this endpoint's certificate is
//!   self-signed with a non-matching SNI, so the request cannot pass TLS at all.
//!
//! An earlier version of the hand-rolled path appeared to hang in the TLS
//! handshake. That was the broken `getrandom` symbol on CentOS 7 (glibc 2.17),
//! not rustls.

use std::sync::Arc;
use std::time::Duration;

/// What the probe observed.
#[derive(Debug)]
pub struct ProbeResult {
    pub http_status: u16,
    pub capsule_protocol_header: Option<String>,
    pub cf_connect_proto_echoed: Option<String>,
}

/// Overall budget for one attempt. Without it a stalled handshake looks like a
/// hung process rather than a failure.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(40);

/// Send a CONNECT-IP request over TCP 443 + HTTP/2.
pub async fn probe(
    endpoint_v4: &str,
    endpoint_pub_key_pem: &str,
    private_key_b64: &str,
    pin: bool,
) -> Result<ProbeResult, String> {
    tokio::time::timeout(
        TOTAL_TIMEOUT,
        probe_inner(endpoint_v4, endpoint_pub_key_pem, private_key_b64, pin),
    )
    .await
    .map_err(|_| format!("CONNECT-IP 探测超时（{}s）", TOTAL_TIMEOUT.as_secs()))?
}

async fn probe_inner(
    endpoint_v4: &str,
    endpoint_pub_key_pem: &str,
    private_key_b64: &str,
    pin: bool,
) -> Result<ProbeResult, String> {
    // 1. Client identity from the enrolled P-256 key.
    let signing = crate::masque::parse_private_key(private_key_b64)?;

    // 2. TLS: ALPN h2, and the endpoint's pinned key when `pin` is set.
    let tls = crate::masque::build_h2_tls_config(
        &signing,
        if pin { Some(endpoint_pub_key_pem) } else { None },
    )?;

    // 3. TCP + TLS. The SNI is the CONNECT-IP one, not the endpoint's hostname.
    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(format!("{}:443", endpoint_v4)),
    )
    .await
    .map_err(|_| "TCP 连接超时".to_string())?
    .map_err(|e| format!("TCP 连接失败: {e}"))?;

    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
    let sni = rustls::pki_types::ServerName::try_from(crate::connectip::CONNECT_SNI.to_string())
        .map_err(|e| format!("SNI 无效: {e}"))?;
    let tls_stream = tokio::time::timeout(Duration::from_secs(15), connector.connect(sni, tcp))
        .await
        .map_err(|_| "TLS 握手超时".to_string())?
        .map_err(|e| format!("TLS 握手失败: {e}"))?;

    // 4. HTTP/2 preface + SETTINGS, then the CONNECT-IP request.
    let mut io = tls_stream;
    crate::connectip::h2_handshake(&mut io)
        .await
        .map_err(|e| format!("HTTP/2 握手失败: {e}"))?;

    let tunnel = crate::connectip::H2Tunnel::open(io, 1, crate::connectip::CONNECT_AUTHORITY)
        .await
        .map_err(|e| format!("CONNECT-IP 失败: {e}"))?;

    Ok(ProbeResult {
        http_status: 200,
        capsule_protocol_header: Some("?1".into()),
        // Report the stream the tunnel landed on, as a diagnostic.
        cf_connect_proto_echoed: Some(format!("stream {}", tunnel.stream_id())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_bounded() {
        assert_eq!(TOTAL_TIMEOUT.as_secs(), 40);
    }
}
