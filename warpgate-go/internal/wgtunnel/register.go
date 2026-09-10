package wgtunnel

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"encoding/base64"
	"fmt"
	"math/big"
	"time"

	"github.com/Diniboy1123/usque/api"
	"github.com/Diniboy1123/usque/models"
)

// usque internal 包不可跨模块导入，这里复刻其中的常量与密钥/证书逻辑
// （来源：usque internal/consts.go、internal/utils.go，MIT License）。
const (
	l4ConnectSNI      = "consumer-masque-proxy.cloudflareclient.com"
	defaultModel      = "PC"
	defaultLocale     = "en_US"
	defaultEndpointH2 = "162.159.198.2"
)

// generateECKeyPair 生成 P-256 密钥对：
// 返回 (base64(SEC1 DER) 私钥, PKIX DER 公钥)。
// 与 usque internal.GenerateEcKeyPair 输出格式一致。
func generateECKeyPair() (privB64 string, pubDER []byte, err error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return "", nil, err
	}
	sec1, err := x509.MarshalECPrivateKey(key)
	if err != nil {
		return "", nil, err
	}
	pkix, err := x509.MarshalPKIXPublicKey(&key.PublicKey)
	if err != nil {
		return "", nil, err
	}
	return base64.StdEncoding.EncodeToString(sec1), pkix, nil
}

// selfSignedCertDER 复刻 usque internal.GenerateCert：
// 序列号 0、有效期 24h 的自签证书，仅用于 MASQUE 握手。
func selfSignedCertDER(priv *ecdsa.PrivateKey) ([][]byte, error) {
	der, err := x509.CreateCertificate(rand.Reader,
		&x509.Certificate{
			SerialNumber: big.NewInt(0),
			NotBefore:    time.Now(),
			NotAfter:     time.Now().Add(24 * time.Hour),
		},
		&x509.Certificate{}, &priv.PublicKey, priv)
	if err != nil {
		return nil, err
	}
	return [][]byte{der}, nil
}

// RegisterAccount 向 Cloudflare 注册全新匿名 WARP 账号并登记 MASQUE 设备密钥，
// 返回可直接落盘的 usque.json 配置（等价于 `usque register -a`）。
//
// jwt 为团队模式 token（config.team_token），匿名注册传空。
func RegisterAccount(deviceName, jwt string) (*UsqueConfig, error) {
	// acceptTos 必须为 true，跳过 usque 的交互式 TOS 确认
	account, err := api.Register(defaultModel, defaultLocale, jwt, true)
	if err != nil {
		return nil, fmt.Errorf("Cloudflare 注册失败: %w", err)
	}

	privB64, pubDER, err := generateECKeyPair()
	if err != nil {
		return nil, fmt.Errorf("生成设备密钥失败: %w", err)
	}

	updated, err := api.EnrollKey(account.ID, account.Token, pubDER, deviceName)
	if err != nil {
		return nil, fmt.Errorf("登记 MASQUE 密钥失败: %w", err)
	}

	if len(updated.Config.Peers) == 0 {
		return nil, fmt.Errorf("注册响应缺少 peers")
	}
	peer := updated.Config.Peers[0]

	cfg := &UsqueConfig{
		PrivateKey: privB64,
		// usque register 的处理：v4 去掉 ":0" 后缀，v6 去掉 "[...]:0" 的方括号与端口
		EndpointV4:     stringsTrimSuffix(peer.Endpoint.V4, ":0"),
		EndpointV6:     stringsTrimBracketsPort(peer.Endpoint.V6),
		EndpointH2V4:   defaultEndpointH2,
		EndpointPubKey: peer.PublicKey,
		ID:             updated.ID,
		AccessToken:    account.Token,
		IPv4:           updated.Config.Interface.Addresses.V4,
		IPv6:           updated.Config.Interface.Addresses.V6,
	}
	if cfg.IPv6 == "" {
		return nil, fmt.Errorf("注册响应未分配 IPv6")
	}
	return cfg, nil
}

func stringsTrimSuffix(s, suffix string) string {
	if len(s) > len(suffix) && s[len(s)-len(suffix):] == suffix {
		return s[:len(s)-len(suffix)]
	}
	return s
}

// "[2606:4700:...]:0" -> "2606:4700:..."
func stringsTrimBracketsPort(s string) string {
	if len(s) >= 4 && s[0] == '[' {
		if idx := indexByte(s, ']'); idx > 0 {
			return s[1:idx]
		}
	}
	return s
}

func indexByte(s string, b byte) int {
	for i := 0; i < len(s); i++ {
		if s[i] == b {
			return i
		}
	}
	return -1
}

// AccountInfo 摘要（日志/展示用）。
func AccountInfo(c *UsqueConfig) string {
	return fmt.Sprintf("id=%s ipv6=%s", c.ID, c.IPv6)
}

var _ = models.AccountData{} // 保持对 models 的显式依赖说明
