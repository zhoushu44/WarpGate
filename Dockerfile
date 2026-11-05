# WarpGate — Rust 原生 MASQUE 实现的 WARP 代理池
#
# 多阶段构建：Rust 编译 → 精简运行镜像。
#
# 设计要点：
#   * 运行时不需要 Node、npm、sing-box。数据面是 Rust 原生 CONNECT-IP
#     （crates/wg-tunnel）。
#   * 保留 usque：UDP 443 到 Cloudflare 在多数机房被封，原生 QUIC 无法
#     握手，WARP_NATIVE=quic 时必须走 usque 子进程。
#   * 基于 debian:bookworm-slim 而非 distroless：需要扫 /proc 清理孤儿
#     引擎进程（v0.10.0 的修复）和 shell 做健康检查。
#
# 构建上下文 = 仓库根目录（warpgate-rs/ 是 Cargo workspace）。

# ---------- builder ----------
# 跟随最新稳定版：Cargo.lock 里的 block-buffer 0.12 需要 edition2024，
# 固定 1.83 会报 "feature edition2024 is required" 而构建失败。
FROM rust:1-bookworm AS builder

WORKDIR /src

# 先拷清单再拷源码，让依赖层单独缓存：改业务代码不必重下 crates。
COPY warpgate-rs/Cargo.toml warpgate-rs/Cargo.lock ./
COPY warpgate-rs/crates ./crates
RUN cargo build --release --bin warpgate \
 && strip target/release/warpgate

# ---------- runtime ----------
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl unzip tini \
 && rm -rf /var/lib/apt/lists/*

# usque：MASQUE 引擎，warp_transport=masque + WARP_NATIVE=quic 时每个实例用。
# 版本与宿主机生产一致。
ARG USQUE_TAG=v4.2.1
RUN set -eux; \
    curl -fsSL -o /tmp/usque.zip \
      "https://github.com/Diniboy1123/usque/releases/download/${USQUE_TAG}/usque_${USQUE_TAG#v}_linux_amd64.zip"; \
    mkdir -p /tmp/usque && unzip -o /tmp/usque.zip -d /tmp/usque; \
    install -m 0755 /tmp/usque/usque /usr/local/bin/usque; \
    rm -rf /tmp/usque.zip /tmp/usque; \
    usque version | head -2

WORKDIR /app
COPY --from=builder /src/target/release/warpgate /usr/local/bin/warpgate
COPY public ./public

# 实例数据与配置运行期挂载（见 docker-compose.yml）。
VOLUME ["/root/warpgate-rs/instances"]

# 管理台 4433；实例端口段 = base_port ~ base_port+resident_count-1
EXPOSE 4433

# tini 回收 usque 子进程，避免僵尸堆积。
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["warpgate", "--config", "/app/config.json", "--instances", "/root/warpgate-rs/instances"]
