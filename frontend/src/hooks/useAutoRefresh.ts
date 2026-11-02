import { useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { ScheduledTaskItem } from '../types';

interface AutoRefreshConfig {
  enabled: boolean;
  /** 毫秒 */
  intervalMs: number;
}

const FALLBACK: AutoRefreshConfig = { enabled: false, intervalMs: 0 };

/**
 * 读取某条前端自动刷新任务的配置（来自间隔型定时任务的存储）。
 * 读取失败视为关闭 —— 界面不该因为一个读不到的配置就完全停止刷新（那看起来像坏了）。
 */
async function readAutoRefresh(taskId: string): Promise<AutoRefreshConfig> {
  try {
    const res = await api.getScheduledTasks();
    const task: ScheduledTaskItem | undefined = res?.data?.tasks?.find(
      (item) => item.id === taskId,
    );
    if (task && task.enabled && task.interval > 0) {
      const unitMs = task.unit === 'minutes' ? 60_000 : 1000;
      return { enabled: true, intervalMs: task.interval * unitMs };
    }
  } catch {
    // 静默：读不到配置就不自动刷新，不打断手动刷新
  }
  return FALLBACK;
}

/**
 * 自动刷新钩子（对照 Agent2API `logs-panel` 的轮询定时器）。
 *
 * 按「定时任务」页配置的开关与间隔自动调用 `refresh`，页面不可见时跳过。
 * 配置在挂载时读一次，并在页面重新可见 / 周期重读时刷新 —— 用户在「定时任务」
 * 页改完开关或间隔，无需刷新本页即生效（最多滞后一个周期）。
 *
 * `polling` 守卫：上一轮还在途中就跳过这一拍，避免慢接口下定时器堆叠请求
 * （间隔可以调到很小，setInterval 不等上一次完成）。
 *
 * @param taskId 前端任务的 id（如 `requestsAutoRefresh` / `logsAutoRefresh` / `reportAutoRefresh`）
 * @param refresh 刷新回调（调用页面自身的 load）
 */
export function useAutoRefresh(
  taskId: string,
  refresh: () => void | Promise<void>,
): void {
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const [cfg, setCfg] = useState<AutoRefreshConfig>(FALLBACK);
  const pollingRef = useRef(false);

  // 读取并应用配置：配置没变就不重排定时器（避免无谓的 effect 重跑）
  useEffect(() => {
    let cancelled = false;
    const sync = async (): Promise<void> => {
      const next = await readAutoRefresh(taskId);
      if (cancelled) return;
      setCfg((prev) =>
        prev.enabled === next.enabled && prev.intervalMs === next.intervalMs ? prev : next,
      );
    };
    void sync();
    // 周期重读配置（让「定时任务」页的改动生效）
    const reconfig = window.setInterval(() => {
      if (document.hidden) return;
      void sync();
    }, 30_000);
    // 页面重新可见时立即重读一次（可能刚被改过）
    const onVisible = (): void => {
      if (document.hidden) return;
      void sync();
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      cancelled = true;
      document.removeEventListener('visibilitychange', onVisible);
      window.clearInterval(reconfig);
    };
  }, [taskId]);

  // 轮询定时器：配置一变就重排；卸载时清表。任务被关掉时不排定时器
  // （而不是排一个永不触发的），否则「关掉了但定时器还在跑」会让「间隔改了却像没生效」
  // 变得难排查。
  useEffect(() => {
    if (!cfg.enabled || cfg.intervalMs <= 0) return;
    const timer = window.setInterval(() => {
      // 只在页面可见时轮询，避免后台无谓请求
      if (document.hidden) return;
      if (pollingRef.current) return; // 上一轮没回来就跳过这一拍
      pollingRef.current = true;
      void Promise.resolve(refreshRef.current())
        .catch(() => undefined)
        .finally(() => {
          pollingRef.current = false;
        });
    }, cfg.intervalMs);
    return () => {
      window.clearInterval(timer);
    };
  }, [cfg]);
}
