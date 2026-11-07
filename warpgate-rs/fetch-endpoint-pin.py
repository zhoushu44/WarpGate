#!/usr/bin/env python3
"""Fetch the live edge certificate public key for the MASQUE endpoint.

Cloudflare rotates the key its consumer-masque edge presents, and usque pins the
one captured at registration time. When they diverge every dial fails with
`x509: no valid chains built: remote endpoint has a different public key`, and
with --always-reconnect that failure retries once per second, leaking a socket
each time.

Run this to learn the current pin, then rewrite endpoint_pub_key in the instance
configs.

Usage: fetch_endpoint_pin.py <host> [port]
Prints the SPKI PEM, which is exactly the format usque.json expects.
"""

import socket
import ssl
import sys

SNI = "consumer-masque.cloudflareclient.com"


def main() -> int:
    host = sys.argv[1] if len(sys.argv) > 1 else "162.159.192.7"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 443

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    # The edge presents a Cloudflare-managed certificate that is not in the
    # system store, so verification is off here; the point is only to read the
    # public key that the same peer will present to usque.
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    ctx.set_alpn_protocols(["h2"])

    raw = socket.create_connection((host, port), timeout=15)
    conn = ctx.wrap_socket(raw, server_hostname=SNI)

    der = conn.getpeercert(binary_form=True)
    if not der:
        print("no peer certificate", file=sys.stderr)
        return 1

    # Re-parse to get the subject public key info.
    from cryptography import x509
    from cryptography.hazmat.primitives import serialization

    cert = x509.load_der_x509_certificate(der)
    spki = cert.public_key().public_bytes(
        encoding=serialization.Encoding.DER,
        format=serialization.PublicFormat.SubjectPublicKeyInfo,
    )
    pem = serialization.Encoding.PEM.encode if False else None  # unused

    b64 = __import__("base64").b64encode(spki).decode()
    lines = [b64[i:i + 64] for i in range(0, len(b64), 64)]
    print("-----BEGIN PUBLIC KEY-----")
    for ln in lines:
        print(ln)
    print("-----END PUBLIC KEY-----")

    print("# subject: " + str(cert.subject.rfc4514_string()), file=sys.stderr)
    print("# alpn: " + str(conn.selected_alpn_protocol()), file=sys.stderr)
    conn.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
