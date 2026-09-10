# WarpGate-Go：单二进制镜像（对比 Node 版 node:20-slim+curl ~230MB，本镜像 ~15MB）
# 构建上下文 = 仓库根目录：docker build -f warpgate-go/Dockerfile -t zhoushu1/warp-pool-go:1.0 .
FROM golang:1.26-alpine AS build
WORKDIR /src
COPY warpgate-go/go.mod warpgate-go/go.sum ./
RUN go mod download
COPY warpgate-go/ ./
RUN CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags="-s -w" -o /out/warpgate .

FROM alpine:3.20
RUN apk add --no-cache curl ca-certificates && adduser -D -u 1000 warp
COPY --from=build /out/warpgate /app/warpgate
COPY public/ /app/public/
WORKDIR /app
VOLUME ["/root/warp-instances"]
EXPOSE 4433 10010-10029
USER warp
ENTRYPOINT ["/app/warpgate", "--config", "/app/config.json"]
