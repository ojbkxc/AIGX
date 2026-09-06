import React from 'react';
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
import Keys from './pages/Keys';
import Mappings from './pages/Mappings';
import Users from './pages/Users';
import Wallet from './pages/Wallet';
import Orders from './pages/Orders';
import Epay from './pages/Epay';
import Settings from './pages/Settings';
import Profile from './pages/Profile';
import Logs from './pages/Logs';
import Redemptions from './pages/Redemptions';
import Channels from './pages/Channels';
import Pricing from './pages/Pricing';
import Groups from './pages/Groups';
import Notify from './pages/Notify';
import Playground from './pages/Playground';
import Security from './pages/Security';
import IpManagement from './pages/IpManagement';
import NetworkLayer from './pages/NetworkLayer';

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
        <Routes>
          <Route path="/login" element={<PublicRoute><Login /></PublicRoute>} />
          <Route path="/register" element={<PublicRoute><Register /></PublicRoute>} />
          <Route path="/" element={<ProtectedLayout><Dashboard /></ProtectedLayout>} />
          <Route path="/accounts" element={<Navigate to="/channels" replace />} />
          <Route path="/channels" element={<AdminRoute><Channels /></AdminRoute>} />
          <Route path="/keys" element={<ProtectedLayout><Keys /></ProtectedLayout>} />
          <Route path="/mappings" element={<AdminRoute><Mappings /></AdminRoute>} />
          <Route path="/pricing" element={<AdminRoute><Pricing /></AdminRoute>} />
          <Route path="/users" element={<AdminRoute><Users /></AdminRoute>} />
          <Route path="/groups" element={<AdminRoute><Groups /></AdminRoute>} />
          <Route path="/wallet" element={<ProtectedLayout><Wallet /></ProtectedLayout>} />
          <Route path="/orders" element={<AdminRoute><Orders /></AdminRoute>} />
          <Route path="/epay" element={<AdminRoute><Epay /></AdminRoute>} />
          <Route path="/logs" element={<AdminRoute><Logs /></AdminRoute>} />
          <Route path="/redemptions" element={<AdminRoute><Redemptions /></AdminRoute>} />
          <Route path="/notify" element={<AdminRoute><Notify /></AdminRoute>} />
          <Route path="/playground" element={<ProtectedLayout><Playground /></ProtectedLayout>} />
          <Route path="/security" element={<AdminRoute><Security /></AdminRoute>} />
          <Route path="/ip-management" element={<AdminRoute><IpManagement /></AdminRoute>} />
          <Route path="/network-layer" element={<AdminRoute><NetworkLayer /></AdminRoute>} />
          <Route path="/settings" element={<AdminRoute><Settings /></AdminRoute>} />
          <Route path="/profile" element={<ProtectedLayout><Profile /></ProtectedLayout>} />
          <Route path="*" element={<Navigate to="/" replace />} />
          <Route path="*" element={<RouteErrorPage />} />
        </Routes>
      </ToastProvider>
    </ErrorBoundary>
  );
}
