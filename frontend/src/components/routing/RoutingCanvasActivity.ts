/**
 * RoutingCanvasActivity —— 画布「流动态」的数据源（对齐 A2A `routing-canvas-activity.ts` 的口径）。
 *
 * A2A 的口径：只有**有请求在途**的连线才 `.is-running`（实心主色 + 虚线流动）。
 * 它的数据来自 `getRoutingFlow({ scope: 'running' })` 的 request trace：
 * 顶层 `running` 计数 + 逐个 attempt 的 accountId/memberId。
 *
 * 本仓映射（**不猜**，能归因到哪一层就只画到哪一层）：
 *   · 数据源 = `GET /api/logs/inflight`（正在跑、还没落日志的请求；Logs 页已在用）
 *   · 它只有 model / channel_id，**没有 configSnapshot.groupId、没有账号标识**
 *     → 只按「模型」归因：
 *       - 模型级层：`inflight.model === 本层模型名`
 *       - 全局层：`inflight.model` 没有独立模型级档案（否则它走的是模型级）
 *       - 路由组：`组.model === inflight.model`，或「某个模型级档案的 default_group 指向该组
 *         且 inflight.model 正是那个模型」（= 后端真实的组命中链，不是猜测）
 *       - 成员槽位行：**不置 running** —— 没有账号/channel→成员的归因依据，
 *         宁可静态也不假装流动（任务口径 #2）
 *   · 端点不可用 / 404 / 500 / 未登录：静默降级为「全静态」，不抛错、不刷控制台
 *   · 轮询间隔 2500ms（A2A 同值），页面隐藏时不拉
 */
import { useEffect, useRef, useState } from 'react';
import { api } from '../../api';
import type { RoutingGroup, RoutingProfiles } from '../../types/routing-profile';

export type RoutingCanvasActivity = {
  /** 在途请求条数 */
  running: number;
  /** 在途请求的模型名（小写、去重） */
  models: string[];
};

const EMPTY: RoutingCanvasActivity = { running: 0, models: [] };

/** 轮询间隔（A2A routing-canvas-activity.ts:129 同值） */
const POLL_MS = 2500;

export function useRoutingCanvasActivity(active: boolean): RoutingCanvasActivity {
  const [activity, setActivity] = useState<RoutingCanvasActivity>(EMPTY);
  // 依赖面只有「是否在画布视图」：切走就停，切回来立刻拉一次
  const alive = useRef(active);
  alive.current = active;

  useEffect(() => {
    if (!active) {
      setActivity(EMPTY);
      return;
    }
    let cancelled = false;
    const load = async (): Promise<void> => {
      // 单测里 api mock 可能没有这个端点 → 不报错，保持静态
      if (typeof api.listInflightRequests !== 'function') return;
      if (document.hidden) return;
      try {
        const res = await api.listInflightRequests();
        if (cancelled || !alive.current) return;
        const rows = Array.isArray(res?.data) ? res.data : [];
        const models = new Set<string>();
        for (const row of rows) {
          const model = typeof row?.model === 'string' ? row.model.trim().toLowerCase() : '';
          if (model) models.add(model);
        }
        setActivity({ running: rows.length, models: [...models] });
      } catch {
        // 端点未就绪（404/405/500/网络）一律按「无在途」处理：
        // 画布宁可静态，也不能因为一次失败把连线画成流动
        if (!cancelled) setActivity(EMPTY);
      }
    };
    void load();
    const timer = window.setInterval(() => {
      void load();
    }, POLL_MS);
    const onVisible = (): void => {
      if (!document.hidden) void load();
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [active]);

  return active ? activity : EMPTY;
}

/** 模型级层：在途请求里有没有这个模型 */
export function isModelRunning(activity: RoutingCanvasActivity, model: string | null): boolean {
  const name = (model ?? '').trim().toLowerCase();
  if (!name) return false;
  return activity.models.includes(name);
}

/**
 * 全局层：只有在途模型**没有**自己的模型级档案时才走全局兜底
 * （有独立档案的模型走模型级，画到全局层就是假归因）。
 */
export function isGlobalRunning(
  activity: RoutingCanvasActivity,
  modelNames: string[],
): boolean {
  const named = new Set(modelNames.map((m) => m.trim().toLowerCase()));
  return activity.models.some((m) => !named.has(m));
}

/**
 * 路由组：命中链有两处真实依据 —— 组的限定模型命中，或某个模型级档案的
 * `default_group` 指向该组且该模型正在途。两处都不成立就不置 running。
 */
export function isGroupRunning(
  activity: RoutingCanvasActivity,
  group: RoutingGroup,
  profiles: RoutingProfiles | null,
): boolean {
  if (isModelRunning(activity, group.model)) return true;
  if (!profiles) return false;
  for (const [model, profile] of Object.entries(profiles.models)) {
    if (profile?.default_group !== group.id) continue;
    if (isModelRunning(activity, model)) return true;
  }
  return false;
}
