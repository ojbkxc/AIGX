import React from 'react';
import { Routes, Route, Navigate } from 'react-router-dom';
import Sidebar from './components/Sidebar';
import ErrorBoundary from './components/ErrorBoundary';
import { ToastProvider } from './components/Toast';
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

function isAuthenticated() {
  const token = localStorage.getItem('token');
  const expiresAt = localStorage.getItem('expires_at');
  if (!token || !expiresAt) return false;
  return Date.now() < parseInt(expiresAt, 10);
}

function ProtectedLayout({ children }) {
  if (!isAuthenticated()) {
    return <Navigate to="/login" replace />;
  }
  return (
    <div className="app-container">
      <div className="bg-orbs-container">
        <div className="bg-orb bg-orb-1"></div>
        <div className="bg-orb bg-orb-2"></div>
      </div>
      <Sidebar />
      <main className="main-content">
        {children}
      </main>
    </div>
  );
}

function PublicRoute({ children }) {
  if (isAuthenticated()) {
    return <Navigate to="/" replace />;
  }
  return children;
}

export default function App() {
  return (
    <ErrorBoundary>
      <ToastProvider>
        <Routes>
          <Route path="/login" element={<PublicRoute><Login /></PublicRoute>} />
          <Route path="/register" element={<PublicRoute><Register /></PublicRoute>} />
          <Route path="/" element={<ProtectedLayout><Dashboard /></ProtectedLayout>} />
          <Route path="/accounts" element={<Navigate to="/channels" replace />} />
          <Route path="/channels" element={<ProtectedLayout><Channels /></ProtectedLayout>} />
          <Route path="/keys" element={<ProtectedLayout><Keys /></ProtectedLayout>} />
          <Route path="/mappings" element={<ProtectedLayout><Mappings /></ProtectedLayout>} />
          <Route path="/pricing" element={<ProtectedLayout><Pricing /></ProtectedLayout>} />
          <Route path="/users" element={<ProtectedLayout><Users /></ProtectedLayout>} />
          <Route path="/groups" element={<ProtectedLayout><Groups /></ProtectedLayout>} />
          <Route path="/wallet" element={<ProtectedLayout><Wallet /></ProtectedLayout>} />
          <Route path="/orders" element={<ProtectedLayout><Orders /></ProtectedLayout>} />
          <Route path="/epay" element={<ProtectedLayout><Epay /></ProtectedLayout>} />
          <Route path="/logs" element={<ProtectedLayout><Logs /></ProtectedLayout>} />
          <Route path="/redemptions" element={<ProtectedLayout><Redemptions /></ProtectedLayout>} />
          <Route path="/notify" element={<ProtectedLayout><Notify /></ProtectedLayout>} />
          <Route path="/playground" element={<ProtectedLayout><Playground /></ProtectedLayout>} />
          <Route path="/security" element={<ProtectedLayout><Security /></ProtectedLayout>} />
          <Route path="/ip-management" element={<ProtectedLayout><IpManagement /></ProtectedLayout>} />
          <Route path="/network-layer" element={<ProtectedLayout><NetworkLayer /></ProtectedLayout>} />
          <Route path="/settings" element={<ProtectedLayout><Settings /></ProtectedLayout>} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </ToastProvider>
    </ErrorBoundary>
  );
}