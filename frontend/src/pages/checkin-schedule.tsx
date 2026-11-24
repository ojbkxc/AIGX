/**
 * 「定时签到」视图（对齐 Agent2API `checkin-schedule-tab.tsx`）。
 *
 * ── 一块条目的三层信息 ─────────────────────────────────────
 * 1. 计划头：名称 + 启用状态 + 「默认 HH:MM · N 个平台 · G 组签到时间」；
 * 2. 今日主签到：`已执行 n / total 个账号`（`providerRows` 的 attemptedCount /
 *    accountCount 相加；没有排程时退到「已执行 n / total 个平台」）；
 * 3. 时间分组：每个生效区间一行（`HH:MM–HH:MM`），带状态徽标、平台图标、
 *    下一账号时刻、组内平台明细 —— 这正是 A2A 把「几点签 / 跑过没有」摆在
 *    条目上的那套呈现。
 *
 * ── 设置卡为什么还在 ───────────────────────────────────────
 * 总开关 / 账号间隔 / 签到时区 / 全局重试间隔 / 台账保留天数是 AIGX 的独占
 * 配置面（A2A 把这些放在别处或没有），保留在「定时签到」页顶部，评审时单列。
 */

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Plus, Play, Trash2 } from 'lucide-react';
import { Badge, Card, EmptyState, Switch, Tooltip } from '../components/ui';
import { providerIcon } from '../lib/providerIcons';
import { nextRunLabel } from '../lib/checkinTz';
import type { CheckinAutoState, CheckinScheduleEntry, CheckinSettings } from '../types';
import { checkinRangeText, formatShort, groupProviderRowsByRange } from './checkin-shared';

/** 时区偏移候选（小时）。0 = 跟随系统本地时区（语义见后端 store.rs） */
const TZ_OFFSETS = [-12, -11, -10, -9, -8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

/** 同一时刻的多个平台：最多画 6 个图标，其余折成 `+N` */
function ProviderLogos({ ids, labels }: { ids: string[]; labels: Record<string, string> }): JSX.Element {
  const { t } = useTranslation();
  return (
    <span className="ck-provider-logos">
      {ids.slice(0, 6).map((id) => {
        const icon = providerIcon(id);
        return icon ? (
          <img key={id} className="ck-rail-logo" src={icon} alt="" title={t(labels[id] ?? id)} />
        ) : (
          <span key={id} className="ck-rail-logo ck-rail-letter">
            {(labels[id] ?? id ?? '·').slice(0, 1).toUpperCase()}
          </span>
        );
      })}
      {ids.length > 6 ? <span className="ck-time-subtle">+{ids.length - 6}</span> : null}
    </span>
  );
}

function SchedulePlanCard({
  entry,
  auto,
  labels,
  busyKey,
  onEdit,
  onRun,
  onDelete,
  onToggle,
  onGoMakeup,
}: {
  entry: CheckinScheduleEntry;
  auto: CheckinAutoState;
  labels: Record<string, string>;
  busyKey: string | null;
  onEdit: () => void;
  onRun: () => void;
  onDelete: () => void;
  onToggle: (enabled: boolean) => void;
  onGoMakeup: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const masterOn = auto.enabled === true;
  const paused = !masterOn || !entry.enabled;
  const rows = entry.providerRows ?? [];
  const groups = useMemo(() => groupProviderRowsByRange(entry.providerRows ?? []), [entry.providerRows]);
  const accounts = rows.reduce((sum, row) => sum + (row.accountCount ?? 0), 0);
  const attempted = rows.reduce((sum, row) => sum + (row.attemptedCount ?? 0), 0);
  const fired = rows.filter((row) => row.firedToday).length;
  const overdue = rows.some((row) => row.firedToday !== true && row.overdue === true);
  const nextAccountAt = rows
    .filter((row) => row.firedToday !== true)
    .map((row) => Number(row.nextAccountAt))
    .filter((at) => Number.isFinite(at) && at > 0)
    .sort((a, b) => a - b)[0];
  const last = entry.lastResult;

  return (
    <Card
      title={
        <span className="ck-sched-name">
          {entry.displayLabel || entry.label || entry.time}
          <Badge tone={entry.enabled ? 'info' : 'neutral'}>
            {entry.enabled ? t('每天执行') : t('已停用')}
          </Badge>
          {!masterOn && <Badge tone="neutral">{t('随总开关暂停')}</Badge>}
        </span>
      }
      actions={
        <div className="ck-actions">
          <Switch
            className="ck-sched-switch"
            checked={entry.enabled}
            ariaLabel={t('启用签到计划 {{name}}', { name: entry.displayLabel || entry.label })}
            onChange={onToggle}
          />
          <button
            type="button"
            className="btn btn-outline btn-sm"
            onClick={onRun}
            disabled={busyKey !== null || entry.enabled !== true}
            title={t('立即执行这一条（不等待平台时点；窗口未开的平台可能仍无法领取）')}
          >
            <Play size={12} />
            {busyKey === `sched:${entry.id}` ? t('执行中…') : t('立即执行')}
          </button>
          <button type="button" className="btn btn-outline btn-sm" onClick={onEdit}>
            {t('编辑')}
          </button>
          <button type="button" className="btn btn-danger btn-sm" onClick={onDelete} title={t('删除')}>
            <Trash2 size={12} />
          </button>
        </div>
      }
    >
      <p className="ck-time-subtle">
        {t('默认 {{range}} · {{n}} 个平台 · {{groups}} 组签到时间', {
          range: checkinRangeText(entry.time, entry.timeEnd),
          n: rows.length,
          groups: groups.length,
        })}
      </p>

      {/* 今日主签到：账号级进度（没有排程时退到平台级） */}
      <div className="ck-sched-desc">
        <strong>{t('今日主签到')}</strong>{' '}
        {accounts > 0
          ? t('已执行 {{n}} / {{total}} 个账号', { n: attempted, total: accounts })
          : t('已执行 {{n}} / {{total}} 个平台', { n: fired, total: rows.length })}
        {overdue && !paused ? <Badge tone="warning">{t('有平台到点待执行')}</Badge> : null}
      </div>

      {groups.length > 0 ? (
        <div className="ck-sched-provider-rows">
          {groups.map((group) => {
            const groupFired = group.rows.filter((row) => row.firedToday).length;
            const groupAccounts = group.rows.reduce((sum, row) => sum + (row.accountCount ?? 0), 0);
            const groupAttempted = group.rows.reduce((sum, row) => sum + (row.attemptedCount ?? 0), 0);
            const groupOverdue = group.rows.some((row) => row.firedToday !== true && row.overdue === true);
            const completed = groupFired === group.rows.length;
            const status =
              groupAccounts === 0
                ? t('没有可执行账号')
                : completed
                  ? t('主签到已执行')
                  : paused
                    ? t('已暂停')
                    : groupAttempted > 0
                      ? t('区间内陆续签到')
                      : groupOverdue
                        ? t('到点待执行')
                        : t('等待签到区间');
            const builtin = group.rows.every((row) => !row.builtin && !entry.providerTimes?.[row.id]);
            const allBuiltin = group.rows.every(
              (row) => row.builtin === row.time && !entry.providerTimes?.[row.id],
            );
            return (
              <div
                key={group.range}
                className={`ck-sched-provider-row${completed ? ' is-done' : ''}${groupOverdue ? ' is-overdue' : ''}`}
              >
                <code className="ck-sched-provider-time">{group.range}</code>
                <Badge tone={completed && groupAccounts > 0 ? 'success' : groupOverdue && !paused ? 'warning' : 'neutral'}>
                  {status}
                </Badge>
                <span className="ck-time-subtle">
                  {builtin ? t('跟随默认时间') : allBuiltin ? t('平台专属时间') : t('自定义时间')}
                </span>
                <span className="ck-time-subtle">{t('{{n}} 个平台', { n: group.rows.length })}</span>
                <ProviderLogos ids={group.rows.map((row) => row.id)} labels={labels} />
                {groupAccounts > 0 ? (
                  <span className="ck-time-subtle">
                    {t('已执行 {{n}} / {{total}} 个账号', { n: groupAttempted, total: groupAccounts })}
                  </span>
                ) : null}
              </div>
            );
          })}
        </div>
      ) : null}

      {nextAccountAt && !paused ? (
        <p className="form-hint">{t('下一账号：{{time}}', { time: formatShort(nextAccountAt) })}</p>
      ) : null}

      {/* 补签与重试的当前口径（逐条目三字段） */}
      <p className="ck-time-subtle">
        {entry.retryTime
          ? t('补签 {{time}}', { time: entry.retryTime })
          : t('不跑补签轮')}
        {' · '}
        {(entry.retryLimit ?? 0) > 0
          ? t('每账号每天重试 {{n}} 次 / 间隔 {{gap}} 分钟', {
              n: entry.retryLimit,
              gap: entry.retryGapMinutes ?? 0,
            })
          : t('不自动重试')}
        {(entry.retryCount ?? 0) > 0
          ? ` · ${t('等窗口重试 {{n}} 个', { n: entry.retryCount })}`
          : ''}
        {(entry.retryExhausted ?? []).length > 0
          ? ` · ${t('今天重试已用尽 {{n}} 个', { n: (entry.retryExhausted ?? []).length })}`
          : ''}
      </p>

      <p className="ck-time-subtle ck-sched-last">
        {entry.lastFiredDate
          ? t('上次执行：{{d}} · {{r}} · 成功 {{ok}}·保活 {{ka}}·失败 {{f}}', {
              d: entry.lastFiredDate,
              r: last?.reason || '—',
              ok: last?.succeeded ?? 0,
              ka: last?.active ?? 0,
              f: last?.failedCount ?? 0,
            })
          : t('从未执行')}
        {last?.failed && last.failed.length > 0 ? (
          <span className="ck-sched-failures">
            {' '}
            {t('失败明细：')}
            {last.failed.join('、')}
          </span>
        ) : null}
      </p>

      <p className="ck-time-subtle">
        {entry.enabled
          ? entry.nextRunAt
            ? t('下次 {{when}}', { when: new Date(entry.nextRunAt).toLocaleString() })
            : nextRunLabel(entry.time, auto.settings?.timezone_offset ?? 8)
          : '—'}
      </p>

      {/* A2A 条目脚注：这里只管理签到时间，补签/重试在独立页设置 */}
      <p className="form-hint">
        {t('这里只管理签到时间；补签与活动窗口重试在独立页设置。')}
        <button type="button" className="btn btn-outline btn-sm" onClick={onGoMakeup} style={{ marginLeft: 8 }}>
          {t('设置定时补签')}
        </button>
      </p>
    </Card>
  );
}

export default function CheckinScheduleView({
  auto,
  settings,
  labels,
  unsupported,
  busyKey,
  editingId,
  editor,
  gapMsEdit,
  gapSaving,
  retryMinEdit,
  keepDaysEdit,
  advSaving,
  onGapMsChange,
  onSaveGapMs,
  onRetryMinChange,
  onKeepDaysChange,
  onSaveAdvanced,
  onTimezoneChange,
  onToggleMaster,
  onToggleEntry,
  onRunEntry,
  onEditEntry,
  onDeleteEntry,
  onNewEntry,
  onGoMakeup,
  onRunTravel,
  onSaveTravelTimes,
  travelTimesValue,
  onTravelTimesChange,
}: {
  auto: CheckinAutoState | undefined;
  settings: CheckinSettings;
  labels: Record<string, string>;
  unsupported: string[];
  busyKey: string | null;
  editingId: string | null;
  editor: JSX.Element | null;
  gapMsEdit: string;
  gapSaving: boolean;
  retryMinEdit: string;
  keepDaysEdit: string;
  advSaving: boolean;
  onGapMsChange: (value: string) => void;
  onSaveGapMs: () => void;
  onRetryMinChange: (value: string) => void;
  onKeepDaysChange: (value: string) => void;
  onSaveAdvanced: () => void;
  onTimezoneChange: (offset: number) => void;
  onToggleMaster: (enabled: boolean) => void;
  onToggleEntry: (entry: CheckinScheduleEntry, enabled: boolean) => void;
  onRunEntry: (entry: CheckinScheduleEntry) => void;
  onEditEntry: (entry: CheckinScheduleEntry) => void;
  onDeleteEntry: (entry: CheckinScheduleEntry) => void;
  onNewEntry: () => void;
  /** 「设置定时补签」：A2A 的条目卡上把这条跳转摆得比编辑更显眼 */
  onGoMakeup: () => void;
  onRunTravel?: () => void;
  onSaveTravelTimes?: () => void;
  travelTimesValue?: string;
  onTravelTimesChange?: (value: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const entries = auto?.schedules ?? [];
  const enabledCount = entries.filter((entry) => entry.enabled).length;
  const nextRunAt = useMemo(() => {
    if (auto?.enabled !== true) return null;
    const stamps = (auto.schedules ?? [])
      .filter((entry) => entry.enabled)
      .map((entry) => Number(entry.nextRunAt))
      .filter((stamp) => Number.isFinite(stamp) && stamp > 0)
      .sort((a, b) => a - b);
    return stamps[0] ?? null;
  }, [auto?.schedules, auto?.enabled]);

  return (
    <>
      {/* 分区标题（A2A `CheckinScheduleHeading`） */}
      <h2 className="ck-section-title">{t('定时签到')}</h2>
      <p className="form-hint">
        {t('设置每天的签到区间，各账号在区间内随机错峰执行。')}
      </p>
      {/* 总开关（A2A `CheckinScheduleMaster`） */}
      <Card title={t('自动执行总开关')}>
        <div className="ck-setting-row">
          <div>
            <div className="ck-setting-label">
              {auto?.enabled ? t('已开启') : t('已暂停')}
              {auto?.running ? <Badge tone="info">{t('执行中…')}</Badge> : null}
            </div>
            <div className="ck-setting-hint">
              {t('同时控制定时签到、定时补签和活动窗口重试；猫猫旅行独立运行。')}
            </div>
          </div>
          <div className="ck-setting-value">
            <span className="ck-time-subtle">
              {auto?.enabled ? t('下次自动执行') : t('自动执行已暂停')}
              {' · '}
              {auto?.enabled && nextRunAt ? new Date(nextRunAt).toLocaleString() : '—'}
            </span>
            <span className="form-hint">
              {t('已启用 {{n}} 个签到计划', { n: enabledCount })}
            </span>
          </div>
          <Switch
            checked={auto?.enabled === true}
            onChange={onToggleMaster}
            ariaLabel={t('自动执行总开关')}
          />
        </div>
        {auto?.enabled !== true && (
          <p className="form-hint">
            {t('签到与补签计划已暂停，配置仍保留；手动执行不受总开关影响。')}
          </p>
        )}
      </Card>

      {/* AIGX 独有：签到设置（执行间隔 / 时区 / 全局重试 / 保留期） */}
      <Card title={t('签到设置')}>
        <div className="ck-setting-row">
          <div>
            <div className="ck-setting-label">{t('账号间隔（毫秒）')}</div>
            <div className="ck-setting-hint">{t('逐个签到之间的等待，避免并发触发上游风控。')}</div>
          </div>
          <div className="ck-gap-edit">
            <input
              type="number"
              min={0}
              step={100}
              className="form-input ck-gap-input"
              value={gapMsEdit}
              onChange={(e) => onGapMsChange(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') onSaveGapMs();
              }}
              aria-label={t('账号间隔（毫秒）')}
            />
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={onSaveGapMs}
              disabled={gapSaving}
            >
              {gapSaving ? t('保存中...') : t('保存')}
            </button>
          </div>
        </div>
        <div className="ck-setting-row">
          <div>
            <div className="ck-setting-label">{t('签到时区')}</div>
            <div className="ck-setting-hint">
              {t('计划与「下次执行」按此时区计算。缺省 UTC+8；选「跟随系统」= 用服务器本地时区（偏移 0）。')}
            </div>
          </div>
          <select
            className="form-input"
            style={{ maxWidth: 200 }}
            value={String(settings.timezone_offset ?? 8)}
            onChange={(e) => onTimezoneChange(Number(e.target.value))}
            aria-label={t('签到时区')}
          >
            {TZ_OFFSETS.map((off) => (
              <option key={off} value={off}>
                {off === 0 ? t('跟随系统本地时区') : `UTC${off > 0 ? '+' : ''}${off}`}
              </option>
            ))}
          </select>
        </div>
        <div className="ck-setting-row">
          <div>
            <div className="ck-setting-label">{t('失败/窗口重试间隔（分钟）')}</div>
            <div className="ck-setting-hint">
              {t('签到失败或窗口未开时，当天按此间隔重试；0 = 关闭重试（签到失败当天不再补）。')}
            </div>
          </div>
          <div className="ck-gap-edit">
            <input
              type="number"
              min={0}
              max={1440}
              className="form-input ck-gap-input"
              value={retryMinEdit}
              onChange={(e) => onRetryMinChange(e.target.value)}
              aria-label={t('失败/窗口重试间隔（分钟）')}
            />
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={onSaveAdvanced}
              disabled={advSaving}
            >
              {advSaving ? t('保存中...') : t('保存')}
            </button>
          </div>
        </div>
        <div className="ck-setting-row">
          <div>
            <div className="ck-setting-label">{t('台账保留天数')}</div>
            <div className="ck-setting-hint">
              {t('签到记录超过该天数自动清理；0 = 永久保留。')}
            </div>
          </div>
          <div className="ck-gap-edit">
            <input
              type="number"
              min={0}
              className="form-input ck-gap-input"
              value={keepDaysEdit}
              onChange={(e) => onKeepDaysChange(e.target.value)}
              aria-label={t('台账保留天数')}
            />
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={onSaveAdvanced}
              disabled={advSaving}
            >
              {advSaving ? t('保存中...') : t('保存')}
            </button>
          </div>
        </div>
        {unsupported.length > 0 && (
          <p className="form-hint">
            {t('以下上游的实现随上游进入插件仓、主仓未接入：')}
            {unsupported.join('、')}
          </p>
        )}
      </Card>

      {/* 签到计划（A2A `SchedulePlan` 列表） */}
      <Card
        title={t('签到计划')}
        actions={
          <button
            type="button"
            className="btn btn-outline btn-sm"
            onClick={onNewEntry}
            disabled={editingId !== null}
            style={{ gap: 6 }}
          >
            <Plus size={13} /> {t('新建签到计划')}
          </button>
        }
      >
        <p className="ck-time-subtle">
          {t('{{n}} 个计划 · 时间按配置时区计算', { n: entries.length })}
        </p>
        {editingId === 'new' && editor ? <div className="ck-sched-row is-editing">{editor}</div> : null}
        {entries.length === 0 && editingId !== 'new' ? (
          <EmptyState icon="⏰" message={t('还没有签到计划，先选择平台和默认时间。')} />
        ) : (
          <div className="ck-sched-list">
            {entries.map((entry) =>
              editingId === entry.id && editor ? (
                <div key={entry.id} className="ck-sched-row is-editing">
                  {editor}
                </div>
              ) : (
                <SchedulePlanCard
                  key={entry.id}
                  entry={entry}
                  auto={auto as CheckinAutoState}
                  labels={labels}
                  busyKey={busyKey}
                  onEdit={() => onEditEntry(entry)}
                  onRun={() => onRunEntry(entry)}
                  onDelete={() => onDeleteEntry(entry)}
                  onToggle={(enabled) => onToggleEntry(entry, enabled)}
                  onGoMakeup={onGoMakeup}
                />
              ),
            )}
          </div>
        )}
      </Card>

      {/* 执行规则（A2A `CheckinScheduleRules`） */}
      <Card title={t('执行规则')}>
        <p className="form-hint">
          {t('所有时间按配置时区计算。每个账号每天随机安排一次，重启保留当天安排；错过区间时会补跑未执行账号。')}
        </p>
        <p className="form-hint">
          {t('主签到已执行不等于每个账号都已领取成功；账号结果请查看签到记录。')}
        </p>
      </Card>

      {/* AIGX 独有：猫猫旅行定时排程 */}
      {onRunTravel && onTravelTimesChange ? (
        <Card
          title={t('猫猫旅行定时排程')}
          actions={
            <div className="ck-actions">
              <Tooltip label={t('每天按这里的时刻自动巡检一遍：没有猫就领养、在家就派出、到站就领奖。')}>
                <span className="ck-tip-dot" aria-label={t('说明')}>
                  ?
                </span>
              </Tooltip>
              <button type="button" className="btn btn-outline btn-sm" onClick={onRunTravel}>
                {t('立即巡检全部')}
              </button>
            </div>
          }
        >
          <div className="ck-setting-row">
            <div>
              <div className="ck-setting-label">{t('巡检时刻（每行一个 HH:MM）')}</div>
              <div className="ck-setting-hint">
                {t('逗号或换行分隔；到点后最多晚 30 秒执行，错过时点开机后会补跑一轮。')}
              </div>
            </div>
            <div className="ck-gap-edit ck-travel-edit">
              <textarea
                className="form-input ck-travel-times"
                rows={3}
                value={travelTimesValue ?? ''}
                placeholder={'09:00\n21:00'}
                onChange={(e) => onTravelTimesChange(e.target.value)}
                aria-label={t('巡检时刻（每行一个 HH:MM）')}
              />
              <button type="button" className="btn btn-outline btn-sm" onClick={onSaveTravelTimes}>
                {t('保存时刻')}
              </button>
            </div>
          </div>
        </Card>
      ) : null}
    </>
  );
}
