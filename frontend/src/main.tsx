import React from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import App from './App';
import './App.css';
import './styles/density.css';
import { i18nReady } from './i18n';
import { initTheme } from './lib/theme';
import { initZoom } from './lib/zoom';

// 初始化主题（system/light/dark 三态，system 跟随系统明暗）
initTheme();

// 初始化界面缩放（设置 → 站点 → 界面缩放，持久化在 localStorage）
initZoom();

// TanStack Query 全局客户端（保守默认值，轮询策略下沉到具体查询）：
// - refetchInterval: false —— 默认**不**轮询；需要实时性的查询（如 Dashboard
//   realtime 面板）自行开 refetchInterval，避免「迁一个页面就多一份后台轮询」。
// - staleTime 30s —— 30s 内视为新鲜，切页/重挂载不重复打后端。
// - retry 1 次避免后端抖动放大。
// - refetchOnWindowFocus: false —— 焦点变化不触发全量重取。
// 注意：目前仅 Dashboard（hooks/useDashboard.ts）使用 TanStack Query，
// 其余页面均为手写 useState+useEffect，故本默认值只影响 Dashboard 的内部行为。
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchInterval: false,
      retry: 1,
      staleTime: 30_000,
      refetchOnWindowFocus: false,
    },
  },
});

const rootElement = document.getElementById('root');

if (!rootElement) {
  throw new Error('Root element #root not found');
}

// i18n 就绪后再挂载：非默认语言（en）的资源包是动态 import 的，必须等它到位，
// 否则英文界面首帧会回退显示中文。资源加载失败也继续挂载（回退 zh），不让白屏。
async function bootstrap(): Promise<void> {
  try {
    await i18nReady;
  } catch {
    /* i18n 初始化/资源加载失败：继续以 fallback 语言挂载 */
  }
  ReactDOM.createRoot(rootElement as HTMLElement).render(
    <React.StrictMode>
      <QueryClientProvider client={queryClient}>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </QueryClientProvider>
    </React.StrictMode>
  );
}

void bootstrap();
