import React, { lazy, Suspense } from 'react';
import { Routes, Route, Navigate } from 'react-router-dom';
import Sidebar from './components/Sidebar';
import ErrorBoundary from './components/ErrorBoundary';
import RouteErrorPage from './components/RouteErrorPage';
import PermissionDenied from './components/PermissionDenied';
import { ToastProvider } from './components/Toast';
import { isAdmin } from './lib/utils';
import Login from './pages/Login';
import Register from './pages/Register';
import Dashboard from './pages/Dashboard';

// 路由级懒加载：首屏只拉 Dashboard 相关代码，管理页按需加载
const Keys = lazy(() => import('./pages/Keys'));
const Users = lazy(() => import('./pages/Users'));
const Wallet = lazy(() => import('./pages/Wallet'));
const Settings = lazy(() => import('./pages/Settings'));
const Profile = lazy(() => import('./pages/Profile'));
const Logs = lazy(() => import('./pages/Logs'));
const Redemptions = lazy(() => import('./pages/Redemptions'));
const Plans = lazy(() => import('./pages/Plans'));
const Channels = lazy(() => import('./pages/Channels'));
const Chat = lazy(() => import('./pages/Chat'));

const Models = lazy(() => import('./pages/Models'));
const Prompts = lazy(() => import('./pages/Prompts'));
const Pricing = lazy(() => import('./pages/Pricing'));

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
  return (
    <div className="app-container">
      <Sidebar />
      <main className="main-content" key={location}>
        <div className="page-fade-enter">{children}</div>
      </main>
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
        <Suspense fallback={<PageFallback />}>
          <Routes>
            <Route path="/login" element={<PublicRoute><Login /></PublicRoute>} />
            <Route path="/register" element={<PublicRoute><Register /></PublicRoute>} />
            <Route path="/" element={<ProtectedLayout><Dashboard /></ProtectedLayout>} />
            <Route path="/accounts" element={<Navigate to="/channels" replace />} />
            <Route path="/channels" element={<AdminRoute><Channels /></AdminRoute>} />
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
            <Route path="/profile" element={<ProtectedLayout><Profile /></ProtectedLayout>} />
            <Route path="*" element={<RouteErrorPage status={404} />} />
          </Routes>
        </Suspense>
      </ToastProvider>
    </ErrorBoundary>
  );
}
