// 侧边栏布局端点（独立文件，由 api/index.ts barrel 合并进统一 `api` 对象）。
//
// 契约（后端由主控接线）：GET/PUT `/api/sidebar-layout`，请求/响应都是
// `{ layout: <布局对象> | null }`。`layout: null` = 库里没存过 / 恢复默认，
// 前端回落 defaultLayout()。后端只需校验「顶层是对象或 null」，不做键白名单
// （合法形状由前端 lib/sidebar-layout.ts 拥有，且随版本增删菜单漂移）。
import { API_BASE, request } from '../client';
import type { SidebarLayout } from '@/lib/sidebar-layout';

/** 端点返回形态。兼容裸 `{layout}` 与 `{data:{layout}}`（见 lib 的 extractLayout）。 */
export interface SidebarLayoutEnvelope {
  layout?: SidebarLayout | null;
  data?: { layout?: SidebarLayout | null };
}

export const endpoints = {
  /** 读服务端权威布局（null = 没存过，前端用默认布局） */
  getSidebarLayout: (): Promise<SidebarLayoutEnvelope> =>
    request<SidebarLayoutEnvelope>('GET', `${API_BASE}/sidebar-layout`),
  /** 整份写回；传 null = 恢复默认（后端落 `{}`，读出仍按 null 处理） */
  saveSidebarLayout: (layout: SidebarLayout | null): Promise<SidebarLayoutEnvelope> =>
    request<SidebarLayoutEnvelope>('PUT', `${API_BASE}/sidebar-layout`, { layout }),
};
