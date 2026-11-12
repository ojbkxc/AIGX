import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CalendarCheck, ChevronRight, Play, Plus, RefreshCw, Trash2, X, Columns3 } from 'lucide-react';
import { api } from '../api';
import { providerIcon } from '../lib/providerIcons';
import { useToast } from '../components/Toast';
import { Badge, Card, EmptyState, Loading, Switch } from '../components/ui';
import {
  applyTableColumns,
  readTableColumns,
  resetTableColumns,
  writeTableColumns,
  type TableColumnDef,
  type TableColumnItem,
  type TableColumnState,
} from '../lib/tableColumns';
import TableColumnSettings from '../components/ui/TableColumnSettings';
import { Tooltip } from '../components/ui';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import BenefitsPanel from '../components/BenefitsPanel';
import { useServerPaging, SERVER_PAGE_SIZES } from '../components/ui/TableShell';
import type { CheckinCenterData, CheckinRecord, CheckinRunResult, CheckinSchedule } from '../types';
import './CheckinCenter.css';

type TabKey = 'center' | 'schedule' | 'records' | 'benefits';

const TAB_STORAGE_KEY = 'aigx-checkin-tab';

const OUTCOME_TONE: Record<string, 'success' | 'info' | 'warning' | 'danger'> = {
  success: 'success',
  already: 'info',
  pending: 'warning',
  failed: 'danger',
  captcha: 'warning',
};

const OUTCOME_LABEL: Record<string, string> = {
  success: '成功领取',
  already: '今日已领',
  pending: '待重试',
  failed: '失败',
  // 1.0.49 契约新增：上游要求人机验证（is_ok=false，需管理员介入）
  captcha: '需人机验证',
};

/** 台账「类型」列（与 source 同值双写：manual | schedule | makeup | retry） */
const KIND_LABEL: Record<string, string> = {
  manual: '手动',
  schedule: '定时',
  makeup: '补签',
  retry: '重试',
};

const SOURCE_LABEL: Record<string, string> = {
  schedule: '定时',
  manual: '手动',
  startup: '补签',
  makeup: '补签',
  single: '单签',
  activity: '活动',
};

/** 签到时区偏移候选（小时）。0 = 跟随系统本地时区（语义见后端 store.rs：
 *  beijing() 只在 0 时用本地时区，其余按固定偏移执行——缺省 8 = UTC+8） */
const TZ_OFFSETS = [-12, -11, -10, -9, -8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

const HHMM_RE = /^([01]\d|2[0-3]):[0-5]\d$/;

/** 起止日期（<input type="date"> 的 "YYYY-MM-DD"）→ unix 秒。
 *  起始取当日 00:00:00、结束取当日 23:59:59（含当天，与后端闭区间 [start,end] 对齐）。 */
const dateToStartSec = (d: string): number | undefined => {
  if (!d) return undefined;
  const ms = new Date(`${d}T00:00:00`).getTime();
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : undefined;
};
const dateToEndSec = (d: string): number | undefined => {
  if (!d) return undefined;
  const ms = new Date(`${d}T23:59:59`).getTime();
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : undefined;
};

/**
 * 各家上游的签到链路说明（对齐 Agent2API 提供商行的一句话说明）。
 * 只做静态展示，不参与任何逻辑。
 */
const PROVIDER_DESC: Record<string, string> = {
  loomy: '讯飞 Loomy：每日领取会员权益，签到即领',
  kuku: 'Kuku：每日签到领积分，签到即领',
  qoder: 'Qoder：每日签到领额度，签到即领',
  'qoder-intl': 'Qoder 国际版：签到领额度；风控组件 UMID 见福利通道',
  xiaohuanxiong: '小浣熊：桌面端 / 移动端各领一次登录积分',
  autoclaw: 'AutoClaw：每日签到领额度，签到即领',
  'autoclaw-intl': 'AutoClaw 国际版：每日签到领额度，签到即领',
  workbuddy: 'WorkBuddy：每日签到领额度，签到即领',
  'workbuddy-intl': 'WorkBuddy 国际版：签到领日活奖励；保活链见福利通道',
  trae: 'Trae：每日签到领额度，签到即领',
  codearts: 'CodeArts：每日签到领额度；新人礼见福利通道',
  officeace: 'OfficeAce：每日签到与新人礼同链，签到即领',
};

/**
 * 签到后的自动链（对齐 Agent2API「签到后自动领福利」）。
 * 各家签到之外还有一次性/限时福利可领：codearts 新人礼、loomy 新手任务、
 * 小浣熊登录积分、workbuddy-intl 保活+领取 —— 签到成功即串行跑一遍，
 * 不用管理员再切到「福利」页逐个点。
 */
const AUTO_CHAIN: Record<string, string[]> = {
  codearts: ['newbie'],
  loomy: ['onboarding', 'onboarding_claim'],
  xiaohuanxiong: ['onboarding'],
  'workbuddy-intl': ['keepalive_claim'],
};

interface ScheduleForm {
  id: string;
  label: string;
  enabled: boolean;
  time: string;
  providers: string[];
  /** 逐家时刻覆盖（仅非空项提交；空 = 回落内置专属/条目时刻） */
  providerTimes: Record<string, string>;
  /* ── 1.0.49 契约字段（GET/POST schedules 均带同名 5 字段）── */
  /** 随机签到区间结束 HH:MM（空 = 无窗口，仅按 time 固定执行） */
  timeEnd: string;
  /** true = 每个账号在 [time, timeEnd] 内取确定性随机时刻 */
  randomize: boolean;
  /** 每账号每天重试上限（0 = 关闭重试；缺省 3，与后端 default_retry_limit 一致） */
  retryLimit: string;
  /** 重试间隔（分钟；<=0 回落全局 retry_gap_ms；缺省 5） */
  retryGapMinutes: string;
  /** 补签轮时刻 HH:MM（空 = 不跑补签轮） */
  makeupTime: string;
}

const EMPTY_SCHEDULE: ScheduleForm = {
  id: '',
  label: '每日签到',
  enabled: true,
  time: '09:00',
  providers: [],
  providerTimes: {},
  timeEnd: '',
  randomize: false,
  retryLimit: '3',
  retryGapMinutes: '5',
  makeupTime: '',
};

/* 「下次执行 / 今日已执行」的时区口径已抽到 src/lib/checkinTz.ts（纯函数 +
 * 边界值单测 checkinTz.test.ts）：按 settings.timezone_offset 计算（缺省 8
 * = UTC+8；0 = 跟随系统本地时区），与后端执行口径一致——此前硬编码
 * Asia/Shanghai，设置 offset=0 后展示与执行脱钩（#21 补做项）。 */
import { localDateKey, nextRunLabel } from '../lib/checkinTz';


const LS_TCOLS = 'aigx-tbl-cols.checkin-records';
/** 签到记录表列声明（时间/说明锁定） */
const CK_REC_COL_DEFS: TableColumnDef[] = [
  { key: 'time', label: '时间', width: 150, locked: true },
  { key: 'account', label: '账号', width: 180 },
  { key: 'provider', label: '上游', width: 110 },
  { key: 'source', label: '来源', width: 90 },
  // 1.0.49 契约：kind 与 source 同值双写，单列展示记录类型
  { key: 'kind', label: '类型', width: 80 },
  { key: 'result', label: '结果', width: 100 },
  { key: 'note', label: '说明', width: 260 },
  // 1.0.49 契约：失败原文（成功 / 今日已领时为空）
  { key: 'error', label: '失败原因', width: 260 },
];

interface CheckinCenterProps {
  /** 作为供应商页「签到」子页内嵌时隐藏自身标题区（页内 4 个二级 Tab 保留） */
  embedded?: boolean;
}

export default function CheckinCenter({ embedded = false }: CheckinCenterProps = {}): JSX.Element {
  // 签到记录表列设置（公共层；分页为服务端 offset/limit 原口径不动）
  const [recCols, setRecCols] = useState<TableColumnState>(() => readTableColumns(LS_TCOLS, CK_REC_COL_DEFS));
  const [recPanelOpen, setRecPanelOpen] = useState(false);
  const recPanelRef = useRef<HTMLDivElement | null>(null);
  const commitRecCols = useCallback((next: TableColumnItem[], persist: boolean) => {
    setRecCols((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);
  const resetRecCols = useCallback(() => setRecCols(resetTableColumns(LS_TCOLS, CK_REC_COL_DEFS)), []);
  const appliedRecCols = useMemo(() => applyTableColumns(recCols, CK_REC_COL_DEFS), [recCols]);
  const recKeySet = useMemo(() => new Set(appliedRecCols.map((c) => c.key)), [appliedRecCols]);
  const { t } = useTranslation();
  const addToast = useToast();

  const [tab, setTab] = useState<TabKey>(() => {
    const saved = localStorage.getItem(TAB_STORAGE_KEY);
    return saved === 'schedule' || saved === 'records' || saved === 'benefits' ? saved : 'center';
  });
  const [data, setData] = useState<CheckinCenterData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  // 单飞：一次只允许一个签到动作在途（上游有全局 running 防重），
  // 但按钮级 busy 只落在对应行上，不让一颗在跑拖灰整页
  const [busyKey, setBusyKey] = useState<string | null>(null);
  // 签到后的自动链在途：按钮禁用并显示进度，避免与手动点福利打架
  const [autoChaining, setAutoChaining] = useState(false);
  /** 自动链逐个动作进度（done=已完成数，action=当前动作名） */
  const [autoChainProgress, setAutoChainProgress] = useState<null | {
    total: number;
    done: number;
    action: string;
    stopped: boolean;
  }>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  // 定时计划：行内编辑（editingId='new' 显示首行新建表单）
  const [editingId, setEditingId] = useState<string | null>(null);
  const [scheduleForm, setScheduleForm] = useState<ScheduleForm>(EMPTY_SCHEDULE);
  const [savingSchedule, setSavingSchedule] = useState(false);

  // 账号间隔（gap_ms）编辑：独立于 data 快照，避免整卡片重渲染
  const [gapMsEdit, setGapMsEdit] = useState('');
  const [gapSaving, setGapSaving] = useState(false);
  // 失败/窗口重试间隔（分钟）与台账保留天数（后端 retry_gap_ms 毫秒 / keep_days 天）
  const [retryMinEdit, setRetryMinEdit] = useState('');
  const [keepDaysEdit, setKeepDaysEdit] = useState('');
  const [advSaving, setAdvSaving] = useState(false);

  // 记录
  const [records, setRecords] = useState<CheckinRecord[]>([]);
  const [recordsLoading, setRecordsLoading] = useState(false);
  const [recordDays, setRecordDays] = useState(7);
  const [recordOutcome, setRecordOutcome] = useState('');
  const [recordProvider, setRecordProvider] = useState('');
  // 1.0.49 契约：类型（kind）与起止日期（→ unix 秒 start/end）
  const [recordKind, setRecordKind] = useState('');
  const [recordStart, setRecordStart] = useState('');
  const [recordEnd, setRecordEnd] = useState('');
  const [keyword, setKeyword] = useState('');
  // 关键词走后端过滤（与分页同口径）；kwApplied 是「已提交」的关键词
  const [kwApplied, setKwApplied] = useState('');
  // 分页（服务端真分页，0-based 后端口径由 hook 的 1-based 页码换算）：
  // 公共 hook 统一承担越界钳位 / size 变更回第 1 页 / 筛选变化回第 1 页 + 档位记忆。
  const { page, setPage, pageSize, size, setSize, total, setTotal, totalPages } = useServerPaging({
    key: 'aigx-tbl-pagesize.checkin',
    sizes: SERVER_PAGE_SIZES,
    fallback: 50,
    resetDeps: [recordDays, recordProvider, recordOutcome, recordKind, recordStart, recordEnd, kwApplied],
  });

  const load = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getCheckinCenter();
      const d = res?.data ?? null;
      setData(d);
      if (d?.settings) {
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

  const loadRecords = useCallback(async () => {
    setRecordsLoading(true);
    // 日期字符串 → unix 秒（闭区间：起 00:00:00 / 止 23:59:59）
    const dateStart = dateToStartSec(recordStart);
    const dateEnd = dateToEndSec(recordEnd);
    try {
      const res = await api.getCheckinRecords({
        // 起止日期生效时把 days 放开到「全部」以区间为准——否则 days 的默认 7 天
        // 会与区间取交集，选 30 天前~10 天前也会被裁成 7 天内
        days: dateStart != null || dateEnd != null ? 3650 : recordDays,
        provider: recordProvider || undefined,
        outcome: recordOutcome || undefined,
        kind: recordKind || undefined,
        start: dateStart,
        end: dateEnd,
        keyword: kwApplied || undefined,
        offset: (page - 1) * size,
        limit: size,
      });
      setRecords(res?.data?.records ?? []);
      setTotal(res?.data?.total ?? 0);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setRecordsLoading(false);
    }
  }, [recordDays, recordProvider, recordOutcome, recordKind, recordStart, recordEnd, kwApplied, page, size, setTotal, addToast]);

  useEffect(() => {
    if (tab === 'records') void loadRecords();
  }, [tab, loadRecords]);

  /**
   * 签到后的自动链：把签到结果里各家的一次性/限时福利串行跑一遍。
   * 间隔 300ms 让上游看清「逐个执行」的画像；单个动作失败即停（不拖垮后续）。
   * keepalive_claim 成功后回写台账（保活标记），让「保活」在记录里可见。
   */
  const autoChain = async (results: CheckinRunResult[]): Promise<void> => {
    const chain = results
      .filter((r) => r.success && AUTO_CHAIN[r.provider])
      .flatMap((r) => (AUTO_CHAIN[r.provider] ?? []).map((action) => ({ r, action })));
    if (chain.length === 0) return;
    setAutoChaining(true);
    setAutoChainProgress({ total: chain.length, done: 0, action: '', stopped: false });
    try {
      for (let i = 0; i < chain.length; i++) {
        const { r, action } = chain[i];
        setAutoChainProgress({ total: chain.length, done: i, action, stopped: false });
        const res = await api.runCheckinBenefit(r.account_id, action);
        const result = res?.data;
        if (result && !result.ok) {
          // 失败即停：标记停步原因，进度停在失败动作
          setAutoChainProgress({ total: chain.length, done: i + 1, action, stopped: true });
          break;
        }
        // workbuddy-intl 保活+领取成功后回写台账（保活标记）
        if (action === 'keepalive_claim' && result?.ok) {
          try {
            await api.markKeptActive(r.account_id, r.provider);
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
  };

  /** 保存账号间隔：校验非负数字后整份 settings 回写（与启用开关同接口） */
  const saveGapMs = async (): Promise<void> => {
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
  };

  /** 保存重试间隔（分钟→毫秒）与台账保留天数（0 = 永久）；两字段一次回写 */
  const saveAdvanced = async (): Promise<void> => {
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
  };

  /** 切换签到时区（小时偏移；0 = 跟随系统本地时区）。改完即存，下次执行生效。 */
  const changeTimezone = async (offset: number): Promise<void> => {
    if (!data) return;
    try {
      await api.updateCheckinSettings({ ...data.settings, timezone_offset: offset });
      addToast(t('时区已保存（对下一次计划执行生效）'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  /* ── 旅行/成长巡检（be-fix #16-B 契约；data.travel 缺省 = 不渲染该卡）── */
  const travel = data?.travel;
  const [travelTimesDraft, setTravelTimesDraft] = useState<string | null>(null);
  const travelTimesValue = travelTimesDraft ?? (travel?.times ?? []).join('\n');

  const saveTravel = async (body: { enabled?: boolean; times?: string[] }): Promise<void> => {
    try {
      await api.updateCheckinTravel(body);
      addToast(t('巡检设置已保存'));
      setTravelTimesDraft(null);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  const commitTravelTimes = (): void => {
    const raw = travelTimesValue.split('\n').map((s) => s.trim()).filter(Boolean);
    if (raw.some((s) => !HHMM_RE.test(s))) {
      addToast(t('巡检时刻格式应为 HH:MM（每行一个）'), 'error');
      return;
    }
    void saveTravel({ times: raw });
  };

  const runTravel = async (): Promise<void> => {
    try {
      const res = await api.runCheckinTravelNow();
      const d = res?.data;
      addToast(`${t('巡检完成')}${d?.summary ? `：${d.summary}` : ''}`);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  const runAll = async (providers: string[] = [], key = 'all') => {
    setBusyKey(key);
    try {
      const res = await api.runCheckin({ providers });
      const d = res?.data;
      if (d) {
        addToast(`${t('签到完成')}：${d.done}/${d.total}`);
        void autoChain(d.results);
      }
      await load();
      if (tab === 'records') await loadRecords();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyKey(null);
    }
  };

  const runOne = async (accountId: string) => {
    setBusyKey(accountId);
    try {
      const res = await api.runCheckin({ account_ids: [accountId] });
      const r = res?.data?.results?.[0];
      if (r) {
        addToast(`${r.account_name}：${r.message}`, r.success ? 'success' : 'error');
        if (res.data) void autoChain(res.data.results);
      }
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyKey(null);
    }
  };

  const toggleSetting = async (enabled: boolean) => {
    if (!data) return;
    try {
      await api.updateCheckinSettings({ ...data.settings, enabled });
      await load();
      addToast(enabled ? t('已开启定时签到') : t('已关闭定时签到'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  /** 定时条目行内开关：直接 PATCH 同 id 其余字段不动 */
  const toggleSchedule = async (schedule: CheckinSchedule, enabled: boolean) => {
    try {
      await api.upsertCheckinSchedule({
        id: schedule.id,
        label: schedule.label,
        enabled,
        time: schedule.time,
        providers: schedule.providers,
      });
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  const openNewSchedule = () => {
    setScheduleForm({ ...EMPTY_SCHEDULE });
    setEditingId('new');
  };

  const openEditSchedule = (schedule: CheckinSchedule) => {
    setScheduleForm({
      id: schedule.id,
      label: schedule.label,
      enabled: schedule.enabled,
      time: schedule.time,
      providers: schedule.providers,
      providerTimes: { ...(schedule.provider_times ?? {}) },
      timeEnd: schedule.time_end ?? '',
      randomize: schedule.randomize ?? false,
      retryGapMinutes: String(schedule.retry_gap_minutes ?? 5),
      retryLimit: String(schedule.retry_limit ?? 3),
      makeupTime: schedule.makeup_time ?? '',
    });
    setEditingId(schedule.id);
  };

  const saveSchedule = async () => {
    // 逐家覆盖校验：填了就必须是合法 HH:MM（空串 = 撤销覆盖）
    for (const [p, v] of Object.entries(scheduleForm.providerTimes)) {
      if (v && !HHMM_RE.test(v)) {
        addToast(t('{{provider}} 的覆盖时刻格式应为 HH:MM', { provider: p }), 'error');
        return;
      }
    }
    // 校验：区间结束 / 补签时刻须为合法 HH:MM；开区间随机时结束须晚于开始（后端 400 前置拦截）
    if (scheduleForm.timeEnd && !HHMM_RE.test(scheduleForm.timeEnd)) {
      addToast(t('区间结束时刻格式应为 HH:MM'), 'error');
      return;
    }
    if (scheduleForm.makeupTime && !HHMM_RE.test(scheduleForm.makeupTime)) {
      addToast(t('补签时刻格式应为 HH:MM'), 'error');
      return;
    }
    if (
      scheduleForm.randomize &&
      (!scheduleForm.timeEnd || scheduleForm.timeEnd <= scheduleForm.time)
    ) {
      addToast(t('开启区间随机时，结束时刻须晚于开始时刻'), 'error');
      return;
    }
    // 契约已冻结（1.0.49）：5 字段一律提交；旧后端虽有 serde(default) 会忽略未知字段，
    // 但前后端同版本发布，不再做 feature-detect（否则「零条目」时新建表单会缺控件）。
    const providerTimes = Object.fromEntries(
      Object.entries(scheduleForm.providerTimes).filter(([, v]) => v),
    );
    setSavingSchedule(true);
    try {
      await api.upsertCheckinSchedule({
        id: scheduleForm.id,
        label: scheduleForm.label,
        enabled: scheduleForm.enabled,
        time: scheduleForm.time,
        providers: scheduleForm.providers,
        provider_times: providerTimes,
        time_end: scheduleForm.timeEnd,
        randomize: scheduleForm.randomize,
        retry_limit: Number(scheduleForm.retryLimit) || 0,
        retry_gap_minutes: Number(scheduleForm.retryGapMinutes) || 0,
        makeup_time: scheduleForm.makeupTime,
      });
      addToast(t('计划已保存'));
      setEditingId(null);
      setScheduleForm({ ...EMPTY_SCHEDULE });
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSavingSchedule(false);
    }
  };

  const removeSchedule = (schedule: CheckinSchedule) => {
    setConfirmState({
      title: t('删除签到计划'),
      message: `${schedule.label}（${schedule.time}）`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        await api.removeCheckinSchedule(schedule.id);
        addToast(t('已删除'));
        await load();
      },
    });
  };

  const runSchedule = async (schedule: CheckinSchedule) => {
    setBusyKey(`sched:${schedule.id}`);
    try {
      const res = await api.runCheckinSchedule(schedule.id);
      const d = res?.data;
      addToast(
        `${schedule.label}：${d?.summary ?? ''}` +
          (d ? `（成功 ${d.ok}·保活 ${d.keepalive}·失败 ${d.failed}）` : ''),
      );
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyKey(null);
    }
  };

  const clearRecords = () => {
    setConfirmState({
      title: t('清空签到记录'),
      message: t('将删除最近 {{days}} 天的全部签到台账（不受当前筛选限制），不可恢复。', {
        days: recordDays,
      }),
      confirmText: t('清空'),
      danger: true,
      onConfirm: async () => {
        const res = await api.clearCheckinRecords(recordDays);
        addToast(`${t('已删除')} ${res?.data?.removed ?? 0} ${t('条记录')}`);
        await loadRecords();
      },
    });
  };

  const providers = useMemo(() => data?.providers ?? [], [data?.providers]);
  const supported = useMemo(() => data?.supported_providers ?? [], [data?.supported_providers]);
  /** 1.0.49 契约：实现未进主仓的三家（置灰 + 「插件仓」标注）；旧后端无此字段则为空集 */
  const unsupported = useMemo(() => data?.unsupported_providers ?? [], [data?.unsupported_providers]);
  const unsupportedSet = useMemo(() => new Set(unsupported), [unsupported]);

  const totals = useMemo(() => {
    let done = 0;
    let total = 0;
    for (const p of providers) {
      done += p.today_done;
      total += p.today_total;
    }
    return { done, total, accounts: providers.reduce((n, p) => n + p.accounts.length, 0) };
  }, [providers]);

  const hasRecordFilter = !!kwApplied || !!recordOutcome || !!recordProvider || !!recordKind || !!recordStart || !!recordEnd;

  /* 1.0.49 契约字段已冻结：编辑器控件无条件渲染（旧后端 serde(default) 忽略未知字段） */
  const builtinTimes = data?.builtin_times;

  /** 生效时刻三档回落：逐家覆盖 > 内置专属 > 条目时刻（对齐上游 effectiveScheduleTime） */
  const effectiveTime = useCallback(
    (provider: string, form: ScheduleForm): string =>
      form.providerTimes[provider] || builtinTimes?.[provider] || form.time,
    [builtinTimes],
  );

  /** 实际签到安排预览：按生效时刻分组（对齐上游「实际签到安排」侧栏） */
  const schedulePreview = useMemo(() => {
    if (!scheduleForm.providers.length) return [];
    const groups = new Map<string, string[]>();
    for (const p of scheduleForm.providers) {
      const t = effectiveTime(p, scheduleForm);
      groups.set(t, [...(groups.get(t) ?? []), p]);
    }
    return [...groups.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [scheduleForm, effectiveTime]);

  /** 顶栏徽标（对齐 Agent2API #checkin-badge）：日期比对按配置时区 */
  const autoBadge = useMemo(() => {
    if (!data) return null;
    const enabled = data.settings.enabled;
    const tz = data.settings.timezone_offset ?? 8;
    const firedToday = (data.schedules ?? []).some(
      (s) => s.enabled && s.last_fired_date === localDateKey(tz),
    );
    if (!enabled) {
      return (
        <span className="ck-tab-badge is-off">{t('定时已关闭')}</span>
      );
    }
    return (
      <span className={`ck-tab-badge ${firedToday ? 'is-ok' : 'is-on'}`}>
        {firedToday ? t('已开启 · 今日已执行') : t('定时已开启')}
      </span>
    );
  }, [data, t]);

  const toggleGroup = (provider: string): void => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(provider)) next.delete(provider);
      else next.add(provider);
      return next;
    });
  };

  const tabs = [
    { key: 'center' as TabKey, label: t('签到中心') },
    { key: 'schedule' as TabKey, label: t('定时签到') },
    { key: 'records' as TabKey, label: t('签到记录') },
    { key: 'benefits' as TabKey, label: t('福利通道') },
  ];

  /** 提供商行徽章（对齐 Agent2API GroupBadge） */
  const groupBadge = (done: number, total: number): JSX.Element => {
    if (total === 0) return <Badge tone="neutral">{t('无账号')}</Badge>;
    if (done === total) return <Badge tone="success">{t('已签')}</Badge>;
    if (done > 0) return <Badge tone="warning">{t('部分')}</Badge>;
    return <Badge tone="info">{t('待签')}</Badge>;
  };

  const scheduleEditor = (
    <div className="ck-editor">
      <div className="ck-editor-grid">
        <label className="form-group">
          <span className="ck-editor-label">{t('名称')}</span>
          <input
            className="form-input"
            value={scheduleForm.label}
            onChange={(e) => setScheduleForm({ ...scheduleForm, label: e.target.value })}
          />
        </label>
        <label className="form-group">
          <span className="ck-editor-label">{t('每天时刻（按下方时区·错过自动补签）')}</span>
          <input
            type="time"
            className="form-input ck-time-input"
            value={scheduleForm.time}
            onChange={(e) => setScheduleForm({ ...scheduleForm, time: e.target.value })}
          />
        </label>
        <label className="ck-editor-enabled">
          <Switch
            checked={scheduleForm.enabled}
            onChange={(next) => setScheduleForm({ ...scheduleForm, enabled: next })}
            ariaLabel={t('启用')}
          />
          <span>{t('启用')}</span>
        </label>
      </div>
      <div className="form-group">
        <div className="ck-scope-head">
          <span className="ck-editor-label">{t('签到范围（可逐家覆盖时刻）')}</span>
          <span className="ck-scope-tools">
            <button
              type="button"
              onClick={() =>
                setScheduleForm({
                  ...scheduleForm,
                  // 全选跳过未接入主仓的家（置灰项不可选）
                  providers: supported.filter((p) => !unsupportedSet.has(p)),
                })
              }
            >
              {t('全选')}
            </button>
            <button
              type="button"
              onClick={() => setScheduleForm({ ...scheduleForm, providers: [] })}
            >
              {t('清空')}
            </button>
          </span>
        </div>
        <div className="ck-provider-picks">
          {supported.map((provider) => {
            const checked = scheduleForm.providers.includes(provider);
            const override = scheduleForm.providerTimes[provider] ?? '';
            const eff = effectiveTime(provider, scheduleForm);
            const isBuiltin = !override && Boolean(builtinTimes?.[provider]);
            // 1.0.49 契约：实现随上游搬进闭源插件仓的三家 → 置灰 + 说明，不可选
            const unsupported = unsupportedSet.has(provider);
            return (
              <div
                key={provider}
                className={`ck-provider-pick ck-provider-pick-row${checked ? ' is-checked' : ''}${unsupported ? ' is-unsupported' : ''}`}
              >
                <label className="ck-provider-pick-name">
                  <input
                    type="checkbox"
                    checked={checked}
                    disabled={unsupported}
                    onChange={(e) => {
                      const next = e.target.checked
                        ? [...scheduleForm.providers, provider]
                        : scheduleForm.providers.filter((p) => p !== provider);
                      const times = { ...scheduleForm.providerTimes };
                      if (!e.target.checked) delete times[provider];
                      setScheduleForm({ ...scheduleForm, providers: next, providerTimes: times });
                    }}
                  />
                  {provider}
                  {isBuiltin && <span className="ck-pick-tag">{t('平台专属')}</span>}
                  {unsupported && (
                    <Tooltip label={t('实现随上游进入插件仓，主仓未接入')}>
                      <span className="ck-pick-tag ck-pick-tag-unsupported">
                        {t('插件仓')}
                      </span>
                    </Tooltip>
                  )}
                </label>
                {checked && !unsupported && (
                  <span className="ck-pick-time">
                    <input
                      type="time"
                      className="form-input ck-time-input"
                      aria-label={t('{{provider}} 签到时刻', { provider })}
                      value={override || eff}
                      onChange={(e) =>
                        setScheduleForm({
                          ...scheduleForm,
                          providerTimes: { ...scheduleForm.providerTimes, [provider]: e.target.value },
                        })
                      }
                    />
                    {override ? (
                      <button
                        type="button"
                        className="ck-pick-reset"
                        onClick={() =>
                          setScheduleForm(() => {
                            const times = { ...scheduleForm.providerTimes };
                            delete times[provider];
                            return { ...scheduleForm, providerTimes: times };
                          })
                        }
                      >
                        {t('恢复默认')}
                      </button>
                    ) : (
                      <span className="ck-pick-tag ck-pick-tag-mute">
                        {isBuiltin ? t('内置时刻') : t('跟随默认')}
                      </span>
                    )}
                  </span>
                )}
              </div>
            );
          })}
        </div>
        {scheduleForm.providers.length === 0 && (
          <span className="form-hint">{t('不选 = 全部支持的上游')}</span>
        )}
      </div>

      {/* 实际签到安排预览（对齐上游编辑器右侧栏）：按生效时刻分组 */}
      {scheduleForm.providers.length > 0 && (
        <div className="form-group">
          <span className="ck-editor-label">{t('实际签到安排（按生效时刻分组）')}</span>
          <div className="ck-preview-groups">
            {schedulePreview.map(([time, list]) => (
              <div key={time} className="ck-preview-group">
                <code>{time}</code>
                <span>{list.join('、')}</span>
                <Badge tone="neutral">{t('{{n}} 家', { n: list.length })}</Badge>
              </div>
            ))}
          </div>
          <span className="form-hint">
            {t('逐家覆盖或内置专属时刻优先生效；未覆盖的按条目时刻执行。')}
          </span>
        </div>
      )}

      {/* 随机签到区间：time_end + randomize（1.0.49 契约字段） */}
      <div className="form-group">
        <span className="ck-editor-label">{t('随机签到区间（可选）')}</span>
        <div className="ck-editor-grid">
          <label className="form-group">
            <span className="ck-editor-label ck-label-with-tip">
              {t('区间结束时刻')}
              <Tooltip
                label={t(
                  '窗口结束时刻（HH:MM）。开启随机后，每个账号在 [开始, 结束] 内取确定性随机时刻（同账号同日同刻、重启不漂）；留空 = 仅按开始时刻执行。内置专属时刻（如 qoder 10:01）与逐家覆盖的家不参与随机。',
                )}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>?</span>
              </Tooltip>
            </span>
            <input
              type="time"
              className="form-input ck-time-input"
              aria-label={t('区间结束时刻')}
              value={scheduleForm.timeEnd}
              onChange={(e) => setScheduleForm({ ...scheduleForm, timeEnd: e.target.value })}
            />
          </label>
          <label className="ck-editor-enabled ck-random-toggle">
            <Switch
              checked={scheduleForm.randomize}
              onChange={(next) => setScheduleForm({ ...scheduleForm, randomize: next })}
              ariaLabel={t('区间内随机')}
            />
            <span className="ck-label-with-tip">
              {t('区间内随机')}
              <Tooltip
                label={t(
                  '开启后每个账号在 [开始, 结束] 区间内随机取一个时刻执行，避免同刻集中触发上游风控；关闭 = 所有账号按开始时刻执行。',
                )}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>?</span>
              </Tooltip>
            </span>
          </label>
        </div>
        <span className="form-hint">
          {t('开启随机时结束时刻必须晚于开始时刻；留空结束时刻 = 固定时刻执行。')}
        </span>
      </div>

      {/* 逐条目重试 + 补签轮：retry_limit / retry_gap_minutes / makeup_time（1.0.49 契约字段） */}
      <div className="form-group ck-retry-block">
        <span className="ck-editor-label">{t('补签与重试')}</span>
        <div className="ck-editor-grid">
          <label className="form-group">
            <span className="ck-editor-label ck-label-with-tip">
              {t('每账号每天最多重试（次）')}
              <Tooltip
                label={t('当天未签上时最多重试几次；0 = 关闭本条目重试，仅靠补签轮兜底。次数按账号每天单独计算、次日重置。')}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>?</span>
              </Tooltip>
            </span>
            <input
              type="number"
              min={0}
              max={20}
              className="form-input"
              value={scheduleForm.retryLimit}
              onChange={(e) => setScheduleForm({ ...scheduleForm, retryLimit: e.target.value })}
            />
          </label>
          <label className="form-group">
            <span className="ck-editor-label ck-label-with-tip">
              {t('重试间隔（分钟）')}
              <Tooltip
                label={t('每次失败重试之间等待多少分钟；填 0 = 回落全局「失败/窗口重试间隔」设置。')}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>?</span>
              </Tooltip>
            </span>
            <input
              type="number"
              min={0}
              max={120}
              className="form-input"
              value={scheduleForm.retryGapMinutes}
              onChange={(e) => setScheduleForm({ ...scheduleForm, retryGapMinutes: e.target.value })}
            />
          </label>
          <label className="form-group">
            <span className="ck-editor-label ck-label-with-tip">
              {t('补签时刻（留空 = 关闭）')}
              <Tooltip
                label={t(
                  '到该时刻挑当天本条目下仍未签上的账号再跑一次（类型记为「补签」），每天至多一轮；留空 = 不跑补签轮。',
                )}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>?</span>
              </Tooltip>
            </span>
            <input
              type="time"
              className="form-input ck-time-input"
              aria-label={t('补签时刻')}
              value={scheduleForm.makeupTime}
              onChange={(e) => setScheduleForm({ ...scheduleForm, makeupTime: e.target.value })}
            />
          </label>
        </div>
        <span className="form-hint">
          {t('重试与补签只处理当天未签上的账号；补签轮在指定时刻兜底一次。')}
        </span>
      </div>

      <div className="ck-form-actions">
        <button type="button" className="btn btn-outline btn-sm" onClick={() => setEditingId(null)}>
          {t('取消')}
        </button>
        <button
          type="button"
          className="btn btn-primary btn-sm"
          onClick={() => void saveSchedule()}
          disabled={savingSchedule}
        >
          {savingSchedule ? t('保存中...') : t('保存')}
        </button>
      </div>
    </div>
  );

  return (
    <div>
      {!embedded && (
        <div className="page-header">
          <div>
            <h1>{t('签到中心')}</h1>
            <p>
              {t(
                '替账号池里的上游账号每日领取它们自带的额度。与钱包里的「用户签到」是两套。'
              )}
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
            key={item.key}
            className={`sub-tab${tab === item.key ? ' active' : ''}`}
            onClick={() => setTab(item.key)}
          >
            {item.label}
          </button>
        ))}
        <span className="ck-tab-badge-slot">{autoBadge}</span>
      </div>

      {loading ? (
        <Loading text={t('加载签到中心')} />
      ) : tab === 'center' ? (
        <>
          <div className="ck-stats">
            <div className="ck-stat">
              <CalendarCheck size={16} className="ck-stat-icon" />
              <div className="ck-stat-body">
                <div className="ck-stat-value">
                  {totals.done}/{totals.total}
                </div>
                <div className="ck-stat-label">{t('今日完成')}</div>
                <div className="ck-progress" role="progressbar" aria-valuenow={totals.done} aria-valuemax={totals.total}>
                  <span
                    className="ck-progress-fill"
                    style={{
                      width: totals.total > 0 ? `${Math.min(100, (totals.done / totals.total) * 100)}%` : '0%',
                    }}
                  />
                </div>
              </div>
            </div>
            <div className="ck-stat">
              <div className="ck-stat-body">
                <div className="ck-stat-value">{totals.accounts}</div>
                <div className="ck-stat-label">{t('可签到账号')}</div>
              </div>
            </div>
            <div className="ck-stat">
              <div className="ck-stat-body">
                <div className="ck-stat-value">{supported.length}</div>
                <div className="ck-stat-label">{t('支持的上游')}</div>
              </div>
            </div>
            <button
              type="button"
              className="ck-stat ck-stat-link"
              onClick={() => setTab('records')}
            >
              <div className="ck-stat-body">
                <div className="ck-stat-value">{data?.records_total ?? 0}</div>
                <div className="ck-stat-label">{t('累计记录数')}</div>
              </div>
            </button>
            <button type="button" className="ck-stat ck-stat-link" onClick={() => setTab('schedule')}>
              <div className="ck-stat-body">
                <div className="ck-stat-value">
                  {(data?.schedules ?? []).filter((s) => s.enabled).length}/
                  {(data?.schedules ?? []).length}
                </div>
                <div className="ck-stat-label">{t('定时计划启用')}</div>
              </div>
            </button>
          </div>

          <Card
            title={t('每日签到')}
            actions={
              <button
                className="btn btn-primary btn-sm"
                onClick={() => void runAll()}
                disabled={busyKey !== null || autoChaining || totals.accounts === 0}
                style={{ gap: 6 }}
              >
                <Play size={12} />
                {busyKey === 'all' ? t('签到中...') : autoChaining ? t('自动链执行中...') : t('立即全部签到')}
              </button>
            }
          >
            {providers.length === 0 ? (
              <EmptyState
                icon="📅"
                message={t(
                  '账号池里还没有可签到的账号。支持的上游：' + supported.join('、')
                )}
              />
            ) : (
              <div className="ck-prov-list">                {providers.map((group) => {
                  const open = expanded.has(group.provider);
                  const icon = providerIcon(group.provider);
                  return (
                    <div key={group.provider} className="ck-prov-block">
                      <div
                        className={`ck-prov-row${open ? ' is-open' : ''}`}
                        role="button"
                        tabIndex={0}
                        aria-expanded={open}
                        onClick={() => toggleGroup(group.provider)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            e.preventDefault();
                            toggleGroup(group.provider);
                          }
                        }}
                      >
                        {icon ? (
                          <img className="ck-prov-logo" src={icon as string} alt="" />
                        ) : (
                          <span className="ck-prov-logo ck-prov-letter">
                            {(group.label || group.provider).slice(0, 1).toUpperCase()}
                          </span>
                        )}
                        <div className="ck-prov-main">
                          <span className="ck-prov-name">{group.label}</span>
                          <span className="ck-prov-sub">
                            {t('{{n}} 个账号', { n: group.accounts.length })} · {t('已签')} {group.today_done}
                          </span>
                          {PROVIDER_DESC[group.provider] && (
                            <span className="ck-prov-desc">{PROVIDER_DESC[group.provider]}</span>
                          )}
                        </div>
                        {groupBadge(group.today_done, group.today_total)}
                        <ChevronRight size={14} className={`ck-prov-caret${open ? ' is-open' : ''}`} />
                      </div>
                      {open && (
                        <div className="ck-prov-accounts">
                          <div className="ck-prov-acts">
                            <button
                              type="button"
                              className="btn btn-outline btn-sm"
                              onClick={(e) => {
                                e.stopPropagation();
                                void runAll([group.provider], `g:${group.provider}`);
                              }}
                              disabled={busyKey !== null}
                            >
                              <Play size={12} />{' '}
                              {busyKey === `g:${group.provider}` ? t('签到中...') : t('签到本组')}
                            </button>
                          </div>
                          <table className="ck-table">
                            <thead>
                              <tr>
                                <th>{t('账号')}</th>
                                <th style={{ width: 90 }}>{t('今日')}</th>
                                <th style={{ width: 150 }}>{t('上次签到')}</th>
                                <th>{t('最近结果')}</th>
                                <th style={{ width: 90 }}>{t('操作')}</th>
                              </tr>
                            </thead>
                            <tbody>
                              {group.accounts.map((account) => (
                                <tr key={account.id}>
                                  <td>{account.name || account.id}</td>
                                  <td>
                                    {account.checked_in_today ? (
                                      <Badge tone="success">{t('已签')}</Badge>
                                    ) : (
                                      <Badge tone="info">{t('待签')}</Badge>
                                    )}
                                  </td>
                                  <td className="ck-time-subtle">
                                    {account.last_ts
                                      ? new Date(account.last_ts * 1000).toLocaleString()
                                      : '—'}
                                  </td>
                                  <td className="ck-message">
                                    {account.last_outcome && (
                                      <Badge tone={OUTCOME_TONE[account.last_outcome] ?? 'neutral'}>
                                        {t(OUTCOME_LABEL[account.last_outcome] ?? account.last_outcome)}
                                      </Badge>
                                    )}{' '}
                                    {account.last_message || '—'}
                                  </td>
                                  <td>
                                    <button
                                      type="button"
                                      className="btn btn-outline btn-sm"
                                      onClick={(e) => {
                                        e.stopPropagation();
                                        void runOne(account.id);
                                      }}
                                      disabled={busyKey !== null}
                                    >
                                      {busyKey === account.id
                                        ? t('签到中…')
                                        : account.checked_in_today
                                          ? t('重签')
                                          : t('签到')}
                                    </button>
                                  </td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </Card>

          {/* 自动链进度：签到成功后逐个动作串行执行的可视化（失败即停标记） */}
          {autoChainProgress && (
            <div className="ck-chain-progress">
              <div className="ck-chain-head">
                <span className="ck-chain-label">
                  {t('自动链')}{autoChainProgress.stopped ? t('（已停止）') : ''}
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
            </div>
          )}
        </>
      ) : tab === 'schedule' ? (
        <>
          <Card title={t('定时签到')}>
            <div className="ck-setting-row">
              <div>
                <div className="ck-setting-label">{t('启用定时签到')}</div>
                <div className="ck-setting-hint">
                  {t('开启后按下面计划的时刻（北京时间）自动签到，当天只跑一次。')}
                </div>
              </div>
              <Switch
                checked={!!data?.settings.enabled}
                onChange={(next) => void toggleSetting(next)}
                ariaLabel={t('启用定时签到')}
              />
            </div>
            <div className="ck-setting-row">
              <div>
                <div className="ck-setting-label">{t('账号间隔（毫秒）')}</div>
                <div className="ck-setting-hint">
                  {t('逐个签到之间的等待，避免并发触发上游风控。')}
                </div>
              </div>
              <div className="ck-gap-edit">
                <input
                  type="number"
                  min={0}
                  step={100}
                  className="form-input ck-gap-input"
                  value={gapMsEdit}
                  onChange={(e) => setGapMsEdit(e.target.value)}
                  onBlur={() => {
                    if (gapMsEdit.trim() === '') setGapMsEdit(String(data?.settings.gap_ms ?? 800));
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void saveGapMs();
                  }}
                  aria-label={t('账号间隔（毫秒）')}
                />
                <button
                  type="button"
                  className="btn btn-outline btn-sm"
                  onClick={() => void saveGapMs()}
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
                value={String(data?.settings.timezone_offset ?? 8)}
                onChange={(e) => void changeTimezone(Number(e.target.value))}
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
                  onChange={(e) => setRetryMinEdit(e.target.value)}
                  aria-label={t('失败/窗口重试间隔（分钟）')}
                />
                <button
                  type="button"
                  className="btn btn-outline btn-sm"
                  onClick={() => void saveAdvanced()}
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
                  onChange={(e) => setKeepDaysEdit(e.target.value)}
                  aria-label={t('台账保留天数')}
                />
                <button
                  type="button"
                  className="btn btn-outline btn-sm"
                  onClick={() => void saveAdvanced()}
                  disabled={advSaving}
                >
                  {advSaving ? t('保存中...') : t('保存')}
                </button>
              </div>
            </div>
          </Card>

          <Card
            title={t('签到计划')}
            actions={
              <button
                className="btn btn-outline btn-sm"
                onClick={openNewSchedule}
                disabled={editingId !== null}
                style={{ gap: 6 }}
              >
                <Plus size={13} /> {t('添加条目')}
              </button>
            }
          >
            {(data?.schedules ?? []).length === 0 && editingId !== 'new' ? (
              <EmptyState icon="⏰" message={t('还没有签到计划')} />
            ) : (
              <div className="ck-sched-list">
                {editingId === 'new' && (
                  <div className="ck-sched-row is-editing">
                    <div className="ck-sched-body">{scheduleEditor}</div>
                  </div>
                )}
                {(data?.schedules ?? []).map((schedule) => {
                  const scope =
                    schedule.providers.length === 0 ||
                    schedule.providers.length >= supported.length
                      ? t('全部 {{n}} 家', { n: supported.length })
                      : schedule.providers.join('、');
                  if (editingId === schedule.id) {
                    return (
                      <div key={schedule.id} className="ck-sched-row is-editing">
                        <div className="ck-sched-body">{scheduleEditor}</div>
                      </div>
                    );
                  }
                  return (
                    <div key={schedule.id} className={`ck-sched-row${schedule.enabled ? '' : ' is-off'}`}>
                      <Switch
                        className="ck-sched-switch"
                        checked={schedule.enabled}
                        ariaLabel={t('启用/停用')}
                        onChange={(next) => void toggleSchedule(schedule, next)}
                      />
                      <div className="ck-sched-main">
                        <div className="ck-sched-name">
                          {schedule.label}
                          {!schedule.enabled && <Badge tone="neutral">{t('已停用')}</Badge>}
                        </div>
                        <div className="ck-sched-desc">
                          {t('每天 {{time}} · {{scope}}', { time: schedule.time, scope })}
                        </div>
                        <div className="ck-sched-desc ck-sched-last">
                          {schedule.last_fired_date
                            ? t('上次执行：{{d}} · {{r}} · 成功 {{ok}}·保活 {{ka}}·失败 {{f}}', {
                                d: schedule.last_fired_date,
                                r: schedule.last_result || '—',
                                ok: schedule.last_ok,
                                ka: schedule.last_keepalive,
                                f: schedule.last_failed,
                              })
                            : t('从未执行')}
                        </div>
                      </div>
                      <div className="ck-sched-next">
                        {schedule.enabled
                          ? t('下次 {{when}}', { when: nextRunLabel(schedule.time, data?.settings.timezone_offset ?? 8) })
                          : '—'}
                      </div>
                      <div className="ck-sched-ops">
                        <button
                          type="button"
                          className="btn btn-outline btn-sm"
                          onClick={() => openEditSchedule(schedule)}
                        >
                          {t('编辑')}
                        </button>
                        <button
                          type="button"
                          className="btn btn-outline btn-sm"
                          onClick={() => void runSchedule(schedule)}
                          disabled={busyKey !== null}
                        >
                          {busyKey === `sched:${schedule.id}` ? t('执行中...') : t('立即')}
                        </button>
                        <button
                          type="button"
                          className="btn btn-danger btn-sm"
                          onClick={() => removeSchedule(schedule)}
                        >
                          <Trash2 size={12} />
                        </button>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </Card>
          {/* 旅行/成长巡检（growth_scheduler；be-fix #16-B 下发 data.travel 后出现） */}
          {travel && (
            <Card title={t('成长巡检（travel / school）')}>
              {travel.available === false && (
                <div className="error-message">
                  {t('自动巡检已暂停：{{reason}}', { reason: travel.paused_reason || t('能力不可用') })}
                </div>
              )}
              <div className="ck-setting-row">
                <div>
                  <div className="ck-setting-label">{t('自动巡检')}</div>
                  <div className="ck-setting-hint">
                    {travel.enabled
                      ? t('开着：按下面的时刻自动巡检成长任务（travel/school）并领取')
                      : t('关着：不自动巡检（仍可手动巡检）')}
                  </div>
                </div>
                <Switch
                  checked={!!travel.enabled}
                  onChange={(next) => void saveTravel({ enabled: next })}
                  ariaLabel={t('自动巡检')}
                />
              </div>
              <div className="ck-setting-row">
                <div>
                  <div className="ck-setting-label">{t('巡检时刻（每行一个 HH:MM）')}</div>
                  <div className="ck-setting-hint">
                    {t('每天在这些时刻巡检成长任务；今日已跑 {{n}} 次：{{fired}}', {
                      n: (travel.fired_today ?? []).length,
                      fired: (travel.fired_today ?? []).join('、') || '—',
                    })}
                  </div>
                </div>
                <div className="ck-gap-edit ck-travel-edit">
                  <textarea
                    className="form-input ck-travel-times"
                    rows={3}
                    value={travelTimesValue}
                    placeholder={'09:00\n21:00'}
                    onChange={(e) => setTravelTimesDraft(e.target.value)}
                    aria-label={t('巡检时刻（每行一个 HH:MM）')}
                  />
                  <div className="ck-travel-actions">
                    <button type="button" className="btn btn-outline btn-sm" onClick={commitTravelTimes}>
                      {t('保存时刻')}
                    </button>
                    <button
                      type="button"
                      className="btn btn-primary btn-sm"
                      onClick={() => void runTravel()}
                      disabled={travel.running}
                    >
                      {travel.running ? t('巡检中...') : t('立即巡检全部')}
                    </button>
                  </div>
                </div>
              </div>
              {travel.last_result && (
                <div className="ck-setting-hint">{t('上次结果：{{r}}', { r: travel.last_result })}</div>
              )}
            </Card>
          )}
        </>
      ) : tab === 'benefits' ? (
        <BenefitsPanel />
      ) : (
        <Card
          title={t('签到记录')}
          actions={
            <div className="ck-records-tools">
              {/* 列设置（公共层；签到记录表专用） */}
              <div className="as-colset" ref={recPanelRef} style={{ position: 'relative', display: 'inline-block' }}>
                <Tooltip label={t('列设置（显隐 / 顺序 / 对齐 / 列宽拖动）')}>
                  <button
                    type="button"
                    className={`btn btn-outline btn-sm${recPanelOpen ? ' as-colset-open' : ''}`}
                    onClick={() => setRecPanelOpen((v) => !v)}
                    aria-label={t('列设置')}
                  >
                    <Columns3 size={14} />
                  </button>
                </Tooltip>
                {recPanelOpen && (
                  <TableColumnSettings
                    open
                    onClose={() => setRecPanelOpen(false)}
                    defs={CK_REC_COL_DEFS}
                    items={recCols.items}
                    onChange={commitRecCols}
                    onReset={resetRecCols}
                    anchorRef={recPanelRef}
                  />
                )}
              </div>
              <div className="ck-seg" role="group" aria-label={t('时间筛选')}>
                {[
                  { v: 1, label: t('今天') },
                  { v: 7, label: t('7 天') },
                  { v: 30, label: t('30 天') },
                  { v: 3650, label: t('全部') },
                ].map((item) => (
                  <button
                    key={item.v}
                    type="button"
                    className={`ck-seg-item${recordDays === item.v ? ' is-active' : ''}`}
                    onClick={() => setRecordDays(item.v)}
                    // 起止日期生效时以区间为准（避免「7 天」与区间取交集导致查不到）
                    title={recordStart || recordEnd ? t('起止日期生效时，此快选不参与过滤') : undefined}
                  >
                    {item.label}
                  </button>
                ))}
              </div>
              <select
                className="form-input"
                style={{ maxWidth: 120 }}
                value={recordProvider}
                onChange={(e) => setRecordProvider(e.target.value)}
              >
                <option value="">{t('全部上游')}</option>
                {supported.map((p) => (
                  <option key={p} value={p}>
                    {p}
                  </option>
                ))}
              </select>
              <select
                className="form-input"
                style={{ maxWidth: 110 }}
                value={recordOutcome}
                onChange={(e) => setRecordOutcome(e.target.value)}
              >
                <option value="">{t('全部结果')}</option>
                <option value="success">{t('成功领取')}</option>
                <option value="already">{t('今日已领')}</option>
                <option value="pending">{t('待重试')}</option>
                <option value="failed">{t('失败')}</option>
                {/* 1.0.49 契约：上游要求人机验证（is_ok=false） */}
                <option value="captcha">{t('需人机验证')}</option>
              </select>
              <select
                className="form-input"
                style={{ maxWidth: 100 }}
                value={recordKind}
                onChange={(e) => setRecordKind(e.target.value)}
                aria-label={t('全部类型')}
              >
                <option value="">{t('全部类型')}</option>
                <option value="manual">{t('手动')}</option>
                <option value="schedule">{t('定时')}</option>
                <option value="makeup">{t('补签')}</option>
                <option value="retry">{t('重试')}</option>
              </select>
              {/* 1.0.49 契约：起止日期（本地时区走当日 00:00:00–23:59:59，后端闭区间） */}
              <input
                type="date"
                className="form-input ck-date-input"
                style={{ maxWidth: 140 }}
                value={recordStart}
                onChange={(e) => setRecordStart(e.target.value)}
                aria-label={t('起始日期')}
                title={t('起始日期')}
              />
              <span className="ck-date-sep">–</span>
              <input
                type="date"
                className="form-input ck-date-input"
                style={{ maxWidth: 140 }}
                value={recordEnd}
                onChange={(e) => setRecordEnd(e.target.value)}
                aria-label={t('结束日期')}
                title={t('结束日期')}
              />
              <input
                className="form-input"
                style={{ maxWidth: 150 }}
                placeholder={t('搜索账号或说明')}
                value={keyword}
                onChange={(e) => {
                  setKeyword(e.target.value);
                  if (e.target.value === '') setKwApplied('');
                }}
                onBlur={() => setKwApplied(keyword.trim())}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') setKwApplied(keyword.trim());
                  if (e.key === 'Escape') {
                    setKeyword('');
                    setKwApplied('');
                  }
                }}
              />
              <button className="btn btn-danger btn-sm" onClick={clearRecords}>
                <X size={12} /> {t('清空')}
              </button>
            </div>
          }
        >
          {recordsLoading ? (
            <Loading text={t('加载记录')} />
          ) : records.length === 0 ? (
            <EmptyState
              icon="📝"
              message={hasRecordFilter ? t('没有匹配的签到记录') : t('暂无签到记录')}
            />
          ) : (
            <>
              <table className="ck-table">
                <colgroup>
                  {appliedRecCols.map((c) => (
                    <col key={c.key} data-col-key={c.key} style={c.width != null ? { width: `${c.width}px` } : undefined} />
                  ))}
                </colgroup>
                <thead>
                  <tr>
                    {appliedRecCols.map((c, idx) => {
                      const gripProps = { title: t('拖动调整列宽 · 双击还原') };
                      return (
                      <th key={c.key} style={c.width != null ? { width: `${c.width}px` } : undefined}>
                        {t(CK_REC_COL_DEFS.find((d) => d.key === c.key)?.label ?? c.key)}
                        {c.key !== 'time' && c.key !== 'note' && idx >= 0 && (
                          <span className="as-col-grip" {...gripProps} />
                        )}
                      </th>
                      );
                    })}
                  </tr>
                </thead>
                <tbody>
                  {records.map((record, index) => (
                    <tr key={`${record.ts}-${record.account_id}-${index}`}>
                      {recKeySet.has('time') && (
                      <td>
                        {new Date(record.ts * 1000).toLocaleString()}
                        {record.date && <div className="ck-time-subtle">{record.date}</div>}
                      </td>
                      )}
                      {recKeySet.has('account') && (
                      <td>
                        {record.account_name || record.account_id}
                        {record.kept_active && <Badge tone="info">{t('保活')}</Badge>}
                      </td>
                      )}
                      {recKeySet.has('provider') && (
                      <td>{record.provider}</td>
                      )}
                      {recKeySet.has('source') && (
                      <td className="ck-record-source">
                        {t(SOURCE_LABEL[record.source] ?? record.source)}
                      </td>
                      )}
                      {recKeySet.has('kind') && (
                      <td className="ck-record-kind">
                        {/* 存量记录 kind 为空串 → 回退按 source 展示 */}
                        {(() => {
                          const kindKey = record.kind || record.source || '';
                          return kindKey ? t(KIND_LABEL[kindKey] ?? kindKey) : '—';
                        })()}
                      </td>
                      )}
                      {recKeySet.has('result') && (
                      <td>
                        <Badge tone={OUTCOME_TONE[record.outcome] ?? 'neutral'}>
                          {t(OUTCOME_LABEL[record.outcome] ?? record.outcome)}
                        </Badge>
                      </td>
                      )}
                      {recKeySet.has('note') && (
                      <td className={record.outcome === 'failed' || record.outcome === 'captcha' ? 'ck-message ck-record-detail-fail' : 'ck-message'}>
                        {record.message || '—'}
                      </td>
                      )}
                      {recKeySet.has('error') && (
                      <td className={record.error ? 'ck-message ck-record-detail-fail' : 'ck-message'}>
                        {record.error || '—'}
                      </td>
                      )}
                    </tr>
                  ))}
                </tbody>
              </table>
              <div className="ck-pager">
                <span className="ck-pager-info">
                  {t('共 {{n}} 条 · 第 {{page}}/{{pages}} 页', {
                    n: total,
                    page,
                    pages: totalPages,
                  })}
                </span>
                <div className="ck-pager-tools">
                  <select
                    className="form-input"
                    style={{ maxWidth: 90 }}
                    value={pageSize}
                    onChange={(e) => setSize(Number(e.target.value))}
                  >
                    {SERVER_PAGE_SIZES.map((size) => (
                      <option key={size} value={size}>
                        {size} / 页
                      </option>
                    ))}
                  </select>
                  <button
                    className="btn btn-outline btn-sm"
                    disabled={page <= 1}
                    onClick={() => setPage(Math.max(1, page - 1))}
                  >
                    {t('上一页')}
                  </button>
                  <button
                    className="btn btn-outline btn-sm"
                    disabled={page >= totalPages}
                    onClick={() => setPage(page + 1)}
                  >
                    {t('下一页')}
                  </button>
                </div>
              </div>
            </>
          )}
        </Card>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
