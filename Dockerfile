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
FROM rust:1.97-alpine AS backend-builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

# 构建静态链接的 Linux amd64 二进制
RUN cargo build --release --target x86_64-unknown-linux-musl

# ============================================================
# Stage 3: 运行阶段
# ============================================================
FROM alpine:3.19

RUN apk add --no-cache ca-certificates tzdata

WORKDIR /app

# 复制后端二进制
COPY --from=backend-builder /app/target/x86_64-unknown-linux-musl/release/cf-ai-gw .

# 复制前端静态文件
COPY --from=frontend-builder /app/dist ./static

EXPOSE 8080

VOLUME ["/root/.cf-ai-gw"]

CMD ["./cf-ai-gw"]