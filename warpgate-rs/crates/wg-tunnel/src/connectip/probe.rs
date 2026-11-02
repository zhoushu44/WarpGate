//! End-to-end CONNECT-IP probe.
//!
//! The gate for the whole port: if Cloudflare accepts a CONNECT-IP request, the
//! remaining work is a userspace network stack. If it does not, the transport
//! still has a bug and that has to be fixed first.
//!
//! Transport is `reqwest` because a hand-built rustls config stalls in the TLS
//! handshake against this endpoint — even though `openssl s_client` with no
//! client certificate at all completes fine. That discrepancy is a rabbit hole,
//! and the transport is not where the interesting protocol work lives.

use std::time::Duration;

/// What the probe observed.
#[derive(Debug)]
pub struct ProbeResult {
    pub http_status: u16,
    pub capsule_protocol_header: Option<String>,
    pub cf_connect_proto_echoed: Option<String>,
}

/// Send a CONNECT-IP request over TCP 443 + HTTP/2.
pub async fn probe(
    endpoint_v4: &str,
    private_key_b64: &str,
) -> Result<ProbeResult, String> {
    use p256::pkcs8::EncodePrivateKey;

    let signing = crate::masque::parse_private_key(private_key_b64)?;
    let cert_der = crate::masque::self_signed_der(&signing)?;
    let pkcs8 = signing
        .to_pkcs8_der()
        .map_err(|e| format!("导出私钥失败: {e}"))?
        .as_bytes()
        .to_vec();

    // reqwest only accepts a PKCS#12 identity, so convert via openssl.
    let p12 = to_pkcs12(&cert_der, &pkcs8)?;
    let identity = reqwest::Identity::from_pkcs12_der(&p12, "")
        .map_err(|e| format!("客户端身份失败: {e}"))?;

    let url = format!("https://{}:443/", endpoint_v4);
    let client = reqwest::Client::builder()
        .identity(identity)
        // The endpoint's certificate is self-signed and the SNI is not its
        // hostname, so neither name nor chain validation can succeed. Its
        // identity is pinned separately in `masque`.
        .danger_accept_invalid_certificates(true)
        .https_only(false)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("客户端构建失败: {e}"))?;

    let resp = client
        .request(reqwest::Method::CONNECT, &url)
        .header("host", "cloudflareaccess.com")
        .header("cf-connect-proto", "cf-connect-ip")
        .header("pq-enabled", "false")
        .header("capsule-protocol", "?1")
        .header("user-agent", "")
        .send()
        .await
        .map_err(|e| {
            // reqwest wraps the cause; walk the chain so the real reason shows.
            let mut chain = format!("{}", e);
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                chain.push_str(&format!(" <- {}", s));
                src = s.source();
            }
            format!("CONNECT-IP 请求失败: {}", chain)
        })?;

    let status = resp.status().as_u16();
    let capsule = resp
        .headers()
        .get("capsule-protocol")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let cf_proto = resp
        .headers()
        .get("cf-connect-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    Ok(ProbeResult {
        http_status: status,
        capsule_protocol_header: capsule,
        cf_connect_proto_echoed: cf_proto,
    })
}

/// Bundle a cert and key into a PKCS#12 file (reqwest's only identity format).
fn to_pkcs12(cert_der: &[u8], key_der: &[u8]) -> Result<Vec<u8>, String> {
    let mut pem = pem_wrap(cert_der, "CERTIFICATE");
    pem.push_str(&pem_wrap(key_der, "PRIVATE KEY"));

    let out_path = std::env::temp_dir().join(format!("warpgate-p12-{}.p12", std::process::id()));
    let status = std::process::Command::new("openssl")
        .args([
            "pkcs12",
            "-export",
            "-out",
            out_path.to_str().unwrap(),
            "-passout",
            "pass:",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            if let Some(si) = c.stdin.as_mut() {
                si.write_all(pem.as_bytes())?;
            }
            c.wait_with_output()
        })
        .map_err(|e| format!("openssl 失败: {e}"))?;

    if !status.status.success() {
        return Err("openssl pkcs12 导出失败".into());
    }
    let bytes = std::fs::read(&out_path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&out_path);
    Ok(bytes)
}

fn pem_wrap(der: &[u8], label: &str) -> String {
    use base64::Engine;
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {}-----\n", label);
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        out.push('\n');
    }
    out.push_str(&format!("-----END {}-----\n", label));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sni_matches_usque() {
        assert_eq!(
            crate::connectip::CONNECT_SNI,
            "consumer-masque.cloudflareclient.com"
        );
    }

    #[test]
    fn pem_wrapping_is_valid() {
        let pem = pem_wrap(&[1, 2, 3], "CERTIFICATE");
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
    }
}
