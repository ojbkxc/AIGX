# AIGX 完整构建 Dockerfile

# 构建阶段 - Rust 后端（多平台支持）
# 首先为交叉编译安装必要的工具
FROM rust:1.75-slim AS builder-multiplatform

WORKDIR /app

# 多平台构建时安装cross编译工具链
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER=qemu-aarch64-static
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc

RUN apt-get update && \
    apt-get install -y \
    binutils-aarch64-linux-gnu \
    qemu-user-static \
    gcc-aarch64-linux-gnu \
    gcc-x86-64-linux-gnu \
    pkg-config \
    libssl-dev:arm64 \
    libsqlite3-dev:arm64 \
    libjemalloc-dev \
    git \
    wget \
    zlib1g-dev:arm64 \
    && rm -rf /var/lib/apt/lists/*

# 默认x86_64平台构建
FROM builder-multiplatform AS builder-x86_64
FROM builder-multiplatform AS builder-aarch64

# 生产环境基础镜像（多平台）
FROM debian:bookworm-slim

WORKDIR /app

# 安装运行时依赖（arm64和amd64都需要）
RUN apt-get update && \
    apt-get install -y \
    ca-certificates \
    libssl3 \
    libgcc1 \
    libstdc++6 \
    libc6 \
    libsqlite3-0 \
    libjemalloc2 \
    wget \
    curl \
    && rm -rf /var/lib/apt/lists/*

# 从构建阶段复制二进制文件（amd64或aarch64）
COPY --from=builder-x86_64 /app/target/release/aigx /app/aigx
# COPY --from=builder-aarch64 /app/target/release/aigx /app/aigx

# 生产环境基础镜像 - 性能优化
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
    libjemalloc2 \
    wget \
    curl \
    && rm -rf /var/lib/apt/lists/*

# 从构建阶段复制二进制文件
COPY --from=builder /app/target/release/aigx /app/aigx

# 创建用户
RUN useradd -r -u 1000 -g nogroup -s /sbin/nologin -d /app aigx && \
    mkdir -p /app/data /app/logs /app/config && \
    chown -R aigx:aigx /app

USER aigx

# 暴露端口
EXPOSE 9527

# 健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --spider -q http://localhost:9527/healthz || exit 1

# 启动应用
CMD ["./aigx"]
