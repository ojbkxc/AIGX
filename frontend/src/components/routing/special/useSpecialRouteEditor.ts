/**
 * useSpecialRouteEditor —— 「整块配置 + 修订号」编辑器的通用状态容器。
 *
 * 压缩 / 标题 / 视觉兼容三块配置的交互形状一致：读取 → 改草稿 → 保存（带
 * revision）→ 冲突处理。收在一个 hook 里，三个面板的「保存后刷新 / 冲突提示 /
 * 放弃修改」语义是同一份代码（散写会得到三套略微不同的行为）。
 *
 * 三条硬约束：
 * 1. **修订号冲突（409）不许悄悄覆盖**：冲突时保留草稿、保留旧 revision，
 *    把错误原文抬到界面上（面板渲染 `conflict` 提示条），由用户选择
 *    「重新读取」（丢掉草稿拿新 revision）或稍后再存。
 * 2. **草稿与视图分离**：`dirty = JSON(草稿) !== JSON(视图配置)`，rail 切换
 *    只是条件渲染（不卸载），未保存草稿在切走再切回后仍在。
 * 3. **端点未就绪（404/405）明确降级**：`notReady = true`，面板显示
 *    「端点尚未就绪」，不谎报成功、不吞错误。
 *
 * `active` 只在真正显示该面板时为 true：三块同时挂载但只拉当前一块的数据，
 * 切回来时才请求。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ApiResponse } from '../../../types';

export interface SpecialEditorState<T, V extends { config?: Partial<T> | null; revision?: string }> {
  /** 服务端最新视图（revision 的来源；保存成功前不随草稿变化） */
  view: V | null;
  draft: T | null;
  loading: boolean;
  saving: boolean;
  /** 一般错误（读取失败 / 保存失败的后端文案） */
  error: string;
  /** 409 冲突标志（面板据此渲染「修订号冲突」提示条与「重新读取」按钮） */
  conflict: boolean;
  /** 端点未就绪（404/405）：面板按降级空态渲染 */
  notReady: boolean;
  dirty: boolean;
  update(mutate: (draft: T) => T): void;
  save(): Promise<boolean>;
  reload(): Promise<void>;
  discard(): void;
}

interface EditorApi<V> {
  load(): Promise<ApiResponse<V>>;
  save(payload: { config: unknown; revision: string }): Promise<ApiResponse<V>>;
}

function statusOf(err: unknown): number | undefined {
  return (err as { status?: number } | null)?.status;
}

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export function useSpecialRouteEditor<T, V extends { config?: Partial<T> | null; revision?: string }>(
  api: EditorApi<V>,
  normalize: (raw: Partial<T> | null | undefined) => T,
  active: boolean,
): SpecialEditorState<T, V> {
  const [view, setView] = useState<V | null>(null);
  const [draft, setDraft] = useState<T | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [notReady, setNotReady] = useState(false);
  const mounted = useRef(true);
  const sequence = useRef(0);
  /** 最近一次服务端视图：保存要用它的 revision */
  const latest = useRef<V | null>(null);
  /** api 适配器按 render 更新进 ref：调用方通常内联传 `{ load, save }`，
   *  直接进依赖会让 reload 每次渲染换身份 → 首次加载期间 effect 反复重拉。 */
  const apiRef = useRef(api);
  apiRef.current = api;
  /** normalize 同理：调用方即使内联传函数，也不该改变回调身份触发重拉 */
  const normalizeRef = useRef(normalize);
  normalizeRef.current = normalize;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      sequence.current += 1;
    };
  }, []);

  const adopt = useCallback((next: V): void => {
    latest.current = next;
    setView(next);
    setDraft(normalizeRef.current(next.config));
  }, []);

  const reload = useCallback(async (): Promise<void> => {
    const token = ++sequence.current;
    setLoading(true);
    setError('');
    setConflict(false);
    try {
      const res = await apiRef.current.load();
      if (!mounted.current || token !== sequence.current) return;
      const data = res?.data ?? null;
      if (data) {
        adopt(data);
        setNotReady(false);
      } else {
        setNotReady(true);
        setView(null);
        setDraft(null);
      }
    } catch (err) {
      if (!mounted.current || token !== sequence.current) return;
      const st = statusOf(err);
      if (st === 404 || st === 405) {
        setNotReady(true);
        setView(null);
        setDraft(null);
      } else {
        setError(messageOf(err));
      }
    } finally {
      if (mounted.current && token === sequence.current) setLoading(false);
    }
  }, [adopt]);

  useEffect(() => {
    if (!active) return;
    // 首次显示时才拉；已拉过（含降级态）就不再自动重试 —— 手动「重新读取」兜底
    if (latest.current || notReady) return;
    void reload();
  }, [active, notReady, reload]);

  const update = useCallback((mutate: (current: T) => T): void => {
    setError('');
    setDraft((current) => (current === null ? current : mutate(current)));
  }, []);

  const save = useCallback(async (): Promise<boolean> => {
    const current = latest.current;
    const next = draft;
    if (!current || next === null || saving) return false;
    setSaving(true);
    setError('');
    setConflict(false);
    try {
      const saved = await apiRef.current.save({ config: next, revision: current.revision ?? '' });
      if (mounted.current) {
        const data = saved?.data ?? null;
        if (data) {
          adopt(data);
        } else if (current.revision !== undefined) {
          // 后端 2xx 但没回 data：视作已保存，只更新 revision 占位
          latest.current = { ...current } as V;
          setView({ ...current } as V);
        }
      }
      return true;
    } catch (err) {
      if (mounted.current) {
        const st = statusOf(err);
        setConflict(st === 409);
        setError(messageOf(err));
        // 冲突 / 失败都不能丢弃草稿：把当前编辑原样留在界面上
        setDraft((d) => (d === null ? d : { ...d }));
      }
      return false;
    } finally {
      if (mounted.current) setSaving(false);
    }
  }, [adopt, draft, saving]);

  const discard = useCallback((): void => {
    if (latest.current) setDraft(normalizeRef.current(latest.current.config));
    setError('');
    setConflict(false);
  }, []);

  const dirty =
    view !== null && draft !== null && JSON.stringify(draft) !== JSON.stringify(normalizeRef.current(view.config));

  return { view, draft, loading, saving, error, conflict, notReady, dirty, update, save, reload, discard };
}