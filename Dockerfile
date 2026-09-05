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

# 默认配置：容器内必须监听 0.0.0.0，否则端口映射失效
# （应用配置来自 ~/.aigx/config.toml，未提供环境变量覆盖）
RUN mkdir -p /app/.aigx && \
    printf '[server]\nhost = "0.0.0.0"\nport = 8080\n' > /app/.aigx/config.toml && \
    chown -R aigx:nogroup /app/.aigx

USER aigx

# 确保 HOME 指向 aigx 用户目录（配置与数据均位于 /app/.aigx）
ENV HOME=/app

# 暴露端口（与 Rust 默认监听端口一致，可由 ~/.aigx/config.toml 覆盖）
EXPOSE 8080

# 健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --spider -q http://localhost:8080/livez || exit 1

# 启动应用
CMD ["./aigx"]
