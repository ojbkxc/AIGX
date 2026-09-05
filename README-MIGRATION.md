# AIGX UI现代化迁移指南

> 本文档提供了从当前React 18.2 + Vite到现代化技术栈的迁移路线图

## 🎯 迁移目标

基于 `rustapi/new-api` 的验证，实现：
- React 19 + TypeScript 6.0
- Tailwind CSS 4 + shadcn/ui
- Rsbuild构建工具
- TanStack全家桶
- 完整的100年不过时架构

## 📁 已创建的基础文件

### 前端
- `frontend/tsconfig.json` - TypeScript类型检查配置
- `frontend/tailwind.config.js` - Tailwind CSS主题配置
- `frontend/modern-frontend.config.js` - 构建配置规范
- `frontend/main.jsx` - 改良的React入口点
- `frontend/package.json.backup` - 当前版本备份

### 后端
- `Cargo.toml` - Rust依赖配置
- `rust-toolchain.toml` - Rust工具链版本
- `.github/workflows/frontend-validation.yml` - CI验证工作流
- `.gitignore` - 完整的忽略规则

## 🚀 实施步骤

### Phase 1: 基础准备（已完成）
- [x] TypeScript配置
- [x] Tailwind CSS配置
- [x] Rust后端配置
- [x] CI/CD设置

### Phase 2: 依赖升级
```bash
cd frontend

# 1. 创建备份
cp package.json package.json-v1.0.1.backup

# 2. 升级依赖（需要在GitHub上验证）
npm install react@~19.2.7 react-dom@~19.2.7
npm install typescript@~5.3.0 @types/react@^19.2.17 @types/react-dom@^19.2.3
npm install tailwindcss@latest postcss autoprefixer -D
npm install clsx@^2.1.1 tailwind-merge@^3.5.0

# 3. 安装shadcn/ui（参考new-api）
npx shadcn@latest init

# 4. 安装核心组件
npx shadcn@latest add button input table dialog badge toast select

# 5. 安装TanStack全家桶
npm install @tanstack/react-query@^5.0.0 @tanstack/react-table@^8.21.3
npm install framer-motion@^12.0.0 zod@^4.4.3
npm install axios react-hook-form @hookform/resolvers/zod

# 6. 安装图标库
npm install lucide-react react-icons

# 7. 安装动画库
npm install @emotion/react @emotion/styled
```

### Phase 3: 代码迁移

#### 1. Moving from state to Query
```bash
# 建议先迁移Dashboard
# src/pages/Dashboard.tsx
import { useQuery, useMutation } from '@tanstack/react-query';

function Dashboard() {
  const { data, isLoading, error } = useQuery({
    queryKey: ['realtimeData'],
    queryFn: async () => {
      const res = await fetch('/api/dashboard/realtime');
      return res.json();
    },
    refetchInterval: 5000, // 5秒刷新
    staleTime: 5000
  });

  if (isLoading) return <DashboardSkeleton />;

  return (/* UI Components */);
}
```

#### 2. Replacing api.js with TypeScript API client
```typescript
// src/api/client.ts
export interface AuthResponse {
  success: boolean;
  data?: {
    token: string;
    email: string;
    username: string;
    expires_at: number;
  };
  error?: string;
}

export const apiClient = {
  login: async (email: string, password: string): Promise<AuthResponse> => {
    const res = await fetch('/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email, password })
    });
    return res.json();
  },

  // ... 其他 API 方法
};
```

### Phase 4: UI组件现代化

#### 新的GlassCard组件
```tsx
// src/components/ui/GlassCard.tsx
import { cva, type VariantProps } from 'class-variance-authority';
import { cn } from '@/lib/utils';

const glassCardVariants = cva(
  'glass-card',
  {
    variants: {
      variant: {
        default: 'from-purple-500/20 via-pink-500/20 to-indigo-500/20',
        danger: 'from-red-500/20 via-orange-500/20 to-yellow-500/20',
        success: 'from-green-500/20 via-blue-500/20 to-cyan-500/20'
      },
      size: {
        sm: 'p-4',
        md: 'p-6',
        lg: 'p-8'
      }
    },
    defaultVariants: {
      variant: 'default',
      size: 'md'
    }
  }
);

export interface GlassCardProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof glassCardVariants> {
  title?: string;
  subtitle?: string;
}

export function GlassCard({ className, variant, size, title, subtitle, ...props }: GlassCardProps) {
  return (
    <div className={cn(glassCardVariants({ variant, size }), className)} {...props}>
      {title && <h3 className="text-xl font-semibold mb-2">{title}</h3>}
      {subtitle && <p className="text-muted-foreground mb-4">{subtitle}</p>}
      {props.children}
    </div>
  );
}
```

#### 动态侧边栏（基于Role）
```typescript
// src/components/layout/RoleBasedSidebar.tsx
import { useEffect } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';

interface MenuItem {
  id: string;
  labelKey: string;
  icon: string;
  permission: PermissionType;
  path: string;
}

const MENU_ITEMS: MenuItem[] = [
  {
    id: 'dashboard',
    labelKey: '仪表盘',
    icon: 'Dashboard',
    permission: PERMISSIONS.ALL,
    path: '/'
  },
  {
    id: 'channels',
    labelKey: '渠道管理',
    icon: 'Channel',
    permission: PERMISSIONS.MANAGE_CHANNELS,
    path: '/channels'
  },
  // ... 其他菜单项
];

export function RoleBasedSidebar() {
  const [user, setUser] = useState<User | null>(null);

  useEffect(() => {
    // 从API获取当前用户信息和权限
    // 未来可替换为TanStack Store
    loadUser();
  }, []);

  return (
    <Sidebar>
      <nav>
        {MENU_ITEMS.filter(item => hasPermission(item.permission)).map(item => (
          <NavLink
            key={item.id}
            to={item.path}
            className={({ isActive }) => cn(
              'glass-card p-3 rounded-lg transition-all',
              isActive && 'ring-2 ring-purple-500'
            )}
          >
            <Icon name={item.icon} />
            <span>{item.labelKey}</span>
          </NavLink>
        ))}
      </nav>
    </Sidebar>
  );
}
```

## 🔄 兼容性考虑

### 1. 双轨兼容
```tsx
// src/components/legacy/ComponentAdapter.tsx
import LegacyComponent from './Component.jsx';
import ModernComponent from './NewComponent.tsx';

export function ComponentAdapter({ version = 'modern' }: { version?: 'legacy' | 'modern' }) {
  if (version === 'legacy') {
    return <LegacyComponent />;
  }
  return <ModernComponent />;
}
```

### 2. Feature Flags
```typescript
// src/config/feature-flags.ts
export const FEATURE_FLAGS = {
  realTimeMonitoring: true,    // React Query轮询
  roleBasedAccess: true,        // 权限系统
  darkMode: true,               // 暗黑主题
  mobileOptimized: false,       // 移动端适配（尚未）
  advancedDashboard: true      // 高级流量监控
};

// 使用
if (FEATURE_FLAGS.realTimeMonitoring) {
  // 加载现代组件
}
```

## 📊 验证策略

### GitHub Actions验证流程
1. **TypeScript检查**: `bun run typecheck`
2. **Lint检查**: `bun run lint`
3. **测试运行**: `bun run test`
4. **生产构建**: `bun run build`
5. **进行分割**: 每个功能模块单独验证

### 代码审查清单
- [ ] TypeScript类型覆盖率>80%
- [ ] 组件无硬编码值
- [ ] 所有文本都使用i18n
- [ ] 响应式设计考虑了移动端
- [ ] 无console.log遗留
- [ ] 所有API调用都经过类型检查

## 🎯 下一步行动建议

### 立即可行的第一步：
1. 推送创建了基础文件到GitHub
2. 触发CI验证已存在配置
3. 在测试分支创建SPI（后端适配器）演示
4. 小步迭代（每个sprint完成1-2个页面迁移）

### 着手的前5个页面：
1. **Login/Register页** - 最简单，已有基础
2. **Settings页** - 配置管理，复用度高
3. **Channels页** - 核心功能，但复杂
4. **Dashboard页** - 展示现代化效果
5. **Keys页** - 可直接集成shadcn组件

## 🔗 参考资源

### 内部引用
- `[UI全面分析](uic-analysis-final-2026-09-05.md)` - 详细分析报告
- `[架构路线2100](ARCHITECTURE-HORIZON-2100.md)` - 长期架构
- `[API架构2100](API-ARCHITECTURE-2100.md)` - 后端Rust架构

### 外部参考（经过验证）
- [rustapi/new-api-web](../rustapi/new-api-main/new-api-main/web) - 最先进的实现参照
- [shadcn/ui](https://shadcn.com/ui) - 组件库设计
- [TanStack全家桶](https://tanstack.com/) - 数据管理

---

*迁移是为未来预留机会，不是追赶潮流。我们的目标是构建一个100年内都适用的系统。*