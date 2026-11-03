# CONNECT-IP over HTTP/2 — what usque actually sends, and what we got wrong

Source studied: `github.com/Diniboy1123/usque` @ `6aa03fc` (v4.2.1, the exact
build running on 45.205.27.41) and its dependency
`github.com/Diniboy1123/connect-ip-go` @ `66cba32`.

Everything below was measured against the live edge from the deployment host,
not inferred from the RFC. Where the RFC and the endpoint disagree, the endpoint
wins and the divergence is called out.

---

## 1. The request, byte for byte

`connect-ip-go/client_h2.go:32-51` builds it:

```go
pr, pw := io.Pipe()
req, err := http.NewRequestWithContext(ctx, http.MethodConnect, u.String(), pr)
req.Host = authorityFromURL(u)   // "cloudflareaccess.com:443"
req.ContentLength = -1
req.Header = make(http.Header)
for k, v := range additionalHeaders {   // from usque api/masque.go:121-124
    req.Header[k] = v                  //   cf-connect-proto: cf-connect-ip
}                                      //   pq-enabled: false
                                       //   user-agent: ""
rsp, err := client.Do(req)
```

`authorityFromURL` (`client_h2.go:67-76`) appends `:443` when the URI carries no
port. That is why `:authority` is `cloudflareaccess.com:443`, not the bare host.

Captured on the wire (Go's HPACK, decoded):

```
:authority: cloudflareaccess.com:443      <- first, and with the port
:method:    CONNECT
cf-connect-proto: cf-connect-ip
pq-enabled:       false
accept-encoding:  gzip
```

**There is no `:protocol`, no `:scheme`, no `:path`, and no `capsule-protocol`.**
The header order Go happens to produce is an artefact of its map iteration; the
edge does not care about order, only that pseudo-headers precede regular ones.

### `:protocol` — the single biggest wrong assumption

`connect-ip-go`'s HTTP/3 path passes `requestProtocol = "cf-connect-ip"` into
the HTTP/3 request's `:protocol` (`client.go:49-54`), but the **HTTP/2 path never
sets `:protocol` at all**. It is a plain CONNECT differentiated only by the
`cf-connect-proto` header.

And the edge does not advertise `SETTINGS_ENABLE_CONNECT_PROTOCOL`:

```
server SETTING 0x3 (MAX_CONCURRENT_STREAMS) = 256
server SETTING 0x4 (INITIAL_WINDOW_SIZE)    = 1048576
server SETTING 0x5 (MAX_FRAME_SIZE)         = 16384
server SETTING 0x6 (MAX_HEADER_LIST_SIZE)   = 131072
```

No `0x8`. Per RFC 8441 §3 a `:protocol` request against a peer that did not
advertise `0x8` is a protocol violation, and the edge enforces it:
`RST_STREAM(PROTOCOL_ERROR)`, no HTTP status. Sending `SETTINGS_ENABLE_CONNECT_PROTOCOL`
in *our* SETTINGS changes nothing — the client advertising it is meaningless.

### Is `capsule-protocol` needed?

**No** on HTTP/2. We sent the plain-CONNECT set with and without it, 10 times
each, on connections that presented the MASQUE certificate:

```
with capsule-protocol:    11 x 200, 0 failed
without capsule-protocol:  9 x 200, 0 failed
```

It *is* mandatory on the HTTP/3 path (`connect-ip-go/request.go:71-95` rejects a
request without it). Adding it on h2 is harmless, so we keep it for symmetry —
but it is not what gates the request.

### What *is* mandatory

```
no cf-connect-proto  -> 10/10 RST_STREAM(PROTOCOL_ERROR)
with :protocol       -> 11/11 RST_STREAM(PROTOCOL_ERROR)
```

`cf-connect-proto: cf-connect-ip` is the one header that makes this CONNECT-IP
rather than a tunnel to nowhere.

### SETTINGS we send

Not load-bearing for acceptance — the edge accepted an empty SETTINGS frame —
but the current implementation sends the same shape `x/net/http2` does:

| ID | Value | Meaning |
|----|-------|---------|
| 0x2 | 0 | `ENABLE_PUSH` off |
| 0x4 | 16 MiB | `INITIAL_WINDOW_SIZE` |
| 0x5 | 16384 | `MAX_FRAME_SIZE` |
| 0x6 | 1 MiB | `MAX_HEADER_LIST_SIZE` |

Plus `WINDOW_UPDATE(0, 16 MiB)` right after the preface.

### Is an ORIGIN frame or other extension needed?

No. The trace of a working session is only: client preface + SETTINGS +
WINDOW_UPDATE, server SETTINGS + ACK + WINDOW_UPDATE, then HEADERS/200 and DATA.
Nothing else.

### TLS

`api/masque.go:126-141, 237-238`: the same `*tls.Config` as the HTTP/3 path,
with `NextProtos = ["h2"]`. Client certificate is a **self-signed P-256 cert
with an empty subject** — `internal/utils.go:106-117` passes
`&x509.Certificate{SerialNumber: 0, NotBefore, NotAfter}` and **no** `Subject`,
**no** `DNSNames`, **no** extensions. `InsecureSkipVerify: true` plus a manual
`VerifyPeerCertificate` that compares `ecdsa.PublicKey` values.

Measured: the certificate's subject and SANs are irrelevant. A cert with
`CN=consumer-masque.cloudflareclient.com` + a matching SAN, and one with an
empty subject, both complete the handshake and both get `200`. **Do not pin the
certificate to the SNI** — that idea cost time and was a dead end.

SNI: `consumer-masque.cloudflareclient.com` (`internal/consts.go:6`). This is
correct and was never the problem.

---

## 2. The symptom was lying

```
connect-ip: FAILED io: received fatal alert: CertificateUnknown
```

A TLS alert arriving *after* a successful handshake cannot come from the
CONNECT-IP request: HTTP/2 has no way to make the peer emit a TLS alert. It was
TLS-level client-auth rejection, delivered late.

`nativecheck` reported `tls: OK (client cert accepted)` because its TLS stage
called `masque::build_tls_for_test`, which **minted a fresh random P-256 key**
instead of using the enrolled one. Cloudflare accepts the handshake (it only
checks that *a* certificate was presented) and then rejects the connection once
the request names an account; rustls surfaces that as
`AlertReceived(CertificateUnknown)` on the next read.

Reproduced exactly:

```
enrolled key:   rustls handshake returns OK ... then: silence for 6s (no alert)
unenrolled key: rustls handshake returns OK ... then: READ ERROR AlertReceived(CertificateUnknown)
```

So the CONNECT-IP code path was never reached in that diagnostic. `native.rs`
now uses `build_h2_tls_config(&signing, ...)` with the enrolled key in
`diagnose()` and in `compare_pin()`'s bisection.

---

## 3. The second, independent failure: pinning vs the anycast VIP

`162.159.198.2` is an anycast VIP, and **not every node behind it is a MASQUE
edge**. Over 15 fresh connections:

```
13 x 0421a53b…  CN=masque.cloudflareclient.com   issuer = Cloudflare
 2 x 0475bcc0…  CN=cloudflareclient.com          issuer = Let's Encrypt
```

Correlated with request outcome (24 connections, certificate recorded during the
TLS handshake):

```
CN=masque.cloudflareclient.com  200=21  400=0
CN=cloudflareclient.com         200=0   400=3
```

The second node answers the CONNECT-IP request with `400` and a Cloudflare HTML
error page. The pinning verifier rejects it as `UnknownIssuer` — **correctly**:
it is a different key, and accepting it would be a MITM hole.

This is not our bug. `usque` hits it too, with its own message:

```
Failed to connect tunnel: ... x509: no valid chains built:
remote endpoint has a different public key than what we trust in config.json
```

and it survives because `MaintainTunnel` retries forever at
`--reconnect-delay` (default 1 s). We now do the same, bounded:
`CONNECT_ATTEMPTS = 8`, `CONNECT_RETRY_DELAY = 1 s`, each attempt on a **new TCP
connection** (the backend pick is per-flow, so retrying the same connection can
never help).

The bad-node share drifts with time and load — measured between 3/24 and 8/10
across the session. During the final validation window it was high enough that
`usque` itself failed 3 out of 3 runs, which is why end-to-end numbers taken in
that window are pessimistic. Earlier, with 4 quick retries, ten consecutive
`nativecheck` runs on one account went 10/10 and eight different accounts went
8/8.

---

## 4. Differences found, one line each

| # | Ours (before) | usque | Effect |
|---|---|---|---|
| 1 | `:protocol: connect-ip` | absent | `RST_STREAM(PROTOCOL_ERROR)` every time |
| 2 | `:scheme` + `:path` on a CONNECT | absent | illegal for plain CONNECT; folded into #1 |
| 3 | no `:method`/`:authority` order guarantee | `:authority` first | cosmetic; pseudo-headers only need to precede regular ones |
| 4 | `:authority: cloudflareaccess.com` | `cloudflareaccess.com:443` | 400 on some edges; usque appends the port in `authorityFromURL` |
| 5 | `capsule-protocol: ?1` | absent on h2 | harmless; kept |
| 6 | `pq-enabled: false` | same | none |
| 7 | `user-agent: ""` | same | none |
| 8 | empty SETTINGS, never read server's | full SETTINGS, ACKs, honours window | flow control / desync, not acceptance |
| 9 | capsules framed with **HPACK** integers (7-bit prefix) | QUIC varints (2-bit prefix) | corrupts every packet ≥ 64 bytes |
| 10 | datagram payload had no Context ID handling | Cloudflare h2 omits it; handled explicitly | silent drop |
| 11 | `poll_read` EOF only on `State::Closed` | n/a (Go netstack) | `read_to_end` hung to its timeout on every normal close |
| 12 | client cert `build_tls_for_test` random key | enrolled key | the `CertificateUnknown` red herring |
| 13 | single TLS attempt | infinite retry @1 s | ~12-30 % of bring-ups lost to the non-MASQUE node |

Items 1-4 and 8-10 were fixed in `h2.rs`/`mod.rs` (commit `750ef23`).
Items 11-13 are in this change.

---

## 5. What this change adds

`crates/wg-tunnel/src/connectip/stack.rs` — `poll_read` now treats
`!may_recv()` as EOF and mirrors the peer's FIN. smoltcp leaves a normally-closed
socket in `CLOSE-WAIT`, not `CLOSED`, so the old `state == Closed` test never
fired and every `read_to_end` / `copy_bidirectional` sat until its own timeout
even though all the bytes had arrived. Regression test:
`may_recv_detects_eof_where_state_closed_does_not`.

`crates/wg-tunnel/src/connectip/native.rs` — bounded retry around the whole
handshake, 8 attempts, 1 s apart, fresh TCP connection each time.

Verified after both fixes, on the live edge:

```
tunnel UP
0 api.ipify.org:80   OK   225 bytes  127 ms
1 example.com:80     OK   928 bytes   44 ms
2 api.ipify.org:80   OK   225 bytes  108 ms
3 example.com:80     OK   928 bytes   44 ms
... 8/8 sequential
concurrent 8/8
```

---

## 6. What is still not right

* **The anycast VIP occasionally has no MASQUE backend.** Between 12 % and 80 %
  of new flows land on `CN=cloudflareclient.com` depending on the hour, and that
  node answers `400`. Retrying is the only remedy; it costs up to 8 s of
  bring-up latency in the bad windows. A long-term fix would be to hold the
  tunnel open across reconnects (usque's `--always-reconnect`) rather than
  re-dialling per instance — not attempted here.
* **One `api.ipify.org` dial out of eight still timed out** in the final run even
  on a good tunnel, while its neighbours on the same tunnel succeeded in ~50 ms.
  That is a smoltcp-level issue (SYN retransmit / socket reuse), not the h2
  transport, and it is not fixed.
* **The 1 MB bulk transfer was never confirmed.** Every large-file endpoint
  tried returned an error page through the tunnel (the requests themselves
  worked; the chosen URLs did not serve the file).
* **Bulk numbers are missing**, so sustained throughput is unknown.
