// 页面自动刷新控件（beta.7 对齐上游 `page-auto-refresh-controls.tsx`）。
//
// 三条前端 runner 任务（logsAutoRefresh / requestsAutoRefresh / reportAutoRefresh）
// 仍存在后端 `scheduled-tasks` 里，但设置入口从「系统设置 → 定时任务」挪回**各页面
// 自身工具栏**：用户看到自动刷新开关，就在它生效的那一页。读写走
// `GET /api/scheduled-tasks` + `PATCH /api/scheduled-tasks/{id}`，无需后端改动。
//
// 与 `hooks/useAutoRefresh.ts` 共用同一份配置：改完这里，页面下一次重读即生效
// （最多滞后一个周期，与既有行为一致）。

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { Switch } from './ui';
import type { ScheduledTaskItem } from '../types';

interface PageAutoRefreshControlProps {
  /** 前端任务 id：logsAutoRefresh / requestsAutoRefresh / reportAutoRefresh */
  taskId: string;
}

export default function PageAutoRefreshControl({ taskId }: PageAutoRefreshControlProps): JSX.Element | null {
  const { t } = useTranslation();
  const [task, setTask] = useState<ScheduledTaskItem | null>(null);
  const [draft, setDraft] = useState('');
  const [saving, setSaving] = useState(false);
  const mounted = useRef(true);
  const seq = useRef(0);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      seq.current += 1;
    };
  }, []);

  useEffect(() => {
    const ticket = ++seq.current;
    void (async () => {
      try {
        const res = await api.getScheduledTasks();
        if (!mounted.current || ticket !== seq.current) return;
        const found = res?.data?.tasks?.find((item) => item.id === taskId) ?? null;
        setTask(found);
        setDraft(found ? String(found.interval) : '');
      } catch {
        // 读不到配置就当作关闭：不打断页面本身的手动刷新。
      }
    })();
    return () => {
      seq.current += 1;
    };
  }, [taskId]);

  if (!task) return null;

  const save = async (patch: { enabled?: boolean; interval?: number }): Promise<void> => {
    setSaving(true);
    try {
      const res = await api.patchScheduledTask(task.id, patch);
      if (mounted.current && res?.success && res.data) {
        setTask(res.data);
        setDraft(String(res.data.interval));
      }
    } catch {
      // 保存失败保留旧值，用户可再试。
    } finally {
      if (mounted.current) setSaving(false);
    }
  };

  /** 提交间隔草稿：越界回滚，未变化则跳过 */
  const commitInterval = (): void => {
    const parsed = Number.parseInt(draft, 10);
    if (!draft || Number.isNaN(parsed) || parsed < task.min || parsed > task.max) {
      setDraft(String(task.interval));
      return;
    }
    if (parsed === task.interval) return;
    void save({ interval: parsed });
  };

  return (
    <div style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }} title={t('页面自动刷新')}>
      <span style={{ color: 'var(--text-muted)', fontSize: 12 }}>{t('自动刷新')}</span>
      <Switch
        checked={task.enabled}
        onChange={(next) => void save({ enabled: next })}
        disabled={saving}
        ariaLabel={t('页面自动刷新')}
      />
      <input
        type="number"
        className="form-input"
        style={{ width: 64 }}
        min={task.min}
        max={task.max}
        value={draft}
        disabled={saving || !task.enabled}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commitInterval}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            commitInterval();
          }
        }}
        aria-label={t('自动刷新间隔（秒）')}
      />
      <span style={{ color: 'var(--text-muted)', fontSize: 12 }}>{t('秒')}</span>
    </div>
  );
}