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
 * `GET /scheduled-tasks` 返回的是**全量**任务清单，但本 hook 每个实例只关心其中一条。
 * 原先每个页面实例每 30s 拉一次全量，只为读一个开关/间隔 —— 请求量随任务数增长，
 * 且与页面真正的刷新周期无关（间隔设成 5 分钟时，配置轮询反而更频繁）。
 *
 * 改为模块级缓存：TTL 内所有 taskId、所有页面共享同一次请求。配合「页面重新可见时
 * 强制重读」，用户切回标签页即拿到最新配置；同页内生效最多滞后一个 TTL。
 */
const CONFIG_TTL_MS = 60_000;

interface ConfigCache {
  at: number;
  byId: Map<string, AutoRefreshConfig>;
}

let cache: ConfigCache | null = null;
let inflight: Promise<Map<string, AutoRefreshConfig>> | null = null;

/** 把一次全量清单折叠成「taskId → 配置」映射（一次 O(n)，多页面复用）。 */
function mapTasks(tasks: ScheduledTaskItem[] | undefined): Map<string, AutoRefreshConfig> {
  const byId = new Map<string, AutoRefreshConfig>();
  for (const task of tasks ?? []) {
    if (task.enabled && task.interval > 0) {
      const unitMs = task.unit === 'minutes' ? 60_000 : 1000;
      byId.set(task.id, { enabled: true, intervalMs: task.interval * unitMs });
    } else {
      byId.set(task.id, FALLBACK);
    }
  }
  return byId;
}

/** 拉取并落缓存；并发调用共享同一个 in-flight 请求，避免挂载期重复打接口。 */
function fetchConfigs(): Promise<Map<string, AutoRefreshConfig>> {
  if (inflight) return inflight;
  inflight = (async () => {
    try {
      const res = await api.getScheduledTasks();
      const byId = mapTasks(res?.data?.tasks);
      cache = { at: Date.now(), byId };
      return byId;
    } finally {
      inflight = null;
    }
  })();
  return inflight;
}

/**
 * 读取某条前端自动刷新任务的配置。
 * 读取失败视为关闭 —— 界面不该因为一个读不到的配置就完全停止刷新（那看起来像坏了）。
 *
 * @param force 绕过 TTL 强制重读（页面重新可见时用）
 */
async function readAutoRefresh(taskId: string, force = false): Promise<AutoRefreshConfig> {
  const fresh = cache !== null && Date.now() - cache.at < CONFIG_TTL_MS;
  if (!force && fresh && cache) {
    return cache.byId.get(taskId) ?? FALLBACK;
  }
  try {
    const byId = await fetchConfigs();
    return byId.get(taskId) ?? FALLBACK;
  } catch {
    // 请求失败：有旧缓存就沿用，避免一次网络抖动把自动刷新静默关掉
    return cache ? cache.byId.get(taskId) ?? FALLBACK : FALLBACK;
  }
}

/**
 * 自动刷新钩子（对照 Agent2API `logs-panel` 的轮询定时器）。
 *
 * 按「定时任务」页配置的开关与间隔自动调用 `refresh`，页面不可见时**暂停定时器**
 * （不是空转）。配置读取走模块级缓存（TTL 内不重复请求），并在页面重新可见时强制
 * 重读一次 —— 用户在「定时任务」页改完开关或间隔，切回本页即生效。
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

  // 读取并应用配置：配置没变就不重排定时器（避免无谓的 effect 重跑）。
  // 页面隐藏时暂停配置轮询；重新可见时强制重读一次。
  useEffect(() => {
    let cancelled = false;
    const sync = async (force: boolean): Promise<void> => {
      const next = await readAutoRefresh(taskId, force);
      if (cancelled) return;
      setCfg((prev) =>
        prev.enabled === next.enabled && prev.intervalMs === next.intervalMs ? prev : next,
      );
    };
    void sync(false);

    // 周期重读配置（TTL 内命中缓存，实际网络请求 ≤ 1 次/TTL）
    let reconfigTimer: number | undefined;
    const startReconfig = (): void => {
      if (reconfigTimer !== undefined) return;
      reconfigTimer = window.setInterval(() => {
        if (document.hidden) return;
        void sync(false);
      }, 30_000);
    };
    const stopReconfig = (): void => {
      if (reconfigTimer === undefined) return;
      window.clearInterval(reconfigTimer);
      reconfigTimer = undefined;
    };
    // 页面重新可见时：恢复周期轮询并立即（绕过 TTL）重读一次，可能刚被改过
    const onVisible = (): void => {
      if (document.hidden) {
        stopReconfig();
        return;
      }
      startReconfig();
      void sync(true);
    };

    if (!document.hidden) startReconfig();
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      cancelled = true;
      stopReconfig();
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [taskId]);

  // 数据轮询定时器：配置一变就重排；卸载时清表。任务被关掉时不排定时器
  // （而不是排一个永不触发的），否则「关掉了但定时器还在跑」会让「间隔改了却像没生效」
  // 变得难排查。页面隐藏时**真正停止** interval（不是让它空转），可见时再起。
  useEffect(() => {
    if (!cfg.enabled || cfg.intervalMs <= 0) return;

    const tick = (): void => {
      if (document.hidden) return; // 双保险：可见性切换竞态下也不在后台打请求
      if (pollingRef.current) return; // 上一轮没回来就跳过这一拍
      pollingRef.current = true;
      void Promise.resolve(refreshRef.current())
        .catch(() => undefined)
        .finally(() => {
          pollingRef.current = false;
        });
    };

    let timer: number | undefined;
    const start = (): void => {
      if (timer !== undefined) return;
      timer = window.setInterval(tick, cfg.intervalMs);
    };
    const stop = (): void => {
      if (timer === undefined) return;
      window.clearInterval(timer);
      timer = undefined;
    };
    const onVisibility = (): void => {
      if (document.hidden) stop();
      else start();
    };

    if (!document.hidden) start();
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      stop();
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [cfg]);
}
