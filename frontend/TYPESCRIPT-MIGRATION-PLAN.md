# AIGX 前端 TypeScript 迁移计划

## 现状分析

### 已完成部分 ✅
- **TypeScript 基础环境**: v5.3.3, @types/react, @types/react-dom
- **类型定义**: `types/index.ts` 已创建
- **API 层**: `api/index.ts` 已迁移到 TypeScript
- **架构配置**: Vite + React + TypeScript 支持

### 待迁移部分 📋
- **页面组件**: 27 个 .jsx → .tsx
- **UI 组件库**: 12 个 .jsx → .tsx
- **工具函数**: 部分仍为 JS
- **国际化**: .js 文件需类型化

## 迁移策略

### 阶段1: 基础设施准备
1. 创建 types 目录结构
2. 定义全局类型定义
3. 配置 ESLint TypeScript
4. 设置构建检查

### 阶段2: 组件库迁移（低风险）
**理由**: 纯 UI 组件依赖少，迁移风险低

```jsx:components/ui/*.jsx → components/ui/*.tsx
Button.jsx, Badge.jsx, Input/*.jsx
Loading.jsx, EmptyState.jsx, SectionCard.jsx
```

**组件级联**:
1. `utils/index.js` → `utils/index.ts` (工具函数类型化)
2. `components/ui/*.jsx` → `*.tsx` (保持 API 兼容)
3. `components/glass/*.jsx` → `*.tsx`

### 阶段3: 页面组件迁移（中等风险）
**优先级**: 按访问频率排序

```jsx:pages/*.jsx → *.tsx
App.jsx, Login.jsx, Register.jsx ❣️ 高频
Dashboard.jsx, Channels.jsx, Users.jsx ❣️ 高频
Settings.jsx, Security.jsx
Keys.jsx, Groups.jsx, Wallet.jsx
```

**迁移要点**:
1. 组件 Props 类型化
2. 事件处理函数类型定义
3. API 响应类型化
4. 状态定义类型检查

### 阶段4: 特殊页面迁移
```jsx:pages/*.jsx → *.tsx
Playground.jsx (需要更多类型定义)
Mappings.jsx (复杂映射逻辑)
IpManagement.jsx (配置相关)
Logs.jsx (数据分析)
Redemptions.jsx (表格编辑)
Orders.jsx, Epay.jsx, Pricing.jsx
```

### 阶段5: 基础设施完善
1. `api.js` → `api/index.ts` (最终 API 层统一)
2. `lib/utils.js` → `lib/utils.ts`
3. `i18n/index.js` → `i18n/index.ts` (国际化类型化)
4. 创建 `components/**/*.d.ts`（组件导出声明）

## 文件映射表

| 类别 | 当前格式 | 目标格式 | 复杂度 |
|------|----------|----------|--------|
| UI 组件 | Button.jsx | Button.tsx | 低 |
| 玻璃态组件 | GlassCard.jsx | GlassCard.tsx | 低 |
| 面板组件 | StatCard.jsx, SectionCard.jsx | *.tsx | 低 |
| 页面组件 | Channels.jsx, Users.jsx | *.tsx | 中 |
| 复杂页面 | Playground.jsx | *.tsx | 高 |
| 工具函数 | utils.js | utils.ts | 中 |
| 国际化 | i18n/index.js | i18n/index.ts | 低 |

## 迁移顺序建议

### 顺序1: 无依赖组件 (约15分钟)
```bash
components/ui/Button.jsx → Button.tsx
components/ui/Badge.jsx → Badge.tsx
components/ui/Loading.jsx → Loading.tsx
components/ui/EmptyState.jsx → EmptyState.tsx
components/ui/SectionCard.jsx → SectionCard.tsx
components/utils/**.js → *.ts
```

### 顺序2: 组件库 (约20分钟)
```bash
components/glass/GlassCard.jsx → GlassCard.tsx
components/glass/GlassDialog.jsx → GlassDialog.tsx
components/glass/GlassInput.jsx → GlassInput.tsx
components/Sidebar.jsx → Sidebar.tsx
components/Toast.jsx → Toast.tsx
components/ConfirmDialog.jsx → ConfirmDialog.tsx
components/ErrorBoundary.jsx → ErrorBoundary.tsx
```

### 顺序3: 页面组件 (约60分钟)
```bash
pages/App.jsx → App.tsx (核心路由)
pages/Login.jsx → Login.tsx
pages/Register.jsx → Register.tsx
pages/Dashboard.jsx → Dashboard.tsx
pages/Settings.jsx → Settings.tsx
pages/Channels.jsx → Channels.tsx
pages/Users.jsx → Users.tsx
pages/Keys.jsx → Keys.tsx
pages/Groups.jsx → Groups.tsx
pages/Wallet.jsx → Wallet.tsx
pages/Notify.jsx → Notify.tsx
```

### 顺序4: 特殊页面 (约40分钟)
```bash
pages/Playground.jsx → Playground.tsx
pages/Mappings.jsx → Mappings.tsx
pages/IpManagement.jsx → IpManagement.tsx
pages/Logs.jsx → Logs.tsx
pages/Redemptions.jsx → Redemptions.tsx
pages/Orders.jsx → Orders.tsx
pages/Epay.jsx → Epay.tsx
pages/Pricing.jsx → Pricing.tsx
pages/Security.jsx → Security.tsx
```

## 类型安全目标

### 目标1: 零隐式 any 组件Props
```typescript
// 迁移前
function Button({ children, onClick }: any) { ... }

// 迁移后
interface ButtonProps {
  children: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
}
function Button({ children, onClick, disabled }: ButtonProps) { ... }
```

### 目标2: API 响应类型可信
```typescript
// api/index.ts
interface ApiResponse {
  success: boolean;
  any;
}

// 具体类型化
interface Channel {
  id: string;
  name: string;
  apiKey: string;
}
```

### 目标3: 错误处理类型化
```typescript
// 更好的错误类型
class ApiError extends Error {
  status: number;
  data?: any;
  constructor(message: string, status: number, data?: any) {
    super(message);
    this.status = status;
    this.data = data;
  }
}
```

## CI/CD 集成

### Vite 构建配置
```javascript
// vite.config.ts 中已配置
typescript: {
  include: ['src/**/*.ts', 'src/**/*.tsx'],
  exclude: ['src/**/*.d.ts'],
  compilerOptions: {
    jsx: 'react-jsx',
    allowJs: true,
    checkJs: false,
    esModuleInterop: true,
    skipLibCheck: true,
  }
}
```

### TypeScript 检查
```bash
npm run typecheck  # tsc --noEmit
```

### ESLint 配置
```javascript
{
  "extends": [
    "eslint:recommended",
    "plugin:@typescript-eslint/recommended",
    "plugin:react-hooks/recommended"
  ]
}
```

## 回滚策略

### Git 分支保护
```bash
# 迁移前创建分支
git checkout -b feature/tsx-migration

# 小步提交
git add src/components/ui/Button.tsx
git commit -m "migrate Button component to TypeScript"
```

### 错误处理
```bash
# 如果类型检查失败
npm run typecheck

# 定位错误
tsc --noEmit 2>&1 | grep "src/"

# 回滚有问题的组件
git checkout HEAD -- web/components/ui/Button.jsx
```

## 验收标准

✅ 所有 `.jsx` 文件迁移到 `.tsx`
✅ `npm run typecheck` 无错误
✅ `npm run build` 成功
✅ 无 `any` 类型入 Props（关键）
✅ 复杂组件至少 50% Props 已类型化
✅ 迁移日志完善（commit message 记录）

## 进度追踪

- [ ] 阶段1: 基础设施准备
- [ ] 顺序1: 无依赖组件迁移
- [ ] 顺序2: 组件库迁移
- [ ] 顺序3: 主页面组件迁移
- [ ] 顺序4: 特殊页面迁移
- [ ] 阶段5: 基础设施完善

## 已知风险与缓解

### 风险1: 可能有循环引用
**缓解**: 迁移前分析依赖图，先迁移无依赖组件

### 风险2: JSX 语法兼容性问题
**缓解**: 使用 `// @ts-ignore` 临时标记，逐步修复

### 风险3: 现有运行时依赖不确定性
**缓解**: 使用 `window.Dependent` 或 `external` 声明

### 风险4: 文件重构不影响功能
**缓解**: 每完成一个组件立即测试：`npm run dev`

## 预估工时

- **阶段1 (准备)**: 15分钟
- **顺序1-2 (UI库)**: 40分钟
- **顺序3 (主页面)**: 2小时
- **顺序4 (特殊页面)**: 1.5小时
- **阶段5 (完善)**: 30分钟
- **总计**: 约 5 小时

## 验证清单

- [ ] 本地开发环境 (npm run dev) 正常
- [ ] 类型检查 (npm run typecheck) 通过
- [ ] 构建失败 (npm run build) 成功
- [ ] E2E 测试通过 (playwright test)
- [ ] 所有 commits 正确格式化
- [ ] 代码审查完成 (PR 验证)