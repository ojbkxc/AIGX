# AIGX 网络层 API 文档

## 基础信息

- **Base URL**: `/api/network`
- **Content-Type**: `application/json`
- **认证方式**: Bearer Token (JWT)

## 接口列表

---

## 获取网络层状态

### 接口信息

```
GET /api/network/status
```

**描述**: 获取网络层的完整状态信息

**权限**: 管理员权限

**请求参数**: 无

**响应参数**:

```json
{
  "code": 0,
  "message": "success",
  "data": {
    // 网络层整体状态
    "enabled": true,
    "last_check_at": 1696523491,

    // 账号池状态
    "account_pool": {
      "total_accounts": 10,
      "available_accounts": 8,
      "busy_accounts": 1,
      "error_accounts": 1,
      "invalid_accounts": 0,
      "total_requests": 158340,
      "failed_requests": 45
    },

    // 连接池状态
    "connection_pool": {
      "total_connections": 10,
      "active_connections": 6,
      "idle_connections": 4,
      "total_connections_created": 10,
      "total_connections_closed": 15,
      "successful_requests": 158295,
      "failed_requests": 45,
      "avg_latency_ms": 28.6
    },

    // 会话池状态
    "session_pool": {
      "total_sessions": 8,
      "active_sessions": 5,
      "idle_sessions": 3,
      "session_ttl_hours": 72
    },

    // 负载均衡策略
    "load_balance_strategy": "latency-aware+weighted+circuit"
  }
}
```

### 错误码

- `401`: 未认证
- `403`: 无权限
- `500`: 服务器内部错误

---

## 更新网络层配置

### 接口信息

```
PUT /api/network/config/:config_id
```

**描述**: 更新网络层的配置参数

**权限**: 管理员权限

**请求参数**:

```json
{
  "enabled": true,
  "strategy": "latency-aware",
  "account_pool_min": 2,
  "account_pool_max": 10,
  "connection_pool_max": 10,
      "session_pool_max": 50
}
```

**路径参数**:

| 参数名 | 类型 | 必填 | 说明 |
|--------|------|------|------|
| config_id | string | 是 | 配置ID，默认为"default" |

**响应参数**:

```json
{
  "code": 0,
  "message": "配置更新成功",
  "data": {
    "enabled": true,
    "strategy": "latency-aware+weighted+circuit",
    "account_pool_min": 2,
    "account_pool_max": 10,
    "connection_pool_max": 10,
    "session_pool_max": 50
  }
}
```

### 错误码

- `400`: 请求参数错误
- `401`: 未认证
- `403`: 无权限
- `500`: 服务器内部错误

---

## 重启网络层

### 接口信息

```
POST /api/network/restart
```

**描述**: 重启网络层的所有组件

**权限**: 超级管理员权限

**请求参数**: 无

**响应参数**:

```json
{
  "code": 0,
  "message": "网络层重启完成",
  "data": {
    "success": true,
    "status": "started",
    "restart_time": 1696523500
  }
}
```

**注意**: 重启会重置所有断路器和健康追踪状态

### 错误码

- `401`: 未认证
- `403`: 无权限
- `500`: 服务器内部错误

---

## 添加网络层账号

### 接口信息

```
POST /api/network/accounts/:account_id
```

**描述**: 添加新的账号到网络层

**权限**: 管理员权限

**路径参数**:

| 参数名 | 类型 | 必填 | 说明 |
|--------|------|------|------|
| account_id | string | 是 | 账号ID |

**请求参数**:

```json
{
  "name": "GitHub账户1",
  "account_id": "ghp_placeholder",
  "api_token": "ghp_placeholder_secret",
  "status": "active",
  "priority": 1,
  "endpoint_url": "https://api.github.com"
}
```

**响应参数**:

```json
{
  "code": 0,
  "message": "网络层账号已添加",
  "data": {
    "success": true,
    "account": {
      "id": "uuid-xxx",
      "name": "GitHub账户1",
      "account_id": "ghp_placeholder",
      "status": "active",
      "priority": 1,
      "created_at": 1696523400
    }
  }
}
```

### 错误码

- `400`: 请求参数错误
- `401**: 未认证
- `403`: 无权限
- `409`: 账号已存在
- `500`: 服务器内部错误

---

## 删除网络层账号

### 接口信息

```
DELETE /api/network/accounts/:account_id
```

**描述**: 删除网络层中的账号

**权限**: 管理员权限

**路径参数**:

| 参数名 | 类型 | 必填 | 说明 |
|--------|------|------|------|
| account_id | string | 是 | 账号ID |

**响应参数**:

```json
{
  "code": 0,
  "message": "网络层账号已删除",
  "data": {
    "success": true,
    "deleted_id": "uuid-xxx"
  }
}
```

### 错误码

- `401`: 未认证
- `403`: 无权限
- `404`: 账号不存在
- `500`: 服务器内部错误

---

## 获取监控指标

### 接口信息

```
GET /api/network/metrics
```

**描述**: 获取详细的系统监控指标

**权限**: 管理员权限

**请求参数**:

| 参数名 | 类型 | 必填 | 说明 |
|--------|------|------|------|
| type | string | 否 | 指标类型: system, network, app |
| format | string | 否 | 输出格式: json, prometheus |

**响应参数**:

```json
{
  "code": 0,
  "message": "success",
  "data": {
    // CPU 使用率
    "cpu_usage": 42.5,
    "cpu_cores": 28,

    // 内存使用率
    "memory_usage": 64.8,
    "memory_total_gb": 256,
    "memory_available_gb": 91.4,

    // 磁盘使用率
    "disk_usage": 78.2,
    "disk_path": "/",
    "disk_total_gb": 1024,
    "disk_available_gb": 225.2,

    // 网络指标
    "network_tx_bytes": 854320213,
    "network_rx_bytes": 1234567890,
    "network_tx_bps": 4523000,
    "network_rx_bps": 3215000,

    // 应用指标
    "active_connections": 156,
    "total_requests": 158340,
    "failed_requests": 45,
    "success_rate": 0.9997,
    "avg_latency_ms": 28.6,
    "throughput_req_s": 125,

    // 网络层指标
    "node_id": "aigx-node-1",
    "node_status": "online",
    "health_score": 92,
    "distributed_nodes": 4,

    // 系统运行时间
    "uptime_hours": 87.3
  }
}
```

### 错误码

- `401`: 未认证
- `403`: 无权限
- `500`: 服务器内部错误

---

## 获取监控历史数据

### 接口信息

```
GET /api/network/metrics/history
```

**描述**: 获取历史监控数据用于趋势分析

**权限**: 管理员权限

**请求参数**:

| 参数名 | 类型 | 必填 | 说明 |
|--------|------|------|------|
| start_time | timestamp | 是 | 开始时间(Unix时间戳) |
| end_time | timestamp | 是 | 结束时间(Unix时间戳) |
| interval | string | 是 | 时间间隔: 1m, 5m, 15m, 1h, 1d |
| metric | string | 否 | 指标类型: cpu, memory, network, latency |

**响应参数**:

```json
{
  "code": 0,
  "message": "success",
  "data": {
    "metrics": [
      {
        "timestamp": 1696523500,
        "cpu_usage": 45.2,
        "memory_usage": 68.5,
        "avg_latency_ms": 32.1,
        "throughput_req_s": 118
      },
      {
        "timestamp": 1696523560,
        "cpu_usage": 42.8,
        "memory_usage": 65.3,
        "avg_latency_ms": 28.9,
        "throughput_req_s": 125
      }
    ],
    "summary": {
      "average_cpu": 43.8,
      "average_memory": 66.9,
      "average_latency": 30.5,
      "max_latency": 52.3,
      "min_latency": 18.7
    }
  }
}
```

### 错误码

- `400`: 请求参数错误
- `401**: 未认证
- `403`: 无权限
- `500`: 服务器内部错误

---

## 分布式节点管理

### 获取节点列表

### 接口信息

```
GET /api/network/distributed/nodes
```

**描述**: 获取所有分布式节点列表

**权限**: 管理员权限

**响应参数**:

```json
{
  "code": 0,
  "message": "success",
  "data": {
    "nodes": [
      {
        "id": "aigx-node-1",
        "name": "主节点(Leader)",
        "address": "192.168.1.100:9527",
        "status": "online",
        "version": "1.0.0",
        "health_score": 98,
        "cpu_usage": 35.2,
        "memory_usage": 52.3,
        "data_center": "dc1",
        "is_leader": true,
        "replication_status": [
          {
            "channel_id": "openai-gpt4",
            "status": "synced",
            "latency_ms": 5
          },
          {
            "channel_id": "anthropic-claude",
            "status": "syncing",
            "latency_ms": 120
          }
        ],
        "last_heartbeat": 1696523500
      }
    ],
    "total_nodes": 4,
    "online_nodes": 4
  }
}
```

### 获取节点健康状态

### 接口信息

```
GET /api/network/distributed/nodes/:node_id/health
```

**描述**: 获取指定节点的详细健康状态

**权限**: 管理员权限

**响应参数**:

```json
{
  "code": 0,
  "message": "success",
  "data": {
    "node_id": "aigx-node-1",
    "status": {
      "healthy": true,
      "score": 98,
      "components": {
        "cpu": {
          "status": "healthy",
          "usage": 35.2,
          "max_threshold": 80.0
        },
        "memory": {
          "status": "healthy",
          "usage": 52.3,
          "max_threshold": 85.0
        },
        "network": {
          "status": "healthy",
          "latency_ms": 5.2,
          "max_threshold": 50.0
        },
        "disk": {
          "status": "healthy",
          "usage": 68.5,
          "max_threshold": 90.0
        }
      },
      "recent_errors": [],
      "recommendations": []
    }
  }
}
```

---

## 扩缩容管理

### 获取扩缩容状态

### 接口信息

```
GET /api/network/scaling/status
```

**描述**: 获取自动扩缩容的当前状态

**权限**: 管理员权限

**响应参数**:

```json
{
  "code": 0,
  "message": "success",
  "data": {
    "enabled": true,
    "mode": "auto",
    "current_nodes": 4,
    "min_nodes": 2,
    "max_nodes": 10,
    "average_load": 45.3,
    "is_scaling": false,
    "cooldown_remaining_seconds": 180,
    "scaling_history": [
      {
        "id": "scale-2023-10-12-1001",
        "action": "scale_up",
        "timestamp": 1696540000,
        "nodes_count": 4,
        "reason": "request_rate_high"
      }
    ],
    "appropriate_action": null,
    "predicted": {
      "current_nodes": 4,
      "target_nodes": 4,
      "reason": "no_action",
      "estimated_time": null
    }
  }
}
```

### 更新扩缩容配置

### 接口信息

```
PUT /api/network/scaling
```

**描述**: 更新自动扩缩容的配置

**权限**: 超级管理员权限

**请求参数**:

```json
{
  "enabled": true,
  "min_nodes": 2,
  "max_nodes": 10,
  "mode": "auto",
  "custom_settings": {
    "cpu_high_threshold": 70.0,
    "cpu_low_threshold": 30.0,
    "request_rate_high_threshold": 5000.0,
    "cooldown_period_seconds": 300
  }
}
```

**响应参数**:

```json
{
  "code": 0,
  "message": "扩缩容配置更新成功",
  "data": {
    "enabled": true,
    "min_nodes": 2,
    "max_nodes": 10,
    "mode": "auto",
    "custom_settings": {
      "cpu_high_threshold": 70.0,
      "cpu_low_threshold": 30.0,
      "request_rate_high_threshold": 5000.0,
      "cooldown_period_seconds": 300
    }
  }
}
```

---

## 告警管理

### 获取告警配置

### 接口信息

```
GET /api/network/alerts/config
```

**描述**: 获取告警规则配置

**权限**: 管理员权限

**响应参数**:

```json
{
  "code": 0,
  "message": "success",
  "data": {
    "enabled": true,
    "channels": {
      "email": {
        "enabled": true,
        "smtp_config": {
          "host": "smtp.example.com",
          "port": 465,
          "username": "alert@example.com",
          "from": "alerts@aigx.io"
        }
      },
      "slack": {
        "enabled": true,
        "webhook_url": "https://hooks.slack.com/services/xxx"
      },
      "telegram": {
        "enabled": false,
        "bot_token": "xxx",
        "chat_id": "xxx"
      },
      "webhook": {
        "enabled": true,
        "url": "https://webhook.example.com/alerts"
      }
    },
    "thresholds": {
      "cpu_usage": 70.0,
      "memory_usage": 75.0,
      "disk_usage": 85.0,
      "error_rate": 0.05,
      "response_time_ms": 100
    },
    "cooldown_duration_seconds": 300
  }
}
```

### 更新告警配置

### 接口信息

```
PUT /api/network/alerts/config
```

**描述**: 更新告警规则配置

**权限**: 超级管理员权限

**请求参数**:

```json
{
  "enabled": true,
  "channels": {
    "slack": {
      "enabled": true,
      "webhook_url": "https://hooks.slack.com/services/xxx"
    }
  },
  "thresholds": {
    "cpu_usage": 60.0,
    "memory_usage": 65.0,
    "error_rate": 0.03
  }
}
```

---

## 导出监控数据

### 接口信息

```
GET /api/network/metrics/export
```

**描述**: 导出监控数据为指定格式

**权限**: 管理员权限

**请求参数**:

| 参数名 | 类型 | 必填 | 说明 |
|--------|------|------|------|
| format | string | 是 | 导出格式: json, csv, prometheus |
| start_time | timestamp | 是 | 开始时间(Unix时间戳) |
| end_time | timestamp | 是 | 结束时间(Unix时间戳) |

**响应**: 文件下载

```bash
# JSON格式
GET /api/network/metrics/export?format=json&start_time=1696400000&end_time=1696486400

# Prometheus格式
GET /api/network/metrics/export?format=prometheus&start_time=1696400000&end_time=1696486400
```

---

## HTTP 请求示例

### Python 示例

```python
import requests

# 获取网络层状态
response = requests.get(
    'http://localhost:9527/api/network/status',
    headers={'Authorization': 'Bearer YOUR_TOKEN'}
)
print(response.json())

# 添加账号
account_data = {
    'name': 'GitHub账户1',
    'account_id': 'ghp_placeholder',
    'api_token': 'ghp_placeholder',
    'priority': 1
}
response = requests.post(
    'http://localhost:9527/api/network/accounts/ghp_placeholder',
    json=account_data,
    headers={'Authorization': 'Bearer YOUR_TOKEN'}
)
print(response.json())

# 获取监控指标
response = requests.get(
    'http://localhost:9527/api/network/metrics',
    headers={'Authorization': 'Bearer YOUR_TOKEN'}
)
metrics = response.json()
print(f"CPU使用率: {metrics['data']['cpu_usage']}%")
```

### cURL 示例

```bash
# 获取网络层状态
curl -X GET "http://localhost:9527/api/network/status" \
  -H "Authorization: Bearer YOUR_TOKEN"

# 重启网络层
curl -X POST "http://localhost:9527/api/network/restart" \
  -H "Authorization: Bearer YOUR_TOKEN" \
  -H "Content-Type: application/json"

# 获取监控指标
curl -X GET "http://localhost:9527/api/network/metrics?format=json" \
  -H "Authorization: Bearer YOUR_TOKEN"
```

---

## Websocket 事件流

### 连接信息

```
WS://localhost:9527/api/network/stream
```

#### 连接参数

| 参数名 | 类型 | 必填 | 说明 |
|--------|------|------|------|
| token | string | 是 | JWT Token |
| events | string[] | 否 | 订阅事件类型 |

#### 支持的事件

- `network_status`: 网络/状态变化事件
- `scaling`: 扩缩容事件
- `alert`: 告警事件
- `node_status`: 节点状态变化事件

#### 订阅事件示例

```javascript
const ws = new WebSocket('ws://localhost:9527/api/network/stream?token=YOUR_TOKEN&events=network_status,scaling');

ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  switch(data.type) {
    case 'network_status':
      console.log('网络状态更新:', data.payload);
      break;
    case 'scaling':
      console.log('扩缩容事件:', data.payload);
      break;
    case 'alert':
      console.log('告警事件:', data.payload);
      break;
  }
};
```

---

## 签名验证

所有 API 请求都需要在头部添加 JWT Token：

```http
Authorization: Bearer YOUR_JWT_TOKEN
```

Token 生成：
```python
import jwt

token = jwt.encode({
    'user_id': 'admin',
    'username': 'admin',
    'role': 'admin',
    'exp': time.time() + 3600
}, SECRET_KEY, algorithm='HS256')
```
---

## 工具服务与维护任务 API

> 对齐 Agent2API v3.5.0-beta.1 的「工具服务」页与「定时任务」维护明细。
> 管理面端点走管理员会话鉴权（`verify_admin`，与 `/api/*` 其余管理接口同一口径）；
> 运行时端点走网关 API Key 鉴权，单独列在下方「运行时端点（数据面）」。

### 工具服务管理面

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/tools/servers` | 服务 × 协议清单（`?protocol=skill\|mcp`） |
| PATCH | `/api/tools/servers/{id}` | 改开关 / 执行超时（`{protocol, enabled?, timeout_seconds?}`） |
| GET | `/api/tools/providers` | 搜索提供商清单（密钥型 + 账号型；只回 `configured`/`key_hint`） |
| GET | `/api/tools/provider-selection` | 搜索 / 抓取提供商选择 |
| PUT | `/api/tools/provider-selection` | 改选择（`{search_providers?, fetch_provider?}`） |
| PUT | `/api/tools/providers/{provider}` | 配密钥 / 绑定账号 / 启停（`{api_key?, account_id?, enabled?}`） |
| DELETE | `/api/tools/providers/{provider}/key` | 清除密钥 |
| POST | `/api/tools/providers/{provider}/test` | 连通性测试 |
| GET | `/api/tools/api-keys` | 本机接入可用的网关 Key（id/name/掩码） |
| GET | `/api/tools/vision/config` | 视觉分析配置 + 可选 Key |
| PUT | `/api/tools/vision/config` | 保存视觉配置（提供商/上游模型/思考等级/内部 Key） |
| DELETE | `/api/tools/vision/config` | 清除视觉配置 |
| POST | `/api/tools/vision/config/test` | 视觉连通性测试（1x1 PNG） |
| GET | `/api/tools/image-generation/config` | 图片生成配置面（管理视图） |
| PUT | `/api/tools/image-generation/config` | 保存图片生成配置 |
| DELETE | `/api/tools/image-generation/config` | 清除图片生成配置 |
| GET | `/api/tools/install-targets` | 本机客户端目标探测（`?service=&protocol=skill\|mcp`） |
| GET | `/api/tools/install-info` | 接入信息（端点/MCP 工具清单/桥接脚本地址） |
| POST | `/api/tools/install-skill` | 安装 Skill（`{api_key_id, variant, clients}`） |
| POST | `/api/tools/install-mcp` | 安装 MCP（`{service_id, mode?, api_key_id, clients}`） |
| POST | `/api/tools/uninstall-skill` | 卸载 Skill（`{service_id, clients}`） |
| POST | `/api/tools/uninstall-mcp` | 卸载 MCP（`{service_id, clients}`） |
| GET | `/api/tools/mcp-bridge/vision.js` | 视觉本地桥接脚本（text/javascript） |

#### 错误约定

- 参数校验失败：400 `{success:false,error:"…"}`
- 未登录 / 非管理员：`verify_admin` 的 401/403 信封
- 未知服务/协议/提供商：400
- 内部存储异常：500（文案已做凭据脱敏）

### 维护任务管理面

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/maintenance` | 总览：两条维护任务 + 最近一轮 + 记录分页（`taskId/source/status/keyword/offset/limit`） |
| GET | `/api/maintenance/records/{id}` | 单轮完整明细（`run` + `results[]`） |
| POST | `/api/maintenance/run/{task}` | 立即执行（`credentialMaintenance` / `modelRefresh`） |
| POST | `/api/maintenance/records/{id}/retry` | 重试失败项；可选 `{itemId}` 只重试一条 |

#### 记录形状

- `run`：`id/taskId/source/retryOf/startedAt/finishedAt/durationMs/status/refreshed/skipped/failed/total/error/metadata`
- `results[]`：`id/provider/providerLabel/accountId/accountName/status/message/count/refreshedAt/expiresBefore/expiresAfter/credentialChanged/durationMs/fixed/retryAt/forced/invalid`
- `status`：`refreshed` / `skipped` / `failed`；轮次状态：`running` / `completed` / `partial` / `failed` / `skipped` / `interrupted`
- 保留：最近 30 天、最多 2000 轮（`retentionDays` / `maxRecords` 在总览里返回）

### 运行时端点（数据面，网关 API Key 鉴权）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/skill/web-search` | Skill 服务元数据（免鉴权） |
| POST | `/skill/web-search/search` | 批量搜索（1–10 条 queries） |
| POST | `/skill/web-search/fetch` | 批量抓取（1–10 条公网 URL） |
| GET | `/api/web-search` | REST 元数据（免鉴权） |
| POST | `/api/web-search/search` | 批量搜索 |
| POST | `/api/web-search/fetch` | 批量抓取 |
| POST | `/mcp/web-search` | 网络搜索 MCP（JSON-RPC 2.0，Streamable HTTP） |
| POST | `/mcp/vision` | 视觉分析 MCP |

鉴权：`Authorization: Bearer <AIGX API Key>` 或 `x-api-key`；跳过模型白名单，保留启用/过期/额度/IP 检查。

## 模型管理（管理员，对齐 Agent2API v3.5.0-beta.1）

> 本节覆盖 T5 新增 / 变更的模型管理端点。映射是
> `{alias, target, provider(可空), reasoning(可空), enabled}` 条目；
> `provider` 缺省（= `*`）表示对所有承载 `target` 的提供商生效（存量
> `alias → target` 表的迁移形态）。`reasoning` / `enabled` / `create`
> 三者的**键缺失 = 不动**，给值 = 显式设置。

### 映射（逐条读写）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/models/mappings` | 逐条映射清单。响应 `data: {mappings[], dangling[], reasoningLevels}`。`mappings[]` 一项：`alias/target/provider/enabled/reasoning/isDefault/dangling/carried` |
| POST | `/api/models/mappings` | 新增/更新一条映射（三元组 `(alias, target, provider)` 定位）。请求 `{alias, target, provider?, reasoning?, enabled?, create?}`。`create=true` 已存在 → 409；`create=false` 不存在 → 404。成功回出最新完整 manage 视图 |
| POST | `/api/models/mappings/remove` | 按三元组删除映射。请求 `{alias, target, provider?}`。自映射（`alias==target`）仅当模型行已不存在时可删，否则 400「请关闭它的开关」。不存在 → 404 |

### 管理视图与刷新

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/models/manage` | 统一模型管理视图。T5 增补：`mappings` 变为**逐条数组**（同 `/api/models/mappings` 的 `mappings[]` 形状）、`mappingsFlat` 保留旧扁平对象、`dangling[]` + `dangling_mappings[]`、`providers[].source`（`builtin`/`remote`/`none`）与 `providers[].refreshedAt`（unix 秒，0=从未拉过）、`models[].capabilityFlags`（六键三态生效值，未声明为 `null`）、`models[].capOverrides`（被用户覆盖的键）、`capabilityStoreReady`（能力三态存储是否已初始化；false 时覆盖写入一律 500） |
| POST | `/api/models/refresh` | 手动全量刷新（恒 2xx）。响应 `data` 字段（**6 旧 + 5 新，全部保留**）见下表 |

`POST /api/models/refresh` 的 11 个字段语义：

| 字段 | 状态 | 语义 |
| --- | --- | --- |
| `results` | 旧·保留 | 逐家结果数组。AIGX 无逐家上游目录刷新，恒 `[]`（A2A 兼容位） |
| `refreshed` | 旧·保留 | 成功刷新计数（0/1） |
| `skipped` | 旧·保留 | 合并窗内被跳过（0/1） |
| `failed` | 旧·保留 | 失败计数（0/1） |
| `count` | 旧·保留 | 本轮装载的元数据条目数 |
| `message` | 旧·保留 | 失败/跳过原因（成功为 `null`） |
| `models[]` | **新** | 刷新后的聚合清单（`id/name/provider/providerLabel/owned_by/context_length/capabilities/capabilityFlags/isDefault/enabled`），前端就地重绘 |
| `mappings[]` | **新** | 全量映射条目（同 manage 视图） |
| `dangling[]` | **新** | 未挂载映射（同 manage 视图 `dangling`） |
| `reasoningLevels` | **新** | 思考等级候选表 `["off","none","minimal","low","medium","high","xhigh","max"]` |
| `modelsDev` | **新** | models.dev 元数据状态（见下节 `metadata` 形状） |

### models.dev 元数据

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/models/metadata` | `data`: `{source, rawSource, updatedAt(毫秒), count, enabled, modelsDevEnabled}` |
| PUT | `/api/models/metadata` | 请求 `{enabled}` 或 `{modelsDevEnabled}`（两者都收，缺字段 400）；返回同 GET |
| POST | `/api/models/metadata/refresh` | 手动拉一次（5 秒合并窗，恒 2xx）。`data: {status(refreshed/disabled/skipped/failed), message, count, metadata}` |

`source` 四档：`builtin`（内嵌快照）/ `cache`（库内上一份刷新结果）/
`network`（本次联网拉到的）/ `user`。`rawSource` 是 AIGX 原有原始字符串
（`embedded` / `cache` / `https://models.dev`），旧前端读它。

能力位三态出口（`POST /api/models/capabilities` 新形态
`{provider, id, capabilities: {key: bool|null}}`，替代/并列于旧标签集形态
`{model, capabilities: [string]}`）：六键
`supportsToolCall/supportsImages/supportsVideo/supportsReasoning/supportsEmbedding/supportsRerank`
（外加 `supportsAudio`/`supportsChat`）；优先级 **user > upstream > models.dev**；
`null` = 清除该键覆盖（回到 inherit）。

## 模型清单对外接口（`/v1/models`，数据面，网关 API Key 鉴权）

> 与上面「模型管理」不同：这一节是**对外协议面**（OpenAI / Anthropic 客户端
> 直接消费），鉴权走网关 API Key；「模型管理」是管理员面。

按请求头分两种响应形态：

| 请求头 | 响应形态 |
| --- | --- |
| 默认（OpenAI 客户端） | `{object:"list", data:[…], meta:{source, count}}` |
| `anthropic-version: …` **或** `x-api-key: …` | `{data:[{id, type:"model", display_name, created_at}], has_more, first_id, last_id}`（Anthropic 官方 List Models 形状） |

> Anthropic 形态存在的原因：Claude Code / Claude Desktop 的网关发现**直接打
> `{ANTHROPIC_BASE_URL}/v1/models`**（自己拼路径、不加前缀），拿到 OpenAI 的
> `{object:"list"}` 会解析不出任何模型；拆成 `/anthropic/v1/models` 等于让那条
> 发现永远失败。两条形态的 `data` **同源**，只有字段映射不同。

OpenAI 形态的 `data[]` 一项（对齐 Agent2API `list_item`）：

| 字段 | 语义 |
| --- | --- |
| `id` / `object` / `created` | 模型名 / 固定 `"model"` / 清单刷新时刻（unix 秒） |
| `owned_by` | 归属厂商（渠道类型优先，否则名字推断，兜底 `aigx`） |
| `context_length` | 上下文窗口（未知 `null`） |
| `capabilities` | 能力**标签集**（`chat`/`tools`/`vision`/…；`!键名` 表示用户显式关闭） |
| `supports_tool_call` / `supports_images` / `supports_video` / `supports_reasoning` / `supports_embedding` / `supports_rerank` | 布尔能力位（未声明 = `false`；三态覆盖优先于上游声明） |
| `input_modalities` | `["text"]` + `"image"` / `"video"`（与上面布尔位同源） |
| `reasoning_levels` | 该模型认识的思考档位（`null` = 未声明） |
| `cap_sources` | 逐键来源 `user` / `upstream` / `modelsdev` / `null` |

Anthropic 形态的 `data[]` 一项额外保留上表全部字段（Anthropic 客户端会忽略
不认识的键），并补 `type:"model"` / `display_name`（回落 `id`）/ `created_at`
（ISO-8601）。

`GET /v1/models/{model}` 详情：同上字段 + `permission: []` 占位。

## 签到中心（管理员，替上游账号领额度）

> 与用户侧 `/api/checkin`（钱包配额奖励）是**两套**：本节全部是管理员替**上游账号**每日签到。
> 既有 `/api/checkin-center/*` 路径保持不变；以下为 T6 新增/扩展的端点。
> 所有端点需管理员会话（Bearer JWT）。

### 明细台账

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/checkin-records` | 一页明细 + 同条件总数 + 统计 |
| DELETE | `/api/checkin-records` | 按筛选删除；全清必须显式 `all=1` |

GET 查询参数：`limit`（默认 200，上限 1000）、`offset`、`outcome`（success/already/pending/captcha/failed）、
`kind`（manual/schedule/makeup 三档）、`provider`、`keyword`、`start`/`end`（毫秒时间戳，闭区间）。

响应：`{records:[{row_id,ts,date,source,kind,reason,account_id,account_name,provider,provider_id,outcome,ok,message,error,kept_active,streak_days}], total, stats:{success,already,pending,captcha,failed,kept_active}}`（**snake_case**）。

DELETE：带任一筛选参数 = 只删命中；不带筛选必须带 `all=1`（否则 400）。响应 `{success:true, removed}`。

### 定时签到条目

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/checkin-schedules` | 状态（总开关 + 条目数组 + 提供商清单 + 设置） |
| POST | `/api/checkin-schedules` | 保存一条（按 `id` upsert，未传字段继承存量） |
| DELETE | `/api/checkin-schedules/{id}/delete` | 删除一条（POST 同受理） |
| POST | `/api/checkin-schedules/{id}/run` | 立即执行这一条 |

条目字段（响应 snake_case）：`id/label/display_label/enabled/time/time_end/randomize/providers/provider_times/provider_rows/retry_time(=makeup_time)/retry_gap_minutes/retry_limit/last_fired_date/last_fired_today/last_fired_providers/last_retried_providers/pending_providers/next_run_at/last_result/last_ok/last_keepalive/last_failed`。
校验：`time` 必填 `HH:MM`；`providers` 至少一家（只认签到白名单）；条目上限 10 条；`time_end` 不早于 `time`。

### 自动签到总开关

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/auto-checkin` | 状态（兼容视图 + `schedules[]` + `provider_options` + `settings`，snake_case） |
| POST | `/api/auto-checkin` | 改总开关 `enabled` / 默认条目 `time`、`providers`、`time_end`、`retry_*` 等 |
| POST | `/api/auto-checkin/run` | 立即执行一次（全部可签账号） |

「正在执行中」返回 400（串行闸：手动与定时共用，避免叠跑风控）。

### 人工验证桥（ZCode 定时领取）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/api/zcode/claim-verification` | `action=poll` 查询待求解验证；`action=complete` 提交求解令牌（可选 `planId` 立即领取） |

`poll` → `{supported, ready, task}`（令牌池空且存在 ZCode 账号时 `task.kind="captcha"` 且 `task.account_id` 回显）。
`complete` → `{supported, task:{account_id, outcome, message?}, ready}`；输入两种键都认（`accountId` / `account_id`、`planId` / `plan_id`）；`outcome` 取 `claimed/already_claimed/confirming/failed`（`confirming` = 已提交待上游确认）。

### `/api/checkin-center` 扩展字段

既有聚合快照新增：`auto`（多条目自动签到状态，snake_case）、`campaigns`（活动/月度赠送/限时套餐的账号清单，含本地 `claim_plans`）、
`campaign_preview`（A2A CampaignPreview 空形状）、`records_stats`（按 outcome 的当日统计）；账号行新增 `streak_days`、`daily_checkin`、`available`、`edition`。
台账记录新增 `reason`（触发原因）与 `streak_days`（连登天数）；台账**全量保留**（不再默认 90 天裁剪，`keep_days=0` 即永久保留）。

## 账号池管理（`/api/account-sessions`，管理员鉴权）

> 对齐 Agent2API v3.5.0-beta.1 账号池口径。以下为 T4 新增/变更端点；
> 既有 CRUD / 登录 / 导入 / 统计端点不变。

### 新增端点

> A2A 兼容影子注册：`/api/accounts/sorting`（GET/PUT）、`/api/accounts/refresh-expiring`（POST）、`/api/accounts/export-files`（POST）指向同一组 handler。
> 既有 `POST /api/account-sessions/usage`（同步手动查询）与 `GET /api/account-sessions/usage/snapshot`（最近读数快照）保持不变；`GET /usage/states` 只报异步任务态。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/account-sessions/sorting` | 服务端排序规则：`configured`（管理员配置）、`rules`（生效规则，缺省 credit）、`account_order`（逐 provider 实际先试次序）、`credit_expires_at`、`balances` |
| PUT | `/api/account-sessions/sorting` | 保存某 provider 的排序规则。请求 `{provider, rules: [...] | null, onlyIfUnset?}`；规则项 `{"on":"credit|expires|balance|added|quota_reset","dir":"asc|desc"}`（`credit` 归一为 `expires`，上限 2 条）；`null` = 恢复默认 |
| POST | `/api/account-sessions/refresh-expiring` | 批量刷新全部临期账号；返回 `{refreshed, failed, invalid, message}`（复用后台巡检的串行刷新与单飞实现） |
| POST | `/api/account-sessions/usage/start` | 异步发起余额查询任务。请求 `{id?, replace?}`；返回 `{jobs: [{id, requestId, status, error, startedAt, deadlineAt}], started}` |
| POST | `/api/account-sessions/usage/cancel` | 取消查询任务。请求 `{id, requestId}`；返回 `{cancelled}`（requestId 必须与当前任务一致，防误杀） |
| GET | `/api/account-sessions/usage/states` | 查询任务状态：`{jobs: [...], results: [...], skipped, at}`；任务终态保留 5 分钟 |
| POST | `/api/account-sessions/export-files` | 按 `{id}` 或 `{provider}` 导出单文件：`{total, files: [{id, name, file_name, content}], failed: []}`；`content.format = "agent2api-account"`，插件凭证在 `content.secret` / `content.public` 段 |
| POST | `/api/account-sessions/{id}/rate-limits/clear` | 清除插件的逐模型限流标记。请求 `{model?}`（缺省 = 全部）；返回 `{cleared}` |

### 变更端点

| 方法 | 路径 | 变更 |
| --- | --- | --- |
| GET | `/api/account-sessions` | 响应追加快照：`providers: [{id,label,count}]`、`currentAccountIds: {provider: 账号id}`、`hiddenAccounts: {total, byProvider, accounts[]}`；`data` 默认只含 `hidden=false` 的账号，`?include_hidden=true` 时并入。账号行追加 `is_plugin_account` / `plugin`（公开投影：`hasAccessToken`/`hasRefreshToken`/`canRefresh`/`expiresAt` 等）/ `identity_key` / `hidden` / `needs_relogin` / `allow_route_skip` / `user_agent` / `usage_query` / `rate_limits` |
| GET | `/api/account-sessions/providers` | 返回原生 + 插件 provider。原生行追加 `source:"native"`、`icon`、`login:{supported, methods:["web","paste"]}`；插件行 `source:"plugin"`、`category:"plugin"`、`login:{supported:true, methods:["plugin"]}`、`models`、`pack_id`、`pack_label`、`installed_from`（`native-pack`/`script`）。原生优先去重 |
| POST | `/api/account-sessions` | 请求体支持 `authFileContent` / `authFileB64` / `authFileName`（账号文件，文本或 base64；base64 的 SQLite Cookies 库按只读副本解析）、`pluginSecret` / `pluginPublic` / `pluginIdentity`、`hidden`、`allowRouteSkip`、`userAgent`、`usageQuery:{retries,timeout_seconds}`、`rateLimits`。插件 provider 创建时自动挂到隐藏的 `ChannelType::Plugin` 承载渠道（名称=provider） |
| POST | `/api/account-sessions/bulk` | 请求体支持 `authFiles: [{provider?, name, authFileContent?, authFileB64?, authFileName?}]`；`channel_id` 可省略（纯账号文件批量时按 provider 自动落隐藏插件渠道） |
| POST | `/api/account-sessions/import/json` | 条目支持上述账号文件字段与 `plugin_secret` / `plugin_public` / `plugin_identity` / `hidden` / `allow_route_skip` / `user_agent` / `usage_query` / `rate_limits`；插件条目自动走插件账号合同 |
| PUT/PATCH | `/api/account-sessions/{id}` | 支持上述插件元数据字段（三态：缺省保持 / 显式覆盖）；插件账号可挂在插件承载渠道下 |
| POST/GET | `/api/account-sessions/login/start|wait|cancel|callback` | 插件 provider 自动转交宿主 `login.start/poll/callback/cancel`；`state` 形如 `plugin:<uuid>`，成功后按插件账号合同落 `plugin_secret` |

插件账号落库契约：`plugin_secret`（不透明 JSON）、`plugin_public`、`plugin_state`（CAS，`{key:{revision,schema,value}}`，总量 ≤2 MiB）、`identity_key`（`sha256(provider + 稳定身份字段)` 派生主键，重复添加幂等）。

---

## 插件管理面子页 API

> 对齐 Agent2API v3.5.0-beta.1 的插件管理面（`api/plugin_*.rs` 七个模块）。
> 除 `GET /api/session/login/wasm-callback`（浏览器免鉴权回调，安全性由一次性
> `state` 承担）与三条静态资源路由（`/plugins/runtime/*`、`/plugins/{pack}/ui/*`、
> `/plugins/script-icons/{file}`，iframe/img 不携带令牌头、内容不含数据）外，
> 全部走管理员会话鉴权（`verify_admin`）。
>
> 静态资源三条路由**不进 OpenAPI 自描述**（它们是资源出口，不是管理 API），
> 在 `tools/extract_routes.py` 里以「不以 /api 开头」自然排除。

### 包视图与设置

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/plugins` | 列表。`packs[]` 每包追加 A2A 对齐字段：`icons`（图标目录相对路径）、`ui`（页面清单 `{id,title,icon,url,group,order}`）、`providerEnabled`（`{provider:bool}`）、`startup`（`{phase,running,crashes,error}`）、`homepage`、`providerCategory`；顶层追加 `maxPackBytes`、`scripts`（脚本宿主摘要）、`startup`（启动总览） |
| GET | `/api/plugins/startup` | 启动/加载阶段快照：`{packs:[{id,enabled,running,phase,error}], pending, script, now}` |
| GET | `/api/plugins/settings` | 插件更新通道与检查计划（`[update]` 段的 A2A 形状投影）：`{autoCheck, interval, stable, beta, autoUpdate, channelMode:"follow", checkMode:"follow"}` |
| PATCH | `/api/plugins/settings` | 保存插件更新设置（写回 `[update]`）。请求 `{autoCheck?, intervalMinutes?, stable?, beta?, autoUpdate?, channelMode?, checkMode?}`；`channelMode`/`checkMode` 仅接受 `follow`；间隔 60~10080 分钟 |
| POST | `/api/plugins/reload` | 热重启一个**已启用**的外置包（停→启，不改变 `enabled` 选择）。请求 `{id}` |

### provider 设置（CAS）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/plugins/provider-settings` | 全部 provider 的设置快照：`{settings:[{provider,label,package,schema,values,effective,invalid,revision}]}` |
| PATCH | `/api/plugins/provider-settings` | 保存某家设置。请求 `{provider, revision, values}`（键白名单，≤128 KiB）；`revision` 不符 → 409 |
| GET/PATCH | `/api/provider-settings` | 同上两条的旧名别名（同一 handler） |

### 插件后台任务与原生辅助

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/plugins/tasks` | 任务快照：`{tasks:[{provider, task:{id,label,enabled,times,interval_ms,...}, runs:[{account,running,lastRunAt,nextRunAt,error,result}]}]}`（只含已加载的 wasm provider） |
| PATCH | `/api/plugins/tasks` | 保存单个任务设置。请求 `{provider, task, settings}`；保存后回全量快照 |
| POST | `/api/plugins/tasks/run` | 立即跑一轮（`manual=true` 跳过启用与排期判定，仍受在途占位约束）。请求 `{provider, task}` |
| GET | `/api/plugins/helpers` | 原生辅助程序：`{grants, requested, candidates}`（`?pack=` 可过滤） |
| PATCH | `/api/plugins/helpers` | 授权 / 撤销。请求 `{provider, helper, target, pack?, confirmed?}`；`target=null` 为撤销，授权须 `confirmed=true`（≤256 KiB） |

### 插件登录交互

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/session/login/interaction` | 读当前交互步骤（`?state=<网关登录 state>`）。`{interaction:null}` = 无待填步骤；任务不存在/过期 → 404 |
| POST | `/api/session/login/interaction` | 提交一步。请求 `{state, interaction, inputs?, captcha?}`（≤256 KiB）；完成时落账号并回 `accountId` |
| GET | `/api/session/login/wasm-callback` | 浏览器 OAuth 回调（**免鉴权**：调用方是用户浏览器，安全性由一次性 `state` 承担）。返回人读纯文本页 |

### 静态资源（不进 OpenAPI 自描述）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/plugins/runtime/{*rest}` | 插件页运行期资源：`theme.css`/`tokens.css`（宿主设计令牌）、`providers/<id>.png\|svg`（品牌图标，读 `static/providers/`）；`islands/*` 在 AIGX 明确 404（无 islands 构建）。免鉴权、`no-store` |
| GET | `/plugins/{pack}/ui/{*rest}` | 插件包 `ui/` 目录托管（只服务**已启用**包；`..`/绝对路径/盘符/反斜杠全拒，canonicalize 双保险）。免鉴权、`no-store` |
| GET | `/plugins/script-icons/{file}` | 脚本插件图标（内容摘要文件名）：`ETag` + `If-None-Match`→304，`Cache-Control: public, max-age=31536000, immutable`，附 `nosniff`/`no-referrer`/沙箱 CSP |

## 签到活动福利与前端字段对表

### 活动福利 / 月度赠送

| 方法 | 路径 | 影子路径 | 说明 |
| --- | --- | --- | --- |
| GET | `/api/accounts/{id}/campaigns` | `/api/account-sessions/{id}/campaigns` | 活动福利预览（CampaignPreview 形状） |
| POST | `/api/accounts/{id}/campaigns/claim` | `/api/account-sessions/{id}/campaigns/claim` | 领取一份活动福利（body: `{campaignId, captchaVerifyParam?, captchaVerifyRegion?}`） |

语义（前端据此区分「不支持」与真错误）：
- **HTTP 400 + 文案含「活动福利」** = 该 provider 没有活动链路 → 前端显示空态；
- **HTTP 404** 只用于「账号 id 不存在」（真正的资源缺失）；
- 目前只有 ZCode 两地（`zcode` / `zcode-intl`）有限时套餐链路；其余家一律 400。
- 响应字段沿用 A2A `checkin-campaign-types.ts` 的 camelCase（`campaignId/title/status/...`、`riskNote/monthlyGifts/redemption`）。

> **注（有意的混用，勿统一）**：`/api/checkin-center` 的**账号行**字段为 snake_case
> （AIGX 既有风格：`streak_days` / `daily_checkin` / `available` …），而**活动对象**
> 字段为 camelCase（沿用 A2A `checkin-campaign-types.ts`，前端 `CheckinCampaignCard.tsx`
> 已按 `campaignId` / `monthlyGifts` / `riskNote` 读取）。两种风格并存是刻意设计，
> 「统一」任何一边都会让前端静默读成 `undefined`（显示 `—` 而不报错）。

### 前端读取点 ↔ 后端字段对表（snake_case 口径）

| 前端读取点 | 后端字段 |
| --- | --- |
| `CheckinAccountRow.checked_in_today/last_outcome/last_message/last_ts` | `/api/checkin-center` 的 `providers[].accounts[]` 同名字段 |
| `CheckinAccountRow.streak_days` | `accounts[].streak_days`（签到后回读成长中心连登） |
| `CheckinAccountRow.daily_checkin` | `accounts[].daily_checkin`（`{date,status,claim_at,message}`；仅 success/already 带 claim_at） |
| `CheckinAccountRow.available` | `accounts[].available`（`status == enabled && !expired`） |
| `CheckinRecord.account_id/account_name/kept_active/streak_days` | `/api/checkin-records` 与 `/api/checkin-center/records` 同名字段 |
| `CheckinSchedule.last_fired_date/last_fired_providers/provider_times/time_end/retry_limit/retry_gap_minutes/makeup_time` | `/api/checkin-schedules` 的 `schedules[]` 同名字段（`retry_time` 与 `makeup_time` 同值双写） |
| `CheckinCampaignReward.campaignId/startsAt/...` | `/api/accounts/{id}/campaigns` 的 `campaigns[]`（camelCase，A2A 契约） |
| `CheckinCenterData.campaigns/campaign_preview/records_stats/auto` | `/api/checkin-center` 同名字段 |
