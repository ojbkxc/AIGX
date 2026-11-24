/**
 * 插件申报的签到侧自动任务（成长 / 奖励类）。
 *
 * ── 数据从哪来 ───────────────────────────────────────────────
 * `GET /api/checkin-center` 的 `pluginTasks`（后端
 * `plugin_host::wasm::tasks::checkin_snapshot()`）：只含**不属于**签到窗口、
 * 但分类归签到侧的任务（成长 / 奖励）。签到窗口本身由签到中心自己的条目驱动，
 * 不在这里重复列出 —— 两种口径混在一起会让同一家出现两条。
 *
 * ── 为什么是只读 ─────────────────────────────────────────────
 * A2A 这一块是 `PluginTaskSettings`（可改开关与时刻表）。AIGX 的写入口是
 * `PATCH /api/plugins/tasks`，相关任务设置的编辑面目前只在插件页接线；签到中心
 * 这里先如实展示「谁申报了什么任务、今天跑过没有」，避免出现一颗点了没有作用的
 * 开关（那比不显示更坏）。
 */

import { useTranslation } from 'react-i18next';
import { Badge } from '../components/ui';
import { formatShort } from './checkin-shared';

/** 任务分类（后端 `Category`，serde 为 snake_case） */
export type PluginTaskCategory = 'checkin' | 'growth' | 'rewards' | 'maintenance' | 'other';

interface PluginTaskDef {
  id?: string;
  label?: string;
  enabled?: boolean;
  times?: string[];
  interval_ms?: number | null;
}

interface PluginTaskRun {
  account?: string;
  running?: boolean;
  lastRunAt?: number | null;
  nextRunAt?: number | null;
  error?: string | null;
  result?: unknown;
}

export interface PluginTaskRow {
  provider: string;
  category?: string;
  task: PluginTaskDef;
  runs?: PluginTaskRun[];
}

const CATEGORY_LABEL: Record<string, string> = {
  checkin: '签到',
  growth: '成长',
  rewards: '奖励',
  maintenance: '维护',
  other: '其他',
};

/** 把快照里的 `pluginTasks` 收成带形状的数组（后端给的是未清点的 JSON 数组） */
export function asPluginTasks(value: unknown): PluginTaskRow[] {
  if (!Array.isArray(value)) return [];
  return value.filter((row): row is PluginTaskRow => {
    if (!row || typeof row !== 'object') return false;
    const candidate = row as { provider?: unknown; task?: unknown };
    return typeof candidate.provider === 'string' && !!candidate.task && typeof candidate.task === 'object';
  });
}

/** 任务的执行时刻读数：时刻表优先，间隔型退到「每 N 分钟」 */
function scheduleText(task: PluginTaskDef, t: (key: string, vars?: Record<string, unknown>) => string): string {
  const times = Array.isArray(task.times) ? task.times.filter((item) => typeof item === 'string' && item) : [];
  if (times.length > 0) return times.join(' · ');
  const interval = Number(task.interval_ms);
  if (Number.isFinite(interval) && interval > 0) {
    return t('每 {{n}} 分钟', { n: Math.round(interval / 60_000) });
  }
  return t('未设时刻');
}

export default function CheckinPluginTasks({
  center,
  provider = '',
  categories,
  title,
}: {
  center: { pluginTasks?: unknown };
  /** 选中的提供商（空 = 全部） */
  provider?: string;
  /** 只列这些分类（不传 = 全部签到侧分类） */
  categories?: string[];
  title?: string;
}): JSX.Element | null {
  const { t } = useTranslation();
  const rows = asPluginTasks(center.pluginTasks).filter(
    (row) =>
      (!provider || row.provider === provider) &&
      (!categories || (typeof row.category === 'string' && categories.includes(row.category))),
  );
  if (rows.length === 0) return null;

  return (
    <div className="ck-plugin-tasks">
      <h4>{title || t('插件自动任务')}</h4>
      <p className="form-hint">
        {t('由插件申报的任务能力；开关与时刻表在「插件」页设置，这里只显示申报与今日运行情况。')}
      </p>
      <table className="ck-table">
        <thead>
          <tr>
            <th>{t('提供商')}</th>
            <th>{t('任务')}</th>
            <th style={{ width: 80 }}>{t('分类')}</th>
            <th style={{ width: 180 }}>{t('执行时刻')}</th>
            <th style={{ width: 120 }}>{t('状态')}</th>
            <th>{t('今日运行')}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const runs = Array.isArray(row.runs) ? row.runs : [];
            const running = runs.some((run) => run.running === true);
            const errors = runs.filter((run) => typeof run.error === 'string' && run.error.length > 0);
            return (
              <tr key={`${row.provider}:${row.task.id ?? ''}`}>
                <td>{row.provider}</td>
                <td>{row.task.label || row.task.id || '—'}</td>
                <td className="ck-time-subtle">
                  {t(CATEGORY_LABEL[row.category ?? 'other'] ?? CATEGORY_LABEL.other)}
                </td>
                <td className="ck-time-subtle">{scheduleText(row.task, t)}</td>
                <td>
                  {running ? (
                    <Badge tone="info">{t('执行中')}</Badge>
                  ) : row.task.enabled ? (
                    <Badge tone="success">{t('已启用')}</Badge>
                  ) : (
                    <Badge tone="neutral">{t('已停用')}</Badge>
                  )}
                </td>
                <td className="ck-time-subtle">
                  {runs.length === 0
                    ? t('今天还没有运行记录')
                    : runs
                        .slice(0, 3)
                        .map(
                          (run) =>
                            `${run.account ?? '—'}: ${
                              typeof run.error === 'string' && run.error
                                ? run.error
                                : run.lastRunAt
                                  ? formatShort(run.lastRunAt)
                                  : t('未运行')
                            }`,
                        )
                        .join(' · ')}
                  {errors.length > runs.slice(0, 3).length && errors.length > 0 && runs.length > 3
                    ? ` · ${t('另有 {{n}} 个账号失败', { n: errors.length })}`
                    : ''}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
