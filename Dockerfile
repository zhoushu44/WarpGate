# WARP 代理池 - Docker 化镜像
# 构建上下文 = /root/warp（含 server.js、logger.js、public、package*.json 等）
# 镜像自包含 sing-box/wgcf，可 docker save/load 迁移到任意 x86_64 Linux 机器
FROM node:20-slim

# 基础工具：健康检查/注册 API/风控检测均依赖 curl；unzip 用于解压 usque 发布包
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates unzip \
 && rm -rf /var/lib/apt/lists/*

# sing-box（与宿主机生产版本 1.14.0-beta.3 保持一致；换版本改此 ARG 并重新构建）
ARG SINGBOX_TAG=v1.14.0-beta.3
RUN set -eux; \
    curl -fsSL -o /tmp/sb.tar.gz \
      "https://github.com/SagerNet/sing-box/releases/download/${SINGBOX_TAG}/sing-box-${SINGBOX_TAG#v}-linux-amd64.tar.gz"; \
    tar -xzf /tmp/sb.tar.gz -C /tmp; \
    mv "/tmp/sing-box-${SINGBOX_TAG#v}-linux-amd64/sing-box" /usr/local/bin/sing-box; \
    chmod +x /usr/local/bin/sing-box; \
    rm -rf /tmp/sb.tar.gz /tmp/sing-box-*; \
    sing-box version | head -1

# wgcf（WARP 账号注册回退路径，v2.2.32 单文件二进制）
ARG WGCF_TAG=v2.2.32
RUN set -eux; \
    curl -fsSL -o /usr/local/bin/wgcf \
      "https://github.com/ViRb3/wgcf/releases/download/${WGCF_TAG}/wgcf_${WGCF_TAG#v}_linux_amd64"; \
    chmod +x /usr/local/bin/wgcf

# usque（MASQUE 引擎，warp_transport=masque 时每个实例用它建 MASQUE 隧道）
ARG USQUE_TAG=v4.2.1
RUN set -eux; \
    curl -fsSL -o /tmp/usque.zip \
      "https://github.com/Diniboy1123/usque/releases/download/${USQUE_TAG}/usque_${USQUE_TAG#v}_linux_amd64.zip"; \
    mkdir -p /tmp/usque && unzip -o /tmp/usque.zip -d /tmp/usque; \
    install -m 0755 /tmp/usque/usque /usr/local/bin/usque; \
    rm -rf /tmp/usque.zip /tmp/usque; \
    usque version | head -2

# 应用代码
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm install --omit=dev && npm cache clean --force
COPY server.js logger.js config.json ./
COPY public ./public

# 实例数据目录（运行期挂载宿主机 /root/warp-instances）
VOLUME ["/root/warp-instances"]

EXPOSE 4433 10010-10029
CMD ["node", "server.js"]
