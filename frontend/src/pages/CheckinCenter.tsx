/**
 * 签到中心（页级容器）。
 *
 * ── 这一页的结构照 Agent2API v3.5.1 ─────────────────────────
 * 一级 Tab 顺序：每日签到 / 新手福利 / 活动福利 / 成长任务（四个「任务」分区，
 * 由 `checkin-sections.tsx` 渲染）+ 定时签到 / 定时补签 / 签到记录（三个视图）。
 * 顶部是四格总览（今日签到 / 定时签到 / 待领新手福利 / 累计签到记录），
 * 每格可点，跳到对应页。
 *
 * ── 本文件只剩「壳」────────────────────────────────────────
 * 数据加载、定时/补签的写动作、旅行巡检、签到后的自动链都留在这里；四个分区与
 * 三个视图各自独立成文件（`checkin-sections` / `checkin-schedule` /
 * `checkin-makeup` / `checkin-records`），判据与文案表在 `checkin-shared`。
 *
 * ── AIGX 独有（评审项）─────────────────────────────────────
 * 1. 「福利通道」第 8 个 Tab（一次性/限时动作的手动入口，A2A 把这些并进分区行）；
 * 2. 签到设置卡（账号间隔 / 签到时区 / 全局重试间隔 / 台账保留天数）；
 * 3. 猫猫旅行定时排程（A2A 的 travel 由插件面板驱动，不在签到页）；
 * 4. 逐账号福利预览卡与轮次时间线（A2A 快照同样带 extras / history，但页面不渲染）；
 * 5. 活动福利卡（`CheckinCampaignCard`：A2A 用 plans section 内的 CampaignRow）；
 * 6. 签到后的自动链（AIGX 用 `AUTO_CHAIN` 串一次性福利，A2A 无此链路）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CalendarCheck, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Card, EmptyState, Loading } from '../components/ui';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import BenefitsPanel from '../components/BenefitsPanel';
import CheckinCampaignCard from '../components/CheckinCampaignCard';
import type {
  CheckinCenterData,
  CheckinHistoryRow,
  CheckinRunResult,
  CheckinScheduleEntry,
} from '../types';
import CheckinCenterSections from './checkin-sections';
import CheckinScheduleView from './checkin-schedule';
import CheckinMakeupView from './checkin-makeup';
import CheckinRecordsView from './checkin-records';
import CheckinScheduleEditor, {
  EMPTY_SCHEDULE,
  formOf,
  type ScheduleForm,
} from './checkin-editor';
import {
  CHECKIN_SECTION_TABS,
  CHECKIN_VIEW_TABS,
  formatTime,
  normalizeCheckinTab,
  type CheckinTab,
} from './checkin-shared';
import './CheckinCenter.css';

type TabKey = CheckinTab;

const TAB_STORAGE_KEY = 'aigx-checkin-tab';

/**
 * 签到后的自动链（AIGX 独有）：签到成功即串行跑一遍该家的一次性/限时福利，
 * 不用管理员再切到「福利通道」逐个点。单个动作失败即停（不拖垮后续）。
 */
const AUTO_CHAIN: Record<string, string[]> = {
  codearts: ['newbie'],
  loomy: ['onboarding', 'onboarding_claim'],
  xiaohuanxiong: ['onboarding'],
  'workbuddy-intl': ['keepalive_claim'],
};

interface CheckinCenterProps {
  /** 作为供应商页「签到」子页内嵌时隐藏自身标题区（页内二级 Tab 保留） */
  embedded?: boolean;
}

/** 轮次时间线（A2A `history`；AIGX 独有渲染面）：新条目在前 */
function CheckinHistoryTimeline({ rows }: { rows: CheckinHistoryRow[] }): JSX.Element | null {
  const { t } = useTranslation();
  if (rows.length === 0) return null;
  return (
    <Card title={t('最近签到轮次')}>
      <ol className="ck-history">
        {rows.map((row, index) => {
          const failed = Number(row.failedCount ?? row.failed?.length ?? 0);
          const succeeded = Number(row.succeeded ?? 0);
          const active = Number(row.active ?? 0);
          const total = Number(row.total ?? 0);
          return (
            <li key={`${row.at}-${index}`} className="ck-history-item">
              <span className="ck-history-time">
                {row.at ? new Date(row.at).toLocaleString() : row.date || '—'}
              </span>
              <span className="ck-history-reason">{row.reason || t('批量签到')}</span>
              <Badge tone={failed > 0 ? 'warning' : 'success'}>
                {t('成功 {{ok}}/{{total}}', { ok: succeeded, total })}
              </Badge>
              {active > 0 && <Badge tone="info">{t('保活 {{n}}', { n: active })}</Badge>}
              {failed > 0 && <Badge tone="danger">{t('失败 {{n}}', { n: failed })}</Badge>}
              {row.failed && row.failed.length > 0 && (
                <span className="ck-history-failures">{row.failed.join('、')}</span>
              )}
            </li>
          );
        })}
      </ol>
    </Card>
  );
}

/** 逐账号福利预览（A2A `extras`；AIGX 独有渲染面）：入口清单零上游请求 */
function CheckinExtrasPreview({ center }: { center: CheckinCenterData }): JSX.Element | null {
  const { t } = useTranslation();
  const { onboarding, plans, campaigns, growth } = center.extras ?? {
    onboarding: [],
    plans: [],
    campaigns: [],
    growth: [],
  };
  const hasAny =
    onboarding.length > 0 || plans.length > 0 || campaigns.length > 0 || growth.length > 0;
  if (!hasAny) return null;
  const groups: Array<{ title: string; rows: JSX.Element[] }> = [];
  if (onboarding.length > 0) {
    groups.push({
      title: t('新手福利'),
      rows: onboarding.map((row) => (
        <li key={`onboarding-${row.id}`} className="ck-extra-row">
          <span className="ck-extra-name">{row.name || row.id}</span>
          <span className="ck-extra-provider">{row.provider}</span>
          {row.settled ? (
            <Badge tone="success">{t('已结清')}</Badge>
          ) : (row.pending ?? 0) > 0 ? (
            <Badge tone="warning">{t('{{n}} 项待领', { n: row.pending })}</Badge>
          ) : row.pending == null ? (
            <Badge tone="neutral">{t('待盘点')}</Badge>
          ) : (
            <Badge tone="neutral">{t('暂无待领')}</Badge>
          )}
        </li>
      )),
    });
  }
  if (plans.length > 0 || campaigns.length > 0) {
    const seen = new Set<string>();
    const rows: JSX.Element[] = [];
    for (const row of [...plans, ...campaigns]) {
      if (seen.has(row.id)) continue;
      seen.add(row.id);
      const planRow = plans.find((item) => item.id === row.id);
      const claimed = planRow?.claimPlans ? Object.keys(planRow.claimPlans).length : 0;
      rows.push(
        <li key={`plans-${row.id}`} className="ck-extra-row">
          <span className="ck-extra-name">{row.name || row.id}</span>
          <span className="ck-extra-provider">{row.provider}</span>
          {claimed > 0 ? (
            <Badge tone="success">{t('今日已领 {{n}} 份', { n: claimed })}</Badge>
          ) : (
            <Badge tone="neutral">{t('未领取')}</Badge>
          )}
        </li>,
      );
    }
    groups.push({ title: t('活动福利 / 限时套餐'), rows });
  }
  if (growth.length > 0) {
    groups.push({
      title: t('成长任务'),
      rows: growth.map((row) => (
        <li key={`growth-${row.id}`} className="ck-extra-row">
          <span className="ck-extra-name">{row.name || row.id}</span>
          <span className="ck-extra-provider">{row.provider}</span>
          <Badge tone="neutral">{t('在「福利通道」执行')}</Badge>
        </li>
      )),
    });
  }
  return (
    <Card title={t('福利预览（逐账号）')}>
      <p className="form-hint">
        {t('入口清单来自快照（零上游请求）；任务明细与领取在「福利通道」页执行。')}
      </p>
      <div className="ck-extra-groups">
        {groups.map((group) => (
          <div key={group.title} className="ck-extra-group">
            <h4>{group.title}</h4>
            <ul>{group.rows}</ul>
          </div>
        ))}
      </div>
    </Card>
  );
}

export default function CheckinCenter({ embedded = false }: CheckinCenterProps = {}): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [tab, setTab] = useState<TabKey>(() =>
    normalizeCheckinTab(localStorage.getItem(TAB_STORAGE_KEY)),
  );
  const [data, setData] = useState<CheckinCenterData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  // 单飞：一次只允许一个签到动作在途（上游有全局 running 防重），
  // 但按钮级 busy 只落在对应行上，不让一颗在跑拖灰整页
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [autoChaining, setAutoChaining] = useState(false);
  const [autoChainProgress, setAutoChainProgress] = useState<null | {
    total: number;
    done: number;
    action: string;
    stopped: boolean;
  }>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // 定时条目编辑（行内表单：editingId='new' 显示首行新建）
  const [editingId, setEditingId] = useState<string | null>(null);
  const [scheduleForm, setScheduleForm] = useState<ScheduleForm>(EMPTY_SCHEDULE);
  const [savingSchedule, setSavingSchedule] = useState(false);

  // 设置卡的三个输入（独立于 data 快照，避免整卡重渲染）
  const [gapMsEdit, setGapMsEdit] = useState('');
  const [gapSaving, setGapSaving] = useState(false);
  const [retryMinEdit, setRetryMinEdit] = useState('');
  const [keepDaysEdit, setKeepDaysEdit] = useState('');
  const [advSaving, setAdvSaving] = useState(false);
  const [travelTimesDraft, setTravelTimesDraft] = useState<string | null>(null);
  /** 设置卡是否已按首份快照播种（回读不再覆盖用户正在填的值） */
  const settingsSeeded = useRef(false);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const d = await api.getCheckinCenter();
      setData(d);
      /*
       * 设置卡的三个输入只在**首次**读数时播种：A2A v3.5.1 的同类修复
       * （`checkin-store` 保留未保存草稿）—— 后台回读/保存后的刷新不该把
       * 用户正在填的间隔或保留天数清空。
       */
      if (d?.settings && !settingsSeeded.current) {
        settingsSeeded.current = true;
        setGapMsEdit(String(d.settings.gap_ms ?? 800));
        setRetryMinEdit(String(Math.round((d.settings.retry_gap_ms ?? 600_000) / 60_000)));
        setKeepDaysEdit(String(d.settings.keep_days ?? 0));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    localStorage.setItem(TAB_STORAGE_KEY, tab);
  }, [tab]);

  /* ── 派生 ─────────────────────────────────────────────── */

  const providers = useMemo(() => data?.daily?.providers ?? [], [data?.daily?.providers]);
  const outOfScope = useMemo(() => data?.daily?.outOfScope ?? [], [data?.daily?.outOfScope]);
  const supported = useMemo(() => data?.supportedProviders ?? [], [data?.supportedProviders]);
  const unsupported = useMemo(() => data?.unsupportedProviders ?? [], [data?.unsupportedProviders]);
  const schedules = useMemo(() => data?.auto?.schedules ?? [], [data?.auto?.schedules]);
  /** 范围复选框清单（A2A `available_checkin_providers` → `providerOptions`） */
  const pickOptions = useMemo(() => data?.auto?.providerOptions ?? [], [data?.auto?.providerOptions]);

  /** provider id → 展示名（左栏 / 分组 / 记录筛选共用） */
  const labels = useMemo(() => {
    const map: Record<string, string> = {};
    for (const option of pickOptions) map[option.id] = option.label;
    for (const group of providers) map[group.id] = group.label || group.id;
    return map;
  }, [pickOptions, providers]);

  /** 内置专属时刻（A2A `providerOptions[].builtinTime` → {provider: "HH:MM"}） */
  const builtinTimes = useMemo(() => {
    const map: Record<string, string> = {};
    for (const option of pickOptions) if (option.builtinTime) map[option.id] = option.builtinTime;
    return map;
  }, [pickOptions]);

  const totals = useMemo(
    () => ({
      done: data?.daily?.todayDone ?? 0,
      total: data?.daily?.todayEligible ?? 0,
      accounts: providers.reduce((n, group) => n + group.accounts.length, 0),
    }),
    [data?.daily?.todayDone, data?.daily?.todayEligible, providers],
  );

  const outOfScopeCount = useMemo(
    () => outOfScope.reduce((sum, item) => sum + (Number(item.count) || 0), 0),
    [outOfScope],
  );

  /** 启用的定时条目数（总览第二格） */
  const enabledSchedules = useMemo(
    () => schedules.filter((entry) => entry.enabled).length,
    [schedules],
  );

  /** 最近一次将执行的时刻（总开关关着 / 无启用条目 = null） */
  const nextScheduleAt = useMemo(() => {
    if (data?.auto?.enabled !== true) return null;
    const stamps = schedules
      .filter((entry) => entry.enabled)
      .map((entry) => Number(entry.nextRunAt))
      .filter((stamp) => Number.isFinite(stamp) && stamp > 0)
      .sort((a, b) => a - b);
    return stamps[0] ?? null;
  }, [schedules, data?.auto?.enabled]);

  /** 新手福利待领项合计（`null` = 还没盘点过，不计入合计） */
  const onboardingPending = useMemo(
    () =>
      (data?.extras?.onboarding ?? []).reduce(
        (sum, row) => sum + (typeof row.pending === 'number' ? row.pending : 0),
        0,
      ),
    [data?.extras?.onboarding],
  );
  const onboardingChecked = useMemo(
    () => (data?.extras?.onboarding ?? []).filter((row) => typeof row.pending === 'number').length,
    [data?.extras?.onboarding],
  );

  /** 顶栏徽标（对齐 A2A `AutoBadge`：执行中 / 已暂停 / 还有 N 个平台未执行 / 已开启） */
  const autoBadge = useMemo(() => {
    const auto = data?.auto;
    if (!auto) return null;
    if (auto.running) {
      return <span className="ck-tab-badge is-on">{t('自动签到执行中…')}</span>;
    }
    if (!auto.enabled) return <span className="ck-tab-badge is-off">{t('定时已关闭')}</span>;
    const pending = (auto.schedules ?? [])
      .filter((entry) => entry.enabled)
      .reduce((sum, entry) => sum + (entry.pendingProviders?.length ?? 0), 0);
    const firedToday = (auto.schedules ?? []).some((entry) => entry.enabled && entry.lastFiredToday);
    if (pending > 0) {
      return <span className="ck-tab-badge is-on">{t('还有 {{n}} 个平台未执行', { n: pending })}</span>;
    }
    return (
      <span className={`ck-tab-badge ${firedToday ? 'is-ok' : 'is-on'}`}>
        {firedToday ? t('已开启 · 今日已执行') : t('定时已开启')}
      </span>
    );
  }, [data?.auto, t]);

  /* ── 签到（每日全量 / 单账号）+ 自动链 ────────────────── */

  const autoChain = useCallback(
    async (results: CheckinRunResult[]): Promise<void> => {
      const chain = results
        .filter((row) => row.success && AUTO_CHAIN[row.provider])
        .flatMap((row) => (AUTO_CHAIN[row.provider] ?? []).map((action) => ({ row, action })));
      if (chain.length === 0) return;
      setAutoChaining(true);
      setAutoChainProgress({ total: chain.length, done: 0, action: '', stopped: false });
      try {
        for (let i = 0; i < chain.length; i++) {
          const { row, action } = chain[i];
          setAutoChainProgress({ total: chain.length, done: i, action, stopped: false });
          const res = await api.runCheckinBenefit(row.account_id, action);
          const result = res?.data;
          if (result && !result.ok) {
            setAutoChainProgress({ total: chain.length, done: i + 1, action, stopped: true });
            break;
          }
          // workbuddy-intl 保活+领取成功后回写台账（保活标记）
          if (action === 'keepalive_claim' && result?.ok) {
            try {
              await api.markKeptActive(row.account_id, row.provider);
            } catch {
              // 回写失败不影响主流程（记录仍可手动标记）
            }
          }
          await new Promise((resolve) => setTimeout(resolve, 300));
        }
        addToast(t('自动链完成'), 'success');
      } catch {
        // 自动链失败不拖垮签到结果展示
      } finally {
        setAutoChaining(false);
      }
    },
    [addToast, t],
  );

  const runAll = useCallback(async (): Promise<void> => {
    setBusyKey('all');
    try {
      const res = await api.runCheckin({ providers: [] });
      const d = res?.data;
      if (d) {
        addToast(`${t('签到完成')}：${d.done}/${d.total}`);
        void autoChain(d.results);
      }
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyKey(null);
    }
  }, [addToast, autoChain, load, t]);

  const runOne = useCallback(
    async (accountId: string): Promise<void> => {
      setBusyKey(`sign:${accountId}`);
      try {
        const res = await api.runCheckin({ account_ids: [accountId] });
        const row = res?.data?.results?.[0];
        if (row) {
          addToast(`${row.account_name}：${row.message}`, row.success ? 'success' : 'error');
          if (res.data) void autoChain(res.data.results);
        }
        await load();
      } catch (err) {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      } finally {
        setBusyKey(null);
      }
    },
    [addToast, autoChain, load],
  );

  /* ── 设置写入 ─────────────────────────────────────────── */

  const saveGapMs = useCallback(async (): Promise<void> => {
    if (!data) return;
    const n = Number(gapMsEdit);
    if (!Number.isFinite(n) || n < 0) {
      addToast(t('间隔必须为非负数字'), 'error');
      return;
    }
    setGapSaving(true);
    try {
      await api.updateCheckinSettings({ ...data.settings, gap_ms: Math.floor(n) });
      addToast(t('间隔已保存'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setGapSaving(false);
    }
  }, [addToast, data, gapMsEdit, load, t]);

  const saveAdvanced = useCallback(async (): Promise<void> => {
    if (!data) return;
    const min = Number(retryMinEdit);
    const days = Number(keepDaysEdit);
    if (!Number.isFinite(min) || min < 0 || min > 1440) {
      addToast(t('重试间隔须为 0-1440 分钟（0 = 关闭重试）'), 'error');
      return;
    }
    if (!Number.isFinite(days) || days < 0 || !Number.isInteger(days)) {
      addToast(t('台账保留天数须为非负整数（0 = 永久）'), 'error');
      return;
    }
    setAdvSaving(true);
    try {
      await api.updateCheckinSettings({
        ...data.settings,
        retry_gap_ms: Math.floor(min) * 60_000,
        keep_days: Math.floor(days),
      });
      addToast(t('已保存'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setAdvSaving(false);
    }
  }, [addToast, data, keepDaysEdit, load, retryMinEdit, t]);

  const changeTimezone = useCallback(
    async (offset: number): Promise<void> => {
      if (!data) return;
      try {
        await api.updateCheckinSettings({ ...data.settings, timezone_offset: offset });
        addToast(t('时区已保存（对下一次计划执行生效）'));
        await load();
      } catch (err) {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      }
    },
    [addToast, data, load, t],
  );

  const toggleSetting = useCallback(
    async (enabled: boolean): Promise<void> => {
      if (!data) return;
      try {
        await api.updateCheckinSettings({ ...data.settings, enabled });
        await load();
        addToast(enabled ? t('已开启定时签到') : t('已关闭定时签到'));
      } catch (err) {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      }
    },
    [addToast, data, load, t],
  );

  /* ── 定时条目写动作 ───────────────────────────────────── */

  /** 全量 upsert 一条（未传字段由后端继承存量；这里始终给全量，语义最清楚） */
  const upsertEntry = useCallback(
    async (
      entry: CheckinScheduleEntry,
      changes: Partial<{
        enabled: boolean;
        retryTime: string;
        retryLimit: number;
        retryGapMinutes: number;
      }>,
    ): Promise<void> => {
      setSavingSchedule(true);
      try {
        await api.upsertCheckinSchedule({
          id: entry.id,
          label: entry.label,
          enabled: changes.enabled ?? entry.enabled,
          time: entry.time,
          providers: entry.providers,
          provider_times: entry.providerTimes ?? {},
          time_end: entry.timeEnd ?? '',
          randomize: entry.randomize ?? false,
          retry_limit: changes.retryLimit ?? entry.retryLimit ?? 0,
          retry_gap_minutes: changes.retryGapMinutes ?? entry.retryGapMinutes ?? 0,
          makeup_time: changes.retryTime ?? entry.retryTime ?? '',
        });
        await load();
      } catch (err) {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      } finally {
        setSavingSchedule(false);
      }
    },
    [addToast, load],
  );

  const runSchedule = useCallback(
    async (entry: CheckinScheduleEntry): Promise<void> => {
      setBusyKey(`sched:${entry.id}`);
      try {
        const d = await api.runCheckinSchedule(entry.id);
        addToast(
          `${entry.displayLabel || entry.label}：${d?.summary ?? ''}` +
            (d ? `（成功 ${d.ok}·保活 ${d.keepalive}·失败 ${d.failed}）` : ''),
        );
        await load();
      } catch (err) {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      } finally {
        setBusyKey(null);
      }
    },
    [addToast, load],
  );

  const removeSchedule = useCallback(
    (entry: CheckinScheduleEntry): void => {
      setConfirmState({
        title: t('删除签到计划'),
        message: t('确定删除「{{name}}」？对应的定时补签与活动重试也会一并移除，签到记录保留。', {
          name: entry.displayLabel || entry.label,
        }),
        confirmText: t('删除'),
        danger: true,
        onConfirm: async () => {
          await api.removeCheckinSchedule(entry.id);
          addToast(t('已删除'));
          await load();
        },
      });
    },
    [addToast, load, t],
  );

  const openNewSchedule = useCallback((): void => {
    setScheduleForm({ ...EMPTY_SCHEDULE });
    setEditingId('new');
  }, []);

  const openEditSchedule = useCallback((entry: CheckinScheduleEntry): void => {
    setScheduleForm(formOf(entry));
    setEditingId(entry.id);
  }, []);

  /* ── 旅行巡检（AIGX 独有） ───────────────────────────── */

  const travel = data?.travel;
  const travelTimesValue = travelTimesDraft ?? (travel?.times ?? []).join(', ');

  const saveTravel = useCallback(
    async (body: { enabled?: boolean; times?: string[] }): Promise<void> => {
      try {
        await api.updateCheckinTravel(body);
        addToast(t('巡检设置已保存'));
        // 只清「本次提交过时刻」的草稿，未提交的编辑不被回读冲掉
        if (body.times) setTravelTimesDraft(null);
        await load();
      } catch (err) {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      }
    },
    [addToast, load, t],
  );

  const commitTravelTimes = useCallback((): void => {
    const raw = travelTimesValue
      .split(/[,，\s]+/)
      .map((item) => item.trim())
      .filter(Boolean);
    if (raw.some((item) => !/^([01]\d|2[0-3]):[0-5]\d$/.test(item))) {
      addToast(t('巡检时刻格式应为 HH:MM（每行一个）'), 'error');
      return;
    }
    void saveTravel({ times: raw });
  }, [addToast, saveTravel, t, travelTimesValue]);

  const runTravel = useCallback(async (): Promise<void> => {
    try {
      const d = await api.runCheckinTravelNow();
      if (d?.outcome === 'skipped') {
        addToast(t('本轮已跳过：{{reason}}', { reason: d.message || '—' }));
      } else if (d?.outcome === 'busy') {
        addToast(t('巡检正在执行中，请稍候'), 'error');
      } else {
        const summary = d?.summary?.message || '';
        addToast(`${t('巡检完成')}${summary ? `：${summary}` : ''}`);
      }
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  }, [addToast, load, t]);

  /* ── 视图 ─────────────────────────────────────────────── */

  const tabs = useMemo(
    () => [
      ...CHECKIN_SECTION_TABS.map((item) => ({ ...item, label: t(item.label) })),
      ...CHECKIN_VIEW_TABS.map((item) => ({ ...item, label: t(item.label) })),
    ],
    [t],
  );

  const isSection = CHECKIN_SECTION_TABS.some((item) => item.value === tab);

  const editor =
    editingId !== null ? (
      <CheckinScheduleEditor
        form={scheduleForm}
        onChange={setScheduleForm}
        onClose={() => setEditingId(null)}
        onSaved={() => {
          addToast(t('计划已保存'));
          setEditingId(null);
          setScheduleForm({ ...EMPTY_SCHEDULE });
          void load();
        }}
        onError={(message) => addToast(message, 'error')}
        pickOptions={pickOptions}
        builtinTimes={builtinTimes}
        unsupported={unsupported}
        saving={savingSchedule}
      />
    ) : null;

  return (
    <div>
      {!embedded && (
        <div className="page-header">
          <div>
            <h1>{t('签到中心')}</h1>
            <p>
              {t('替账号池里的上游账号每日领取它们自带的额度。与钱包里的「用户签到」是两套。')}
            </p>
          </div>
          <button
            className="btn btn-outline btn-sm"
            onClick={() => void load()}
            disabled={loading}
            style={{ gap: 6 }}
          >
            <RefreshCw size={13} /> {t('刷新')}
          </button>
        </div>
      )}

      {error && <div className="error-message">{error}</div>}

      <div className="sub-tabs ck-sub-tabs">
        {tabs.map((item) => (
          <button
            key={item.value}
            className={`sub-tab${tab === item.value ? ' active' : ''}`}
            onClick={() => setTab(item.value)}
          >
            {item.label}
          </button>
        ))}
        <span className="ck-tab-badge-slot">{autoBadge}</span>
      </div>

      {loading && !data ? (
        <Loading text={t('加载签到中心')} />
      ) : !data ? (
        <EmptyState message={t('没有读到签到中心数据，请稍后重试。')} />
      ) : tab === 'benefits' ? (
        <BenefitsPanel />
      ) : tab === 'records' ? (
        <CheckinRecordsView center={data} onOpenBenefits={() => setTab('benefits')} />
      ) : tab === 'schedule' ? (
        <CheckinScheduleView
          auto={data.auto}
          settings={data.settings}
          labels={labels}
          unsupported={unsupported}
          busyKey={busyKey}
          editingId={editingId}
          editor={editor}
          gapMsEdit={gapMsEdit}
          gapSaving={gapSaving}
          retryMinEdit={retryMinEdit}
          keepDaysEdit={keepDaysEdit}
          advSaving={advSaving}
          onGapMsChange={setGapMsEdit}
          onSaveGapMs={() => void saveGapMs()}
          onRetryMinChange={setRetryMinEdit}
          onKeepDaysChange={setKeepDaysEdit}
          onSaveAdvanced={() => void saveAdvanced()}
          onTimezoneChange={(offset) => void changeTimezone(offset)}
          onToggleMaster={(enabled) => void toggleSetting(enabled)}
          onToggleEntry={(entry, enabled) => void upsertEntry(entry, { enabled })}
          onRunEntry={(entry) => void runSchedule(entry)}
          onEditEntry={openEditSchedule}
          onDeleteEntry={removeSchedule}
          onNewEntry={openNewSchedule}
          onGoMakeup={() => setTab('makeup')}
          onRunTravel={() => void runTravel()}
          onSaveTravelTimes={commitTravelTimes}
          travelTimesValue={travelTimesValue}
          onTravelTimesChange={setTravelTimesDraft}
        />
      ) : tab === 'makeup' ? (
        <CheckinMakeupView
          auto={data.auto}
          saving={savingSchedule}
          defaultRetryTime={data.auto?.defaultRetryTime || '00:10'}
          onToggleMakeup={(entry, enabled) =>
            void upsertEntry(entry, {
              retryTime: enabled ? data.auto?.defaultRetryTime || '00:10' : '',
            })
          }
          onSaveMakeup={(entry, changes) => void upsertEntry(entry, changes)}
          onSaveRetry={(entry, changes) => void upsertEntry(entry, changes)}
          onEditSchedule={(entry) => {
            openEditSchedule(entry);
            setTab('schedule');
          }}
          onGoSchedule={() => setTab('schedule')}
        />
      ) : isSection ? (
        <>
          {/* 总览指标（A2A `CheckinStats` 四格） */}
          <div className="ck-stats">
            <div className="ck-stat">
              <CalendarCheck size={16} className="ck-stat-icon" />
              <div className="ck-stat-body">
                <div className="ck-stat-value">
                  {totals.done}/{totals.total}
                </div>
                <div className="ck-stat-label">{t('今日签到')}</div>
                <div className="ck-progress" role="progressbar" aria-valuenow={totals.done} aria-valuemax={totals.total}>
                  <span
                    className="ck-progress-fill"
                    style={{
                      width: totals.total > 0 ? `${Math.min(100, (totals.done / totals.total) * 100)}%` : '0%',
                    }}
                  />
                </div>
                <div className="ck-time-subtle">
                  {outOfScopeCount > 0
                    ? t('另有 {{n}} 个账号不参与', { n: outOfScopeCount })
                    : t('全部账号均可签到')}
                </div>
              </div>
            </div>
            <button type="button" className="ck-stat ck-stat-link" onClick={() => setTab('schedule')}>
              <div className="ck-stat-body">
                <div className="ck-stat-value">
                  {enabledSchedules > 0 ? enabledSchedules : '—'}
                  {enabledSchedules > 0 ? ` / ${schedules.length}` : ''}
                </div>
                <div className="ck-stat-label">{t('定时签到')}</div>
                <div className="ck-time-subtle">
                  {data.auto?.enabled !== true
                    ? t('总开关已关闭')
                    : nextScheduleAt
                      ? t('下次 {{time}}', { time: formatTime(nextScheduleAt) })
                      : t('暂无启用的条目')}
                </div>
              </div>
            </button>
            <div className="ck-stat">
              <div className="ck-stat-body">
                <div className="ck-stat-value">
                  {(data.extras?.onboarding ?? []).length > 0
                    ? onboardingChecked > 0
                      ? onboardingPending
                      : '—'
                    : '无'}
                </div>
                <div className="ck-stat-label">{t('待领新手福利')}</div>
                <div className="ck-time-subtle">
                  {(data.extras?.onboarding ?? []).length > 0
                    ? t('已查 {{checked}} / {{total}} 个账号', {
                        checked: onboardingChecked,
                        total: (data.extras?.onboarding ?? []).length,
                      })
                    : t('没有支持新手福利的账号')}
                </div>
              </div>
            </div>
            <button type="button" className="ck-stat ck-stat-link" onClick={() => setTab('records')}>
              <div className="ck-stat-body">
                <div className="ck-stat-value">{data.recordsTotal ?? 0}</div>
                <div className="ck-stat-label">{t('累计签到记录')}</div>
                <div className="ck-time-subtle">{t('每账号一条明细')}</div>
              </div>
            </button>
          </div>

          <CheckinCenterSections
            section={tab}
            center={data}
            busyKey={busyKey}
            onSignOne={(accountId) => void runOne(accountId)}
            onRunAll={() => void runAll()}
            onOpenBenefits={() => setTab('benefits')}
            onReload={() => void load()}
          />

          {/* 签到后的自动链进度（AIGX 独有） */}
          {autoChainProgress && (
            <div className="ck-chain-progress">
              <div className="ck-chain-head">
                <span className="ck-chain-label">
                  {t('自动链')}
                  {autoChainProgress.stopped ? t('（已停止）') : ''}
                  {autoChainProgress.action
                    ? `：${autoChainProgress.action}`
                    : autoChainProgress.done === autoChainProgress.total
                      ? t('：全部完成')
                      : ''}
                </span>
                <span className="ck-chain-count">
                  {autoChainProgress.done}/{autoChainProgress.total}
                </span>
              </div>
              <div className="ck-progress">
                <span
                  className="ck-progress-fill"
                  style={{
                    width:
                      autoChainProgress.total > 0
                        ? `${Math.min(100, (autoChainProgress.done / autoChainProgress.total) * 100)}%`
                        : '0%',
                  }}
                />
              </div>
              {autoChaining ? <span className="form-hint">{t('正在逐个执行一次性福利…')}</span> : null}
            </div>
          )}

          {/* AIGX 独有：活动福利卡（按账号逐张读；活动不进每日调度） */}
          {providers.length > 0 && (
            <CheckinCampaignCard
              accountId={providers[0].accounts[0]?.id ?? ''}
              accountName={providers[0].accounts[0]?.name ?? ''}
              provider={providers[0].id}
              accounts={providers.flatMap((group) =>
                group.accounts.map((account) => ({ id: account.id, name: account.name || account.id })),
              )}
            />
          )}

          {/* AIGX 独有：逐账号福利预览 + 轮次时间线 */}
          <CheckinExtrasPreview center={data} />
          <CheckinHistoryTimeline rows={data.history ?? []} />

          <p className="form-hint">
            {t('支持的上游 {{n}} 家：{{list}}', { n: supported.length, list: supported.join('、') })}
          </p>
        </>
      ) : null}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
