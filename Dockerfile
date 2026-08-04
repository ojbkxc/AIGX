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
# Stage 2: 构建 Rust 后端
# ============================================================
FROM rust:alpine AS backend-builder

RUN apk add --no-cache musl-dev make

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release

# ============================================================
# Stage 3: 运行阶段
# ============================================================
FROM alpine:3.19

RUN apk add --no-cache ca-certificates tzdata

WORKDIR /app

# 复制后端二进制
COPY --from=backend-builder /app/target/release/aigx .

# 复制前端静态文件（vite outDir: ../static）
COPY --from=frontend-builder /static ./static

EXPOSE 8080

VOLUME ["/root/.aigx"]

CMD ["./aigx"]
