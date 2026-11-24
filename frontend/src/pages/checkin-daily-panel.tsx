/**
 * 「每日签到」分区的账号明细（对齐 Agent2API v3.5.1 `checkin-daily-panel.tsx`）。
 *
 * ── 为什么是行列表而不是表格 ───────────────────────────────
 * A2A 的行只放「这个账号自己的差异」：名字 + 连登、一句读数、状态徽标、右侧
 * 动作。这一家的通用规则由分区标题下的说明承担，所以没有表头 —— 表格化的
 * 列名（今日 / 连登 / 上次签到 / 最近结果）会把同一句话在每行复读一遍。
 *
 * ── 分档执行由插件申报 ────────────────────────────────────
 * `modes` 里有 `keepalive / claim / full` 的家出对应的福利按钮（领取才加积分、
 * 保活只维持活跃，分开是刻意的），没有的家出一颗「签到」。
 */

import { useTranslation } from 'react-i18next';
import { Badge, Button } from '../components/ui';
import { providerIcon } from '../lib/providerIcons';
import type { CheckinAccountRow, CheckinProviderGroup } from '../types';
import {
  ACCOUNT_STATUS_LABEL,
  ACCOUNT_STATUS_TONE,
  MODE_BENEFIT_ACTION,
  MODE_LABEL,
  MODE_TITLE,
  accountStatus,
  formatShort,
  modesOf,
} from './checkin-shared';

export interface DailyEntry {
  group: CheckinProviderGroup;
  accounts: CheckinAccountRow[];
}

function busyKeyOf(accountId: string, action: string): string {
  return `${accountId}:${action}`;
}

/** 账号的小图标：有内置图标用图标，否则用首字母方块（与左栏同一观感） */
function AccountLogo({ id, label }: { id: string; label: string }): JSX.Element {
  const icon = providerIcon(id);
  if (icon) return <img className="ck-rail-logo" src={icon} alt="" />;
  return <span className="ck-rail-logo ck-rail-letter">{(label || id || '·').slice(0, 1).toUpperCase()}</span>;
}

function AccountLine({
  account,
  group,
  busyKey,
  onSignOne,
  onRunBenefit,
  onOpenBenefits,
}: {
  account: CheckinAccountRow;
  group: CheckinProviderGroup;
  busyKey: string | null;
  onSignOne: (accountId: string) => void;
  onRunBenefit: (accountId: string, action: string) => void;
  onOpenBenefits: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const status = accountStatus(account);
  const modes = modesOf(group);
  const modeButtons = modes.filter((mode) => MODE_BENEFIT_ACTION[mode] !== undefined);
  const manualPlans = group.manualPlans === true;

  /*
   * A2A 的 `dailyCheckin` 读数：`completedCount / totalCount`（套餐份数）与
   * `kind === 'rolling'` 的 `until`（下次检查）。AIGX 后端目前只写
   * `{date,status,claimAt,message}` —— 结构与判据照 A2A 落好，字段一补齐即生效。
   */
  const daily = account.dailyCheckin as
    | {
        message?: unknown;
        kind?: unknown;
        until?: unknown;
        completedCount?: unknown;
        totalCount?: unknown;
      }
    | null
    | undefined;
  const message = typeof daily?.message === 'string' ? daily.message : '';
  const planTotal = Number(daily?.totalCount);
  const planDone = Number(daily?.completedCount);
  const planProgress =
    Number.isFinite(planTotal) && planTotal > 0 && Number.isFinite(planDone)
      ? t('已领 {{done}} / {{total}} 份套餐', { done: planDone, total: planTotal })
      : '';
  const rollingUntil = daily?.kind === 'rolling' && Number(daily?.until) > 0 ? Number(daily.until) : null;
  const disabled = busyKey !== null || account.available === false;

  return (
    <div
      className={`flex items-center gap-3 border-b border-[var(--border-color)] px-3.5 py-2.5 text-[12.5px] last:border-b-0${status === 'done' ? ' opacity-90' : ''}`}
      data-account-id={account.id}
    >
      <AccountLogo id={group.id} label={account.name || account.id} />
      <div className="min-w-0 flex-1">
        <div className="truncate">
          {account.name || account.id}
          {(account.streakDays ?? 0) > 0 ? (
            <span className="text-[var(--text-muted)]">
              {' '}
              · {t('连登 {{n}} 天', { n: account.streakDays })}
            </span>
          ) : null}
        </div>
        <div className="truncate text-[11.5px] text-[var(--text-muted)]">
          {/* 异常信息优先于读数：它是「为什么还没签上」的答案 */}
          {status !== 'done' && message
            ? message
            : account.lastMessage || (account.checkinAt
                ? t('上次 {{time}}', { time: formatShort(account.checkinAt) })
                : t('从未签到'))}
          {planProgress ? ` · ${planProgress}` : ''}
          {rollingUntil ? ` · ${t('下次检查：{{time}}', { time: formatShort(rollingUntil) })}` : ''}
        </div>
      </div>
      {account.available === false ? <Badge tone="neutral">{t('凭据不可用')}</Badge> : null}
      <Badge tone={ACCOUNT_STATUS_TONE[status]}>{t(ACCOUNT_STATUS_LABEL[status])}</Badge>
      <span className="flex shrink-0 items-center gap-1">
        {modeButtons.length > 0 ? (
          <>
            {modeButtons.map((mode) => {
              const action = MODE_BENEFIT_ACTION[mode];
              const busy = busyKey === busyKeyOf(account.id, action);
              return (
                <Button
                  key={mode}
                  variant="outline"
                  size="sm"
                  disabled={disabled}
                  title={t(MODE_TITLE[mode] ?? '')}
                  onClick={() => onRunBenefit(account.id, action)}
                >
                  {busy ? `${t(MODE_LABEL[mode] ?? mode)}…` : t(MODE_LABEL[mode] ?? mode)}
                </Button>
              );
            })}
            <Button
              variant="outline"
              size="sm"
              disabled={disabled}
              title={t('走这一家的每日签到接口')}
              onClick={() => onSignOne(account.id)}
            >
              {busyKey === `sign:${account.id}` ? t('签到中…') : t('签到')}
            </Button>
            {/* A2A 的「详情」打开保活执行详情；AIGX 暂无该端点 —— 位置先留着 */}
            <Button
              variant="outline"
              size="sm"
              disabled
              title={t('保活执行详情需要网关的 checkin-activity 端点，AIGX 尚未接入')}
            >
              {t('详情')}
            </Button>
          </>
        ) : (
          <Button
            variant="outline"
            size="sm"
            disabled={disabled}
            onClick={() => {
              if (manualPlans) onOpenBenefits();
              else onSignOne(account.id);
            }}
          >
            {manualPlans
              ? t('套餐明细')
              : busyKey === `sign:${account.id}`
                ? t('签到中…')
                : account.checkedInToday
                  ? t('重签')
                  : t('签到')}
          </Button>
        )}
      </span>
    </div>
  );
}

export default function CheckinDailyPanel({
  entries,
  showProvider,
  busyKey,
  onSignOne,
  onRunBenefit,
  onOpenBenefits,
}: {
  entries: DailyEntry[];
  showProvider: boolean;
  busyKey: string | null;
  onSignOne: (accountId: string) => void;
  onRunBenefit: (accountId: string, action: string) => void;
  onOpenBenefits: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const rows = entries.filter((entry) => entry.accounts.length > 0);
  if (rows.length === 0) return <p className="form-hint">{t('没有符合条件的账号。')}</p>;

  return (
    <div>
      {rows.map((entry) => (
        <div key={entry.group.id} className="mb-2">
          {showProvider && (
            <div className="flex items-baseline gap-2 px-3.5 pb-1 pt-3">
              <AccountLogo id={entry.group.id} label={entry.group.label || entry.group.id} />
              <span className="text-[12px] font-semibold">{t(entry.group.label || entry.group.id)}</span>
              <span className="text-[11.5px] text-[var(--text-muted)]">
                {t('{{done}} / {{total}} 个账号已签', {
                  done: entry.group.doneCount,
                  total: entry.group.totalCount,
                })}
              </span>
              {entry.group.description ? (
                <span className="text-[11.5px] text-[var(--text-muted)]">{t(entry.group.description)}</span>
              ) : null}
            </div>
          )}
          {entry.accounts.map((account) => (
            <AccountLine
              key={account.id}
              account={account}
              group={entry.group}
              busyKey={busyKey}
              onSignOne={onSignOne}
              onRunBenefit={onRunBenefit}
              onOpenBenefits={onOpenBenefits}
            />
          ))}
        </div>
      ))}
    </div>
  );
}
