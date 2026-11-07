import React, { lazy, Suspense, useEffect } from 'react';
import { Routes, Route, Navigate, useLocation } from 'react-router-dom';
import Sidebar from './components/Sidebar';
import StatusLine from './components/StatusLine';
import ErrorBoundary from './components/ErrorBoundary';
import RouteErrorPage from './components/RouteErrorPage';
import PermissionDenied from './components/PermissionDenied';
import { ToastProvider } from './components/Toast';
import { isAdmin } from './lib/utils';
import { useMobileColumns } from './hooks/useMobileColumns';
import { start as startCaptchaPool, stop as stopCaptchaPool } from './lib/zcodeCaptchaPool';
import Login from './pages/Login';
import Register from './pages/Register';
import Dashboard from './pages/Dashboard';

// 路由级懒加载：首屏只拉 Dashboard 相关代码，管理页按需加载
//
// `lazyPage`：chunk 加载失败时自动重试一次。常见触发场景是发布新版本后
// 旧 chunk 被替换（哈希文件名变了），用户停在旧页面点菜单 → 请求旧 chunk
// 404 → React.lazy 抛出 → 整页白屏。重试一次通常能命中新 chunk；仍失败
// 则交给页面级 ErrorBoundary 显示可读提示（Sidebar 还在，能切到别的页）。
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyComponent = React.ComponentType<any>;

function lazyPage(loader: () => Promise<{ default: AnyComponent }>) {
  return lazy(() =>
    loader().catch(
      () =>
        new Promise<{ default: AnyComponent }>((resolve) =>
          setTimeout(() => resolve(loader()), 300),
        ),
    ),
  );
}

const Keys = lazyPage(() => import('./pages/Keys'));
const Users = lazyPage(() => import('./pages/Users'));
const Wallet = lazyPage(() => import('./pages/Wallet'));
const Settings = lazyPage(() => import('./pages/Settings'));
const Profile = lazyPage(() => import('./pages/Profile'));
const Logs = lazyPage(() => import('./pages/Logs'));
const Products = lazyPage(() => import('./pages/Products'));
const Gateways = lazyPage(() => import('./pages/Gateways'));
const ProxyPool = lazyPage(() => import('./pages/ProxyPool'));
const Chat = lazyPage(() => import('./pages/Chat'));

const ModelManage = lazyPage(() => import('./pages/ModelManage'));
const Prompts = lazyPage(() => import('./pages/Prompts'));
const Agent = lazyPage(() => import('./pages/Agent'));
const Tickets = lazyPage(() => import('./pages/Tickets'));
const CheckinCenter = lazyPage(() => import('./pages/CheckinCenter'));
const ScheduledTasks = lazyPage(() => import('./pages/ScheduledTasks'));
const Plugins = lazyPage(() => import('./pages/Plugins'));

/** 懒加载页面兜底骨架（与全局 loading 视觉一致） */
function PageFallback(): JSX.Element {
  return <div className="loading">Loading…</div>;
}

function isAuthenticated(): boolean {
  const token = localStorage.getItem('token');
  const expiresAt = localStorage.getItem('expires_at');
  if (!token || !expiresAt) return false;
  return Date.now() < parseInt(expiresAt, 10);
}

interface ProtectedLayoutProps {
  children: React.ReactNode;
}

function ProtectedLayout({ children }: ProtectedLayoutProps) {
  const location = window.location.pathname;
  if (!isAuthenticated()) {
    return <Navigate to="/login" replace />;
  }
  // 首登强制改密：内置管理员初始密码是公开弱口令，登录后写 must_change_password
  // 标记。只在登录时跳一次不够——用户点侧边栏就能绕开，弱口令长期留着。
  // 这里做路由级拦截：除 /profile 外一律送回改密页（改密成功后标记被清除）。
  if (localStorage.getItem('must_change_password') && location !== '/profile') {
    return <Navigate to="/profile?force_password=1" replace />;
  }
  return (
    <div className="app-container">
      <Sidebar />
      {/* /chat 与 /agent 同构走内容流：main-content 高度由内容撑，
          输入条紧随最后一条消息（不再被 100vh 钉死在视口底——用户反馈
          「输入框太靠下、模型看不见」）。历史抽屉的高度来自 .pg-shell 的
          100%，在 auto 父级里退化为内容高度，抽屉自身 CSS 已按该形态
          适配（.sd-root.sidebar 高度由外层 flex 决定，随内容伸缩）。 */}
      <main
        className="main-content"
        key={location}
      >
        <div className="page-fade-enter">
          {/* 页面级边界：单个页面渲染异常/懒加载失败只影响内容区，
              Sidebar 保持可用，用户能切到别的页 —— 而不是整页白屏。 */}
          <ErrorBoundary variant="inline">
            <Suspense fallback={<PageFallback />}>{children}</Suspense>
          </ErrorBoundary>
        </div>
      </main>
      {/* 底部常驻状态行：仅 admin 可见（内部自行判断），普通用户不渲染 */}
      <StatusLine />
    </div>
  );
}

interface PublicRouteProps {
  children: React.ReactNode;
}

function PublicRoute({ children }: PublicRouteProps) {
  if (isAuthenticated()) {
    return <Navigate to="/" replace />;
  }
  return children;
}

interface AdminRouteProps {
  children: React.ReactNode;
}

/**
 * AdminRoute — 管理端路由守卫。
 * 非管理员直接渲染友好提示页，不触发管理接口，避免 401 误踢登录态。
 */
function AdminRoute({ children }: AdminRouteProps) {
  if (!isAdmin()) {
    return <ProtectedLayout><PermissionDenied /></ProtectedLayout>;
  }
  return <ProtectedLayout>{children}</ProtectedLayout>;
}

/**
 * ZCode 验证码令牌池的后台铸造器（管理端登录后自启）。
 *
 * ── 为什么不放在设置页 ────────────────────────────────────────
 * 令牌是**转发链路**的输入（活动套餐通道每条请求都要一枚），而用户不会为了
 * 发请求先去开着设置页。挂在这里 = 管理端一登录就开始守着。
 *
 * ── 为什么不能用 `useEffect([])` ───────────────────────────────
 * 登录是**客户端跳转**（`Login.tsx` 里 `navigate('/')`），App 不会重挂载 ——
 * 一个空依赖的 effect 只在首屏跑一次，那时还没登录，于是「不启动」的结论
 * 会被记一辈子。所以这里 `useLocation()` 订阅路由：登录/登出走客户端跳转时
 * 把 `active` 重新算一遍。
 *
 * 代价是每次路由变化都会 `stop()` + `start()` 一次 —— 两者都是幂等的
 * （`stop` 只清计时器、`start` 只重排一次），不会漏掉循环。
 *
 * 非管理员不启动：这些接口都是管理接口，普通用户每轮只换回一条 401。
 * 没有 ZCode 账号时铸造器自己退让不铸（网关的 `needsTokens` 判据）。
 */
function CaptchaPoolRunner(): null {
  const { pathname } = useLocation();
  const active = isAuthenticated() && isAdmin();
  useEffect(() => {
    if (!active) return undefined;
    startCaptchaPool();
    return () => stopCaptchaPool();
  }, [active, pathname]);
  return null;
}

export default function App(): JSX.Element {
  useMobileColumns();
  return (
    <ErrorBoundary>
      <ToastProvider>
        <CaptchaPoolRunner />
        <Routes>
          <Route path="/login" element={<PublicRoute><Login /></PublicRoute>} />
          <Route path="/register" element={<PublicRoute><Register /></PublicRoute>} />
          <Route path="/" element={<ProtectedLayout><Dashboard /></ProtectedLayout>} />
          <Route path="/accounts" element={<Navigate to="/channels" replace />} />
          {/* 渠道与账号池合并为单页双 Tab：上游渠道默认，账号池走 ?tab=accounts */}
          <Route path="/channels" element={<AdminRoute><Gateways /></AdminRoute>} />
          <Route
            path="/account-sessions"
            element={<Navigate to="/channels?tab=accounts" replace />}
          />
          <Route path="/proxy-pool" element={<AdminRoute><ProxyPool /></AdminRoute>} />
          <Route path="/keys" element={<ProtectedLayout><Keys /></ProtectedLayout>} />
          {/* 已合并的旧路由 → 重定向到合并后位置 */}
          <Route path="/mappings" element={<Navigate to="/channels" replace />} />
          {/* 倍率配置已并入模型管理第三 Tab，旧路由重定向防书签失效 */}
          <Route path="/pricing" element={<Navigate to="/model-manage?tab=ratio" replace />} />
          <Route path="/groups" element={<Navigate to="/settings" replace />} />
          <Route path="/orders" element={<Navigate to="/wallet" replace />} />
          <Route path="/epay" element={<Navigate to="/settings" replace />} />
          <Route path="/notify" element={<Navigate to="/settings" replace />} />
          <Route path="/security" element={<Navigate to="/settings" replace />} />
          <Route path="/ip-management" element={<Navigate to="/settings" replace />} />
          <Route path="/users" element={<AdminRoute><Users /></AdminRoute>} />
          <Route path="/wallet" element={<ProtectedLayout><Wallet /></ProtectedLayout>} />
          <Route path="/logs" element={<ProtectedLayout><Logs /></ProtectedLayout>} />
          {/* 套餐与兑换码合并为单页双 Tab；旧路由重定向防书签失效 */}
          <Route path="/products" element={<AdminRoute><Products /></AdminRoute>} />
          <Route path="/redemptions" element={<Navigate to="/products?tab=redemptions" replace />} />
          <Route path="/plans" element={<Navigate to="/products" replace />} />
          <Route path="/playground" element={<Navigate to="/chat" replace />} />
          <Route path="/chat" element={<ProtectedLayout><Chat /></ProtectedLayout>} />
          {/* 网络层管理已并入系统设置运维 tab，旧路由重定向防书签失效 */}
          <Route path="/network-layer" element={<Navigate to="/settings" replace />} />
          {/* 模型管理（渠道 + 账号池合流，admin）：旧的 /model-presets 目录页并入此处 */}
          <Route path="/model-manage" element={<AdminRoute><ModelManage /></AdminRoute>} />
          <Route path="/model-presets" element={<Navigate to="/model-manage" replace />} />
          <Route path="/prompts" element={<ProtectedLayout><Prompts /></ProtectedLayout>} />
          <Route path="/settings" element={<AdminRoute><Settings /></AdminRoute>} />
          <Route path="/agent" element={<AdminRoute><Agent /></AdminRoute>} />
          <Route path="/checkin" element={<AdminRoute><CheckinCenter /></AdminRoute>} />
          <Route path="/scheduled-tasks" element={<AdminRoute><ScheduledTasks /></AdminRoute>} />
          {/* 系统诊断并入渠道页第三 Tab「健康」（07 E.3：诊断三件套全部以渠道
              为粒度）；独立路由重定向防书签失效 */}
          <Route path="/diagnostics" element={<Navigate to="/channels?tab=health" replace />} />
          {/* 接入文档并入 API 密钥页第二 Tab（07 E.3：密钥与文档是同一件事的
              两半——拿 key → 怎么填）。原 /docs 是管理员专属，并入后文档 Tab
              对全员可见（面板内部按角色分层）；旧路由重定向防书签失效 */}
          <Route path="/docs" element={<Navigate to="/keys?tab=docs" replace />} />
          {/* 插件管理（契约对齐 Agent2API；后端建设中整页降级空态） */}
          <Route path="/plugins" element={<AdminRoute><Plugins /></AdminRoute>} />
          <Route path="/tickets" element={<ProtectedLayout><Tickets /></ProtectedLayout>} />
          <Route path="/profile" element={<ProtectedLayout><Profile /></ProtectedLayout>} />
          <Route path="*" element={<RouteErrorPage status={404} />} />
        </Routes>
      </ToastProvider>
    </ErrorBoundary>
  );
}
