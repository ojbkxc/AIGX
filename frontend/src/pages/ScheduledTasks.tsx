import { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import { useToast } from '../components/Toast';
import {
  Button, Card, Input, Switch, Badge, Loading, EmptyState, SectionCard,
} from '../components/ui';
import type { ScheduledTaskItem } from '../types';

/** unix 毫秒 → 本地可读时间；0/null → 占位符 */
function fmtTime(ms: number | null): string {
  if (!ms) return '—';
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return '—';
  }
}

interface ScheduledTasksProps {
  /** 作为系统设置「定时任务」分类内嵌时隐藏自带 h1 */
  embedded?: boolean;
}

export default function ScheduledTasks({ embedded = false }: ScheduledTasksProps = {}): JSX.Element {
  const { t } = useTranslation();
  const admin = isAdmin();
  const addToast = useToast();

  const [tasks, setTasks] = useState<ScheduledTaskItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [savingId, setSavingId] = useState<string | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);
  /** 间隔输入草稿（按任务 id），与后端值分离，失焦/回车提交 */
  const [draftInterval, setDraftInterval] = useState<Record<string, string>>({});

  const loadSeq = useRef(0);
  const load = async (silent = false) => {
    if (!silent) {
      setLoading(true);
      setError('');
    }
    const seq = ++loadSeq.current;
    try {
      const res = await api.getScheduledTasks();
      if (seq !== loadSeq.current) return;
      if (res?.success && res?.data?.tasks) {
        setTasks(res.data.tasks);
        // 补齐草稿：后端值作为初始草稿（保留用户尚未提交的输入）
        setDraftInterval((prev) => {
          const next = { ...prev };
          for (const task of res.data!.tasks) {
            if (next[task.id] === undefined) next[task.id] = String(task.interval);
          }
          return next;
        });
      } else {
        setError(res?.error?.message || t('加载定时任务失败'));
      }
    } catch (err) {
      if (seq !== loadSeq.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (seq === loadSeq.current) setLoading(false);
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 运行状态轮询：页面可见时每 10s 静默刷新（与后端 TICK_MS 对齐，
  // 用于刷新 backend 任务的 lastRunAt/nextRunAt/running/lastResult）。
  useEffect(() => {
    const tick = () => {
      if (document.hidden) return;
      void load(true);
    };
    tick();
    const timer = window.setInterval(tick, 10_000);
    const onVisible = () => {
      if (!document.hidden) void load(true);
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener('visibilitychange', onVisible);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** 开关切换：即时保存 */
  const toggleEnabled = async (task: ScheduledTaskItem, next: boolean) => {
    setSavingId(task.id);
    try {
      const res = await api.patchScheduledTask(task.id, { enabled: next });
      if (res?.success && res?.data) {
        setTasks((prev) => prev.map((t) => (t.id === task.id ? res.data! : t)));
        addToast(`${task.label}${next ? t('已开启') : t('已关闭')}`);
      } else {
        addToast(res?.error?.message || t('保存失败'));
        void load(true); // 回滚到后端真实值
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : t('保存失败'));
      void load(true);
    } finally {
      setSavingId(null);
    }
  };

  /** 间隔保存：失焦/回车触发，校验范围 */
  const saveInterval = async (task: ScheduledTaskItem) => {
    const raw = draftInterval[task.id];
    const parsed = Number.parseInt(raw, 10);
    if (!raw || Number.isNaN(parsed) || parsed < task.min || parsed > task.max) {
      addToast(`${t('间隔')}${t('需在')} ${task.min}–${task.max}`);
      setDraftInterval((prev) => ({ ...prev, [task.id]: String(task.interval) }));
      return;
    }
    if (parsed === task.interval) return;
    setSavingId(task.id);
    try {
      const res = await api.patchScheduledTask(task.id, { interval: parsed });
      if (res?.success && res?.data) {
        setTasks((prev) => prev.map((t) => (t.id === task.id ? res.data! : t)));
        addToast(`${task.label}${t('间隔已保存')}`);
      } else {
        addToast(res?.error?.message || t('保存失败'));
        void load(true);
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : t('保存失败'));
      void load(true);
    } finally {
      setSavingId(null);
    }
  };

  /** 立即执行（仅后端任务） */
  const runNow = async (task: ScheduledTaskItem) => {
    setRunningId(task.id);
    try {
      const res = await api.runScheduledTask(task.id);
      if (res?.success && res?.data) {
        addToast(res.data.summary);
        if (res.data.task) {
          setTasks((prev) => prev.map((t) => (t.id === task.id ? res.data!.task : t)));
        } else {
          void load(true);
        }
      } else {
        addToast(res?.error?.message || t('执行失败'));
        void load(true);
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : t('执行失败'));
    } finally {
      setRunningId(null);
    }
  };

  if (loading) return <Loading text={t('加载中')} />;
  if (error) {
    return (
      <EmptyState
        message={error}
        action={<Button size="sm" onClick={() => void load()}>{t('重试')}</Button>}
      />
    );
  }
  if (!admin) return <EmptyState message={t('无权限')} />;

  const backendTasks = tasks.filter((t) => t.runner === 'backend');
  const frontendTasks = tasks.filter((t) => t.runner === 'frontend');

  const renderTask = (task: ScheduledTaskItem) => {
    const draft = draftInterval[task.id] ?? String(task.interval);
    const isRunning = task.runner === 'backend' && task.running;
    const unitLabel = task.unit === 'minutes' ? t('分钟') : t('秒');
    return (
      <Card
        key={task.id}
        title={task.label}
        actions={
          <Badge tone={task.runner === 'backend' ? 'info' : 'neutral'}>
            {task.runner === 'backend' ? t('后端') : t('前端')}
          </Badge>
        }
      >
        <p style={{ margin: '0 0 12px', color: 'var(--text-muted)' }}>{task.description}</p>

        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
          <Switch
            checked={task.enabled}
            onChange={(next) => void toggleEnabled(task, next)}
            disabled={savingId === task.id}
            ariaLabel={task.label}
          />
          <span style={{ color: 'var(--text-muted)' }}>
            {task.enabled ? t('已启用') : t('已禁用')}
          </span>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 12 }}>
          <span>{t('间隔')}：</span>
          <Input
            type="number"
            min={task.min}
            max={task.max}
            value={draft}
            onChange={(e) => setDraftInterval((prev) => ({ ...prev, [task.id]: e.target.value }))}
            onBlur={() => void saveInterval(task)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                void saveInterval(task);
              }
            }}
            style={{ width: 96 }}
            disabled={savingId === task.id}
          />
          <span style={{ color: 'var(--text-muted)' }}>
            {unitLabel} · {t('默认')} {task.defaultInterval} · {task.min}–{task.max}
          </span>
        </div>

        {task.runner === 'backend' && (
          <>
            <div style={{ display: 'flex', alignItems: 'center', gap: 12, marginTop: 12, flexWrap: 'wrap' }}>
              {isRunning ? (
                <Badge tone="warning">{t('执行中')}</Badge>
              ) : (
                <Badge tone="neutral">{t('空闲')}</Badge>
              )}
              <span style={{ color: 'var(--text-muted)' }}>
                {t('上次尝试')} {fmtTime(task.lastAttemptAt)}
              </span>
              <span style={{ color: 'var(--text-muted)' }}>
                {t('上次成功')} {fmtTime(task.lastSuccessAt)}
              </span>
              <span style={{ color: 'var(--text-muted)' }}>
                {t('下次执行')} {fmtTime(task.nextRunAt)}
              </span>
            </div>
            {task.retryAt && task.retryAt > Date.now() && (
              <div style={{ marginTop: 6, color: 'var(--warning-text, #d97706)' }}>
                {t('失败冷却中，将于 {{when}} 重试', { when: fmtTime(task.retryAt) })}
              </div>
            )}
            {task.lastResult && (
              <div style={{ marginTop: 6, color: 'var(--text-muted)' }}>
                {t('最近结果')}：{task.lastResult}
              </div>
            )}
            {task.lastError && (
              <div style={{ marginTop: 6, color: 'var(--danger-color)' }}>
                {t('最近错误')}：{task.lastError}
              </div>
            )}
            <div style={{ marginTop: 12, display: 'flex', alignItems: 'center', gap: 12 }}>
              <Button
                size="sm"
                variant="outline"
                onClick={() => void runNow(task)}
                disabled={runningId === task.id || isRunning || !task.canRun}
                title={task.canRun ? undefined : t('该任务当前不可执行')}
              >
                {runningId === task.id ? t('执行中…') : t('立即执行')}
              </Button>
              {!task.canRun && (
                <span style={{ color: 'var(--text-muted)' }}>{t('当前不可执行')}</span>
              )}
            </div>
          </>
        )}

        {task.runner === 'frontend' && (
          <div style={{ marginTop: 6, color: 'var(--text-muted)' }}>
            {t('仅页面可见时生效，不可见不请求')}
          </div>
        )}
      </Card>
    );
  };

  return (
    <div>
      {!embedded && <h1 style={{ marginBottom: 16 }}>{t('定时任务')}</h1>}
      {backendTasks.length > 0 && (
        <SectionCard title={t('后端任务')}>
          {backendTasks.map(renderTask)}
        </SectionCard>
      )}
      {frontendTasks.length > 0 && (
        <SectionCard title={t('前端自动刷新')}>
          {frontendTasks.map(renderTask)}
        </SectionCard>
      )}
    </div>
  );
}
