import React from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import App from './App';
import './App.css';
import './i18n';
import { initTheme } from './lib/theme';

// 初始化主题（system/light/dark 三态，system 跟随系统明暗）
initTheme();

// TanStack Query 全局客户端：
// - 30s 轮询保持原有实时性
// - retry 1 次避免后端抖动放大
// - staleTime 0 保持数据新鲜
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchInterval: 30000,
      retry: 1,
      staleTime: 0,
      refetchOnWindowFocus: false,
    },
  },
});

const rootElement = document.getElementById('root');

if (!rootElement) {
  throw new Error('Root element #root not found');
}

ReactDOM.createRoot(rootElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </QueryClientProvider>
  </React.StrictMode>
);
