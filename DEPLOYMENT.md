# AIGX 部署指南

本文档提供 AIGX 的完整部署方案，支持多平台部署和高度可定制化配置。

## 📋 目录

- [系统要求](#系统要求)
- [快速开始](#快速开始)
- [Docker 部署](#docker-部署)
- [二进制部署](#二进制部署)
- [Kubernetes 部署](#kubernetes-部署)
- [性能优化](#性能优化)
- [监控告警](#监控告警)
- [故障排查](#故障排查)

## 系统要求

### 软件要求

| 组件 | 最低版本 | 推荐版本 |
|------|----------|----------|
| Docker | 20.10 | 24+ |
| Node.js | 18.x | 20.x |
| Rust | 1.75+ | Latest |
| PostgreSQL | 14+ | 15+ |
| Redis | 6.2+ | 7+ |

### 硬件要求

| 节点类型 | CPU | 内存 | 存储 | 网络 |
|----------|-----|------|------|------|
| 开发/测试 | 4核 | 8GB | 50GB SSD | 1Gbps |
| 生产节点 | 8核 | 16GB | 200GB SSD | 10Gbps |
| 高负载节点 | 16核 | 32GB | 500GB SSD | 10Gbps |

## 快速开始

### 方式一：Docker 部署（推荐）

```bash
# 克隆仓库
git clone https://github.com/yourusername/aigx.git
cd aigx

# 构建镜像
make all-docker

# 启动服务
docker-compose up -d

# 查看日志
docker-compose logs -f aigx

# 访问管理面板
open http://localhost:3000
```

### 方式二：二进制部署

```bash
# Linux (本地下载或编译)
wget https://github.com/yourusername/aigx/releases/latest/aigx-linux-x86_64
chmod +x aigx-linux-x86_64

# 运行
./aigx-linux-x86_64

# 使用系统服务
sudo cp aigx-linux-x86_64 /usr/local/bin/aigx
sudo systemctl enable aigx
sudo systemctl start aigx
```

## Docker 部署

### 镜像构建

#### 多平台构建

```bash
# 使用 Makefile 构建
make all-docker

# 使用脚本构建
bash docker-build.sh all

# 指定平台
PLATFORMS="linux/arm64" bash docker-build.sh all
```

#### 本地 AMD64 构建

```bash
make backend-local-amd64
```

#### 本地 ARM64 构建

```bash
make backend-local-arm64
```

### 镜像系统要求

| 平台 | 城市 | 基础镜像 | 大小 |
|------|------|----------|------|
| Linux AMD64 | Debian Bookworm | debian:bookworm-slim | 200MB |
| Linux ARM64 | Debian Bookworm | debian:bookworm-slim | 200MB |
| Linux AMD64 | Alpine | alpine:latest | 150MB |

### 部署配置

```yaml
# docker-compose.yml 配置示例
version: '3.8'

services:
  aigx:
    image: aigx/aigx:latest
    container_name: aigx
    ports:
      - "9527:9527"
    volumes:
      - aigx_data:/aigx/data
      - aigx_logs:/aigx/logs
      - ./config:/aigx/config
    environment:
      - DATABASE_URL=sqlite:///aigx/data/aigx.db
      - RUST_LOG=info
      - # SECRET_KEY=your-secret-key
      - # JWT_SECRET=your-jwt-secret
      - # ADMIN_PASSWORD=admin
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "wget", "--spider", "-q", "http://localhost:9527/healthz"]
      interval: 30s
      timeout: 10s
      retries: 3
    networks:
      - aigx-network

  frontend:
    image: aigx/aigx-frontend:latest
    container_name: aigx-frontend
    ports:
      - "80:80"
    restart: unless-stopped
    networks:
      - aigx-network

volumes:
  aigx_data:
    driver: local
  aigx_logs:
    driver: local

networks:
  aigx-network:
    driver: bridge
```

### 生产环境 Docker Compose

```bash
# 创建生产环境配置
cp docker-compose.prod.yml docker-compose.yml

# 配置环境变量
nano docker-compose.yml

# 启动服务
docker-compose up -d

# 查看状态
docker-compose ps

# 查看日志
docker-compose logs -f
```

## 二进制部署

### Linux 部署

#### 下载预编译版本

```bash
# Linux AMD64
wget https://github.com/yourusername/aigx/releases/latest/aigx-linux-x86_64
chmod +x aigx-linux-x86_64

# Linux ARM64
wget https://github.com/yourusername/aigx/releases/latest/aigx-linux-arm64
chmod +x aigx-linux-arm64
```

#### 下载完整安装包

```bash
# 解压
tar -xzf aigx-linux-x86_64-2024.01.01.tar.gz

# 安装
sudo useradd -r -u 1000 -s /sbin/nologin aigx
sudo install -m 755 aigx /usr/local/bin/aigx

# 配置数据目录
sudo mkdir -p /var/lib/aigx
sudo mkdir -p /var/log/aigx
```

#### 创建 systemd 服务

```bash
# 创建服务文件
sudo tee /etc/systemd/system/aigx.service <<EOF
[Unit]
Description=AIGX AI Gateway
After=network.target

[Service]
Type=simple
User=aigx
ExecStart=/usr/local/bin/aigx
WorkingDirectory=/var/lib/aigx
Restart=on-failure
RestartSec=5s

# 环境变量
Environment="RUST_LOG=info"
Environment="DATABASE_URL=sqlite:///var/lib/aigx/aigx.db"

# 限制
LimitNOFILE=65536

# 安全
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF

# 启动服务
sudo systemctl daemon-reload
sudo systemctl enable aigx
sudo systemctl start aigx

# 检查状态
sudo systemctl status aigx
```

### Windows 部署

#### 下载 exe 文件

```powershell
# Windows AMD64
Invoke-WebRequest -Uri "https://github.com/yourusername/aigx/releases/latest/aigx-windows-x86_64.exe" -OutFile "aigx.exe"

# Windows ARM64
Invoke-WebRequest -Uri "https://github.com/yourusername/aigx/releases/latest/aigx-windows-arm64.exe" -OutFile "aigx.exe"
```

#### 创建服务

```powershell
# 使用 NSSM (Non-Sucking Service Manager)
# 下载并安装 NSSM: https://nssm.cc/download

# 注册为 Windows 服务
nssm install AIGX "C:\path\to\aigx.exe"
nssm set AIGX AppDirectory "C:\path\to\"
nssm set AIGX DisplayName "AIGX AI Gateway"
nssm set AIGX Description "AIGX - AI Gateway Management"
nssm set AIGX Start SERVICE_AUTO_START
nssm set AIGX AppUserDefaultUsername "AIGX"
nssm set AIGX AppUserDefaultPassword ""

# 启动服务
nssm start AIGX

# 查看状态
nssm status AIGX
```

### macOS 部署

#### 使用 Homebrew

```bash
# 添加 tap
brew tap yourusername/aigx

# 安装
brew install aigx

# 运行
aigx --start

# 查看服务状态
brew services list | grep aigx
```

#### 手动安装

```bash
# macOS Intel
curl -L https://github.com/yourusername/aigx/releases/latest/aigx-macos-x86_64 -o aigx
chmod +x aigx

# macOS Apple Silicon
curl -L https://github.com/yourusername/aigx/releases/latest/aigx-macos-arm64 -o aigx
chmod +x aigx

# 使用 launchd 服务
sudo cp aigx /usr/local/bin/
sudo tee /Library/LaunchDaemons/com.aigx.service.plist <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.aigx.service</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/aigx</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>WorkingDirectory</key>
    <string>/var/lib/aigx</string>
</dict>
</plist>
EOF

sudo launchctl load -w /Library/LaunchDaemons/com.aigx.service.plist
```

### 环境变量配置

创建 `.env` 文件（可选）：

```bash
# 核心配置
AIGX_NAME=AIGX-Gateway
AIGX_VERSION=1.0.0
SERVER_HOST=0.0.0.0
SERVER_PORT=9527

# 数据库配置
DATABASE_URL=sqlite:///data/aigx.db
DATABASE_MODE=sqlite

# 限流与安全
JWT_SECRET=your-secure-random-secret-key
ADMIN_PASSWORD=admin
SIGNUP_ENABLED=false

# 服务配置
RUST_LOG=info
WORKER_COUNT=4
MAX_CONNECTIONS=10000

# 告警通知
ALERT_ENABLED=false
ALERT_EMAIL_PROVIER=email
ALERT_THRESHOLD_CPU=70
ALERT_THRESHOLD_MEMORY=75
```

## Kubernetes 部署

### 部署 manifest 文件

```bash
# 部署
kubectl apply -f kubernetes/

# 查看部署状态
kubectl get pods -n aigx -l app=aigx

# 查看服务
kubectl get services -n aigx

# 查看日志
kubectl logs -n aigx -f deployment/aigx
```

### ConfigMap 和 Secret

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: aigx-config
  namespace: aigx
data:
  SERVER_PORT: "9527"
  WORKER_COUNT: "8"
  RUST_LOG: "info"
---
apiVersion: v1
kind: Secret
metadata:
  name: aigx-secret
  namespace: aigx
type: Opaque
stringData:
  JWT_SECRET: "your-secure-secret-key"
  ADMIN_PASSWORD: "admin-password"
  DATABASE_URL: "sqlite:///data/aigx.db"
```

### Deployment 配置

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: aigx
  namespace: aigx
  labels:
    app: aigx
spec:
  replicas: 3
  selector:
    matchLabels:
      app: aigx
  template:
    metadata:
      labels:
        app: aigx
    spec:
      containers:
      - name: aigx
        image: aigx/aigx:1.0.0
        ports:
        - containerPort: 9527
          name: http
        env:
        - name: RUST_LOG
          valueFrom:
            configMapKeyRef:
              name: aigx-config
              key: RUST_LOG
        - name: JWT_SECRET
          valueFrom:
            secretKeyRef:
              name: aigx-secret
              key: JWT_SECRET
        volumeMounts:
        - name: data
          mountPath: /aigx/data
        resources:
          requests:
            cpu: 500m
            memory: 512Mi
          limits:
            cpu: 2000m
            memory: 2Gi
        livenessProbe:
          httpGet:
            path: /healthz
            port: 9527
          initialDelaySeconds: 30
          periodSeconds: 10
        readinessProbe:
          httpGet:
            path: /ready
            port: 9527
          initialDelaySeconds: 10
          periodSeconds: 5
      volumes:
      - name: data
        persistentVolumeClaim:
          claimName: aigx-pvc
---
apiVersion: v1
kind: Service
metadata:
  name: aigx-service
  namespace: aigx
spec:
  selector:
    app: aigx
  ports:
  - port: 80
    targetPort: 9527
    protocol: TCP
  type: LoadBalancer
```

### HPA 配置（自动扩缩容）

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: aigx-hpa
  namespace: aigx
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: aigx
  minReplicas: 3
  maxReplicas: 20
  metrics:
  - type: Resource
    resource:
      name: cpu
      target:
        type: Utilization
        averageUtilization: 70
  - type: Resource
    resource:
      name: memory
      target:
        type: Utilization
        averageUtilization: 80
```

## 性能优化

### 系统级优化

```bash
# Linux 内核参数优化
cat <<EOF | sudo tee /etc/sysctl.d/aigx.conf
# 文件描述符限制
fs.file-max = 65535

# 网络优化
net.core.somaxconn = 4096
net.ipv4.tcp_max_syn_backlog = 8192
net.ipv4.tcp_max_tw_buckets = 20000
net.ipv4.tcp_fin_timeout = 30

# 内存优化
vm.swappiness = 10
EOF

sudo sysctl -p /etc/sysctl.d/aigx.conf
```

### Rust 性能调优

```bash
# 发布构建（默认已启用）
cargo build --release

# 生产环境编译优化
CARGO_PROFILE_RELEASE_LTO=true
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
```

### 配置性能参数

```bash
# 网络配置
NETWORK_MAX_CONNECTIONS=10000
NETWORK_WORKERS=4

# 缓存配置
CACHE_SIZE-MB=512
CACHE_EXPIRE-TTL=3600

# 数据库优化
DATABASE_POOL_SIZE=100
DATABASE_MAX_CONNECTIONS=50
```

### 监控指标优化

```yaml
# 启用监控
ENABLE_METRICS=true
METRICS_EXPORT_INTERVAL=5000  # ms per export

# 告警阈值优化
ALERT_CPU_HIGH=75
ALERT_MEMORY_HIGH=80
ALERT_DISK_HIGH=90
```

## 监控告警

### Prometheus 配置

```yaml
# /path/to/aigx-prometheus.yml
global:
  scrape_interval: 15s
  evaluation_interval: 15s

scrape_configs:
  - job_name: 'aigx'
    static_configs:
      - targets: ['aigx:9527']
    metrics_path: '/metrics'
    scrape_interval: 10s

  - job_name: 'aigx_nodes'
    static_configs:
      - targets: ['aigx:9527']
    metrics_path: '/api/network/metrics'
    scrape_interval: 30s
```

### Grafana 仪表盘

访问 `http://localhost:3000` 搜索 AIGX:

```json
{
  "dashboard_title": "AIGX Panorama",
  "panels": [
    "CPU Usage",
    "Memory Usage",
    "Network Traffic",
    "Request Rate",
    "Latency Distribution",
    "Connection Pool Status"
  ]
}
```

## 故障排查

### 常见问题

#### 服务无法启动

```bash
# 检查日志
docker logs aigx
journalctl -u aigx -n 50

# 检查端口占用
netstat -tlnp | grep 9527
lsof -i :9527

# 检查权限
ls -la /usr/local/bin/aigx
```

#### 性能问题

```bash
# 查看线程数
ps -eLf | grep aigx

# 内存使用
free -m

# 网络连接
ss -antp | head

# 请求延迟
curl -w "@curl-format.txt" http://localhost:9527/healthz
```

### 调试模式

```bash
# 开启调试日志
RUST_LOG=trace ./aigx

# 使用 strace 调试
strace -p $(pidof aigx) -f

# 使用 dmesg
dmesg -w | grep -E aigx|network
```

### 回滚到旧版本

```bash
# Docker 回滚
docker pull aigx/aigx:1.0.0
docker stop aigx
docker rm aigx
docker run -d -p 9527:9527 aigx/aigx:1.0.0

# 二进制版本管理
./aigx-backup
./aigx-rollback 2024-01-01
```

### 性能分析

```bash
# 使用 perf
perf record -g ./aigx
perf report

# 使用 flamegraph
cargo flamegraph
```

## 安全加固

### 权限管理

```bash
# 最小权限原则
chown -R aigx:aigx /var/lib/aigx
chmod 700 /var/lib/aigx
```

### 防火墙配置

```bash
# UFW (Ubuntu)
ufw allow 9527/tcp
ufw allow 3000/tcp
ufw enable

# firewalld (RHEL/CentOS)
firewall-cmd --permanent --add-port=9527/tcp
firewall-cmd --permanent --add-port=3000/tcp
firewall-cmd --reload
```

### SSL/TLS 配置

```bash
# 生成证书
openssl req -x509 -newkey rsa:4096 -nodes \
  -keyout key.pem -out cert.pem -days 365

# 配置反向代理
nginx -c /etc/nginx/aigx.conf
```

## 附录

### 版本文件清单

| 平台 | 文件名 | 大小 | 镜像体系 |
|------|--------|------|----------|
| Linux AMD64 | aigx-linux-x86_64 | ~15MB | debian:bookworm-slim |
| Linux ARM64 | aigx-linux-arm64 | ~13MB | debian:bookworm-slim |
| Windows AMD64 | aigx-windows-x86_64.exe | ~18MB | winframework |
| Windows ARM64 | aigx-windows-arm64.exe | ~16MB | winframework |
| macOS Intel | aigx-macos-x86_64 | ~12MB | mactex |
| macOS ARM64 | aigx-macos-arm64 | ~10MB | mactex |

### 获取帮助

遇到部署问题？

1. 查看日志: `docker logs aigx` 或 `journalctl -u aigx`
2. 确认环境变量是否正确
3. 检查防火墙和网络设置
4. 在 GitHub Issues 寻找类似问题
5. 提交 Issue 和详细配置信息

---

**最后更新**: 2024-01-01  **版本**: v1.0.0