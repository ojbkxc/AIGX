/**
 * 「每日签到」分区的账号明细（对齐 Agent2API `checkin-daily-panel.tsx`）。
 *
 * ── 为什么不再「每家一行、点开展开」 ─────────────────────────
 * 提供商分类挪到左栏之后，右栏直接列账号：左栏选「全部」时按家分组显示（每组
 * 一个小标题），选某一家时只列这一家的账号。少一层展开，一屏能多看几个账号。
 *
 * ── 一行只放这个账号自己的差异 ─────────────────────────────
 * 状态、上次签到、连登与操作都在行上；这一家的通用规则由分区标题下的说明条
 * 承担。`modes` 是插件申报的档位：有 `keepalive / claim / full` 的家出三颗
 * 按钮（领取才加积分、保活只维持活跃，分开是刻意的），没有的家出一颗「签到」。
 */

import { useTranslation } from 'react-i18next';
import { Badge, Button } from '../components/ui';
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

/** 一颗行内按钮的忙碌键：`sign:{id}`（签到底层接口）/ `{id}:{action}`（福利动作） */
function busyKeyOf(accountId: string, action: string): string {
  return `${accountId}:${action}`;
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
  // 分档执行由插件申报：keepalive / claim / full 各一颗按钮
  const modeButtons = modes.filter((mode) => MODE_BENEFIT_ACTION[mode] !== undefined);
  const manualPlans = group.manualPlans === true;
  const daily = account.dailyCheckin as { message?: unknown } | null | undefined;
  const message = typeof daily?.message === 'string' ? daily.message : '';

  return (
    <tr className={`ck-account-row is-${status}`}>
      <td className="ck-account-name">
        {account.name || account.id}
        {(account.streakDays ?? 0) > 0 && (
          <span className="ck-streak-inline">{t('连登 {{n}} 天', { n: account.streakDays })}</span>
        )}
      </td>
      <td>
        <Badge tone={ACCOUNT_STATUS_TONE[status]}>{t(ACCOUNT_STATUS_LABEL[status])}</Badge>
      </td>
      <td className="ck-time-subtle">
        {account.checkinAt ? t('上次 {{time}}', { time: formatShort(account.checkinAt) }) : t('从未签到')}
      </td>
      {/* 异常信息优先于空读数：它是「为什么还没签上」的答案 */}
      <td className="ck-message">
        {status !== 'done' && message ? message : account.lastMessage || '—'}
      </td>
      <td className="ck-account-ops">
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
                  disabled={busyKey !== null || account.available === false}
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
              disabled={busyKey !== null || account.available === false}
              title={t('走这一家的每日签到接口')}
              onClick={() => onSignOne(account.id)}
            >
              {busyKey === `sign:${account.id}` ? t('签到中…') : t('签到')}
            </Button>
          </>
        ) : (
          <Button
            variant="outline"
            size="sm"
            disabled={busyKey !== null || account.available === false}
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
      </td>
    </tr>
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
  if (rows.length === 0) return <p className="ck-rail-note">{t('没有符合条件的账号。')}</p>;

  return (
    <div className="ck-account-list">
      {rows.map((entry) => (
        <div key={entry.group.id} className="ck-account-group-block">
          {showProvider && (
            <div className="ck-account-group">
              <span>{t(entry.group.label || entry.group.id)}</span>
              <span className="ck-time-subtle">
                {t('{{done}} / {{total}} 个账号已签', {
                  done: entry.group.doneCount,
                  total: entry.group.totalCount,
                })}
              </span>
              {entry.group.description ? (
                <span className="ck-prov-desc">{t(entry.group.description)}</span>
              ) : null}
            </div>
          )}
          <table className="ck-table">
            <thead>
              <tr>
                <th>{t('账号')}</th>
                <th style={{ width: 96 }}>{t('状态')}</th>
                <th style={{ width: 160 }}>{t('上次签到')}</th>
                <th>{t('最近结果')}</th>
                <th style={{ width: 260 }}>{t('操作')}</th>
              </tr>
            </thead>
            <tbody>
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
            </tbody>
          </table>
        </div>
      ))}
    </div>
  );
}
