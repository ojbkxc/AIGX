# AIGX Windows Docker 镜像 - Windows AMD64 (生产环境)
# 基于 mcr.microsoft.com/powershell 的轻量级镜像

# 构建阶段 - Rust 后端 (Windows AMD64)
FROM mcr.microsoft.com/mirror/docker/library/rust:1.75-windowsservercore-ltsc2022 AS builder-win-amd64

WORKDIR /app

# 安装包管理和编译依赖
RUN powershell -Command "choco install -y pkg-config gnu-mach-tools gawk tar make mingw-w64-x86_64-toolchain" || true

# 复制并构建
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo "fn main() { println!(\"Build verify\"); }" > src/main.rs && \
    cargo build --release --frozen

# 清理测试代码
RUN rm src/main.rs

# 生产环境基础镜像 - Windows 优化
FROM mcr.microsoft.com/mirror/docker/library/powershell:windowsservercore-ltsc2022

WORKDIR /app

# 安装运行时依赖
RUN powershell -Command "choco install -y curl wget" || true

# 从构建阶段复制二进制文件
COPY --from=builder-win-amd64 /app/target/release/aigx.exe /app/aigx.exe

# 创建文件夹
RUN powershell -Command "New-Item -ItemType Directory -Force -Path 'data', 'logs', 'config'" || true

# 设置用户
RUN net user AIGX hx7J@!x3q * /add && \
    net localgroup Administrators AIGX /add

USER AIGX

# 创建数据目录
VOLUME ["C:\\app\\data", "C:\\app\\logs"]

# 暴露端口
EXPOSE 9527

# 健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 \
    CMD powershell -Command "statusCode = Invoke-WebRequest -Uri 'http://localhost:9527/healthz' -UseBasicParsing | Select-Object -ExpandProperty StatusCode; if ($statusCode -ne 200) { exit 1 }" || exit 1

# 启动命令
ENTRYPOINT ["C:\\app\\aigx.exe"]