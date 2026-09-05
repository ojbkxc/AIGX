# AIGX Docker 镜像 - Linux ARM64 (生产环境)
# 优化的 ARM64 镜像，平衡安全性和性能

# 构建阶段 - Rust 后端 (ARM64)
FROM mcr.microsoft.com/mirror/docker/library/rust:1.75-bookworm AS builder-arm64

WORKDIR /app

# 安装包管理和编译依赖 (ARM64)
RUN apt-get update && \
    apt-get install -y \
    pkg-config \
    libssl-dev \
    libsqlite3-dev \
    libjemalloc-dev \
    gcc \
    g++ \
    curl \
    wget \
    && rm -rf /var/lib/apt/lists/*

# 复制并构建
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo "fn main() { println!(\"Build verify\"); }" > src/main.rs && \
    cargo build --release --frozen

# 清理测试代码
RUN rm src/main.rs

# 生产环境基础镜像 - ARM64 优化
FROM debian:bookworm-slim

WORKDIR /app

# 安装运行时依赖
RUN apt-get Update && \
    apt-get install -y \
    ca-certificates \
    libssl3 \
    libgcc1 \
    libstdc++6 \
    libc6 \
    libsqlite3-0 \
    wget \
    curl \
    && rm -rf /var/lib/apt/lists/*

# 从构建阶段复制二进制文件
COPY --from=builder-arm64 /app/target/release/aigx /app/aigx

# 创建用户
RUN useradd -r -u 1000 -g nogroup -s /sbin/nologin -d /app aigx && \
    groupadd -g 1001 data && \
    mkdir -p /app/data /app/logs /app/config && \
    chown -R aigx:data /app

USER aigx

# 创建数据目录
VOLUME ["/app/data", "/app/logs", "/app/config"]

# 暴露端口
EXPOSE 9527

# 健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:9527/healthz || exit 1

# 启动命令
CMD ["/app/aigx"]