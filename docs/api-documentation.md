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