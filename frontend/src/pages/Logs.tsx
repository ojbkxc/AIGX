import { useState, useEffect, useRef, useCallback, useMemo, type MouseEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Trash2, Eraser, EyeOff, Eye, Search, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { isAdmin, fmtCompact } from '../lib/utils';
import { fmtCost, toCsv } from '../lib/format';
import { getFilter, setFilter } from '../lib/filterMemory';
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
import { useColumnResize } from '../components/ui/useColumnResize';
import { TablePaginationFooter, usePageSize, SERVER_PAGE_SIZES } from '../components/ui/TableShell';
import { Columns3 } from 'lucide-react';
import { useListKeyboard } from '../hooks/useListKeyboard';
import { useAutoRefresh } from '../hooks/useAutoRefresh';
import PageAutoRefreshControl from '../components/PageAutoRefreshControl';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { RequestLogDetail, AuditLogDetail } from '../components/LogDetailDialogs';
import { Button, Card, Loading, EmptyState, Tooltip } from '../components/ui';
import PageHeader from '../components/PageHeader';
import './Logs.css';

export type LogTab = 'requests' | 'audits';
type LogView = 'table' | 'timeline';

export interface RequestLogItem {
  id: string | number;
  created_at?: number;
  user_id?: string;
  /** 后端解析的邮箱展示（user_id → email，解析失败缺省回退 user_id） */
  user_email?: string;
  key_id?: string;
  channel_id?: string;
  channel_name?: string;
  /**
   * 供应商（后端 `RequestLog.provider`）：渠道型 = 上游协议族
   *（openai_compatible / anthropic / gemini / zai / cloudflare / plugin）；
   * 账号池型 = 账号的「家」（trae / xiaohuanxiong / workbuddy …）。
   *
   * 与 `channel_name`（渠道/账号池的名字，运维可改可删）是**两个维度**。
   * 老日志没有这个键（历史数据没记过这一维）→ `undefined`，展示层回退
   * 到 `channel_name`，与改造前的显示逐字一致。
   */
  provider?: string;
  model?: string;
  origin_model?: string;
  input_tokens?: number;
  output_tokens?: number;
  cost?: number;
  channel_cost?: number;
  latency_ms?: number;
  /** 本次请求的上游尝试次数（含重试；后端 `RequestLog.attempts`，缺省 1）。
   *  A2A requests 列表有「重试」列，本页原先只能点开详情/路由回放才知道重试了几次。 */
  attempts?: number;
  status_code?: number;
  error_msg?: string;
  ip?: string;
  /** 请求/响应快照（log_body 开关开启时后端记录；仅 admin 详情展示） */
  debug?: {
    request_body?: string;
    response_body?: string;
    upstream_url?: string;
  };
  /** 对话快照（**默认记录**，不依赖 log_body）：管理员可在详情里直接看对话 */
  conversation?: {
    messages: Array<{ role: string; text: string; truncated?: boolean }>;
    system_prompt_chars?: number;
    truncated_history?: boolean;
  };
  /** 调度决策回放：候选渠道列表（仅 admin 下发，普通用户已被后端剥离） */
  candidate_channels?: string[];
  /** 被过滤的渠道及原因（仅 admin；#26 起有界 64 条，溢出计入 route_truncated） */
  filtered_channels?: Array<{ channel_id: string; reason: string }>;
  /** 逐账号跳过明细（仅 admin；#26 形状：按渠道分组 + 每渠道 64 条上限 + 截断计数，
   *  与 GET /api/routing/replay 投影同构；旧日志无该字段） */
  account_skips?: Array<{
    channel_id: string;
    skipped: Array<{ account_id: string; account_name?: string; reason?: string }>;
    truncated?: number;
  }>;
  /** resolve 层候选截断计数（filtered 上限溢出；仅 admin） */
  route_truncated?: number;
  /** 最终选中渠道（仅 admin） */
  selected_channel?: string;
  /** 识别出的客户端名（规范化；空/缺省 = 老日志未记录） */
  client_name?: string;
  /** 客户端版本（能提取则有值） */
  client_version?: string;
  /** 是否使用破甲控制台（仅 admin 下发，普通用户已被后端剥离） */
  purge_applied?: boolean;
}

export interface AuditLogItem {
  id: string | number;
  created_at?: number;
  admin_id?: string;
  /** 后端解析的邮箱展示（admin_id → email） */
  admin_email?: string;
  action?: string;
  target?: string;
  before?: string;
  after?: string;
}

/** key_id 特殊来源 → 可读标签（非真实 API Key） */
const SOURCE_LABELS: Record<string, string> = {
  chat_test: '供应商调试',
  agent: 'AI 运维',
  'playground:chat': 'Playground 对话',
  'playground:images': 'Playground 图片',
  'playground:tts': 'Playground TTS',
  'playground:transcriptions': 'Playground 转写',
};
export const sourceLabel = (keyId: string | undefined): string | null =>
  (keyId && SOURCE_LABELS[keyId]) ||
  (keyId?.startsWith('playground:') ? keyId.slice('playground:'.length) : null);

/**
 * 状态码归类：`0`/缺省视为「未记录」（中性），`<400` 成功，其余失败。
 *
 * 后端 `RequestLog.status_code` 是 `u16` 且 `#[serde(default)]` —— 未完成/被中断的
 * 老日志会落成 0。旧实现把 `0` 当成功（`<400`）并渲染成绿色「成功」，属于误导；
 * 这里把「无状态码」单独归为 unknown，表格与时间线都按中性处理。
 */
const statusKind = (code?: number | null): 'ok' | 'error' | 'unknown' =>
  code == null || code === 0 ? 'unknown' : code < 400 ? 'ok' : 'error';

/** 服务端导出不支持 status/keyword（ExportQuery 无此二参），
 *  这两项生效时前端改走列表接口逐页取全量后本地生成，保证导出与所见一致。 */
const EXPORT_CLIENT_MAX_ROWS = 50_000;
/** 与后端 export_csv_filtered 的表头逐列对齐，保证两种导出口径一致 */
const EXPORT_CSV_COLUMNS = [
  'id', 'created_at', 'user_id', 'key_id', 'channel_id', 'channel_name',
  'model', 'origin_model', 'input_tokens', 'output_tokens', 'cost',
  'channel_cost', 'latency_ms', 'status_code', 'error_msg', 'ip',
] as const;

type LogItem = RequestLogItem | AuditLogItem;

/** 触发浏览器下载（Safari/Firefox 同帧 revoke 会取消下载，延迟释放） */
function triggerDownload(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

interface Filters {
  user: string;
  model: string;
  channel: string;
  /** 状态：'' 全部 / 'ok' / 'error' */
  status: string;
  /** 关键词：模糊匹配模型/渠道/错误/用户 */
  keyword: string;
  /** 客户端过滤：模糊匹配 client_name / client_version（全局，admin/普通均可用） */
  client: string;
  start: string;
  end: string;
  /**
   * 时间档位（对齐 Agent2API requests-page 的 RANGE_OPTIONS：today / yesterday / 7 / 30 /
   * month / all / custom）。它是 start/end 的**来源**而不是另一份查询条件 —— 点档位就把它
   * 换算成起止时间，请求仍然只带 start/end；手改起止时间则自动落回 custom。
   * 默认 all（= 起止都空 = 不限），与加这个档位之前的行为逐字节一致。
   */
  range: string;
}

/** 请求日志筛选的落盘键（filterMemory：跨次启动记忆，四页各自一键） */
const LS_LOGS_FILTERS = 'aigx-logs-filters';
/** 公共列设置键（按 Tab 分键：请求日志 / 审计日志列集不同） */
const LS_TCOLS_REQ = 'aigx-tbl-cols.logs.requests';
const LS_TCOLS_AUDIT = 'aigx-tbl-cols.logs.audit';
/** 公共每页条数键（服务端真分页；Logs 原为固定 20 无记忆键，无搬迁） */
const LS_PSZ = 'aigx-tbl-pagesize.logs';

/** 请求日志列声明（check/user 渠道为管理员专属，非管理员 defs 不含即不可见不可拖） */
function logsReqColDefs(admin: boolean): TableColumnDef[] {
  return [
    ...(admin ? [{ key: 'check', label: '选择', align: 'center' as const, width: 36, locked: true }] : []),
    ...(admin ? [{ key: 'user', label: '用户', width: 190 }] : []),
    { key: 'time', label: '时间', width: 170, locked: true },
    { key: 'model', label: '模型', width: 150 },
    { key: 'client', label: '客户端', width: 140 },
    ...(admin ? [{ key: 'channel', label: '供应商', width: 120 }] : []),
    { key: 'tokens', label: 'Tokens', align: 'right' as const, width: 110 },
    { key: 'cost', label: '费用', align: 'right' as const, width: 100 },
    { key: 'latency', label: '延迟', align: 'right' as const, width: 90 },
    // 重试次数（A2A requests 的「重试」列）：>1 才上色，1 次照常显示，
    // 让「一次就通」与「试了三家才通」在同一列上可比。
    { key: 'retry', label: '重试', align: 'center' as const, width: 70 },
    { key: 'status', label: '状态', align: 'center' as const, width: 90 },
    ...(admin ? [{ key: 'purge', label: '破甲', align: 'center' as const, width: 70 }] : []),
  ];
}

/** 审计日志列声明 */
function logsAuditColDefs(admin: boolean): TableColumnDef[] {
  return [
    ...(admin ? [{ key: 'check', label: '选择', align: 'center' as const, width: 36, locked: true }] : []),
    { key: 'admin', label: '管理员', width: 190 },
    { key: 'time', label: '时间', width: 170, locked: true },
    { key: 'action', label: '操作', width: 140 },
    { key: 'target', label: '目标', width: 200 },
    { key: 'change', label: '变更', width: 300 },
  ];
}
/** 筛选默认值（空串 = 全部 / 不限；记忆按此收窄，存坏回落默认） */
const DEFAULT_LOGS_FILTERS: Filters = { user: '', model: '', channel: '', status: '', keyword: '', client: '', start: '', end: '', range: 'all' };

/**
 * 时间档位（对齐 Agent2API requests-page 的 RANGE_OPTIONS，含自定义区间）。
 *
 * 与 A2A 的**唯一有意偏离**：默认档是 `all`（不限），不是 `today`。本页原先就没有默认
 * 时间窗（起止为空 = 全量），改成今天会把历史日志静默藏起来 —— 那是产品决定，不是对齐。
 */
const RANGE_OPTIONS: ReadonlyArray<[string, string]> = [
  ['today', '今天'],
  ['yesterday', '昨天'],
  ['7', '近 7 天'],
  ['30', '近 30 天'],
  ['month', '本月'],
  ['all', '全部'],
  ['custom', '自定义'],
];

/** 本地日期 → `YYYY-MM-DD`（datetime-local 的日期段） */
function dayKeyOf(d: Date): string {
  const p = (n: number): string => (n < 10 ? `0${n}` : String(n));
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/**
 * 档位 → 起止时间（datetime-local 字符串 `YYYY-MM-DDTHH:mm`）。
 *
 * 口径与报表页一致：`7` / `30` 是**含今天在内**往前数（A2A 同一口径），今天从 00:00 起、
 * 到「现在」为止；`all` 清空起止（后端按不加时间条件处理）；`custom` 原样返回空串 ——
 * 自定义区间由用户自己在两个输入框里给，这里替它猜日期只会覆盖用户刚填的值。
 */
function rangeBounds(range: string, now = new Date()): { start: string; end: string } {
  const at = (d: Date, h = 0, m = 0): string =>
    `${dayKeyOf(d)}T${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const back = (n: number): Date => new Date(today.getFullYear(), today.getMonth(), today.getDate() - n);
  switch (range) {
    case 'today':
      return { start: at(today, 0, 0), end: at(today, 23, 59) };
    case 'yesterday':
      return { start: at(back(1), 0, 0), end: at(back(1), 23, 59) };
    case '7':
      return { start: at(back(6), 0, 0), end: at(today, 23, 59) };
    case '30':
      return { start: at(back(29), 0, 0), end: at(today, 23, 59) };
    case 'month':
      return { start: at(new Date(today.getFullYear(), today.getMonth(), 1), 0, 0), end: at(today, 23, 59) };
    case 'all':
      return { start: '', end: '' };
    default:
      return { start: '', end: '' };
  }
}

export default function Logs(): JSX.Element {
  const admin = isAdmin();
  const { t, i18n } = useTranslation();
  const lang = i18n.language;
  const [tab, setTab] = useState<LogTab>('requests');
  const [view, setView] = useState<LogView>('table');
  const [logs, setLogs] = useState<LogItem[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  // 每页条数走公共 usePageSize（SERVER 档位：服务端真分页无「全部」档，绝不退化全量取数）
  const [pageSize, setPageSize] = usePageSize(LS_PSZ, SERVER_PAGE_SIZES, 20);
  const size = typeof pageSize === 'number' ? pageSize : 20;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [exporting, setExporting] = useState<'json' | 'csv' | null>(null);
  const addToast = useToast();
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [selected, setSelected] = useState<Set<string | number>>(new Set());
  const [batchDeleting, setBatchDeleting] = useState(false);
  /** 行点击打开的详情条目（null 关闭） */
  const [detail, setDetail] = useState<LogItem | null>(null);
  /** 在途请求面板（管理员：正在跑、尚未落日志的请求） */
  const [showInflight, setShowInflight] = useState(false);
  const [inflight, setInflight] = useState<import('../types').InflightRequestItem[]>([]);
  const [inflightLoading, setInflightLoading] = useState(false);
  /** 敏感信息脱敏开关（管理员：隐藏用户邮箱/渠道名/IP） */
  const [masked, setMasked] = useState(false);

  // 筛选条件的记忆：启动时从 localStorage 恢复（首查参数随之生效），变更即落盘
  const [filters, setFilters] = useState<Filters>(() => {
    const saved = getFilter(LS_LOGS_FILTERS, DEFAULT_LOGS_FILTERS);
    // 档位记忆：没有这个键的老存盘按「有没有起止」判 —— 有起止就是自定义区间，
    // 否则是不限。档位值非法（改坏 / 老版本）同样落回这两档，不猜成「今天」。
    const range = RANGE_OPTIONS.some(([v]) => v === saved.range)
      ? saved.range
      : (saved.start || saved.end ? 'custom' : 'all');
    const kept: Filters = { ...saved, range };
    // user / channel 是管理员专属筛选维度：非管理员界面不渲染这两个输入，
    // 记忆里残留的旧值会让列表被看不见的筛选悄悄过滤（服务端照单全收），恢复时清掉
    return admin ? kept : { ...kept, user: '', channel: '' };
  });
  useEffect(() => {
    setFilter(LS_LOGS_FILTERS, filters);
  }, [filters]);

  // 列设置已迁公共层（tableColumns v2）。两个 Tab 列集不同 → 各自一份键；
  // defs 含管理员专属列（check/user/channel），登录态变化时读回走演进/插回，
  // 非 admin 的 defs 不含这些列（多余的存盘键被公共层丢弃，安全）。
  const [reqCols, setReqCols] = useState<TableColumnState>(() => readTableColumns(LS_TCOLS_REQ, logsReqColDefs(admin)));
  const [auditCols, setAuditCols] = useState<TableColumnState>(() => readTableColumns(LS_TCOLS_AUDIT, logsAuditColDefs(admin)));
  useEffect(() => {
    // 登录态变化（登录/登出）后 defs 形状变了：以新 defs 重读走演进
    setReqCols(readTableColumns(LS_TCOLS_REQ, logsReqColDefs(admin)));
    setAuditCols(readTableColumns(LS_TCOLS_AUDIT, logsAuditColDefs(admin)));
  }, [admin]);
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);
  const activeColState = tab === 'requests' ? reqCols : auditCols;
  const activeSetColState = tab === 'requests' ? setReqCols : setAuditCols;
  const activeLSKey = tab === 'requests' ? LS_TCOLS_REQ : LS_TCOLS_AUDIT;
  const activeDefs = tab === 'requests' ? logsReqColDefs(admin) : logsAuditColDefs(admin);
  /** 列设置变更（面板回调；persist=true 才落盘） */
  const commitCols = useCallback((next: TableColumnItem[], persist: boolean) => {
    activeSetColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(activeLSKey, nextState);
      return nextState;
    });
  }, [activeSetColState, activeLSKey]);
  const resetCols = useCallback(() => {
    activeSetColState(resetTableColumns(activeLSKey, activeDefs));
  }, [activeSetColState, activeLSKey, activeDefs]);
  /** 当前 Tab 的可见列（顺序 + 显隐；列宽/对齐由公共层管理） */
  const appliedCols = useMemo(() => applyTableColumns(activeColState, activeDefs), [activeColState, activeDefs]);
  const visibleKeySet = useMemo(() => new Set(appliedCols.map((c) => c.key)), [appliedCols]);
  const colAlignMap = useMemo(() => {
    const map = {} as Record<string, string>;
    for (const item of activeColState.items) map[item.key] = item.align;
    return map;
  }, [activeColState]);
  /** 列宽拖动（公共 hook；widths 由 activeColState.items 派生，落盘走 commitCols） */
  const colResize = useColumnResize({
    widths: Object.fromEntries(
      activeColState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number]),
    ),
    setWidths: (next) => {
      commitCols(
        activeColState.items.map((i) => {
          const w = next[i.key];
          const defW = activeDefs.find((d) => d.key === i.key)?.width;
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(activeDefs.map((d) => [d.key, d.width])),
    lockedKeys: new Set(activeDefs.filter((d) => d.locked).map((d) => d.key)),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });

  // 键盘导航（cc-haha 列表键盘体系）：/ 聚焦用户筛选、↑↓/j k 行高亮、Enter 打开详情
  const { searchRef, rowRefs, activeIndex, setActiveIndex } = useListKeyboard(
    logs.length,
    (i) => { if (logs[i]) setDetail(logs[i]); },
    !loading && !detail && !confirmState,
  );

  // 仅 tab / 分页变化时自动加载；筛选条件由「查询」按钮显式触发，避免每键一请求
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab, page, size]);

  // 切 tab / 翻页时清空选择，避免跨页选中混淆
  useEffect(() => {
    setSelected(new Set());
  }, [tab, page]);

  // 请求序号：快速翻页/切筛选时旧响应晚到不覆盖新数据
  const loadSeq = useRef(0);
  // override：状态分段控件这类「离散选择」点击即查询时，需用尚未提交的新筛选值
  // 发请求（setFilters 的 state 更新是异步的，闭包里的 filters 还是旧值）。
  const load = async (silent = false, override?: Filters) => {
    const f = override ?? filters;
    const seq = ++loadSeq.current;
    if (!silent) {
      setLoading(true);
      setError('');
    }
    try {
      let resTotal = 0;
      if (tab === 'requests') {
        const params: Record<string, string | number> = { page, size };
        if (f.user) params.user = f.user;
        if (f.model) params.model = f.model;
        if (f.channel) params.channel = f.channel;
        if (f.status) params.status = f.status;
        if (f.keyword.trim()) params.keyword = f.keyword.trim();
        if (f.client.trim()) params.client = f.client.trim();
        // 时间范围筛选：datetime-local 值（本地时区）→ unix 秒
        if (f.start) params.start = Math.floor(new Date(f.start).getTime() / 1000);
        if (f.end) params.end = Math.floor(new Date(f.end).getTime() / 1000);
        const res = await api.listRequestLogs(params);
        if (seq !== loadSeq.current) return; // 已被更新的请求取代
        setLogs(Array.isArray(res?.data) ? (res.data as unknown as LogItem[]) : []);
        resTotal = res?.total || 0;
        setTotal(resTotal);
      } else {
        const res = await api.listAuditLogs({ page, size });
        if (seq !== loadSeq.current) return;
        setLogs(Array.isArray(res?.data) ? (res.data as unknown as LogItem[]) : []);
        resTotal = res?.total || 0;
        setTotal(resTotal);
      }
      // 删空末页唯一条目后钳回有效页（total 变小页码越界显示空页）
      const totalPages = Math.max(1, Math.ceil(resTotal / size));
      if (page > totalPages) {
        setPage(totalPages);
        return; // setPage 会触发 useEffect 重新 load
      }
    } catch (err) {
      if (seq !== loadSeq.current) return;
      if (silent) return; // 轮询失败不覆盖页面错误状态，等下一次或手动刷新
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (seq === loadSeq.current) setLoading(false);
    }
  };

  // 前端自动刷新：按「定时任务」页配置的 logsAutoRefresh/requestsAutoRefresh
  // 轮询本页（requests tab → 请求日志刷新，audits tab → 系统事件刷新）。
  // 页面不可见时跳过，配置改动最多滞后一个周期生效。
  const autoRefreshTaskId = tab === 'requests' ? 'requestsAutoRefresh' : 'logsAutoRefresh';
  useAutoRefresh(autoRefreshTaskId, () => {
    void load(true);
  });

  const handleBatchDelete = () => {
    if (selected.size === 0) return;
    // 后端 DeleteLogsBody.ids 是 Vec<String>：混入数字 id 会反序列化失败（400）。
    const ids = Array.from(selected).map(String);
    const targetLabel = tab === 'requests' ? t('请求日志') : t('审计日志');
    setConfirmState({
      title: t('删除日志'),
      message: (
        <>
          {t('确定删除选中的')} <strong>{ids.length}</strong> {t('条')}{targetLabel}？{t('该操作不可撤销。')}
        </>
      ),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setBatchDeleting(true);
        setError('');
        try {
          const res = tab === 'requests'
            ? await api.deleteRequestLogs(ids)
            : await api.deleteAuditLogs(ids);
          addToast(`${t('已删除')} ${res?.data?.removed ?? ids.length} ${t('条')}`);
          setSelected(new Set());
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        } finally {
          setBatchDeleting(false);
        }
      },
    });
  };

  const handleClearAll = async () => {
    const targetLabel = tab === 'requests' ? t('请求日志') : t('审计日志');
    // 先取预览：告诉管理员「会删多少条、覆盖哪段时间」，
    // 避免一键清空后才发现删错了区间。预览失败不阻断（退回通用文案）。
    let preview: { would_remove: number; oldest?: number | null; newest?: number | null } | null = null;
    if (tab === 'requests') {
      try {
        const res = await api.clearPreview(0);
        preview = res?.data ?? null;
      } catch {
        preview = null;
      }
    }
    const rangeText = preview?.oldest && preview?.newest
      ? `${fmtTime(preview.oldest)} ~ ${fmtTime(preview.newest)}`
      : null;
    setConfirmState({
      title: t('清空日志'),
      message: (
        <>
          {t('确定清空全部')}{targetLabel}？
          {preview ? (
            <div style={{ marginTop: 8, fontSize: 12.5, color: 'var(--text-muted)' }}>
              {t('将删除 {{n}} 条', { n: preview.would_remove })}
              {rangeText ? `（${rangeText}）` : ''}
            </div>
          ) : null}
          {t('该操作不可撤销。')}
        </>
      ),
      confirmText: t('清空'),
      danger: true,
      onConfirm: async () => {
        setBatchDeleting(true);
        setError('');
        try {
          const res = tab === 'requests'
            ? await api.clearRequestLogs()
            : await api.clearAuditLogs();
          addToast(`${t('已清空')} ${res?.data?.removed ?? 0} ${t('条')}`);
          setSelected(new Set());
          if (page !== 1) {
            setPage(1); // useEffect 会重新 load，避免闭包旧 page 双请求
          } else {
            await load();
          }
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        } finally {
          setBatchDeleting(false);
        }
      },
    });
  };

  const toggleSelect = (id: string | number): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleSelectAll = (): void => {
    setSelected((prev) => {
      if (prev.size === logs.length) return new Set();
      return new Set(logs.map((l) => l.id));
    });
  };

  const handleExport = async (format: 'json' | 'csv') => {
    try {
      setExporting(format);
      // 服务端导出（ExportQuery）只认 user/model/channel/start/end，不含 status/keyword。
      // 这两项生效时改走列表接口逐页取全量、本地生成文件，确保「导出 == 页面所见」。
      const needsClientFilter = Boolean(filters.status) || Boolean(filters.keyword.trim());
      if (needsClientFilter) {
        const rows: Record<string, unknown>[] = [];
        const pageSize = 500;
        for (let p = 1; ; p += 1) {
          const params: Record<string, string | number> = { page: p, size: pageSize };
          if (filters.user) params.user = filters.user;
          if (filters.model) params.model = filters.model;
          if (filters.channel) params.channel = filters.channel;
          if (filters.status) params.status = filters.status;
          if (filters.keyword.trim()) params.keyword = filters.keyword.trim();
          if (filters.start) params.start = Math.floor(new Date(filters.start).getTime() / 1000);
          if (filters.end) params.end = Math.floor(new Date(filters.end).getTime() / 1000);
          const res = await api.listRequestLogs(params);
          const chunk = Array.isArray(res?.data) ? (res.data as Record<string, unknown>[]) : [];
          rows.push(...chunk);
          const totalN = Number(res?.total) || 0;
          if (chunk.length === 0 || rows.length >= totalN) break;
          if (rows.length >= EXPORT_CLIENT_MAX_ROWS) {
            throw new Error(
              t('导出条数超过 {{n}} 条上限，请缩小筛选范围后再导出', { n: EXPORT_CLIENT_MAX_ROWS }),
            );
          }
        }
        if (format === 'json') {
          triggerDownload(
            new Blob([JSON.stringify(rows, null, 2)], { type: 'application/json;charset=utf-8' }),
            'request_logs.json',
          );
        } else {
          const ordered = rows.map((r) => {
            const o: Record<string, unknown> = {};
            for (const col of EXPORT_CSV_COLUMNS) o[col] = r[col] ?? '';
            return o;
          });
          // 与 lib/format.downloadText 一致加 BOM，Excel 才能正确识别 UTF-8
          triggerDownload(
            new Blob(['\uFEFF' + toCsv(ordered)], { type: 'text/csv;charset=utf-8' }),
            'request_logs.csv',
          );
        }
        addToast(t('导出成功'));
        return;
      }

      // 常规路径：服务端流式导出（大结果集无需逐页拉取）
      const qs = new URLSearchParams({ format });
      if (filters.user) qs.set('user', filters.user);
      if (filters.model) qs.set('model', filters.model);
      if (filters.channel) qs.set('channel', filters.channel);
      if (filters.start) qs.set('start', String(Math.floor(new Date(filters.start).getTime() / 1000)));
      if (filters.end) qs.set('end', String(Math.floor(new Date(filters.end).getTime() / 1000)));
      const token = localStorage.getItem('token');
      const res = await fetch(`/api/logs/requests/export?${qs.toString()}`, {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (!res.ok) {
        const errData = await res.json().catch(() => null);
        throw new Error(errData?.error || errData?.message || `${t('导出失败')} (${res.status})`);
      }
      const blob = await res.blob();
      triggerDownload(blob, `request_logs.${format}`);
      addToast(t('导出成功'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setExporting(null);
    }
  };

  // 查询按钮触发筛选：已在第 1 页时直接加载，否则重置页码由 useEffect 自动加载（保证单次请求）
  const handleSearch = (e: MouseEvent<HTMLButtonElement>) => {
    e.preventDefault();
    if (page === 1) {
      void load();
    } else {
      setPage(1);
    }
  };

  // 状态分段控件是离散选择：点击即查询。setFilters 的 state 更新是异步的，
  // 闭包里的 filters 仍是旧值，故用 override 显式传入新筛选值。
  // （文本/时间输入仍走「查询」按钮，避免每键一请求。）
  const applyStatus = (status: string): void => {
    const next: Filters = { ...filters, status };
    setFilters(next);
    if (page === 1) {
      void load(false, next);
    } else {
      setPage(1); // useEffect 重新加载，届时已拿到新 filters
    }
  };

  /**
   * 时间档位是离散选择（对齐 A2A requests-page 的 onRangeChange：点即查询）。
   *
   * 档位只负责把 `start/end` 换算出来 —— 请求参数与后端契约一个字都没变（仍然是
   * start/end 的 unix 秒），查询按钮、回车、导出走的也是同一份 filters。
   * `custom` 不清空已有起止：用户想接着上一次的自定义区间改，替它清掉更烦人。
   */
  const applyRange = (range: string): void => {
    const bounds = rangeBounds(range);
    const next: Filters = range === 'custom'
      ? { ...filters, range }
      : { ...filters, range, start: bounds.start, end: bounds.end };
    setFilters(next);
    if (page === 1) {
      void load(false, next);
    } else {
      setPage(1);
    }
  };

  const fmtTime = (ts: number | undefined): string => (ts ? new Date(ts * 1000).toLocaleString() : '—');
  /**
   * 空态文案（对齐 Agent2API requests-page 的 emptyText 三分）：加载中 / 一条都没有 /
   * 筛掉了。前两档靠「有没有生效的筛选」区分 —— 只看 `total === 0` 会把「筛完没结果」
   * 说成「网关没转发过请求」，那是两句意思完全相反的话。
   */
  const hasActiveFilter = Boolean(
    filters.user || filters.model || filters.channel || filters.keyword.trim()
    || filters.client.trim() || filters.status || filters.start || filters.end,
  );
  const emptyText = tab === 'requests'
    ? (hasActiveFilter ? t('没有符合筛选条件的请求') : t('暂无请求日志，网关还没有转发过请求'))
    : t('暂无日志');
  // 分页组件在 totalPages<=1 时不渲染；这里仍至少为 1，避免「0 页」出现
  const totalPages = Math.max(1, Math.ceil(total / size));
  const loadInflight = async (): Promise<void> => {
    setInflightLoading(true);
    try {
      const res = await api.listInflightRequests();
      setInflight((res?.data ?? []) as import('../types').InflightRequestItem[]);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setInflightLoading(false);
    }
  };

  const handleTerminateInflight = (id: string): void => {
    setConfirmState({
      title: t('终止在途请求'),
      message: t('将中断这条请求的上游流并记为失败。确定继续？'),
      confirmText: t('终止'),
      danger: true,
      onConfirm: async () => {
        try {
          await api.terminateInflightRequest(id);
          addToast(t('已终止'));
          await loadInflight();
        } catch (err) {
          addToast(err instanceof Error ? err.message : String(err), 'error');
        }
      },
    });
  };

  const isRequest = (_l: LogItem): _l is RequestLogItem => tab === 'requests';

  /** 脱敏显示：邮箱/渠道/IP 等敏感值打码 */
  const mask = (v: string | undefined): string => (v ? '••••' : '—');
  const userDisplay = (email: string | undefined, id: string | undefined): string => {
    if (masked) return mask(email ?? id);
    return email || id || '—';
  };
  /** 状态徽标：成功 / 失败 / 未记录（status_code=0 或缺失 → 中性） */
  const renderStatusBadge = (code: number | undefined, small = false): JSX.Element => {
    const k = statusKind(code);
    const cls = k === 'ok' ? 'badge badge-success' : k === 'error' ? 'badge badge-danger' : 'badge badge-neutral';
    return (
      <span className={cls} style={small ? { fontSize: 10 } : undefined}>
        {k === 'ok' ? t('成功') : k === 'error' ? t('失败') : '—'}
      </span>
    );
  };

  // ── 时间线聚合：当前页日志按小时分桶（成功/失败计数 + 总费用） ──
  const timelineBuckets = (() => {
    const map = new Map<number, { ok: number; fail: number; cost: number; models: Set<string> }>();
    for (const l of logs) {
      if (!isRequest(l) || !l.created_at) continue;
      const hour = Math.floor(l.created_at / 3600) * 3600;
      const b = map.get(hour) || { ok: 0, fail: 0, cost: 0, models: new Set<string>() };
      // 无状态码（0/缺省）不计入成功也不计入失败，避免把「未记录」误报成成功
      const kind = statusKind(l.status_code);
      if (kind === 'ok') b.ok += 1;
      else if (kind === 'error') b.fail += 1;
      b.cost += l.cost ?? 0;
      if (l.model) b.models.add(l.model);
      map.set(hour, b);
    }
    return Array.from(map.entries()).sort((a, b) => a[0] - b[0]);
  })();

  const renderTimeline = (): JSX.Element => {
    const buckets = timelineBuckets;
    if (buckets.length === 0) return <EmptyState message={t('暂无时间线数据')} />;
    const W = 720;
    const H = 160;
    const PAD_L = 46;
    const PAD_B = 26;
    const PAD_T = 12;
    const PAD_R = 12;
    const plotW = W - PAD_L - PAD_R;
    const plotH = H - PAD_T - PAD_B;
    const maxCount = Math.max(1, ...buckets.map(([, b]) => b.ok + b.fail));
    const barGap = 3;
    const barW = Math.max(2, Math.floor((plotW - barGap * (buckets.length - 1)) / buckets.length));
    const yFor = (n: number): number => PAD_T + plotH - (n / maxCount) * plotH;
    const ticks = [0, Math.round(maxCount / 2), maxCount];
    return (
      <div className="log-timeline">
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" style={{ width: '100%', height: 'auto' }} role="img" aria-label={t('请求时间线')}>
          {ticks.map((tk) => (
            <g key={tk}>
              <line x1={PAD_L} y1={yFor(tk)} x2={W - PAD_R} y2={yFor(tk)} stroke="var(--border-color)" strokeWidth={1} />
              <text x={PAD_L - 6} y={yFor(tk) + 3} fontSize={9} fill="var(--text-muted)" textAnchor="end">{tk}</text>
            </g>
          ))}
          {buckets.map(([hour, b], i) => {
            const x = PAD_L + i * (barW + barGap);
            const okH = (b.ok / maxCount) * plotH;
            const failH = (b.fail / maxCount) * plotH;
            const totalH = okH + failH;
            const label = new Date(hour * 1000).toLocaleString(undefined, { month: '2-digit', day: '2-digit', hour: '2-digit' });
            return (
              <g key={hour} data-hour={hour}>
                <title>{`${label} — ${t('成功')} ${b.ok} / ${t('失败')} ${b.fail}`}</title>
                <rect x={x} y={PAD_T + plotH - totalH} width={barW} height={totalH} rx={2} fill="rgba(47,111,237,0.25)" />
                <rect x={x} y={PAD_T + plotH - okH} width={barW} height={okH} rx={2} fill="#34d399" />
                {b.fail > 0 && <rect x={x} y={PAD_T + plotH - totalH} width={barW} height={failH} rx={2} fill="rgba(239,68,68,0.8)" />}
                {buckets.length <= 14 && (
                  <text x={x + barW / 2} y={H - PAD_B + 12} fontSize={8} fill="var(--text-muted)" textAnchor="middle">{label}</text>
                )}
              </g>
            );
          })}
        </svg>
        <div className="log-timeline-legend">
          <span><i style={{ background: '#34d399' }} />{t('成功')}</span>
          <span><i style={{ background: 'rgba(239,68,68,0.8)' }} />{t('失败')}</span>
          <span className="log-timeline-note">{t('当前页日志按小时聚合')}</span>
        </div>
      </div>
    );
  };

  return (
    <div>
      <PageHeader
        title={t('日志与审计')}
        subtitle={t('请求日志检索与管理员操作审计')}
      />

      {error && <div className="error-message">{error}</div>}

      <Card bodyClassName="">
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center' }}>
          <Button variant={tab === 'requests' ? 'primary' : 'outline'} onClick={() => { setTab('requests'); setPage(1); setView('table'); }}>
            {t('请求日志')}
          </Button>
          {admin && (
            <Button variant={tab === 'audits' ? 'primary' : 'outline'} onClick={() => { setTab('audits'); setPage(1); setView('table'); }}>
              {t('审计日志')}
            </Button>
          )}
          {admin && (
            <Button
              variant={showInflight ? 'primary' : 'outline'}
              onClick={() => {
                const next = !showInflight;
                setShowInflight(next);
                if (next) void loadInflight();
              }}
            >
              {t('在途请求')}
            </Button>
          )}
          {tab === 'requests' && (
            <>
              <Button variant="outline" size="sm" onClick={() => void handleExport('json')} disabled={exporting !== null}>
                {exporting === 'json' ? t('导出中…') : t('导出 JSON')}
              </Button>
              <Button variant="outline" size="sm" onClick={() => void handleExport('csv')} disabled={exporting !== null}>
                {exporting === 'csv' ? t('导出中…') : t('导出 CSV')}
              </Button>
            </>
          )}
          {tab === 'requests' && (
            <div className="ui-tabs" role="tablist" aria-label={t('视图切换')}>
              <button type="button" role="tab" aria-selected={view === 'table'} className={`ui-tab ${view === 'table' ? 'active' : ''}`} onClick={() => setView('table')}>
                {t('表格')}
              </button>
              <button type="button" role="tab" aria-selected={view === 'timeline'} className={`ui-tab ${view === 'timeline' ? 'active' : ''}`} onClick={() => setView('timeline')}>
                {t('时间线')}
              </button>
            </div>
          )}
          {admin && (
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={() => setMasked((v) => !v)}
              title={masked ? t('显示敏感信息') : t('隐藏敏感信息（用户邮箱/供应商名等打码）')}
              style={{ display: 'inline-flex', alignItems: 'center', gap: 4 }}
            >
              {masked ? <EyeOff size={14} /> : <Eye size={14} />}
              {masked ? t('已脱敏') : t('脱敏开关')}
            </button>
          )}
          {/* 页面自动刷新（beta.7）：日志页管 logsAutoRefresh，请求日志 Tab 管 requestsAutoRefresh */}
          <PageAutoRefreshControl taskId={autoRefreshTaskId} />
          {view === 'table' && (
            /* 列设置（公共面板；仅表格视图，时间线视图无列概念） */
            <div className="as-colset" ref={colPanelRef} style={{ position: 'relative', display: 'inline-block' }}>
              <Tooltip label={t('列设置（显隐 / 顺序 / 对齐 / 列宽拖动）')}>
                <button
                  type="button"
                  className={`btn btn-outline btn-sm${colPanelOpen ? ' as-colset-open' : ''}`}
                  onClick={() => setColPanelOpen((v) => !v)}
                  aria-label={t('列设置')}
                >
                  <Columns3 size={14} />
                </button>
              </Tooltip>
              {colPanelOpen && (
                <TableColumnSettings
                  open
                  onClose={() => setColPanelOpen(false)}
                  defs={activeDefs}
                  items={activeColState.items}
                  onChange={commitCols}
                  onReset={resetCols}
                  anchorRef={colPanelRef}
                />
              )}
            </div>
          )}
        </div>
      </Card>

      {/* 在途请求面板：正在跑、尚未落库的请求，可逐条终止 */}
      {admin && showInflight && (
        <Card
          title={t('在途请求（{{n}}）', { n: inflight.length })}
          actions={
            <Button variant="outline" size="sm" onClick={() => void loadInflight()} disabled={inflightLoading}>
              {inflightLoading ? t('刷新中…') : t('刷新')}
            </Button>
          }
        >
          {inflight.length === 0 ? (
            <EmptyState icon="🕊️" message={t('当前没有在途请求')} />
          ) : (
            <div className="table-wrapper table-wrapper-scroll">
              <table className="dense-table">
                <thead>
                  <tr>
                    <th>{t('请求 ID')}</th>
                    <th>{t('模型')}</th>
                    <th>{t('供应商')}</th>
                    <th>{t('已运行')}</th>
                    <th>{t('类型')}</th>
                    <th>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {inflight.map((r) => (
                    <tr key={r.id}>
                      <td className="log-detail-mono">{r.id}</td>
                      <td>{r.model}</td>
                      <td>{r.channel_name || r.channel_id || '—'}</td>
                      <td>{(r.elapsed_ms / 1000).toFixed(1)}s</td>
                      <td>{r.stream ? t('流式') : t('非流式')}</td>
                      <td>
                        <Button variant="outline" size="sm" onClick={() => handleTerminateInflight(r.id)}>
                          {t('终止')}
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      )}

      {/* 筛选/操作条：请求筛选控件仅 requests Tab 显示；删除/清空/计数两个 Tab
          都要有（审计日志同样支持批量删除与清空，此前这些按钮被整体包在
          requests 分支里，导致审计 Tab 勾选后无处可删）。 */}
      <div className="lg-filters">
        {tab === 'requests' && (
          <>
            {/* 时间档位：分段控件（对齐 Agent2API requests-page 的 RANGE_OPTIONS；
                默认 all「全部」= 起止都空，与加这个档位之前的行为一致） */}
            <div className="lg-seg" role="tablist" aria-label={t('时间范围')}>
              {RANGE_OPTIONS.map(([v, label]) => (
                <button
                  key={v}
                  type="button"
                  className={`lg-seg-btn${filters.range === v ? ' active' : ''}`}
                  title={
                    v === 'all'
                      ? t('不限时间（默认）')
                      : v === 'custom'
                        ? t('用下面两个时间输入框自己给区间')
                        : t('点档位即按该区间查询；「近 7 天 / 近 30 天」含今天在内往前数')
                  }
                  onClick={() => applyRange(v)}
                >
                  {t(label)}
                </button>
              ))}
            </div>
            {/* 状态：分段控件（对齐模型管理 / 供应商页） */}
            <div className="lg-seg" role="tablist" aria-label={t('状态筛选')}>
              {(
                [
                  ['', t('全部状态')],
                  ['ok', t('成功')],
                  ['error', t('失败')],
                ] as Array<[string, string]>
              ).map(([v, label]) => (
                <button
                  key={v || 'all'}
                  type="button"
                  className={`lg-seg-btn${filters.status === v ? ' active' : ''}`}
                  onClick={() => applyStatus(v)}
                >
                  {label}
                </button>
              ))}
            </div>
            {admin ? (
              <>
                <div className="lg-search">
                  <Search size={13} className="lg-search-icon" />
                  <input
                    ref={searchRef}
                    className="form-input"
                    value={filters.user}
                    onChange={(e) => setFilters({ ...filters, user: e.target.value })}
                    placeholder={t('按用户 ID 或邮箱过滤')}
                    title={t('用户')}
                    aria-label={t('用户')}
                  />
                </div>
                <div className="lg-search">
                  <Search size={13} className="lg-search-icon" />
                  <input
                    className="form-input"
                    value={filters.model}
                    onChange={(e) => setFilters({ ...filters, model: e.target.value })}
                    placeholder={t('按模型过滤')}
                    title={t('模型')}
                    aria-label={t('模型')}
                  />
                </div>
                <div className="lg-search">
                  <input
                    className="form-input"
                    value={filters.channel}
                    onChange={(e) => setFilters({ ...filters, channel: e.target.value })}
                    placeholder={t('按供应商过滤')}
                    title={t('供应商 ID')}
                    aria-label={t('供应商 ID')}
                  />
                </div>
              </>
            ) : (
              <div className="lg-search">
                <Search size={13} className="lg-search-icon" />
                <input
                  ref={searchRef}
                  className="form-input"
                  value={filters.model}
                  onChange={(e) => setFilters({ ...filters, model: e.target.value })}
                  placeholder={t('按模型过滤')}
                  title={t('模型')}
                  aria-label={t('模型')}
                />
              </div>
            )}
            {/* 客户端过滤：匹配 client_name / client_version（管理员与普通用户均可用） */}
            <div className="lg-search">
              <Search size={13} className="lg-search-icon" />
              <input
                className="form-input"
                value={filters.client}
                onChange={(e) => setFilters({ ...filters, client: e.target.value })}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    if (page === 1) void load();
                    else setPage(1);
                  }
                }}
                placeholder={t('按客户端过滤')}
                title={t('客户端')}
                aria-label={t('客户端')}
              />
            </div>
            {/* 关键词：Enter 即查询（与「查询」按钮同口径） */}
            <div className="lg-search">
              <input
                className="form-input"
                value={filters.keyword}
                onChange={(e) => setFilters({ ...filters, keyword: e.target.value })}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    if (page === 1) void load();
                    else setPage(1);
                  }
                }}
                placeholder={t('模型 / 供应商 / 错误信息')}
                title={t('关键词')}
                aria-label={t('关键词')}
              />
            </div>
            <input
              type="datetime-local"
              className="form-input lg-time"
              value={filters.start}
              // 手改起止 = 自定义区间：档位跟着回到 custom，否则界面上会同时出现
              // 「近 7 天」这颗高亮的档位和一个与它不符的起止时间
              onChange={(e) => setFilters({ ...filters, start: e.target.value, range: 'custom' })}
              aria-label={t('开始时间')}
            />
            <span className="lg-range-sep">~</span>
            <input
              type="datetime-local"
              className="form-input lg-time"
              value={filters.end}
              onChange={(e) => setFilters({ ...filters, end: e.target.value, range: 'custom' })}
              aria-label={t('结束时间')}
            />
            <Button size="sm" onClick={handleSearch}>{t('查询')}</Button>
            {!admin && <span className="lg-hint">{t('仅显示你自己的请求记录')}</span>}
          </>
        )}
        {admin && (
          <>
            <Button
              variant="danger"
              size="sm"
              onClick={handleBatchDelete}
              disabled={selected.size === 0 || batchDeleting}
            >
              <Trash2 size={14} style={{ marginRight: 4 }} />
              {t('删除选中')} {selected.size > 0 ? `(${selected.size})` : ''}
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void handleClearAll()}
              disabled={batchDeleting}
            >
              <Eraser size={14} style={{ marginRight: 4 }} />
              {t('清空全部')}
            </Button>
          </>
        )}
        <span className="lg-filters-count">{t('共 {{count}} 条', { count: total })}</span>
      </div>

      <Card title={`${tab === 'requests' ? t('请求日志') : t('审计日志')} (${total})`}>
        {loading ? (
          <Loading text={t('加载中')} />
        ) : error && logs.length === 0 ? (
          /* 接口失败与「没数据」不可区分的修复：加载失败且无数据时，卡片区
             明确进入错误态并给重试入口（页级 error-message 已展示完整报错，
             此处不重复文案）。有陈旧数据时保留表格 + 页级横幅。 */
          <div style={{ textAlign: 'center', padding: '32px 16px' }}>
            <div className="error-message">{t('日志加载失败，请重试')}</div>
            <Button size="sm" variant="outline" style={{ marginTop: 12 }} onClick={() => void load()}>
              <RefreshCw size={13} style={{ marginRight: 4 }} />
              {t('重试')}
            </Button>
          </div>
        ) : view === 'timeline' && tab === 'requests' ? (
          renderTimeline()
        ) : logs.length === 0 ? (
          <EmptyState message={emptyText} />
        ) : (
          <div className="table-wrapper table-wrapper-scroll">
            <table className="dense-table">
              <colgroup>
                {appliedCols.map((c) => (
                  <col key={c.key} data-col-key={c.key} style={c.width != null ? { width: `${c.width}px` } : undefined} />
                ))}
              </colgroup>
              <thead>
                {tab === 'requests' ? (
                  <tr>
                    {appliedCols.map((c, idx) => {
                      // 末列也给把手（对齐 A2A table-columns.js 的 `.col-grip-end`）：
                      // 默认 right:-4px 会在表格右缘顶出 4px 横滚，所以末列改 right:0 往内让。
                      // 锁定列同理只锁显隐，不锁列宽。
                      const isLast = idx === appliedCols.length - 1;
                      const grip = colResize.gripProps(c.key, {
                        isLast: false,
                        locked: false,
                        defaultWidth: c.width,
                      });
                      const label = logsReqColDefs(admin).find((d) => d.key === c.key)?.label ?? c.key;
                      return (
                        <th
                          key={c.key}
                          style={c.width != null ? { width: `${c.width}px` } : undefined}
                          className={colAlignMap[c.key] === 'right' ? 'ta-right' : colAlignMap[c.key] === 'center' ? 'ta-center' : undefined}
                        >
                          {c.key === 'check' ? (
                            <input
                              type="checkbox"
                              checked={logs.length > 0 && selected.size === logs.length}
                              onChange={toggleSelectAll}
                              aria-label={t('全选')}
                            />
                          ) : (
                            t(label)
                          )}
                          {c.key !== 'check' && (
                            <span
                              className="as-col-grip"
                              title={t('拖动调整列宽 · 双击还原')}
                              style={isLast ? { right: 0 } : undefined}
                              {...grip}
                            />
                          )}
                        </th>
                      );
                    })}
                  </tr>
                ) : (
                  <tr>
                    {appliedCols.map((c, idx) => {
                      // 审计表的表头同样按「末列也有把手」处理（见请求表那段注释）
                      const isLast = idx === appliedCols.length - 1;
                      const grip = colResize.gripProps(c.key, {
                        isLast: false,
                        locked: false,
                        defaultWidth: c.width,
                      });
                      const label = logsAuditColDefs(admin).find((d) => d.key === c.key)?.label ?? c.key;
                      return (
                        <th
                          key={c.key}
                          style={c.width != null ? { width: `${c.width}px` } : undefined}
                          className={colAlignMap[c.key] === 'right' ? 'ta-right' : colAlignMap[c.key] === 'center' ? 'ta-center' : undefined}
                        >
                          {c.key === 'check' ? (
                            <input
                              type="checkbox"
                              checked={logs.length > 0 && selected.size === logs.length}
                              onChange={toggleSelectAll}
                              aria-label={t('全选')}
                            />
                          ) : (
                            t(label)
                          )}
                          {c.key !== 'check' && (
                            <span
                              className="as-col-grip"
                              title={t('拖动调整列宽 · 双击还原')}
                              style={isLast ? { right: 0 } : undefined}
                              {...grip}
                            />
                          )}
                        </th>
                      );
                    })}
                  </tr>
                )}
              </thead>
              <tbody>
                {logs.map((l, i) =>
                  isRequest(l) ? (
                    <tr
                      key={l.id}
                      ref={(el) => { rowRefs.current[i] = el; }}
                      className={`log-row-clickable ${activeIndex === i ? 'kbd-active' : ''}`}
                      onClick={() => setDetail(l)}
                      onMouseEnter={() => setActiveIndex(i)}
                      title={t('点击查看详情')}
                    >
                      {admin && visibleKeySet.has('check') && (
                        <td onClick={(e) => e.stopPropagation()}>
                          <input
                            type="checkbox"
                            checked={selected.has(l.id)}
                            onChange={() => toggleSelect(l.id)}
                            aria-label={t('选择该行')}
                          />
                        </td>
                      )}
                      {admin && visibleKeySet.has('user') && (
                        <td style={{ maxWidth: 190 }}>
                          <span className="log-user-cell">
                            <span className="log-user-avatar">{(userDisplay(l.user_email, l.user_id) || '?').charAt(0).toUpperCase()}</span>
                            <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{userDisplay(l.user_email, l.user_id)}</span>
                          </span>
                        </td>
                      )}
                      {visibleKeySet.has('time') && (
                      <td style={{ whiteSpace: 'nowrap' }}>
                        {fmtTime(l.created_at)}
                        <div>
                          {renderStatusBadge(l.status_code, true)}
                          {sourceLabel(l.key_id) && (
                            <span className="badge badge-info" style={{ fontSize: 10, marginLeft: 4 }} title={l.key_id}>
                              {sourceLabel(l.key_id)}
                            </span>
                          )}
                        </div>
                      </td>
                      )}
                      {visibleKeySet.has('model') && (
                      <td style={{ maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={l.model || ''}>
                        {l.model || '—'}
                        {/* 上游实际模型名（origin_model）仅 admin 可见：后端已对普通用户
                            把 model 重写为请求侧原名并剥离 origin_model，这里再兜一层，
                            避免普通用户看穿实际被哪个上游模型服务。 */}
                        {admin && l.origin_model && l.origin_model !== l.model && (
                          <div style={{ fontSize: 10, color: 'var(--text-muted)' }}>← {l.origin_model}</div>
                        )}
                      </td>
                      )}
                      {visibleKeySet.has('client') && (
                      <td style={{ maxWidth: 160, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                        title={[l.client_name, l.client_version].filter(Boolean).join(' ')}>
                        {l.client_name ? (
                          <>
                            {l.client_name}
                            {l.client_version && (
                              <span style={{ fontSize: 10, color: 'var(--text-muted)' }}> {l.client_version}</span>
                            )}
                          </>
                        ) : (
                          <span style={{ color: 'var(--text-muted)' }}>{t('未记录')}</span>
                        )}
                      </td>
                      )}
                      {admin && visibleKeySet.has('channel') && (
                        <td
                          style={{ maxWidth: 140, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                          title={[l.provider, l.channel_name, l.channel_id ? `#${l.channel_id}` : '']
                            .filter(Boolean)
                            .join(' · ')}
                        >
                          {/* 「供应商」列 = 后端 `provider`（渠道型=协议族 / 账号池型=账号的家）。
                              `channel_name` 是另一个维度（渠道/池的名字），作弱化副行保留；
                              两者都没有时再退到 `channel_id`，最后才是「—」（真的没记录）。
                              老日志没有 `provider` 键 → 逐字回退到改造前的 `channel_name` 显示。 */}
                          {masked ? (
                            mask(l.provider || l.channel_name || l.channel_id)
                          ) : (
                            <>
                              {l.provider || l.channel_name || (l.channel_id ? `#${l.channel_id}` : '—')}
                              {l.provider && l.channel_name && (
                                <div style={{ fontSize: 10, color: 'var(--text-muted)' }}>{l.channel_name}</div>
                              )}
                            </>
                          )}
                        </td>
                      )}
                      {visibleKeySet.has('tokens') && (
                      <td style={{ whiteSpace: 'nowrap', fontSize: 12 }}>
                        <span>{fmtCompact(l.input_tokens ?? 0)}</span>
                        <span style={{ color: 'var(--text-muted)' }}> / </span>
                        <span>{fmtCompact(l.output_tokens ?? 0)}</span>
                      </td>
                      )}
                      {visibleKeySet.has('cost') && (
                      <td style={{ whiteSpace: 'nowrap' }}>{fmtCost(l.cost, lang)}</td>
                      )}
                      {visibleKeySet.has('latency') && (
                      <td style={{ whiteSpace: 'nowrap' }}>{l.latency_ms != null ? `${l.latency_ms}ms` : '—'}</td>
                      )}
                      {visibleKeySet.has('retry') && (
                      <td style={{ whiteSpace: 'nowrap' }}>
                        {l.attempts != null && l.attempts > 1 ? (
                          <span className="badge badge-warning" title={t('共尝试 {{n}} 次（含重试）', { n: l.attempts })}>
                            {l.attempts}
                          </span>
                        ) : (
                          <span style={{ color: 'var(--text-muted)' }}>{l.attempts ?? 1}</span>
                        )}
                      </td>
                      )}
                      {visibleKeySet.has('status') && (
                      <td>
                        {l.status_code ? (
                          <span className={statusKind(l.status_code) === 'ok' ? 'badge badge-success' : 'badge badge-danger'}>
                            {l.status_code}
                          </span>
                        ) : (
                          <span className="badge badge-neutral">—</span>
                        )}
                      </td>
                      )}
                      {admin && visibleKeySet.has('purge') && (
                      <td>
                        {l.purge_applied ? (
                          <span className="badge badge-warning">{t('破甲')}</span>
                        ) : (
                          <span className="badge badge-neutral">—</span>
                        )}
                      </td>
                      )}
                    </tr>
                  ) : (
                    <tr
                      key={l.id}
                      ref={(el) => { rowRefs.current[i] = el; }}
                      className={`log-row-clickable ${activeIndex === i ? 'kbd-active' : ''}`}
                      onClick={() => setDetail(l)}
                      onMouseEnter={() => setActiveIndex(i)}
                      title={t('点击查看详情')}
                    >
                      {admin && visibleKeySet.has('check') && (
                        <td onClick={(e) => e.stopPropagation()}>
                          <input
                            type="checkbox"
                            checked={selected.has(l.id)}
                            onChange={() => toggleSelect(l.id)}
                            aria-label={t('选择该行')}
                          />
                        </td>
                      )}
                      {visibleKeySet.has('admin') && (
                      <td style={{ maxWidth: 170 }}>
                        <span className="log-user-cell">
                          <span className="log-user-avatar">{(userDisplay(l.admin_email, l.admin_id) || '?').charAt(0).toUpperCase()}</span>
                          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{userDisplay(l.admin_email, l.admin_id)}</span>
                        </span>
                      </td>
                      )}
                      {visibleKeySet.has('time') && (
                      <td style={{ whiteSpace: 'nowrap' }}>{fmtTime(l.created_at)}</td>
                      )}
                      {visibleKeySet.has('action') && (
                      <td><code style={{ background: 'var(--card-bg)', padding: '2px 6px', borderRadius: 4 }}>{l.action}</code></td>
                      )}
                      {visibleKeySet.has('target') && (
                      <td style={{ maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={l.target || ''}>{l.target}</td>
                      )}
                      {visibleKeySet.has('change') && (
                      <td style={{ maxWidth: 300, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 11, color: 'var(--text-muted)' }} title={l.after || '—'}>
                        {l.after || '—'}
                      </td>
                      )}
                    </tr>
                  )
                )}
              </tbody>
            </table>
          </div>
        )}

        {/* 分页页脚两个视图都保留：时间线聚合的是「当前页」，页脚是翻页的唯一入口
            （此前只在表格视图渲染，时间线视图无法翻页，只能看到第 1 页）。 */}
        <TablePaginationFooter
          total={total}
          page={page}
          totalPages={totalPages}
          size={pageSize}
          sizes={SERVER_PAGE_SIZES}
          onSizeChange={(n) => { setPageSize(n); setPage(1); }}
          onPageChange={setPage}
        />
      </Card>

      {/* 行点击详情弹窗 */}
      {detail && isRequest(detail) && (
        <RequestLogDetail log={detail} admin={admin} onClose={() => setDetail(null)} />
      )}
      {detail && !isRequest(detail) && (
        <AuditLogDetail log={detail} onClose={() => setDetail(null)} />
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
