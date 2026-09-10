// Package wgtunnel 把 usque 的 MASQUE 数据面嵌入本进程：
// 配置文件与 usque CLI 逐字段兼容，隧道用 api.L4Proxy 在进程内维持。
package wgtunnel

import (
	"crypto/ecdsa"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"net"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
)

// UsqueConfig 与 usque CLI 的 usque.json 字段一一对应（见 usque/config/config.go）。
type UsqueConfig struct {
	PrivateKey     string `json:"private_key"` // base64(SEC1 EC 私钥 DER)
	EndpointV4     string `json:"endpoint_v4"` // 无端口后缀
	EndpointV6     string `json:"endpoint_v6"` // 无方括号无端口
	EndpointH2V4   string `json:"endpoint_h2_v4"`
	EndpointH2V6   string `json:"endpoint_h2_v6"`
	EndpointPubKey string `json:"endpoint_pub_key"` // PEM(PKIX)
	ID             string `json:"id"`
	AccessToken    string `json:"access_token"`
	IPv4           string `json:"ipv4"`
	IPv6           string `json:"ipv6"`
}

// LoadUsqueConfig 读取实例目录下的 usque.json。
func LoadUsqueConfig(path string) (*UsqueConfig, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("读取 %s 失败: %w", path, err)
	}
	var c UsqueConfig
	if err := json.Unmarshal(raw, &c); err != nil {
		return nil, fmt.Errorf("解析 %s 失败: %w", path, err)
	}
	if c.PrivateKey == "" || c.EndpointPubKey == "" {
		return nil, fmt.Errorf("%s 缺少 private_key/endpoint_pub_key", path)
	}
	return &c, nil
}

// Save 写出 usque.json（缩进格式与 usque CLI 一致，自动创建目录）。
func (c *UsqueConfig) Save(path string) error {
	data, err := json.MarshalIndent(c, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	return os.WriteFile(path, data, 0o600)
}

// ECPrivateKey 解析 base64 SEC1 私钥（语义同 usque config.GetEcPrivateKey）。
func (c *UsqueConfig) ECPrivateKey() (*ecdsa.PrivateKey, error) {
	der, err := base64.StdEncoding.DecodeString(c.PrivateKey)
	if err != nil {
		return nil, fmt.Errorf("解码私钥失败: %w", err)
	}
	key, err := x509.ParseECPrivateKey(der)
	if err != nil {
		return nil, fmt.Errorf("解析 EC 私钥失败: %w", err)
	}
	return key, nil
}

// EndpointPubKey 解析 PEM(PKIX) 端点公钥（语义同 usque config.GetEcEndpointPublicKey）。
func (c *UsqueConfig) EndpointPubKeyParsed() (*ecdsa.PublicKey, error) {
	block, _ := pem.Decode([]byte(c.EndpointPubKey))
	if block == nil {
		return nil, fmt.Errorf("endpoint_pub_key PEM 解码失败")
	}
	pub, err := x509.ParsePKIXPublicKey(block.Bytes)
	if err != nil {
		return nil, fmt.Errorf("解析端点公钥失败: %w", err)
	}
	ecPub, ok := pub.(*ecdsa.PublicKey)
	if !ok {
		return nil, fmt.Errorf("端点公钥不是 ECDSA 类型")
	}
	return ecPub, nil
}

// SelectUDPEndpoint 返回 QUIC(MASQUE) 的 UDP 端点。
// 兼容三种存储格式：裸 IP、[v6]、ip:port / [v6]:port。
func (c *UsqueConfig) SelectUDPEndpoint(useIPv6 bool, connectPort int) (*net.UDPAddr, error) {
	host := c.EndpointV4
	if useIPv6 {
		host = c.EndpointV6
	}
	if host == "" {
		return nil, fmt.Errorf("usque.json 缺少 endpoint（ipv6=%v）", useIPv6)
	}
	// 已带端口的形式
	if ap, err := netip.ParseAddrPort(host); err == nil {
		return net.UDPAddrFromAddrPort(ap), nil
	}
	host = strings.Trim(host, "[]")
	ip := net.ParseIP(host)
	if ip == nil {
		return nil, fmt.Errorf("endpoint %q 不是合法地址", host)
	}
	return &net.UDPAddr{IP: ip, Port: connectPort}, nil
}
