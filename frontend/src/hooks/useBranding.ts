/**
 * 面板品牌自定义 hook（对标 Agent2API 3.5.0）。
 *
 * - 登录页 / 侧栏在无会话状态下也能渲染品牌：用公开端点 `GET /api/public/branding`。
 * - 应用启动时调用一次 `applyBrandingToDocument` 即可：设置 `document.title`、
 *   替换 `<link rel="icon">` 的 href。favicon 始终有值（后端已回落到 logo）。
 * - 品牌数据在设置页保存后不自动刷新（需重新加载页面或重新调用 hook）。
 */
import { useEffect, useState } from 'react';
import { api } from '../api';
import type { BrandingData } from '../types';

/** 默认品牌（后端未响应/失败时回退，与后端默认值一致） */
const DEFAULT_BRANDING: BrandingData = {
  panel_name: 'AIGX',
  logo_url: '/logo.png',
  favicon_url: '/logo.png',
  title: 'AIGX — AI 中转网关',
};

/** 应用品牌到 document（title + favicon），幂等。 */
function applyBrandingToDocument(b: BrandingData): void {
  if (typeof document !== 'undefined') {
    document.title = b.title || `${b.panel_name} — AI 中转网关`;
    const faviconLink = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
    if (faviconLink) {
      faviconLink.href = b.favicon_url || b.logo_url || '/logo.png';
    } else if (typeof document !== 'undefined') {
      const link = document.createElement('link');
      link.rel = 'icon';
      link.href = b.favicon_url || b.logo_url || '/logo.png';
      document.head.appendChild(link);
    }
  }
}

/**
 * 获取面板品牌信息。
 *
 * - 首次调用会拉取公开端点；失败回退 DEFAULT_BRANDING。
 * - 返回的数据同时应用于 document（title/favicon）。
 * - `enabled` 控制是否在挂载时自动拉取（默认 true）。
 */
export function useBranding(enabled = true): BrandingData {
  const [branding, setBranding] = useState<BrandingData>(DEFAULT_BRANDING);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    api
      .getPublicBranding()
      .then((res) => {
        if (cancelled || !res.success || !res.data) return;
        const data = res.data;
        setBranding(data);
        applyBrandingToDocument(data);
      })
      .catch(() => {
        // 失败回退默认品牌（与后端默认值一致，不闪屏）
      });
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  return branding;
}

export default useBranding;
