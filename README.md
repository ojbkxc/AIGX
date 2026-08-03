# AIGX

Rust 实现的 OpenAI 兼容 AI 中转网关。参考 new-api / cf-ai-gw / ds2api / aisix 的设计，聚合多账号 Cloudflare Workers AI，并提供多用户配额、易支付（Epay）在线充值与流式 /v1 接口。

## 特性

- **OpenAI 兼容 API**：`/v1/chat/completions`（含 SSE 流式）、`/v1/completions`、`/v1/embeddings`、`/v1/images/generations`、`/v1/audio/transcriptions|translations|speech`、`/v1/models`
- **多账号 Cloudflare Workers AI**：多账号负载均衡 + 故障切换，账号信息加密落盘
- **多用户与配额**：管理员 / 普通用户角色，配额按 token 估算扣费
- **易支付（Epay）对接**：MD5 签名下单、异步通知验签、同步跳转，签名规则与 new-api 一致
- **用量统计**：本地日 / 月 token 统计 + Cloudflare GraphQL neurons 查询
- **管理面板**：cf-ai-gw 风格的玻璃拟态暗色 UI
- **单文件部署**：前端静态资源内嵌，二进制 + `static/` 即可运行

## 快速开始

### 二进制

```bash
# 下载对应平台的 AIGX-<version>-<os>-<arch>.tar.gz 后
tar xzf AIGX-*-linux-amd64.tar.gz
./AIGX-*-linux-amd64
# 浏览器访问 http://127.0.0.1:8080
```

首次启动用用户名 `admin` + 任意密码登录并设置密码。

### Docker

```bash
docker-compose up -d
# 默认监听 8080，数据卷挂载到 /root/.aigx
```

### 从源码构建

```bash
# 前端
cd frontend && npm ci && npm run build   # 产物输出到 ../static/

# 后端
cd .. && cargo build --release
./target/release/aigx
```

## 配置

配置文件位于 `~/.aigx/config.toml`，首次启动自动生成：

```toml
[server]
host = "127.0.0.1"
port = 8080
data_dir = "~/.aigx"
# 站点对外地址，用于构造易支付回调 URL
server_address = ""

[admin]
password = ""          # 首次登录后自动写入 SHA256 哈希
session_secret = ""    # 自动生成

[usage]
daily_limit = 0        # 0 表示不限
monthly_limit = 0
threshold = 0.9

[epay]
pay_address = ""       # 易支付网关地址
epay_id = ""           # 商户 PID
epay_key = ""           # 商户密钥
pay_methods = ["alipay", "wxpay"]
price = 1.0            # 1 元 = 1 配额
min_topup = 1
custom_callback_address = ""
```

也可在管理面板「易支付」页直接配置。

## API 使用

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <你的 API Key>" \
  -d '{"model": "@cf/meta/llama-3.1-8b-instruct", "messages": [{"role": "user", "content": "你好"}]}'
```

API Key 在管理面板「API 密钥」页创建，格式为 `sk-...`。

## 模型映射

在「模型映射」页将外部模型名映射到 Cloudflare Workers AI 模型（`@cf/...`），客户端即可使用自定义模型名调用。

## 路线图

- [x] 多账号 CF Workers AI failover
- [x] OpenAI 兼容流式 / 非流式
- [x] 多用户 + 配额扣费
- [x] 易支付下单 / 回调
- [ ] 通用 OpenAI / Anthropic 上游 Bridge
- [ ] 更多支付方式

## 许可

MIT
