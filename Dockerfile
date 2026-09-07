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
RUN cargo build --release --locked

# ── 阶段 3：运行镜像 ──────────────────────────────────────────
FROM alpine:3.20
RUN addgroup -S aigx && adduser -S aigx -G aigx
WORKDIR /opt/aigx
COPY --from=backend /app/target/release/aigx /opt/aigx/aigx
COPY --from=frontend /app/static /opt/aigx/static
# 数据目录（config.toml / SQLite KV）由挂载卷或匿名卷持久化
RUN mkdir -p /home/aigx/.aigx && chown -R aigx:aigx /opt/aigx /home/aigx/.aigx
USER aigx
VOLUME ["/home/aigx/.aigx"]
EXPOSE 9527
ENTRYPOINT ["/opt/aigx/aigx"]
