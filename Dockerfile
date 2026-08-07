# syntax=docker/dockerfile:1.7
#
# 多阶段构建 AIGX 网关：
#   1) frontend-builder  — Vite 打包前端静态资源
#   2) backend-builder   — Cargo release 构建静态链接的 musl 二进制
#   3) runtime           — Alpine 精简运行时，非 root 用户 + tini 信号转发
#
# BuildKit 是必需的（--mount=type=cache 依赖它）。新版 Docker Desktop / CE
# 默认启用；旧客户端请使用：DOCKER_BUILDKIT=1 docker build -t aigx:dev .
#
# 构建：
#   docker build -t aigx:dev .
#
# 运行（数据持久化到命名卷 aigx-data，挂载到 /data）：
#   docker run --rm -p 8080:8080 -v aigx-data:/data aigx:dev

# ============================================================
# Stage 1: 构建前端
# ============================================================
FROM node:20-alpine AS frontend-builder

WORKDIR /app
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ .
RUN npm run build

# ============================================================
# Stage 2: 构建 Rust 后端（静态链接 musl）
# ============================================================
FROM rust:alpine AS backend-builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

# 构建静态链接的 Linux amd64 二进制（rust:alpine 默认即 musl 目标）。
# BuildKit cache mounts 跨构建缓存 cargo registry 与 target 目录，
# 显著加速增量构建（仅源码变更时复用已编译依赖）。
# 二进制 cp 到 /usr/local/bin/aigx，避免 target/ 被 cache mount 抹掉。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release \
    && cp target/release/aigx /usr/local/bin/aigx

# ============================================================
# Stage 3: 运行阶段
# ============================================================
FROM alpine:3.19

# tini 负责 PID 1 信号转发，确保容器收到 SIGTERM 时能优雅关闭；
# ca-certificates/tzdata 是 HTTPS 请求与时区所需。
RUN apk add --no-cache ca-certificates tzdata tini

# 创建非 root 用户（uid/gid 10001），home 设为 /app，
# 同时准备 /data 数据目录与 /app/.aigx 配置目录并 chown 给 aigx。
# 默认配置文件指向 /data，使数据持久化到 VOLUME /data 而非用户 home。
RUN addgroup -g 10001 -S aigx \
    && adduser -u 10001 -S -G aigx -h /app aigx \
    && mkdir -p /data /app/.aigx \
    && printf '[server]\ndata_dir = "/data"\n' > /app/.aigx/config.toml \
    && chown -R aigx:aigx /app /data

WORKDIR /app

# 复制后端二进制（从 builder 阶段的 /usr/local/bin 取，避开 cache mount 的 target/）
COPY --from=backend-builder /usr/local/bin/aigx ./aigx

# 复制前端静态文件（vite outDir 配置为 ../static，所以构建产物在 /static）
COPY --from=frontend-builder /static ./static

EXPOSE 8080

# 数据目录挂载点（非 root 用户可写）
VOLUME ["/data"]

# 切换到非 root 用户运行
USER aigx

# tini 作为 PID 1 转发信号，再 exec aigx 二进制
ENTRYPOINT ["/sbin/tini", "--", "./aigx"]
