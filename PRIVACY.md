# PRIVACY.md
> AIGX项目隐私政策和数据处理规范

## 数据处理原则

### 1. 数据最小化
- 只收集和存储必要的数据
- 避免收集非业务必要的信息

### 2. 数据保护
- 敏感数据加密存储
- 符合GDPR、CCPA等隐私法规
- 定期安全审计

## 数据处理流程

### 用户数据
```mermaid
graph LR
    A[用户注册] → B[数据加密]
    B → C[数据库存储]
    C → D{数据访问}
    D -- 读取 → E[权限验证]
    D -- 写入 → F[数据校验]
    E → G[返回结果]
    F → H[写入数据库]
```

### API密钥管理
```typescript
// 敏感数据加密存储
export async function encryptApiKey(apiKey: string): Promise<string> {
  const salt = crypto.randomBytes(16);
  const iv = crypto.randomBytes(16);
  const encrypted = await crypto.subtle.encrypt(
    { iv: iv, name: 'AES-GCB' },
    await crypto.subtle.importKey(
      'raw',
      password,
      'AES-GCB',
      false,
      ['encrypt']
    ),
    textEncoder.encode(apiKey)
  );
  return salt + iv + encrypted;
}
```

## 日志和审计

### 日志策略
- 默认禁用敏感信息日志
- 日志脱敏处理
- 日志保留期限制

### 审计日志
```typescript
interface AuditLog {
  timestamp: Date;
  userId: string;
  action: string;
  resource: string;
  ip: string;
  userAgent: string;
  result: 'success' | 'failure';
  details?: string;
}
```

## 权限控制

### 基于角色的访问控制（RBAC）
```typescript
enum UserRole {
  ADMIN = 'admin',
  MANAGER = 'manager',
  USER = 'user',
  AUDITOR = 'auditor'
}

enum Permission {
  SYSTEM_ADMIN = 'system:admin',
  USER_VIEW = 'user:view',
  USER_EDIT = 'user:edit',
  CHANNEL_VIEW = 'channel:view',
  CHANNEL_EDIT = 'channel:edit',
  KEY_VIEW = 'key:view',
  KEY_CREATE = 'key:create',
  KEY_ROTATE = 'key:rotate',
  BILLING_VIEW = 'billing:view',
  LOGS_VIEW = 'logs:view',
  LOGS_EXPORT = 'logs:export'
}
```

## 隐私合规

### GDPR合规
- 数据主体权利支持
- 数据删除请求处理
- 数据可移植性

### 安全措施
```yaml
security:
  tls: true
  encryption: AES-256
  two_factor_auth: true
  session_timeout: 30 min
  password_complexity: true
```

## 数据泄露响应

```mermaid
graph LR
    A[数据泄露检测] → B[立即通知]
    B → C[风险评估]
    C → D[针对性修复]
    D → E[违规声明]
    E → F[持续监控]
```

## 第三方数据处理

- 明确的第三方服务协议
- 数据处理协议（DPA）
- 独立的审计条款

## 用户权利

1. 访问权 - 获取个人数据副本
2. 更正权 - 请求数据更正
3. 删除权 - 请求数据删除（被遗忘权）
4. 可携带权 - 获取数据可移植性
5. 反对权 - 反对数据处理
6. 限制处理权 - 限制数据处理
7. 向监管机构举报权