// Minimal HTTP/2 CONNECT-IP probe against Cloudflare.
//
// Shows what the server actually replies, which is the only way to debug the
// Rust client: hyper reports "http2 error" with no detail, while Go prints the
// status, headers and any GOAWAY frame.
package main

import (
	"context"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"os"
	"time"
)

func main() {
	privB64 := os.Args[1]

	privDer, err := base64.StdEncoding.DecodeString(privB64)
	if err != nil {
		fmt.Println("b64:", err)
		return
	}
	priv, err := x509.ParseECPrivateKey(privDer)
	if err != nil {
		fmt.Println("key:", err)
		return
	}

	tmpl := x509.Certificate{
		SerialNumber: big.NewInt(0),
		NotBefore:    time.Now(),
		NotAfter:     time.Now().Add(24 * time.Hour),
	}
	certDer, err := x509.CreateCertificate(rand.Reader, &tmpl, &tmpl, &priv.PublicKey, priv)
	if err != nil {
		fmt.Println("cert:", err)
		return
	}

	tlsCfg := &tls.Config{
		Certificates:       []tls.Certificate{{Certificate: [][]byte{certDer}, PrivateKey: priv}},
		InsecureSkipVerify: true,
		NextProtos:         []string{"h2"},
		ServerName:         "consumer-masque.cloudflareclient.com",
	}

	client := &http.Client{
		Timeout: 20 * time.Second,
		Transport: &http.Transport{
			TLSClientConfig:  tlsCfg,
			ForceAttemptHTTP2: true,
			DialTLSContext: func(ctx context.Context, network, _ string) (net.Conn, error) {
				d := &net.Dialer{Timeout: 10 * time.Second}
				return tls.DialWithDialer(d, network, "162.159.198.2:443", tlsCfg)
			},
		},
	}

	req, _ := http.NewRequest("CONNECT", "https://cloudflareaccess.com", nil)
	req.Host = "cloudflareaccess.com"
	req.Header.Set("cf-connect-proto", "cf-connect-ip")
	req.Header.Set("pq-enabled", "false")
	req.Header.Set("Capsule-Protocol", "?1")

	resp, err := client.Do(req)
	if err != nil {
		fmt.Println("REQUEST ERROR:", err)
		return
	}
	defer resp.Body.Close()

	fmt.Printf("STATUS: %d %s\n", resp.StatusCode, resp.Status)
	fmt.Printf("PROTO: %s\n", resp.Proto)
	for k, v := range resp.Header {
		fmt.Printf("  %s: %v\n", k, v)
	}
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 512))
	fmt.Printf("BODY(%d): %x\n", len(body), body)
}
