# Node.js 构建阶段 - 多平台支持
FROM node:18-alpine AS builder

WORKDIR /app

# 复制 package.json 和锁文件
COPY package.json package-lock.json ./

# 安装所有依赖（包括开发依赖）
RUN npm ci

# 复制源代码
COPY . .

# 构建生产版本
RUN npm run build

# 生产环境基础镜像
FROM nginx:alpine

WORKDIR /app

# 复制构建产物
COPY --from=builder /app/dist /usr/share/nginx/html

# 复制自定义 nginx 配置
COPY nginx.conf /etc/nginx/nginx.conf

# 复制静态资源
COPY favicon.ico /usr/share/nginx/html/

# 复制公共资源
COPY assets /usr/share/nginx/html/assets 2>/dev/null || true

# 暴露端口
EXPOSE 80

# 设置健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=40s --retries=3 \
    CMD wget --spider -q http://localhost:|| return 1

# 切换到非 root 用户（可选，提高安全性）
RUN addgroup -g 1001 -S nginx && \
    adduser -D -u 1001 -S nginx -G nginx && \
    chown -R nginx:nginx /usr/share/nginx/html && \
    chown -R nginx:nginx /etc/nginx

USER nginx

# 启动 nginx
CMD ["nginx", "-daemon", "off"]