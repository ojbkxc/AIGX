/**
 * 「定时补签」视图（对齐 Agent2API `checkin-makeup-tab.tsx` + `checkin-makeup-editor.tsx`）。
 *
 * ── 两块，两件事 ───────────────────────────────────────────
 * 1. **补签**：主签到区间结束后等 `retryGapMinutes` 分钟，再挑当天还没签上的账号
 *    跑一轮（`retryTime` 是「不早于」的下界，两个取较晚者 = `providerRetryTimes`）。
 * 2. **活动窗口重试**：只处理「窗口未开 / 活动暂未下发」的账号，关掉它不影响补签。
 *
 * 两者共用 `retry_gap_minutes`（后端同口径），但开关与次数分别设置 —— 界面上
 * 分成两组卡片，各自说清自己的口径，避免用户以为「关掉补签就顺带关了重试」。
 */

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Badge, Card, EmptyState, Switch } from '../components/ui';
import type { CheckinAutoState, CheckinScheduleEntry } from '../types';
import { checkinRangeText, groupProviderRowsByRange } from './checkin-shared';

/** 补签不早于的缺省时刻（与后端 `DEFAULT_RETRY_TIME` 同值） */
const DEFAULT_RETRY_TIME = '00:10';

/** 判断「重试是否开启」：间隔与次数都 > 0 才算开着（A2A `activityRetryEnabled`） */
export function activityRetryEnabled(entry: CheckinScheduleEntry): boolean {
  return (entry.retryGapMinutes ?? 0) > 0 && (entry.retryLimit ?? 0) > 0;
}

function MakeupPlan({
  entry,
  masterOn,
  defaultRetryTime,
  saving,
  onToggle,
  onSave,
}: {
  entry: CheckinScheduleEntry;
  masterOn: boolean;
  defaultRetryTime: string;
  saving: boolean;
  onToggle: (enabled: boolean) => void;
  onSave: (changes: { retryTime: string; retryGapMinutes: number }) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const enabled = !!entry.retryTime;
  const paused = !masterOn || !entry.enabled;
  const [editing, setEditing] = useState(false);
  const [time, setTime] = useState(entry.retryTime || defaultRetryTime);
  const [gap, setGap] = useState(String(Math.max(0, entry.retryGapMinutes ?? 0)));
  const gapValue = Number(gap);
  const gapValid = gap.trim() !== '' && Number.isInteger(gapValue) && gapValue >= 0 && gapValue <= 120;

  // 补签时刻 = 逐家 providerRetryTimes（后端算好）> 主签到区间结束 + 等待 > retryTime
  const groups = useMemo(() => {
    const rows = (entry.providerRows ?? []).map((row) => ({
      ...row,
      time:
        entry.providerRetryTimes?.[row.id] ??
        row.time,
    }));
    return groupProviderRowsByRange(rows);
  }, [entry.providerRows, entry.providerRetryTimes]);

  return (
    <Card
      title={
        <span className="ck-sched-name">
          {entry.displayLabel || entry.label || entry.time}
          <Badge tone={enabled ? 'success' : 'neutral'}>
            {enabled ? t('补签已开启') : t('补签已关闭')}
          </Badge>
          {paused && enabled ? <Badge tone="neutral">{t('随签到计划暂停')}</Badge> : null}
        </span>
      }
      actions={
        <div className="ck-actions">
          <Switch
            checked={enabled}
            ariaLabel={t('{{name}} 定时补签开关', { name: entry.displayLabel || entry.label })}
            onChange={onToggle}
          />
          <button
            type="button"
            className="btn btn-outline btn-sm"
            onClick={() => setEditing((open) => !open)}
            disabled={saving}
          >
            {t('设置补签')}
          </button>
        </div>
      }
    >
      <p className="ck-time-subtle">
        {t('对应签到计划 · 沿用 {{n}} 个平台范围，只补未签到账号', { n: entry.providers.length })}
      </p>

      <div className="ck-sched-desc">
        <span>
          {t('主签到区间结束后等待 {{n}} 分钟', { n: Math.max(1, entry.retryGapMinutes ?? 0) })}
        </span>
        {enabled ? <span>{t('补签不早于 {{time}}', { time: entry.retryTime })}</span> : null}
        <Badge tone="neutral">{t('已签到自动跳过')}</Badge>
      </div>

      {enabled ? (
        <div className="ck-sched-provider-rows">
          {groups.map((group) => (
            <div key={group.range} className="ck-sched-provider-row">
              <code className="ck-sched-provider-time">{group.range}</code>
              <Badge tone="neutral">{paused ? t('已暂停') : t('补签检查')}</Badge>
              <span className="ck-time-subtle">{t('{{n}} 个平台', { n: group.rows.length })}</span>
              <span className="ck-time-subtle">{t('每天补查一次')}</span>
              <span className="ck-time-subtle">{group.rows.map((row) => row.label).join('、')}</span>
              <span className="ck-time-subtle">
                {t('对应主签到：{{times}}', {
                  times: [
                    ...new Set(
                      group.rows.map((row) => {
                        const main = (entry.providerRows ?? []).find((item) => item.id === row.id);
                        return main ? checkinRangeText(main.time, main.timeEnd) : row.time;
                      }),
                    ),
                  ].join(' / '),
                })}
              </span>
            </div>
          ))}
        </div>
      ) : (
        <p className="form-hint">
          {t('补签已关闭，不影响主签到；开启后会自动生成各平台的补签安排。')}
        </p>
      )}

      {editing ? (
        <div className="ck-editor-grid">
          <label className="form-group">
            <span className="ck-editor-label">{t('补签不早于')}</span>
            <input
              type="time"
              className="form-input ck-time-input"
              value={time}
              disabled={saving}
              onChange={(e) => setTime(e.target.value)}
            />
          </label>
          <label className="form-group">
            <span className="ck-editor-label">{t('区间结束后等待（分钟）')}</span>
            <input
              type="number"
              min={0}
              max={120}
              className="form-input ck-gap-input"
              value={gap}
              disabled={saving}
              onChange={(e) => setGap(e.target.value)}
            />
          </label>
          <div className="ck-form-actions">
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={saving || !gapValid}
              onClick={() => {
                onSave({ retryTime: time, retryGapMinutes: gapValue });
                setEditing(false);
              }}
            >
              {saving ? t('保存中...') : t('保存设置')}
            </button>
          </div>
        </div>
      ) : null}

      <p className="form-hint">
        {t('实际时间由网关计算，晚窗口的平台会顺延补签；只处理当天未签到账号。')}
      </p>
    </Card>
  );
}

function ActivityRetryPlan({
  entry,
  masterOn,
  saving,
  onSave,
  onEditSchedule,
}: {
  entry: CheckinScheduleEntry;
  masterOn: boolean;
  saving: boolean;
  onSave: (changes: { retryLimit: number; retryGapMinutes: number }) => void;
  onEditSchedule: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const enabled = activityRetryEnabled(entry);
  const active = masterOn && entry.enabled && enabled;
  // 重试关着时这两个数字都不必提（`retryCount` 里可能还留着旧名单，但不会被消费）
  const count = enabled ? Number(entry.retryCount) || 0 : 0;
  // 次数用完的账号已不在 `retryCount` 里 —— 数字归零不等于「没事了」，必须分开说
  const exhausted = enabled ? (entry.retryExhausted?.length ?? 0) : 0;
  const [editing, setEditing] = useState(false);
  const [gap, setGap] = useState(String(Math.max(0, entry.retryGapMinutes ?? 0)));
  const [limit, setLimit] = useState(String(entry.retryLimit || 3));
  const gapValue = Number(gap);
  const limitValue = Number(limit);
  const valid =
    gap.trim() !== '' &&
    Number.isInteger(gapValue) &&
    gapValue >= 0 &&
    gapValue <= 120 &&
    limit.trim() !== '' &&
    Number.isInteger(limitValue) &&
    limitValue >= 1 &&
    limitValue <= 20;

  return (
    <Card
      title={
        <span className="ck-sched-name">
          {entry.displayLabel || entry.label || entry.time}
          <Badge tone={active ? 'info' : 'neutral'}>
            {!enabled ? t('重试关闭') : active ? t('重试开启') : t('重试已暂停')}
          </Badge>
        </span>
      }
      actions={
        <div className="ck-actions">
          <button type="button" className="btn btn-outline btn-sm" onClick={() => setEditing((open) => !open)}>
            {t('调整重试')}
          </button>
          <button type="button" className="btn btn-outline btn-sm" onClick={onEditSchedule}>
            {t('编辑签到计划')}
          </button>
        </div>
      }
    >
      <p className="ck-time-subtle">
        {enabled
          ? t('间隔 {{gap}} 分钟 · 每账号每天最多重试 {{limit}} 次', {
              gap: entry.retryGapMinutes,
              limit: entry.retryLimit,
            })
          : t('不自动重试等待中的活动')}
      </p>

      {!enabled ? (
        <p className="form-hint">
          {t('活动窗口重试已关闭，等待窗口或活动下发的账号不会自动再试。')}
        </p>
      ) : (
        <>
          {count > 0 ? (
            <p className="ck-sched-desc">
              <Badge tone="warning">{t('{{n}} 个账号等待窗口或活动', { n: count })}</Badge>
              {t('只重试这些账号，不会重签整个计划。具体原因见签到记录。')}
            </p>
          ) : null}
          {exhausted > 0 ? (
            <p className="ck-sched-desc">
              <Badge tone="neutral">{t('{{n}} 个账号今天的重试次数已用完', { n: exhausted })}</Badge>
              {t('今天不再自动重试，明天自动重置。要现在再试一次，用签到计划里的「立即执行」。')}
            </p>
          ) : null}
          {count === 0 && exhausted === 0 ? (
            <p className="form-hint">{t('当前没有等待窗口或活动下发的账号。')}</p>
          ) : null}
        </>
      )}

      {editing ? (
        <div className="ck-editor-grid">
          <label className="form-group">
            <span className="ck-editor-label">{t('重试间隔（分钟）')}</span>
            <input
              type="number"
              min={1}
              max={120}
              className="form-input ck-gap-input"
              value={gap}
              disabled={saving}
              onChange={(e) => setGap(e.target.value)}
            />
          </label>
          <label className="form-group">
            <span className="ck-editor-label">{t('每账号每天最多重试（次）')}</span>
            <input
              type="number"
              min={1}
              max={20}
              className="form-input ck-gap-input"
              value={limit}
              disabled={saving}
              onChange={(e) => setLimit(e.target.value)}
            />
          </label>
          <div className="ck-form-actions">
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={saving || !valid}
              onClick={() => {
                onSave({ retryLimit: limitValue, retryGapMinutes: gapValue });
                setEditing(false);
              }}
            >
              {saving ? t('保存中...') : t('保存设置')}
            </button>
          </div>
        </div>
      ) : null}

      <p className="form-hint">
        {t('当前网关中，补签等待间隔与活动重试间隔共用；修改间隔会同步影响两者，开关与重试次数仍分别设置。')}
      </p>
    </Card>
  );
}

export default function CheckinMakeupView({
  auto,
  saving,
  defaultRetryTime,
  onToggleMakeup,
  onSaveMakeup,
  onSaveRetry,
  onEditSchedule,
  onGoSchedule,
}: {
  auto: CheckinAutoState | undefined;
  saving: boolean;
  defaultRetryTime: string;
  onToggleMakeup: (entry: CheckinScheduleEntry, enabled: boolean) => void;
  onSaveMakeup: (entry: CheckinScheduleEntry, changes: { retryTime: string; retryGapMinutes: number }) => void;
  onSaveRetry: (entry: CheckinScheduleEntry, changes: { retryLimit: number; retryGapMinutes: number }) => void;
  onEditSchedule: (entry: CheckinScheduleEntry) => void;
  onGoSchedule: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const entries = auto?.schedules ?? [];
  const masterOn = auto?.enabled === true;

  return (
    <>
      <Card
        title={t('定时补签')}
        actions={
          <div className="ck-actions">
            <span className="ck-makeup-hint">
              {t('补签单独设置，只检查今天还没有签到的账号。')}
            </span>
            <button type="button" className="btn btn-outline btn-sm" onClick={onGoSchedule}>
              {t('管理定时签到')}
            </button>
          </div>
        }
      >
        <p className="form-hint">
          {t('定时签到和定时补签分开管理。补签沿用对应签到计划的平台范围，不重复领取已完成的账号。')}
        </p>
        {!masterOn ? (
          <p className="form-hint">{t('自动执行总开关已暂停，补签不会触发；手动执行不受影响。')}</p>
        ) : null}
      </Card>

      <h3 className="ck-section-title">{t('补签计划')}</h3>
      <p className="form-hint">{t('每个签到计划对应一组补签设置')}</p>
      {entries.length === 0 ? (
        <EmptyState icon="🌙" message={t('先创建签到计划，再为它设置定时补签。')} />
      ) : (
        entries.map((entry) => (
          <MakeupPlan
            key={entry.id}
            entry={entry}
            masterOn={masterOn}
            defaultRetryTime={defaultRetryTime || DEFAULT_RETRY_TIME}
            saving={saving}
            onToggle={(enabled) => onToggleMakeup(entry, enabled)}
            onSave={(changes) => onSaveMakeup(entry, changes)}
          />
        ))
      )}

      {entries.length > 0 ? (
        <>
          <h3 className="ck-section-title">{t('活动窗口重试')}</h3>
          <p className="form-hint">
            {t('与定时补签不同，仅处理窗口未开或活动暂未下发。')}
          </p>
          {entries.map((entry) => (
            <ActivityRetryPlan
              key={entry.id}
              entry={entry}
              masterOn={masterOn}
              saving={saving}
              onSave={(changes) => onSaveRetry(entry, changes)}
              onEditSchedule={() => onEditSchedule(entry)}
            />
          ))}
        </>
      ) : null}

      <p className="form-hint">
        {t('补签与重试都只处理当天未签上的账号；补签轮在指定时刻兜底一次。')}
      </p>
    </>
  );
}
