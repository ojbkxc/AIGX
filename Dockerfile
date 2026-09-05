# AIGX 完整构建 Dockerfile
# ─────────────────────────────────────────────────────────────
# 多阶段构建：Rust 后端（workspace：主网关 + aigx-net）→ 前端（可选）→ 生产镜像
# 构建上下文为仓库根目录；--platform 由 buildx 传入。

# ── 构建阶段：Rust workspace（编译主网关与 aigx-net） ─────────
FROM rust:1-slim AS builder

WORKDIR /app

# 安装构建依赖（pkg-config + openssl；sqlite 走 bundled feature 无需系统库）
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# 先复制清单文件以利用 Docker 层缓存
COPY Cargo.toml Cargo.lock ./
COPY aigx-net/Cargo.toml ./aigx-net/Cargo.toml

# 源码（清单缓存失效时才会重新复制）
COPY src ./src
COPY aigx-net/src ./aigx-net/src

# 编译 release（默认 features：sqlite-kv）
RUN cargo build --release --bin aigx && \
    strip target/release/aigx

# ── 生产环境基础镜像 ─────────────────────────────────────────
FROM debian:bookworm-slim

WORKDIR /app

# 安装运行时依赖
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    libgcc-s1 \
    libstdc++6 \
    libc6 \
    wget \
    curl \
    && rm -rf /var/lib/apt/lists/*

# 从构建阶段复制二进制文件
COPY --from=builder /app/target/release/aigx /app/aigx

# 创建非特权用户与数据目录
RUN useradd -r -u 1000 -g nogroup -s /sbin/nologin -d /app aigx && \
    mkdir -p /app/data /app/logs /app/config && \
    chown -R aigx:nogroup /app

USER aigx

# 暴露端口
EXPOSE 9527

# 健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --spider -q http://localhost:9527/healthz || exit 1

# 启动应用
CMD ["./aigx"]
