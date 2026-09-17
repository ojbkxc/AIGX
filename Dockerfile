# AIGX — 多阶段构建：前端 → 后端 → 精简运行镜像
# 运行时：alpine（musl 静态二进制 + 前端产物 + 数据卷）

# ── 阶段 1：前端构建 ──────────────────────────────────────────
FROM node:22-alpine AS frontend
WORKDIR /app/frontend
COPY frontend/package*.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ── 阶段 2：Rust 后端构建（musl 静态链接）────────────────────
FROM rust:1-alpine AS backend
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY aigx-net ./aigx-net
# 前端产物在编译期不嵌入（运行时读取 ./static），仅需编译 Rust
#
# 编译期加固：不可执行栈(noexecstack)；musl 目标默认即静态链接
# （rust:1-alpine 的 host triple 是 musl），无需显式 crt-static——
# 显式设置会连同 proc-macro（async-trait 等）也按 musl 目标编译，
# 而 proc-macro 必须跑在构建宿主上，直接报 "cannot produce proc-macro"。
# CFLAGS 让 cc crate 编译 bundled SQLite(C 代码) 时开启栈保护。
ENV RUSTFLAGS="-C link-arg=-Wl,-z,noexecstack" \
    CFLAGS="-fstack-protector-strong"
# postgres 特性仅是 SeaORM 的编译期 feature 开关，不引入额外运行时成本；
# 与 build.yml 的 linux-amd64 产物特性集保持一致，避免镜像缺 postgres 后端。
RUN cargo build --release --locked --features "sqlite-kv,postgres"

# ── 阶段 3：运行镜像 ──────────────────────────────────────────
FROM alpine:3.20
# ca-certificates：rustls 校验上游 HTTPS 证书必需（缺省会拒绝所有 TLS 连接，
# 表现为渠道模型拉取/对话全部失败）；tzdata：日志时间戳本地化
RUN apk add --no-cache ca-certificates tzdata
RUN addgroup -S aigx && adduser -S aigx -G aigx
WORKDIR /opt/aigx
COPY --from=backend /app/target/release/aigx /opt/aigx/aigx
COPY --from=frontend /app/static /opt/aigx/static
# 数据目录（config.toml / SQLite KV）由挂载卷或匿名卷持久化
RUN mkdir -p /home/aigx/.aigx && chown -R aigx:aigx /opt/aigx /home/aigx/.aigx
# 容器内绑定 0.0.0.0（默认 127.0.0.1 无法从宿主机访问），端口可用
# AIGX_SERVER__PORT 覆盖；data_dir 固定到卷内路径
ENV AIGX_SERVER__HOST=0.0.0.0 \
    AIGX_SERVER__DATA_DIR=/home/aigx/.aigx
USER aigx
VOLUME ["/home/aigx/.aigx"]
# 与 config.rs default_port() 一致；对外映射由 -p 决定
EXPOSE 8080
ENTRYPOINT ["/opt/aigx/aigx"]
