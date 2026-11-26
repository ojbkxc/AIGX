// 侧边栏布局 React 绑定：状态 + 跨实例/跨端同步 + 变更提交。
//
// 多个实例（桌面侧栏、移动抽屉、设置页编辑面板）共用同一份 localStorage 缓存
// 与服务端权威副本。任何一个实例改动 → 写缓存 + 推库 + 广播 LAYOUT_EVENT，
// 其余实例收到 storage/事件后重读缓存即可保持一致。
import { useCallback, useEffect, useState } from 'react';
import {
  LAYOUT_EVENT,
  readLayout,
  syncFromServer,
  writeLayout,
  type SidebarLayout,
} from '@/lib/sidebar-layout';

export interface SidebarLayoutBinding {
  layout: SidebarLayout;
  /** 提交一次变更；null（无变化）返回 false，否则写盘并返回 true。 */
  apply: (next: SidebarLayout | null) => boolean;
}

export function useSidebarLayout(): SidebarLayoutBinding {
  const [layout, setLayout] = useState<SidebarLayout>(() => readLayout());

  useEffect(() => {
    const sync = (): void => setLayout(readLayout());
    window.addEventListener('storage', sync);
    window.addEventListener(LAYOUT_EVENT, sync);
    // 窗口聚焦时与服务端对齐一次（跨设备同步；失败静默）
    const onFocus = (): void => {
      void syncFromServer();
    };
    window.addEventListener('focus', onFocus);
    void syncFromServer();
    return () => {
      window.removeEventListener('storage', sync);
      window.removeEventListener(LAYOUT_EVENT, sync);
      window.removeEventListener('focus', onFocus);
    };
  }, []);

  const apply = useCallback((next: SidebarLayout | null): boolean => {
    if (!next) return false;
    setLayout(writeLayout(next));
    window.dispatchEvent(new CustomEvent(LAYOUT_EVENT));
    return true;
  }, []);

  return { layout, apply };
}
