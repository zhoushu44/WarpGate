// WarpGate-Go：嵌入式单进程版 WARP 代理池（控制面 + MASQUE 数据面同进程）。
package main

import (
	"context"
	"flag"
	"fmt"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"warpgate/internal/apiserver"
	"warpgate/internal/appconfig"
	"warpgate/internal/logx"
	"warpgate/internal/pool"
)

var version = "1.0.0"

func main() {
	configPath := flag.String("config", "config.json", "config.json 路径")
	instancesDir := flag.String("instances", "", "实例目录（默认 env WARP_INSTANCES_DIR 或 config.instances_dir）")
	showVersion := flag.Bool("version", false, "打印版本")
	flag.Parse()

	if *showVersion {
		fmt.Println("warpgate", version)
		return
	}

	cfg, err := appconfig.Load(*configPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, "启动失败:", err)
		os.Exit(1)
	}
	logx.SetLevel(cfg.LogLevel)
	if cfg.MasqueHTTP2 {
		logx.Warnf("CONFIG", "masque_http2=true 在 v1 中不支持（L4Proxy 仅 QUIC/UDP），已忽略；TCP 443 兜底请暂用 Node 版")
	}
	logx.Infof("BOOT", "实例目录: %s，常驻: %d，端口: %d~%d",
		cfg.InstancesDirResolved(*instancesDir), cfg.ResidentCount,
		cfg.BasePort, cfg.BasePort+cfg.MaxInstances-1)

	_ = os.MkdirAll(cfg.InstancesDirResolved(*instancesDir), 0o755)
	mgr := pool.New(cfg)

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	logx.Infof("BOOT", "开始恢复/创建实例（并发上限 %d）...", cfg.MaxConcurrent)
	bootStart := time.Now()
	mgr.Boot(ctx)
	logx.Infof("BOOT", "实例就绪 %d/%d，耗时 %s",
		mgr.HealthyCount(), cfg.ResidentCount, time.Since(bootStart).Round(time.Second))

	mgr.StartLoops(ctx)

	addr := fmt.Sprintf("%s:%d", cfg.BindAddress, cfg.Port)
	srv := &http.Server{Addr: addr, Handler: apiserver.New(cfg, mgr)}
	go func() {
		logx.Infof("API", "管理台监听 http://%s（管理端口）", addr)
		if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			logx.Errorf("API", "监听失败: %v", err)
			stop()
		}
	}()

	<-ctx.Done()
	logx.Infof("BOOT", "收到退出信号，关闭中...")
	shutCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = srv.Shutdown(shutCtx)
	logx.Infof("BOOT", "已退出（实例账号目录保留，重启后暖恢复）")
}
