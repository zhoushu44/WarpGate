// Package logx 提供与 Node 版 logger.js 语义一致的分级日志：[级别][TAG] 消息。
package logx

import (
	"fmt"
	"os"
	"strings"
	"sync"
	"time"
)

type Level int

const (
	Debug Level = iota
	Info
	Warn
	Error
)

var (
	mu       sync.Mutex
	minLevel = Info
)

// SetLevel 按 Node 版 log_level 字符串设置全局级别。
func SetLevel(s string) {
	switch strings.ToLower(s) {
	case "debug":
		minLevel = Debug
	case "", "info":
		minLevel = Info
	case "warn", "warning":
		minLevel = Warn
	case "error":
		minLevel = Error
	}
}

func enabled(l Level) bool { return l >= minLevel }

func emit(l Level, tag, format string, args ...any) {
	if !enabled(l) {
		return
	}
	name := map[Level]string{Debug: "DEBUG", Info: "INFO", Warn: "WARN", Error: "ERROR"}[l]
	line := fmt.Sprintf("%s [%s][%s] %s\n", time.Now().Format("2006-01-02 15:04:05"), name, tag, fmt.Sprintf(format, args...))
	mu.Lock()
	_, _ = os.Stderr.WriteString(line)
	mu.Unlock()
}

func Debugf(tag, format string, args ...any) { emit(Debug, tag, format, args...) }
func Infof(tag, format string, args ...any)  { emit(Info, tag, format, args...) }
func Warnf(tag, format string, args ...any)  { emit(Warn, tag, format, args...) }
func Errorf(tag, format string, args ...any) { emit(Error, tag, format, args...) }
