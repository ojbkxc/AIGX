import React, { lazy, Suspense } from 'react';
import { Routes, Route, Navigate } from 'react-router-dom';
import Sidebar from './components/Sidebar';
import StatusLine from './components/StatusLine';
import ErrorBoundary from './components/ErrorBoundary';
import RouteErrorPage from './components/RouteErrorPage';
import PermissionDenied from './components/PermissionDenied';
import { ToastProvider } from './components/Toast';
import { isAdmin } from './lib/utils';
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
const Redemptions = lazyPage(() => import('./pages/Redemptions'));
const Plans = lazyPage(() => import('./pages/Plans'));
const Channels = lazyPage(() => import('./pages/Channels'));
const AccountSessions = lazyPage(() => import('./pages/AccountSessions'));
const Chat = lazyPage(() => import('./pages/Chat'));

const Models = lazyPage(() => import('./pages/Models'));
const Prompts = lazyPage(() => import('./pages/Prompts'));
const Pricing = lazyPage(() => import('./pages/Pricing'));
const Agent = lazyPage(() => import('./pages/Agent'));
const Tickets = lazyPage(() => import('./pages/Tickets'));

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
      <main className="main-content" key={location}>
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

export default function App(): JSX.Element {
  return (
    <ErrorBoundary>
      <ToastProvider>
        <Routes>
          <Route path="/login" element={<PublicRoute><Login /></PublicRoute>} />
          <Route path="/register" element={<PublicRoute><Register /></PublicRoute>} />
          <Route path="/" element={<ProtectedLayout><Dashboard /></ProtectedLayout>} />
          <Route path="/accounts" element={<Navigate to="/channels" replace />} />
          <Route path="/channels" element={<AdminRoute><Channels /></AdminRoute>} />
          <Route path="/account-sessions" element={<AdminRoute><AccountSessions /></AdminRoute>} />
          <Route path="/keys" element={<ProtectedLayout><Keys /></ProtectedLayout>} />
          {/* 已合并的旧路由 → 重定向到合并后位置 */}
          <Route path="/mappings" element={<Navigate to="/channels" replace />} />
          <Route path="/pricing" element={<AdminRoute><Pricing /></AdminRoute>} />
          <Route path="/groups" element={<Navigate to="/settings" replace />} />
          <Route path="/orders" element={<Navigate to="/wallet" replace />} />
          <Route path="/epay" element={<Navigate to="/settings" replace />} />
          <Route path="/notify" element={<Navigate to="/settings" replace />} />
          <Route path="/security" element={<Navigate to="/settings" replace />} />
          <Route path="/ip-management" element={<Navigate to="/settings" replace />} />
          <Route path="/users" element={<AdminRoute><Users /></AdminRoute>} />
          <Route path="/wallet" element={<ProtectedLayout><Wallet /></ProtectedLayout>} />
          <Route path="/logs" element={<ProtectedLayout><Logs /></ProtectedLayout>} />
          <Route path="/redemptions" element={<AdminRoute><Redemptions /></AdminRoute>} />
          <Route path="/plans" element={<AdminRoute><Plans /></AdminRoute>} />
          <Route path="/playground" element={<Navigate to="/chat" replace />} />
          <Route path="/chat" element={<ProtectedLayout><Chat /></ProtectedLayout>} />
          {/* 网络层管理已并入系统设置运维 tab，旧路由重定向防书签失效 */}
          <Route path="/network-layer" element={<Navigate to="/settings" replace />} />
          <Route path="/model-presets" element={<ProtectedLayout><Models /></ProtectedLayout>} />
          <Route path="/prompts" element={<ProtectedLayout><Prompts /></ProtectedLayout>} />
          <Route path="/settings" element={<AdminRoute><Settings /></AdminRoute>} />
          <Route path="/agent" element={<AdminRoute><Agent /></AdminRoute>} />
          <Route path="/tickets" element={<ProtectedLayout><Tickets /></ProtectedLayout>} />
          <Route path="/profile" element={<ProtectedLayout><Profile /></ProtectedLayout>} />
          <Route path="*" element={<RouteErrorPage status={404} />} />
        </Routes>
      </ToastProvider>
    </ErrorBoundary>
  );
}
