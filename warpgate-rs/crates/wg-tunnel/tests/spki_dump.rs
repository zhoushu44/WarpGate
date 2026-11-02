//! Dump the server's certificate SPKI next to the pinned one.
//!
//! The pinning check compares raw SPKI bytes, so when it fails the only useful
//! diagnostic is both values side by side. This runs a TLS handshake with an
//! accept-anything verifier that records what the server actually sent.

use p256::ecdsa::SigningKey;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use p256::pkcs8::EncodePrivateKey;

const SAMPLE_KEY: &str = "MHcCAQEEIAh6tuhEPefwuymbW/QQC0mgPIC/U3jzUPkPzpD8/ACEoAoGCCqGSM49AwEHoUQDQgAEhgRduQGUGankFE7brTffrw+57jh1nF4NnkfYjVx1ck2XTtc6xcrP5Mc2/mEg5zVdaHf5YB2JnUr7B0q7aQkLoA==";

/// Records the leaf cert's SPKI instead of rejecting.
#[derive(Debug)]
struct RecordingVerifier {
    seen: Arc<Mutex<Vec<u8>>>,
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Ok((_, cert)) = x509_parser::parse_x509_certificate(end_entity.as_ref()) {
            *self.seen.lock().unwrap() = cert.public_key().raw.to_vec();
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256, SignatureScheme::RSA_PKCS1_SHA256]
    }
}

#[tokio::test]
async fn dump_server_spki_vs_pinned() {
    let pinned_pem = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEIaU7MToJm9NKp8YfGxR6r+/h4mcG\n7SxI8tsW8OR1A5tv/zCzVbCRRh2t87/kxnP6lAy0lkr7qYwu+ox+k3dr6w==\n-----END PUBLIC KEY-----\n";
    let pinned = wg_tunnel::masque::pem_to_spki_der(pinned_pem).unwrap();

    let key = wg_tunnel::masque::parse_private_key(SAMPLE_KEY).unwrap();
    let ours = wg_tunnel::masque::public_key_spki_der(&key);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let verifier = Arc::new(RecordingVerifier { seen: seen.clone() });

    let cert_der = wg_tunnel::masque::self_signed_der(&key).unwrap();
    let pkcs8 = key.to_pkcs8_der().unwrap().as_bytes().to_vec();
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    let mut cfg = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(verifier)
    .with_client_auth_cert(
        vec![CertificateDer::from(cert_der)],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8)),
    )
    .unwrap();
    cfg.alpn_protocols = vec![b"h2".to_vec()];

    // Live endpoint — this test needs network access.
    let sni = ServerName::try_from("consumer-masque.cloudflareclient.com".to_string()).unwrap();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(cfg));
    match tokio::time::timeout(
        Duration::from_secs(10),
        connector.connect(sni, tokio::net::TcpStream::connect("162.159.198.2:443").await.unwrap()),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => println!("handshake error: {e}"),
        Err(_) => println!("handshake timeout"),
    }

    let server = seen.lock().unwrap().clone();
    println!("pinned (usque.json) : {} bytes {:02x?}", pinned.len(), &pinned[..pinned.len().min(20)]);
    println!("server cert         : {} bytes {:02x?}", server.len(), &server[..server.len().min(20)]);
    println!("our own pubkey      : {} bytes {:02x?}", ours.len(), &ours[..ours.len().min(20)]);
    println!("pinned == server    : {}", pinned == server);
}
