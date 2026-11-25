/**
 * 签到中心的四个**任务分区**（对齐 Agent2API `checkin-center-tab.tsx` +
 * `checkin-plans-section.tsx`）。
 *
 * ── 一个分区是什么 ─────────────────────────────────────────
 * 「每日签到 / 新手福利 / 活动福利 / 成长任务」各自是一个任务：左栏是同一个
 * 任务下的提供商分类，右栏是这个任务里逐账号的行。四个分区的结构一样，判据不同
 * （签到看今日已签、新手福利看是否结清、活动福利看今天领没领到、成长任务看
 * 任务树是否全部完成）。
 *
 * ── 数据来源 ──────────────────────────────────────────────
 * 分组判定与「今天领没领到」的落账都在后端快照里（`GET /api/checkin-center`），
 * 这里只画。**不要**按 provider id 写死任何判断 —— 那是插件/实现申报的
 * （`checkinFeatures` 的 `modes` / `uiHooks` / `benefitActions`）。
 *
 * ── 明细为什么现查 ─────────────────────────────────────────
 * 新手福利与成长任务的**明细**要打上游，快照刻意不带（快照必须「零上游请求」）。
 * 行上有「查询任务」按钮，点了才查；查询结果只存在本组件的临时状态里（切分区即丢，
 * 不缓存 —— 缓存会让「已结清」与上游真实状态脱节）。
 */

import { useCallback, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Play } from 'lucide-react';
import { api } from '../api';
import Badge from '../components/ui/Badge';
import Button from '../components/ui/Button';
import Card from '../components/ui/Card';
import EmptyState from '../components/ui/EmptyState';
import type { BenefitResult, CheckinCenterData } from '../types';
import CheckinProviderRail, { type ProviderOption } from './checkin-provider-rail';
import CheckinDailyPanel, { type DailyEntry } from './checkin-daily-panel';
import CheckinPluginTasks, { asPluginTasks } from './checkin-plugin-tasks';
import {
  accountStatus,
  byStatus,
  claimedPlansToday,
  filterCounts,
  formatShort,
  groupByProvider,
  providerLabel,
  type CheckinSection,
  type StatusFilter,
} from './checkin-shared';

/** 一行福利动作的结果：`${accountId}:${action}` → 最近一次执行 */
type BenefitResults = Record<string, BenefitResult>;

const FILTER_LABELS: Record<CheckinSection, readonly [string, string, string]> = {
  daily: ['全部', '已签到', '未签到'],
  onboarding: ['全部', '已领取', '未领取'],
  plans: ['全部', '已领取', '未领取'],
  growth: ['全部', '已完成', '未完成'],
};

const SECTION_TITLE: Record<CheckinSection, string> = {
  daily: '每日签到',
  onboarding: '新手福利',
  plans: '活动福利',
  growth: '成长任务',
};

const SECTION_FOOT: Record<CheckinSection, string> = {
  daily: '单账号签到不受范围限制',
  onboarding: '一次性福利 · 已领取状态保存在网关，领完无需重复查询',
  plans: '资格、额度与有效期以官方返回为准 · 领取不会购买或订阅套餐',
  growth: '查询任务树后逐条领取 · 含开学季与猫猫旅行',
};

/** 福利动作的结果 → Badge tone */
const BENEFIT_TONE: Record<string, 'success' | 'warning' | 'danger' | 'neutral' | 'info'> = {
  done: 'success',
  already: 'info',
  pending: 'warning',
  failed: 'danger',
  skipped: 'neutral',
};

/** 福利动作的结果 → 文案 */
const BENEFIT_LABEL: Record<string, string> = {
  done: '已领取',
  already: '已结清',
  pending: '待重试',
  failed: '失败',
  skipped: '不适用',
};

/** 任务树的一行（上游字段名随插件；取不到就给空值，不猜） */
interface UpstreamTask {
  code?: string;
  title?: string;
  name?: string;
  done?: boolean;
  locked?: boolean;
}

function upstreamTasks(result: BenefitResult | undefined): UpstreamTask[] {
  const data = result?.data as { tasks?: unknown } | undefined;
  if (!data || !Array.isArray(data.tasks)) return [];
  return data.tasks.filter((row): row is UpstreamTask => !!row && typeof row === 'object');
}

function taskKey(task: UpstreamTask, index: number): string {
  const code = typeof task.code === 'string' ? task.code.trim() : '';
  return code || `task-${index}`;
}

/**
 * 一行「福利」账号（新手福利 / 活动福利 / 成长任务共用）。
 *
 * 与每日签到行同一套节奏：名字 + 一句读数 + 状态徽标 + 右侧动作；明细收在行下
 * 的展开块里（查询后才出现）。
 */
function BenefitRow({
  accountId,
  name,
  providerName,
  description,
  badge,
  actions,
  detail,
  note,
}: {
  accountId: string;
  name: string;
  /** 展示名（调用方已按快照 / providerOptions 翻好；取不到就是 id） */
  providerName: string;
  description: string;
  badge?: JSX.Element | null;
  actions: JSX.Element;
  detail?: JSX.Element | null;
  note?: string;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="ck-extra-row" key={accountId}>
      <div className="ck-benefit-head">
        {/* 账号名是**用户数据**，不能当 i18n key：`t()` 对不存在的 key 原样返回，
            但一旦用户把账号命名成「保存」「取消」「确定」这类已存在的 key，
            英文界面就会把它显示成 Save / Cancel / OK —— 同行的 providerName
            本来就是裸显示，这里 `t()` 只会让两列口径不一致。 */}
        <span className="ck-extra-name">{name || accountId}</span>
        <span className="ck-extra-provider">{providerName}</span>
        {badge}
        <span className="ck-benefit-actions">{actions}</span>
      </div>
      <p className="ck-time-subtle">{t(description)}</p>
      {note ? <p className="ck-time-subtle">{note}</p> : null}
      {detail}
    </div>
  );
}

export default function CheckinCenterSections({
  section,
  center,
  busyKey,
  onSignOne,
  onRunAll,
  onOpenBenefits,
  onReload,
}: {
  section: CheckinSection;
  center: CheckinCenterData;
  /** 签到动作的忙碌键（`sign:{id}` / `all`） */
  busyKey: string | null;
  onSignOne: (accountId: string) => void;
  onRunAll: () => void;
  onOpenBenefits: () => void;
  /** 福利动作后的快照回读（结清状态 / 领取台账都在快照里） */
  onReload: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const { daily, extras } = center;
  const [filter, setFilter] = useState<StatusFilter>('all');
  const [providerId, setProviderId] = useState('');
  /**
   * 在跑的动作集合（**每个按钮一条 key**）。
   *
   * 旧实现是单个字符串：任一动作结束就在 `finally` 里 `setRunning('')`，把
   * 同时在跑的其它动作的加载态一并清掉 —— 两行同时点「查询任务」，先回来的
   * 那个会把另一行仍在转的按钮重新点亮（再点一次就是重复请求）。改成 Set
   * 后每个动作只清自己那条，`disabled` / 文案也只认自己那条。
   */
  const [running, setRunning] = useState<Set<string>>(() => new Set());
  const [results, setResults] = useState<BenefitResults>({});
  const [error, setError] = useState('');

  /** 某条 key 是否在跑（`disabled` / 文案判据共用一处，避免各写一遍漂移） */
  const isRunning = (key: string): boolean => running.has(key);

  const beginRunning = (key: string): void => {
    setRunning((prev) => {
      const next = new Set(prev);
      next.add(key);
      return next;
    });
  };
  const endRunning = (key: string): void => {
    setRunning((prev) => {
      if (!prev.has(key)) return prev;
      const next = new Set(prev);
      next.delete(key);
      return next;
    });
  };

  const runBenefit = useCallback(
    async (
      accountId: string,
      action: string,
      params?: Record<string, unknown>,
      /** 逐条动作的忙碌键（默认 `${accountId}:${action}`；批量/逐任务共用同一结果键时传各自的键） */
      busyKey?: string,
    ): Promise<BenefitResult | undefined> => {
      // 结果键与忙碌键分开：结果永远落在 `${accountId}:${action}`（各判据都按它读），
      // 忙碌键只用于「这颗按钮在不在跑」，逐任务领取因此可以各自独立。
      const key = `${accountId}:${action}`;
      const busy = busyKey ?? key;
      beginRunning(busy);
      setError('');
      try {
        const res = await api.runCheckinBenefit(accountId, action, params);
        const result = res?.data;
        if (result) {
          setResults((prev) => ({ ...prev, [key]: result }));
          onReload();
        }
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
        return undefined;
      } finally {
        // 只清自己那条：并发时别人的加载态不受影响
        endRunning(busy);
      }
    },
    [onReload],
  );

  /** 批量动作：整批共用一条 busy key，跑完才清（逐行结果仍各自入 `results`） */
  const runBenefitBulk = useCallback(
    async (action: string, rows: Array<{ id: string }>): Promise<void> => {
      const busy = `bulk:${action}`;
      beginRunning(busy);
      try {
        await Promise.all(rows.map((row) => runBenefit(row.id, action)));
      } finally {
        endRunning(busy);
      }
    },
    [runBenefit],
  );

  /* ── 各分区的「完成」判据 ───────────────────────────── */

  const plansDone = useCallback(
    (id: string): boolean => {
      const row = extras.plans.find((item) => item.id === id);
      return claimedPlansToday(row?.claimPlans).length > 0;
    },
    [extras.plans],
  );

  const growthDone = useCallback(
    (id: string): boolean => {
      const result = results[`${id}:growth`];
      if (result?.outcome === 'already') return true;
      const tasks = upstreamTasks(result).filter((task) => task.locked !== true);
      return tasks.length > 0 && tasks.every((task) => task.done === true);
    },
    [results],
  );

  /* ── 左栏：当前分区里的提供商 ───────────────────────── */

  const onboardingGroups = useMemo(() => groupByProvider(extras.onboarding), [extras.onboarding]);
  const growthGroups = useMemo(() => groupByProvider(extras.growth), [extras.growth]);
  const planGroups = useMemo(() => groupByProvider(extras.plans), [extras.plans]);

  const railOptions: ProviderOption[] = useMemo(() => {
    if (section === 'daily') {
      return daily.providers.map((group) => ({
        id: group.id,
        label: group.label || group.id,
        done: group.doneCount,
        total: group.totalCount,
      }));
    }
    if (section === 'onboarding') {
      return onboardingGroups.map((group) => ({
        id: group.id,
        label: providerLabel(center, group.id),
        done: group.rows.filter((row) => (row.settled ?? null) !== null).length,
        total: group.rows.length,
      }));
    }
    if (section === 'growth') {
      return growthGroups.map((group) => ({
        id: group.id,
        label: providerLabel(center, group.id),
        done: group.rows.filter((row) => growthDone(row.id)).length,
        total: group.rows.length,
      }));
    }
    return planGroups.map((group) => ({
      id: group.id,
      label: providerLabel(center, group.id),
      done: group.rows.filter((row) => plansDone(row.id)).length,
      total: group.rows.length,
    }));
  }, [section, daily.providers, onboardingGroups, growthGroups, planGroups, center, growthDone, plansDone]);

  /*
   * 只有插件自动化任务、没有逐账号列表的家也要能在左栏选到（A2A v3.5.1 同口径）：
   * 这类条目 `total = 0` 且带 `isTask`，因此**不**落进「暂无账号」折叠组。
   */
  const taskCategory =
    section === 'daily' ? 'checkin' : section === 'growth' ? 'growth' : section === 'plans' ? 'rewards' : '';
  if (taskCategory) {
    for (const row of asPluginTasks(center.pluginTasks)) {
      if (row.category !== taskCategory) continue;
      if (railOptions.some((option) => option.id === row.provider)) continue;
      railOptions.push({
        id: row.provider,
        label: providerLabel(center, row.provider),
        done: null,
        total: 0,
        isTask: true,
      });
    }
  }

  const railTotal = railOptions.reduce(
    (sum, option) => ({
      done: sum.done + (option.done ?? 0),
      all: sum.all + option.total,
    }),
    { done: 0, all: 0 },
  );

  /* ── 右栏：分区标题、读数、筛选 ─────────────────────── */

  const scoped = (provider: string): boolean => !providerId || provider === providerId;

  const dailyEntries: DailyEntry[] = daily.providers
    .filter((group) => scoped(group.id))
    .map((group) => ({
      group,
      accounts: byStatus(group.accounts, filter, (account) => accountStatus(account) === 'done'),
    }))
    .filter((entry) => entry.accounts.length > 0);

  const onboardingRows = byStatus(
    extras.onboarding.filter((row) => scoped(row.provider)),
    filter,
    (row) => (row.settled ?? null) !== null,
  );
  const planRows = byStatus(
    extras.plans.filter((row) => scoped(row.provider)),
    filter,
    (row) => plansDone(row.id),
  );
  const growthRows = byStatus(
    extras.growth.filter((row) => scoped(row.provider)),
    filter,
    (row) => growthDone(row.id),
  );

  const sectionTotal =
    section === 'daily'
      ? daily.providers
          .filter((group) => scoped(group.id))
          .reduce((sum, group) => sum + group.totalCount, 0)
      : section === 'onboarding'
        ? extras.onboarding.filter((row) => scoped(row.provider)).length
        : section === 'plans'
          ? extras.plans.filter((row) => scoped(row.provider)).length
          : extras.growth.filter((row) => scoped(row.provider)).length;
  const sectionDone = railOptions.reduce((sum, option) => sum + (option.done ?? 0), 0);
  const counts = filterCounts(sectionTotal, sectionDone);

  const summary =
    section === 'daily'
      ? daily.todayEligible > 0
        ? t('{{done}} / {{total}} 个账号已签', { done: daily.todayDone, total: daily.todayEligible })
        : t('今天没有需要签到的账号')
      : section === 'onboarding'
        ? extras.onboarding.length > 0
          ? t('{{n}} 个账号', { n: extras.onboarding.length })
          : t('没有支持新手福利的账号')
        : section === 'plans'
          ? t('限时活动与体验套餐 · 今天已领 {{n}} 份', {
              n: extras.plans.filter((row) => plansDone(row.id)).length,
            })
          : extras.growth.length > 0
            ? t('{{n}} 个账号', { n: extras.growth.length })
            : t('没有支持成长任务的账号');

  const emptyText =
    section === 'daily'
      ? sectionTotal === 0
        ? t('没有可签到的账号')
        : filter === 'done'
          ? t('今天还没有账号签到，切到「全部」看看哪些还没签。')
          : t('今天的账号都签完了。')
      : section === 'onboarding'
        ? extras.onboarding.length === 0
          ? t('没有支持新手福利的账号。')
          : filter === 'done'
            ? t('还没有账号领完新手福利，切到「全部」看看待领的项。')
            : t('新手福利都领完了。')
        : section === 'plans'
          ? extras.plans.length === 0
            ? t('没有支持活动福利的账号，请添加账号并更新提供商插件。')
            : filter === 'done'
              ? t('今天还没有账号领到过，切到「全部」看看还能领什么。')
              : t('都领到了，暂无待领的账号。')
          : extras.growth.length === 0
            ? t('没有支持成长任务的账号（需要 WorkBuddy 国内版）。')
            : filter === 'done'
              ? t('还没有账号完成全部任务，切到「全部」看看可完成的项。')
              : t('成长任务都完成了。');

  const empty =
    section === 'daily'
      ? dailyEntries.length === 0
      : section === 'onboarding'
        ? onboardingRows.length === 0
        : section === 'plans'
          ? planRows.length === 0
          : growthRows.length === 0;

  /* ── 行内的动作 ─────────────────────────────────────── */

  const resultBadge = (key: string): JSX.Element | null => {
    const result = results[key];
    if (!result) return null;
    return (
      <Badge tone={BENEFIT_TONE[result.outcome] ?? 'neutral'}>
        {t(BENEFIT_LABEL[result.outcome] ?? result.outcome)}
      </Badge>
    );
  };

  const onboardingBadge = (id: string): JSX.Element | null => {
    const row = extras.onboarding.find((item) => item.id === id);
    if (!row) return null;
    if (row.settled) return <Badge tone="success">{t('已领取')}</Badge>;
    if (typeof row.pending === 'number') {
      return row.pending > 0 ? (
        <Badge tone="warning">{t('{{n}} 项待领', { n: row.pending })}</Badge>
      ) : (
        <Badge tone="neutral">{t('暂无可领取福利')}</Badge>
      );
    }
    return <Badge tone="neutral">{t('待盘点')}</Badge>;
  };

  const onboardingSettled = (id: string): boolean =>
    (extras.onboarding.find((row) => row.id === id)?.settled ?? null) !== null;

  const onboardingActions = (id: string): JSX.Element => (
    <>
      <Button
        variant="outline"
        size="sm"
        disabled={isRunning(`${id}:onboarding`) || onboardingSettled(id)}
        onClick={() => void runBenefit(id, 'onboarding')}
      >
        {isRunning(`${id}:onboarding`) ? t('查询中…') : t('查询任务')}
      </Button>
      <Button
        variant="outline"
        size="sm"
        disabled={isRunning(`${id}:onboarding_claim`) || onboardingSettled(id)}
        onClick={() => void runBenefit(id, 'onboarding_claim')}
      >
        {isRunning(`${id}:onboarding_claim`) ? t('领取中…') : t('一键领取')}
      </Button>
      {resultBadge(`${id}:onboarding_claim`) ?? resultBadge(`${id}:onboarding`)}
    </>
  );

  const planActions = (id: string): JSX.Element => (
    <>
      <Button
        variant="outline"
        size="sm"
        disabled={isRunning(`${id}:plan`)}
        onClick={() => void runBenefit(id, 'plan', { mode: 'preview' })}
      >
        {isRunning(`${id}:plan`) ? t('查询中…') : t('查询套餐')}
      </Button>
      <Button variant="primary" size="sm" onClick={onOpenBenefits}>
        {plansDone(id) ? t('套餐明细') : t('去领取')}
      </Button>
      {resultBadge(`${id}:plan`)}
    </>
  );

  const growthActions = (id: string): JSX.Element => (
    <>
      <Button
        variant="outline"
        size="sm"
        disabled={isRunning(`${id}:growth`)}
        onClick={() => void runBenefit(id, 'growth')}
      >
        {isRunning(`${id}:growth`) ? t('查询中…') : t('查询任务')}
      </Button>
      {resultBadge(`${id}:growth`)}
    </>
  );

  /** 成长任务树：查询后逐条领取（`params.code` 给后端跑那一条） */
  const growthDetail = (id: string): JSX.Element | null => {
    const tasks = upstreamTasks(results[`${id}:growth`]);
    if (tasks.length === 0) return null;
    return (
      <div className="ck-extra-detail">
        {tasks.map((task, index) => {
          const code = typeof task.code === 'string' ? task.code.trim() : '';
          return (
            <div className="ck-benefit-head" key={taskKey(task, index)}>
              <span className="ck-extra-name">{task.title || task.name || code || `#${index + 1}`}</span>
              {task.locked ? <Badge tone="neutral">{t('未解锁')}</Badge> : null}
              {task.done ? <Badge tone="success">{t('已领取')}</Badge> : null}
              <span className="ck-benefit-actions">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={
                    !code
                    || task.done === true
                    || task.locked === true
                    || isRunning(`${id}:growth:${code}`)
                  }
                  onClick={() => void runBenefit(id, 'growth', { code }, `${id}:growth:${code}`)}
                >
                  {isRunning(`${id}:growth:${code}`) ? t('领取中…') : t('领取')}
                </Button>
              </span>
            </div>
          );
        })}
      </div>
    );
  };

  /* ── 空态：没装插件 / 没账号（A2A `EmptyState`） ────── */

  if (section === 'daily' && daily.providers.length === 0) {
    const hasFeature = Object.keys(center.checkinFeatures ?? {}).length > 0;
    return (
      <Card title={t('每日签到')}>
        <EmptyState
          icon="📅"
          message={
            hasFeature
              ? t('签到能力已就绪，但还没有可签到的账号。到「账号」页添加并启用账号后，这里会自动出现。')
              : t('还没有可签到的提供商。签到能力由提供商插件/实现提供，请先在「插件」页安装 Agent2API 提供商插件。')
          }
        />
      </Card>
    );
  }

  const pluginCategories =
    section === 'growth'
      ? ['growth']
      : section === 'plans'
        ? ['rewards']
        : section === 'daily'
          ? ['checkin']
          : [];

  return (
    <div className="ck-center-layout">
      <CheckinProviderRail
        options={railOptions}
        selected={providerId}
        total={railTotal}
        onSelect={setProviderId}
      />
      <div className="ck-center-main">
        <Card
          title={t(SECTION_TITLE[section])}
          actions={
            <div className="ck-actions">
              {section === 'daily' ? (
                <Button disabled={busyKey !== null} onClick={onRunAll}>
                  <Play size={12} />
                  {busyKey === 'all' ? t('签到中…') : t('全部签到')}
                </Button>
              ) : null}
              {section === 'onboarding' && onboardingRows.length > 0 ? (
                <>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={isRunning('bulk:onboarding')}
                    onClick={() => void runBenefitBulk('onboarding', onboardingRows)}
                  >
                    {isRunning('bulk:onboarding') ? t('查询中…') : t('全部查询')}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={isRunning('bulk:onboarding_claim')}
                    onClick={() => void runBenefitBulk('onboarding_claim', onboardingRows)}
                  >
                    {isRunning('bulk:onboarding_claim') ? t('领取中…') : t('全部领取')}
                  </Button>
                </>
              ) : null}
              <div className="ck-seg" role="group" aria-label={t('按状态筛选')}>
                {(['all', 'done', 'pending'] as StatusFilter[]).map((value, index) => (
                  <button
                    key={value}
                    type="button"
                    className={`ck-seg-item${filter === value ? ' is-active' : ''}`}
                    onClick={() => setFilter(value)}
                  >
                    {t(FILTER_LABELS[section][index])} {counts[value]}
                  </button>
                ))}
              </div>
            </div>
          }
        >
          <p className="ck-time-subtle">{summary}</p>
          {error ? <div className="error-message">{error}</div> : null}

          {section === 'daily' ? (
            <CheckinDailyPanel
              entries={dailyEntries}
              showProvider={!providerId}
              busyKey={busyKey}
              onSignOne={onSignOne}
              onRunBenefit={(accountId, action) => void runBenefit(accountId, action)}
              onOpenBenefits={onOpenBenefits}
            />
          ) : null}

          {section === 'onboarding'
            ? onboardingRows.map((row) => (
                <BenefitRow
                  key={row.id}
                  accountId={row.id}
                  name={row.name}
                  providerName={providerLabel(center, row.provider)}
                  description={
                    onboardingSettled(row.id)
                      ? t('一次性福利已结清，无需重复查询')
                      : typeof row.pending === 'number'
                        ? t('{{n}} 项待领', { n: row.pending })
                        : t('尚未查询任务状态')
                  }
                  badge={onboardingBadge(row.id)}
                  actions={onboardingActions(row.id)}
                />
              ))
            : null}

          {section === 'plans'
            ? planRows.map((row) => {
                const claimed = claimedPlansToday(row.claimPlans);
                return (
                  <BenefitRow
                    key={row.id}
                    accountId={row.id}
                    name={row.name}
                    providerName={providerLabel(center, row.provider)}
                    description={
                      claimed.length > 0
                        ? t('今天已领 {{n}} 份', { n: claimed.length })
                        : row.claimAt
                          ? t('今天未领 · 上次领取 {{time}}', { time: formatShort(row.claimAt) })
                          : t('今天未领')
                    }
                    badge={
                      claimed.length > 0 ? (
                        <Badge tone="success">{t('今天已领')}</Badge>
                      ) : (
                        <Badge tone="neutral">{t('未领取')}</Badge>
                      )
                    }
                    actions={planActions(row.id)}
                  />
                );
              })
            : null}

          {section === 'growth'
            ? growthRows.map((row) => {
                const result = results[`${row.id}:growth`];
                const tasks = upstreamTasks(result);
                const pending = tasks.filter((task) => task.done !== true && task.locked !== true).length;
                const energy = (result?.data as { energy?: unknown } | undefined)?.energy;
                const streak = (result?.data as { streak?: unknown } | undefined)?.streak;
                const note = result
                  ? [
                      energy != null ? t('能量 {{value}}', { value: Number(energy) }) : '',
                      streak != null ? t('连登 {{days}} 天', { days: Number(streak) }) : '',
                    ]
                      .filter(Boolean)
                      .join(' · ')
                  : '';
                return (
                  <BenefitRow
                    key={row.id}
                    accountId={row.id}
                    name={row.name}
                    providerName={providerLabel(center, row.provider)}
                    description={
                      result
                        ? tasks.length > 0
                          ? t('{{n}} 项任务 · {{pending}} 项可完成', { n: tasks.length, pending })
                          : result.message
                        : t('查询任务树后逐条领取（含开学季与猫猫旅行）')
                    }
                    badge={
                      result ? (
                        pending > 0 ? (
                          <Badge tone="warning">{t('{{n}} 项可完成', { n: pending })}</Badge>
                        ) : (
                          <Badge tone="success">{t('全部完成')}</Badge>
                        )
                      ) : null
                    }
                    actions={growthActions(row.id)}
                    detail={growthDetail(row.id)}
                    note={note || undefined}
                  />
                );
              })
            : null}

          {pluginCategories.length > 0 ? (
            <CheckinPluginTasks center={center} provider={providerId} categories={pluginCategories} />
          ) : null}

          {empty ? <EmptyState icon="📋" message={emptyText} /> : null}

          {section === 'daily' && daily.outOfScope.length > 0 ? (
            <div className="ck-out-of-scope">
              <span>
                {t('另有 {{n}} 个账号不参与签到：', {
                  n: daily.outOfScope.reduce((sum, item) => sum + (Number(item.count) || 0), 0),
                })}
                {daily.outOfScope
                  .map((item) => `${item.label}（${item.reason} ×${item.count}）`)
                  .join('、')}
              </span>
            </div>
          ) : null}

          <p className="form-hint">{t(SECTION_FOOT[section])}</p>
        </Card>
      </div>
    </div>
  );
}
