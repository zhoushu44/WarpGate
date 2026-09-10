// Package store 管理实例目录（warp-<i>）与 meta.json，
// 字段与 Node 版保持可互换。
package store

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// Meta 与 Node 版 meta.json 兼容（JS 侧按字段动态读取）。
type Meta struct {
	Index     int     `json:"index"`
	Name      string  `json:"name"`
	PID       int     `json:"pid"`        // 嵌入版无子进程，恒为 0
	EnginePID int     `json:"engine_pid"` // 同上
	Transport string  `json:"transport"`
	Endpoint  *string `json:"endpoint"`
	IPv6      string  `json:"ipv6"`
	Status    string  `json:"status"`
	CreatedAt int64   `json:"created_at"` // 毫秒时间戳（Node Date.now()）
	Dedicated bool    `json:"dedicated"`
	Remark    string  `json:"dedicated_remark,omitempty"`
	Engine    string  `json:"engine,omitempty"` // "embedded"
}

// Dir 实例目录名（与 Node getInstanceDir 一致）。
func Dir(base string, index int) string {
	return filepath.Join(base, fmt.Sprintf("warp-%d", index))
}

// Name 实例名（与 Node getInstanceName 一致）。
func Name(index int) string {
	return fmt.Sprintf("warp-%d", index)
}

// Exists 实例目录是否存在。
func Exists(base string, index int) bool {
	st, err := os.Stat(Dir(base, index))
	return err == nil && st.IsDir()
}

// ReadMeta 读取 meta.json；不存在返回 nil, nil。
func ReadMeta(dir string) (*Meta, error) {
	raw, err := os.ReadFile(filepath.Join(dir, "meta.json"))
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var m Meta
	if err := json.Unmarshal(raw, &m); err != nil {
		return nil, fmt.Errorf("解析 %s 失败: %w", filepath.Join(dir, "meta.json"), err)
	}
	return &m, nil
}

// WriteMeta 写出 meta.json（两空格缩进，同 Node JSON.stringify(x, null, 2)）。
func WriteMeta(dir string, m *Meta) error {
	data, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(dir, "meta.json"), data, 0o644)
}

// ReadUsqueConf 实例 usque.json 路径（与 Node engineConfigPath 一致）。
func UsqueConfPath(dir string) string {
	return filepath.Join(dir, "usque.json")
}

// Remove 删除整个实例目录。
func Remove(dir string) error {
	return os.RemoveAll(dir)
}
