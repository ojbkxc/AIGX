/**
 * 路由 —— 页面形态对照 Agent2API v3.5.0-beta.1 的 rail 工作台：
 * 左窄栏五项（配置 / 回放 / 压缩 / 标题 / 视觉）+ 右侧内容区；
 * 能力面结合 AIGX 自己的多规则档案体系（#40）与后端 be-special /
 * be-vision / be-routing 的端点。
 *
 * ── 五项 ↔ 旧 Tabs 能力对应（不许丢功能）───────────────────────
 *  配置：旧 config 档的全部内容 —— 多规则总开关、基础策略（RoutingPanel）、
 *        分层档案（全局/模型级/路由组三态编辑 + 保存/放弃/复制 JSON）、
 *        候选与命中层预演（/routing/queue）、能力需求/指定组预演与逐字段来源
 *        （/routing/preview），**再加**两处：
 *          · 模型覆盖对比（RoutingWorkspace，/routing/workspace）
 *          · 粘滞会话（RouteSticky，旧 sticky 档，原样保留在配置视图底部）
 *  回放：旧 replay 档的 RouteReplayPanel（按请求 ID 回放 + 执行时间线），
 *        **再加**请求流向（RoutingFlow）与请求跟踪（RoutingTrace）。
 *  压缩 / 标题 / 视觉：本波新增的三个特殊路由面板（各含开关、规则表增删改、
 *        模型选择、revision 409 冲突提示）。
 *
 * ── 两条硬约束（别在实现里破坏）──────────────────────────────
 * 1. **不在这里算生效结果**：候选裁剪、命中层、覆盖对比、来源层一律取后端返回。
 * 2. **provenance 不许猜**：后端没给来源时显示「来源未知」，绝不按推算冒充。
 *
 * ── 草稿保持 ───────────────────────────────────────────────
 * 配置视图的草稿存在本组件 state（rail 切换只是显隐，不丢）；三个特殊面板
 * **同时挂载**但只有当前那块拉数据（`active` 控制），所以切走再切回，
 * 未保存草稿仍在 —— 与 A2A 的 useSpecialEditor 口径一致。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Archive,
  Copy,
  Eye,
  Heading,
  History,
  Loader2,
  Plus,
  RefreshCw,
  SlidersHorizontal,
  Trash2,
} from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Badge, Button, Card, EmptyState, Loading, Switch } from '../components/ui';
import RouteReplayPanel, { RouteReplayView } from '../components/RouteReplay';
import RouteSticky from '../components/RouteSticky';
import RoutingPanel from '../components/RoutingPanel';
import RoutingErrorsPanel from '../components/RoutingErrorsPanel';
import ProfileForm from '../components/routing/ProfileForm';
import GroupEditor from '../components/routing/GroupEditor';
import {
  RoutingCanvasGroups,
  RoutingCanvasLayout,
  RoutingConfigShell,
} from '../components/routing/RoutingCanvas';
import {
  isGlobalRunning,
  isGroupRunning,
  isModelRunning,
  useRoutingCanvasActivity,
} from '../components/routing/RoutingCanvasActivity';
import { LayerChip } from '../components/routing/TriStateField';
import {
  PROFILE_FIELD_LABELS,
  NEEDS_FIELDS,
  profileFieldText,
} from '../components/routing/profileSummary';
import CompactRoutes from '../components/routing/special/CompactRoutes';
import TitleRoutes from '../components/routing/special/TitleRoutes';
import VisionCompat from '../components/routing/special/VisionCompat';
import SpecialHits from '../components/routing/special/SpecialHits';
// v1.0.93：图片兼容（原系统设置「聊天」Tab 的 ImageCompatPanel）并入路由「特殊请求路由 → 图片兼容」
import ImageCompatPanel from '../components/ImageCompatPanel';
import RoutingWorkspace from '../components/routing/RoutingWorkspace';
import {
  EMPTY_NEEDS,
  EMPTY_SCOPE,
  PROFILE_FIELDS,
  ROUTING_CAPS,
  emptyProfile,
  isProfileEmpty,
  mergeProfiles,
  profileOverrideCount,
  type Layer,
  type RequestNeeds,
  type RoutingGroup,
  type RoutingPolicyWithProfiles,
  type RoutingPreviewItem,
  type RoutingProfile,
  type RoutingProfiles,
  type RoutingQueueWithLayers,
} from '../types/routing-profile';
import type { RoutingQueueEntryItem, RoutingQueueItem } from '../types';
import './Routing.css';
import './RoutingShell.css';
// 对齐 A2A 新增的页头工具条 / 左栏头零件（本页骨架以外的增量样式，
// 不改 pages/RoutingShell.css 的既有类）
import '../components/routing/RoutingShell.css';

/** 顶部模式分段（对齐 Agent2API：路由配置 / 请求回放） */
type RoutingMode = 'config' | 'replay';
/** 配置视图左栏可切换的面板 */
type ConfigPanel = 'global' | 'errors' | 'compact' | 'vision' | 'title' | 'group';
/** 特殊请求路由三项（照 Agent2API SPECIAL_RAIL 的文案与顺序） */
const SPECIAL_RAIL: Array<{ key: 'compact' | 'vision' | 'title'; label: string; icon: typeof SlidersHorizontal }> = [
  { key: 'compact', label: '压缩上下文', icon: Archive },
  { key: 'vision', label: '图片兼容', icon: Eye },
  { key: 'title', label: '标题生成', icon: Heading },
];
/** 按「严格对齐 A2A」隐藏 AIGX 独有的预演 / 覆盖工作台/Flow/Trace（页面不再出现、菜单无入口）；代码保留，待后续单独清理以免本轮大删引入半死态。 */
const SHOW_ROUTING_ENHANCED = false;

/** 层栏选中项：`global` / `m:<模型名>` / `groups` */
type LayerSel = 'global' | 'groups' | `m:${string}`;

const STATUS_HINT: Record<string, string> = {
  open: '断路器打开',
  halfopen: '半开试探',
  closed: '正常',
};

/** 空档案容器（后端未返回 profiles 时的本地草稿起点） */
function emptyProfiles(): RoutingProfiles {
  return { global: emptyProfile(), models: {}, groups: [] };
}

/** 稳定的空组列表引用（useMemo 依赖不能每次新建数组，否则每渲染都重算） */
const EMPTY_GROUPS: RoutingGroup[] = [];

function statusOf(err: unknown): number | undefined {
  return (err as { status?: number } | null)?.status;
}

/** 秒/毫秒自适应（<1e11 视为秒，与账号页 / 回放页同一口径） */
function toMillis(v: number | undefined): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  return v < 1e11 ? v * 1000 : v;
}

/** 只认有限数字（日志/在途表的字段是 unknown，不做隐式转换） */
function replayNum(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}

/**
 * 回放行的「账号 / 提供商」格文本（对齐 A2A `traceProvider` 的 `providerLabel` 优先）。
 *
 * A2A 显示 `provider · accountName`；AIGX 的请求日志只有渠道维度（`RequestLog`
 * 无 account_id），所以这一格显示 `channel_name` —— 管理员视角原样下发
 * （`api/admin/logs.rs` 的 admin 分支不剥离），名字比 `#id` 可读；
 * 没有名字才回落 `provider` → `#channel_id`。**不编造账号名。**
 */
function replayChannelText(row: Record<string, unknown>): string | undefined {
  if (typeof row.channel_name === 'string' && row.channel_name.trim()) return row.channel_name;
  if (typeof row.provider === 'string' && row.provider.trim()) return row.provider;
  return row.channel_id != null ? `#${String(row.channel_id)}` : undefined;
}

/**
 * 回放左栏的行形状（列集照 A2A routing-request-list 的 ReplayRow 落地）。
 *
 * 数据来自 `/request-logs`：能取到 model / status_code / latency_ms / created_at /
 * channel_id / channel_name / request_id；「仅在途」档走 `/logs/inflight`
 * （同一列结构 + `running` 标记）。**取不到** A2A 的 accountId/accountName
 * （AIGX 的 RequestLog 没有账号维度，只有渠道）—— 该格显示渠道名，
 * 缺口在交付清单里点名。
 */
interface ReplayLogRow {
  /** 内部 id（回放明细端点按它查） */
  id: string;
  request_id?: string;
  model?: string;
  status_code?: number;
  latency_ms?: number;
  created_at?: number;
  channel_id?: string;
  /** A2A 行内的账号/提供商格：优先 `channel_name`，回落 `#channel_id` */
  provider?: string;
  /** 在途标记（`仅在途` 档的行）：A2A 的 `scope=running` 同一语义 */
  running?: boolean;
}

/** 时间范围档位（A2A：5 / 10 / 30 / 60 分钟；本仓多一档「全部时间」，因为日志是历史归档） */
const REPLAY_WINDOWS = [5, 10, 30, 60] as const;
/** 每页条数（A2A 固定 30） */
const REPLAY_PAGE_SIZE = 30;

export default function Routing(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [mode, setMode] = useState<RoutingMode>('config');
  const [panel, setPanel] = useState<ConfigPanel>('global');
  /**
   * 回放左栏「最近请求」的行。
   *
   * 列集照 A2A routing-request-list 的行来：模型 / 状态 / 时间 / 账号（提供商）+
   * 耗时。数据源是本仓 `/request-logs`（服务端支持 keyword / model / status /
   * start / size 过滤），不是 A2A 的 flow 端点 —— 列结构按 A2A 落，取不到的
   * 字段（账号名、在途标记）先留空/占位，缺口记在交付清单里。
   */
  const [replayRows, setReplayRows] = useState<ReplayLogRow[]>([]);
  const [replayId, setReplayId] = useState<string | null>(null);
  /** 回放左栏筛选（对齐 A2A RoutingRequestList：搜索 + 请求模型 + 时间范围 + 仅失败 + 翻页） */
  const [replaySearch, setReplaySearch] = useState('');
  const [replayModel, setReplayModel] = useState('');
  const [replayStatus, setReplayStatus] = useState('');
  const [replayMinutes, setReplayMinutes] = useState<number>(0);
  /**
   * 「仅在途」档（对齐 A2A `routing-request-list.tsx` 的 `scope: running`）。
   *
   * A2A 的该档查的是 `/routing/flow?scope=running`（进程内在途表）；AIGX 的
   * 回放左栏走的是自己的请求日志归档（历史），在途不在那份归档里 —— 故这里
   * 用同一语义的 `/api/logs/inflight`（管理面既有的在途表，日志页「终止」按钮
   * 的数据源），渲染成同一列结构并标「在途」。
   *
   * 为什么不是把数据源整体换成 `/routing/flow`：那份端点只覆盖 5/10/30/60 分钟
   * 窗口，而本仓日志是**历史归档**（有「全部时间」档），换过去会丢掉长历史；
   * 差异记在交付清单里，语义（在途 / 已完成、同一列结构）两边一致。
   */
  const [replayRunningOnly, setReplayRunningOnly] = useState(false);
  const [replayPage, setReplayPage] = useState(0);
  const [replayMatched, setReplayMatched] = useState(0);
  const [replayLoading, setReplayLoading] = useState(false);
  /**
   * 回放静默轮询的节拍（对齐 A2A 的 3s 定时器）：每跳一次让列表 effect 重跑。
   *
   * 单独一个 state 而不是在 effect 里 `setInterval` 直接调请求函数：列表
   * 请求的依赖（筛选/分页）有六个，把定时器塞进同一个 effect 会在每次筛选
   * 变化时重建定时器，且失败重试的时序会和筛选变化纠缠。这里只让节拍号当
   * 一个额外依赖 —— 语义仍是「同样的筛选，再读一次」。
   */
  const [replayTick, setReplayTick] = useState(0);
  /** 已被列表 effect 消费的节拍号（判「这次重跑是轮询还是筛选变化」） */
  const replayTickRef = useRef(0);
  const [replayError, setReplayError] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [policy, setPolicy] = useState<RoutingPolicyWithProfiles | null>(null);
  /** 后端未返回 profiles（契约不符的旧版本）；正常后端恒返回空档案容器 */
  const [noProfiles, setNoProfiles] = useState(false);

  const [enabledBusy, setEnabledBusy] = useState(false);

  // 档案草稿（本页编辑，未落库）—— 存在本组件 state：rail 切换不丢
  const [profiles, setProfiles] = useState<RoutingProfiles | null>(null);
  const [savedProfiles, setSavedProfiles] = useState<RoutingProfiles | null>(null);
  const [savingProfiles, setSavingProfiles] = useState(false);
  /** 档案保存端点是否就绪（首次 404/405 后置 false，避免反复试错） */
  const [profilesEndpointReady, setProfilesEndpointReady] = useState(true);
  const [showJson, setShowJson] = useState(false);

  /** 错误处理面板的「有未保存修改」（由面板上报；面板卸载即清零） */
  const [errorsDirty, setErrorsDirty] = useState(false);
  /** rail 头「新增」按钮的落点：把焦点交给 rail 底部的模型档案输入框 */
  const addModelRef = useRef<HTMLInputElement | null>(null);

  // 层选择
  const [layerSel, setLayerSel] = useState<LayerSel>('global');
  const [newModel, setNewModel] = useState('');
  /** rail 点中的路由组（对齐 A2A：点是条目 → 右侧只展开那一个组） */
  const [groupSel, setGroupSel] = useState('');

  // 预览：候选与命中层（走既有 /api/routing/queue）
  const [queueModel, setQueueModel] = useState('');
  const [queue, setQueue] = useState<(RoutingQueueItem & RoutingQueueWithLayers) | null>(null);
  const [queueLoading, setQueueLoading] = useState(false);

  // 预览：能力需求 / 指定组（走 POST /api/routing/preview）
  const [pvModel, setPvModel] = useState('');
  const [pvNeeds, setPvNeeds] = useState<RequestNeeds>({ ...EMPTY_NEEDS });
  const [pvGroup, setPvGroup] = useState('');
  const [pv, setPv] = useState<RoutingPreviewItem | null>(null);
  const [pvLoading, setPvLoading] = useState(false);
  /** 预览端点未就绪（404/405）—— 不谎报，直接告知 */
  const [pvNotReady, setPvNotReady] = useState(false);

  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const applyPolicy = useCallback((p: RoutingPolicyWithProfiles): void => {
    setPolicy(p);
    if (p.profiles) {
      setProfiles(p.profiles);
      setSavedProfiles(p.profiles);
      setNoProfiles(false);
    } else {
      const blank = emptyProfiles();
      setProfiles(blank);
      setSavedProfiles(blank);
      setNoProfiles(true);
    }
  }, []);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getRoutingPolicy();
      if (!mounted.current) return;
      if (res?.data) applyPolicy(res.data);
    } catch (err) {
      if (!mounted.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, [applyPolicy]);

  useEffect(() => {
    void load();
  }, [load]);

  // 进入回放模式（或改筛选）时拉「最近请求」：列结构对齐 A2A routing-request-list，
  // 过滤交给服务端（keyword / model / status / start / size），不前端假装筛选。
  useEffect(() => {
    if (mode !== 'replay') return undefined;
    let alive = true;
    /**
     * 静默重读：`replayTick` 只在**轮询**触发时变，所以「这次 effect 是不是
     * 轮询引起的」可由「节拍号是否已被消费」判定 —— 轮询不亮加载态
     * （A2A 的 3s 定时器同样是静默刷新），否则列表每 3 秒闪一次「读取中…」。
     */
    const polled = replayTickRef.current !== replayTick;
    if (polled) replayTickRef.current = replayTick;
    if (!polled) setReplayLoading(true);
    setReplayError('');
    void (async () => {
      try {
        // 「仅在途」档走在途表（A2A 同一档位语义：还没落终态的请求）。
        // 在途表后端只提供全量列表，筛选在前端做（条数天然很少：进程内正在跑的）。
        if (replayRunningOnly) {
          const res = await api.listInflightRequests();
          if (!alive) return;
          const inflight = (res?.data ?? []) as unknown as Array<Record<string, unknown>>;
          const all: ReplayLogRow[] = inflight
            .map((row) => ({
              id: String(row.id ?? ''),
              request_id: typeof row.request_id === 'string' ? row.request_id : undefined,
              model: typeof row.model === 'string' ? row.model : undefined,
              // 在途没有终态状态码：0 = 未定论（与日志的 `未记录` 同一口径）
              status_code: 0,
              latency_ms: replayNum(row.elapsed_ms),
              // 在途表的时刻是 unix 秒；列结构同日志行（ms 由 toMillis 适配）
              created_at: replayNum(row.started_at),
              channel_id: row.channel_id != null ? String(row.channel_id) : undefined,
              provider: replayChannelText(row),
              running: true,
            }))
            .filter((x) => x.id)
            .filter((x) => !replayModel || x.model === replayModel)
            .filter((x) => !replaySearch.trim() || `${x.model ?? ''} ${x.id} ${x.request_id ?? ''} ${x.provider ?? ''}`.includes(replaySearch.trim()));
          setReplayRows(all);
          setReplayMatched(all.length);
          return;
        }
        const params: Record<string, string | number> = {
          page: replayPage + 1,
          size: REPLAY_PAGE_SIZE,
        };
        if (replaySearch.trim()) params.keyword = replaySearch.trim();
        if (replayModel) params.model = replayModel;
        if (replayStatus) params.status = replayStatus;
        if (replayMinutes > 0) {
          params.start = Math.floor((Date.now() - replayMinutes * 60_000) / 1000);
        }
        const res = await api.listRequestLogs(params);
        if (!alive) return;
        const rows: ReplayLogRow[] = (res?.data ?? [])
          .map((raw) => {
            const row = raw as Record<string, unknown>;
            return {
              id: String(row.id ?? row.request_id ?? ''),
              request_id: typeof row.request_id === 'string' ? row.request_id : undefined,
              model: typeof row.model === 'string' ? row.model : undefined,
              status_code: replayNum(row.status_code),
              latency_ms: replayNum(row.latency_ms),
              created_at: replayNum(row.created_at),
              channel_id: row.channel_id != null ? String(row.channel_id) : undefined,
              provider: replayChannelText(row),
            };
          })
          .filter((x) => x.id);
        setReplayRows(rows);
        setReplayMatched(typeof res?.total === 'number' ? res.total : rows.length);
      } catch (err) {
        if (alive) setReplayError(err instanceof Error ? err.message : String(err));
      } finally {
        if (alive) setReplayLoading(false);
      }
    })();
    return () => { alive = false; };
  }, [mode, replayPage, replaySearch, replayModel, replayStatus, replayMinutes, replayRunningOnly, replayTick]);

  /**
   * 进入回放模式后自动选中最新一条（对齐 A2A `routing-request-list.tsx`：
   * `!selectedKey && entries.length → onOpenRequest(entries[0])`）。
   *
   * 为什么要有：右栏在没选中时只显示一句「选择左侧请求查看选路过程」——
   * 而「打开回放」的人几乎总是先看最近一次，让他在空态里再点一下是白跑的
   * 一步（A2A 已经把它省掉）。
   */
  useEffect(() => {
    if (mode !== 'replay' || replayId || replayLoading) return;
    if (!replayRows.length) return;
    setReplayId(replayRows[0].id);
  }, [mode, replayId, replayLoading, replayRows]);

  /**
   * 回放的静默轮询（对齐 A2A 的两个 3s/5s 定时器：列表 3s、模型候选 5s）。
   *
   * 只在回放模式、页面可见时打；`replayPage`/筛选不变就原地刷新 —— 这样
   * 「正在跑的请求」跑完会自己变成终态，而不用手动点刷新。
   * 依赖里放 `replayRunningOnly` 等筛选：换筛选立刻停旧轮询（effect 重跑）。
   */
  useEffect(() => {
    if (mode !== 'replay') return undefined;
    const timer = window.setInterval(() => {
      if (document.hidden) return;
      setReplayTick((n) => n + 1);
    }, 3_000);
    return () => window.clearInterval(timer);
  }, [mode]);

  /** 总开关：PUT /api/routing 是读-改-写，只传 enabled 不碰其他字段 */
  const toggleEnabled = async (next: boolean): Promise<void> => {
    setEnabledBusy(true);
    try {
      const res = await api.saveRoutingEnabled(next);
      if (res?.data) applyPolicy(res.data as RoutingPolicyWithProfiles);
      addToast(
        next
          ? t('多规则已启用：下一请求即按档案选路')
          : t('多规则已关闭：档案全部短路，行为与旧版一致'),
      );
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      if (mounted.current) setEnabledBusy(false);
    }
  };

  const dirty = useMemo(
    () => JSON.stringify(profiles) !== JSON.stringify(savedProfiles),
    [profiles, savedProfiles],
  );

  const groupIds = useMemo(() => (profiles?.groups ?? []).map((g) => g.id), [profiles]);

  /**
   * 「内置默认」档案：值是后端 `resolve_profile` 的**基座**。
   * strategy / sticky 取自基础策略现值（后端基座就是 policy 的值，不是枚举默认）。
   * 仅用于「继承会得到什么」的提示，不是判定口径。
   */
  const baseProfile: RoutingProfile = useMemo(
    () => ({
      scope: { ...EMPTY_SCOPE },
      strategy: policy?.strategy ?? 'weighted',
      sticky: policy?.sticky ?? 'auto',
      needs: { ...EMPTY_NEEDS },
      sort_rules: [],
      default_group: null,
    }),
    [policy?.strategy, policy?.sticky],
  );

  const modelNames = useMemo(
    () => Object.keys(profiles?.models ?? {}).sort((a, b) => a.localeCompare(b)),
    [profiles],
  );

  /** rail 条目总数（对齐 A2A RoutingRail：2 个默认规则项 + 特殊三项 + 模型 + 路由组） */
  const railItemCount = 2 + SPECIAL_RAIL.length + modelNames.length + (profiles?.groups.length ?? 0);

  const focusAddModel = (): void => {
    addModelRef.current?.focus();
  };

  /**
   * 回放左栏的筛选候选：只取这一页里出现过的模型（A2A 的候选清单同样是
   * 「窗口内请求过的模型」，没出现过的不列出）。过滤本身在服务端做。
   */
  const replayModels = useMemo(() => {
    const seen = new Set<string>();
    replayRows.forEach((r) => { if (r.model) seen.add(r.model); });
    return [...seen].sort((a, b) => a.localeCompare(b));
  }, [replayRows]);

  /** 翻页：A2A 是 30 条一页 + 上一页/下一页 + 「n / m」 */
  const replayPages = Math.max(1, Math.ceil(replayMatched / REPLAY_PAGE_SIZE));
  const replayCanPrev = replayPage > 0 && !replayLoading;
  const replayCanNext = !replayLoading && (replayPage + 1) * REPLAY_PAGE_SIZE < replayMatched;

  /** 页头黄标计数：档案草稿 + 错误处理草稿（两个独立的保存域，各算一处） */
  const dirtyCount = (dirty ? 1 : 0) + (errorsDirty ? 1 : 0);

  /**
   * 「重新读取」的确认（对齐 A2A reloadConfig：有未保存草稿时先确认再丢弃）。
   * 弹窗用本页既有的 ConfirmDialog 实例，不另起一套。
   */
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const reloadWithConfirm = (): void => {
    if (mode === 'replay') {
      void load();
      return;
    }
    if (dirtyCount === 0) {
      void load();
      return;
    }
    setConfirmState({
      title: t('重新读取配置'),
      message: t('重新读取会丢弃未保存的修改（档案草稿 / 错误处理草稿）。确认重新读取？'),
      confirmText: t('重新读取'),
      danger: true,
      onConfirm: () => {
        void load();
      },
    });
  };

  /**
   * 左栏逐条目的「有未保存修改」（对齐 A2A RoutingRail 的 dirtyDot(per item)：
   * 全局默认 / 每个模型档案 / 每个路由组各自一枚黄点，而不是整栏一枚）。
   * 判据是「该层档案与已保存值不同」—— 与整体 dirty 同一把尺，只是分到层。
   */
  const sameLayer = (a: unknown, b: unknown): boolean => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
  const globalDirty = Boolean(profiles && savedProfiles && !sameLayer(profiles.global, savedProfiles.global));
  const modelLayerDirty = (name: string): boolean =>
    Boolean(profiles && savedProfiles && !sameLayer(profiles.models[name], savedProfiles.models[name]));
  const groupLayerDirty = (id: string): boolean =>
    Boolean(
      profiles && savedProfiles
      && !sameLayer(
        profiles.groups.find((g) => g.id === id),
        savedProfiles.groups.find((g) => g.id === id),
      ),
    );

  /** 粘滞会话卡片在配置视图底部，页头「会话粘滞」用它定位（A2A 是弹窗，AIGX 无弹窗体系） */
  const stickyRef = useRef<HTMLDivElement | null>(null);
  const scrollToSticky = (): void => {
    setMode('config');
    // 卡片只在全局/路由组视图渲染，先切回全局默认再滚
    setPanel('global');
    window.setTimeout(() => {
      stickyRef.current?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }, 0);
  };

  /**
   * 策略 / 粘滞的人话标签（对齐 A2A routing-ui 的 strategyLabel / stickyLabel 职责）。
   * 取值用 AIGX 自己的策略枚举，但**文案与 A2A 同义同词**：`session` 在 A2A 叫
   * 「会话内」，本仓的 6h 窗口是同一档位的不同时长参数，故用同一个词，
   * 时长差异靠 tooltip / 说明表达，不靠换名字（换名字等于同一概念两种叫法）。
   */
  const strategyText = (s: string | undefined): string => {
    switch (s) {
      case 'weighted': return t('加权随机');
      case 'round_robin': return t('组内轮询');
      case 'ranked': return t('多因子调度');
      default: return s ? s : t('未记录');
    }
  };
  const stickyText = (s: string | undefined): string => {
    switch (s) {
      case 'auto': return t('自动');
      case 'session': return t('会话内');
      case 'turn': return t('每轮');
      case 'off': return t('关闭');
      default: return s ? s : t('未记录');
    }
  };

  /** 页头那行状态摘要（A2A routing-page-policy 同位置同分派口径） */
  const policyLine = ((): string => {
    if (mode === 'replay') return t('历史只读，不影响当前配置');
    if (panel === 'errors') return t('全局错误规则 · 所有路由共用');
    if (panel === 'compact' || panel === 'vision' || panel === 'title') {
      return t('特殊请求路由 · 只改写模型，不改选账号规则');
    }
    return `${strategyText(policy?.strategy)} · ${t('粘滞')} ${stickyText(policy?.sticky)}`;
  })();

  const modelKey = layerSel.startsWith('m:') ? layerSel.slice(2) : null;

  /** 当前选中层要编辑的档案 */
  const currentProfile: RoutingProfile | null = useMemo(() => {
    if (!profiles) return null;
    if (layerSel === 'global') return profiles.global;
    if (modelKey != null) return profiles.models[modelKey] ?? emptyProfile();
    return null;
  }, [profiles, layerSel, modelKey]);

  /** 下层合并结果：全局层之下是内置默认；模型级之下是「内置默认 + global」 */
  const inheritedProfile: RoutingProfile = useMemo(() => {
    if (layerSel === 'global' || !profiles) return baseProfile;
    return mergeProfiles(baseProfile, profiles.global);
  }, [layerSel, profiles, baseProfile]);

  /* ── 配置画布（对齐 A2A RoutingConfigShell / RoutingCanvasLayout）────────
   * 点阵底 + SVG 连线只用于「账号路由」这两块（全局/模型级层、路由组）；
   * 特殊请求路由与错误处理走 plain —— 去点阵、不画线，与 A2A
   * `RoutingConfigShell({ plain })` 同名同语义。
   *
   * 画布上的「连通」不是装饰：判据是本层**真的**引用了哪个组
   * （全局/模型级的 `default_group`，或正在编辑的那个组）。
   * 「流动」只认在途请求能归因到的那一层；归不到的（成员槽位行 —— 日志没有
   * 账号标识）一律静态，不假装有请求在跑（见 RoutingCanvasActivity.ts 的口径）。 */
  const canvasActive = mode === 'config' && (panel === 'global' || panel === 'group');
  const activity = useRoutingCanvasActivity(canvasActive);
  const groupList = profiles?.groups ?? EMPTY_GROUPS;
  const activeGroupId = panel === 'group' ? groupSel || null : null;
  const activeGroup = activeGroupId
    ? groupList.find((g) => g.id === activeGroupId) ?? null
    : null;

  /** 本层真正引用到的组（连线的 `data-route-connected` 判据） */
  const connectedGroupIds = useMemo(() => {
    const ids = new Set<string>();
    if (activeGroupId) {
      if (activeGroup) ids.add(activeGroup.id);
      return ids;
    }
    const referenced = currentProfile?.default_group?.trim();
    if (referenced) ids.add(referenced);
    return ids;
  }, [activeGroupId, activeGroup, currentProfile]);

  /** 有在途请求落到的组（连线的 `data-route-running` 判据） */
  const runningGroupIds = useMemo(() => {
    const ids = new Set<string>();
    if (!canvasActive) return ids;
    for (const group of groupList) {
      if (isGroupRunning(activity, group, profiles)) ids.add(group.id);
    }
    return ids;
  }, [canvasActive, activity, groupList, profiles]);

  /** 源卡是否有在途：组面板看该组；模型级看本模型；全局层只认没独立档案的模型 */
  const sourceRunning =
    canvasActive &&
    (activeGroup
      ? isGroupRunning(activity, activeGroup, profiles)
      : modelKey != null
        ? isModelRunning(activity, modelKey)
        : isGlobalRunning(activity, modelNames));

  const patchGroupList = (next: RoutingGroup[]): void => {
    if (!profiles) return;
    setProfiles({ ...profiles, groups: next });
  };

  const setModelProfile = (name: string, next: RoutingProfile): void => {
    if (!profiles) return;
    setProfiles({ ...profiles, models: { ...profiles.models, [name]: next } });
  };

  const addModel = (): void => {
    const name = newModel.trim();
    if (!profiles || !name) return;
    if (profiles.models[name]) {
      addToast(t('该模型名已存在档案'), 'error');
      return;
    }
    if (modelNames.length >= ROUTING_CAPS.MAX_MODELS) {
      addToast(t('模型档案数已达上限 {{cap}}', { cap: ROUTING_CAPS.MAX_MODELS }), 'error');
      return;
    }
    setProfiles({ ...profiles, models: { ...profiles.models, [name]: emptyProfile() } });
    setNewModel('');
    setLayerSel(`m:${name}`);
  };

  const removeModel = (name: string): void => {
    if (!profiles) return;
    const next = { ...profiles.models };
    delete next[name];
    setProfiles({ ...profiles, models: next });
    setLayerSel('global');
  };

  const saveProfiles = async (): Promise<void> => {
    if (!profiles) return;
    setSavingProfiles(true);
    try {
      await api.saveRoutingProfiles(profiles);
      setSavedProfiles(profiles);
      setProfilesEndpointReady(true);
      addToast(t('路由档案已保存（下一请求即生效）'));
      void load();
    } catch (err) {
      const st = statusOf(err);
      if (st === 404 || st === 405) {
        setProfilesEndpointReady(false);
        addToast(t('档案保存端点尚未就绪（404/405）：本次修改只在本页，未写入服务端'), 'error');
      } else {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      }
    } finally {
      if (mounted.current) setSavingProfiles(false);
    }
  };

  const copyJson = async (): Promise<void> => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(profiles, null, 2));
      addToast(t('档案 JSON 已复制到剪贴板'));
    } catch {
      addToast(t('复制失败：浏览器未授权剪贴板'), 'error');
    }
  };

  /** 候选与命中层：复用网页版预演端点（数据面同一序列，不自己算） */
  const loadQueue = async (): Promise<void> => {
    const m = queueModel.trim();
    if (!m) return;
    setQueueLoading(true);
    try {
      const res = await api.getRoutingQueue(m);
      setQueue(res?.data ?? null);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      if (mounted.current) setQueueLoading(false);
    }
  };

  /** 能力需求 / 指定组预演：POST /api/routing/preview */
  const runPreview = async (): Promise<void> => {
    const m = pvModel.trim();
    if (!m) return;
    setPvLoading(true);
    try {
      const res = await api.previewRouting({
        model: m,
        needs: pvNeeds,
        group_id: pvGroup.trim() || null,
      });
      setPv(res?.data ?? null);
      setPvNotReady(false);
    } catch (err) {
      const st = statusOf(err);
      if (st === 404 || st === 405) {
        setPvNotReady(true);
        setPv(null);
      } else {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      }
    } finally {
      if (mounted.current) setPvLoading(false);
    }
  };

  if (loading) return <Loading text={t('读取路由配置')} />;

  const enabled = policy?.enabled === true;

  const renderCandidate = (c: RoutingQueueEntryItem, i: number): JSX.Element => (
    <li key={c.id}>
      {i === 0 ? <strong>{`${i + 1}. ${c.name}${t('（队首）')}`}</strong> : `${i + 1}. ${c.name}`}
      <span className="rt-muted form-hint">
        {` · ${c.provider} · ${t(STATUS_HINT[c.state] ?? c.state)}`}
      </span>
    </li>
  );

  const configView = (
    <div>
      {/* ① 总开关 */}
      <Card
        title={t('多规则路由')}
        actions={
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <Badge tone={enabled ? 'success' : 'neutral'}>
              {enabled ? t('已启用') : t('未启用')}
            </Badge>
            <Switch
              checked={enabled}
              disabled={enabledBusy}
              ariaLabel={t('多规则路由总开关')}
              onChange={(v) => void toggleEnabled(v)}
            />
          </div>
        }
      >
        {enabled ? (
          <div className="rt-notice rt-notice-info">
            {t('多规则已启用：请求先按「全局 → 模型级 → 路由组」逐字段合并出生效档案，再按它裁剪候选。')}
          </div>
        ) : (
          <div className="rt-notice rt-notice-warn">
            {t('多规则未启用：行为与旧版完全一致 —— 下面所有档案都被忽略（这是可验证的等价，不是「大概一样」）。档案可以照常编辑，启用后才生效。')}
          </div>
        )}
        {noProfiles ? (
          <div className="rt-notice rt-notice-warn" style={{ marginTop: 8 }}>
            {t('后端未返回 profiles 字段（与契约不符）：本页只能编辑本地草稿，无法读写服务端档案。')}
          </div>
        ) : null}
      </Card>

      {/* ② 基础策略（复用既有 RoutingPanel） */}
      <div style={{ marginTop: 12 }}>
        <RoutingPanel />
      </div>

      {/* ③ 分层档案（草稿存在本组件 state，切 rail 回来仍在） */}
      <div style={{ marginTop: 12 }}>
        <Card
          title={t('分层档案')}
          actions={dirty ? <span className="rt-dirty">{t('有未保存的修改')}</span> : undefined}
        >
          <div className="rt-notice">
            {t('合并口径（逐字段三态）：自左向右取最右侧「不是继承」的那一层的值。全局 → 模型级 → 路由组，后面的层可以只覆盖其中几个字段，其余继续继承。')}
          </div>

          {/*
            画布布局（对齐 A2A RoutingCanvasLayout）：
              左列 = 本层配置（源卡，连线的起点）  右列 = 路由组节点  底行 = 三句图例。
            原来这一块的「层」小 rail 由页面左栏承担（全局默认 / 模型路由 / 自定义路由组），
            两处同义重复，故收进画布一列；层切换仍走左栏，不丢入口。
          */}
          <div style={{ marginTop: 12 }}>
            <RoutingCanvasLayout
              title={
                layerSel === 'groups'
                  ? t('自定义路由组')
                  : layerSel === 'global'
                    ? t('全局默认')
                    : modelKey ?? ''
              }
              note={
                layerSel === 'groups'
                  ? t('只影响这个路由成员，不改变账号启用状态，也不影响其他模型。新增账号需要手动加入。')
                  : layerSel === 'global'
                    ? t('账号启停与账号页同步，立即影响所有路由；策略与排序需保存。')
                    : t('仅影响此路由的后续请求')
              }
              groups={
                <RoutingCanvasGroups
                  groups={groupList}
                  connectedIds={connectedGroupIds}
                  runningIds={runningGroupIds}
                  activeGroupId={activeGroupId}
                  onOpen={(id) => {
                    setPanel('group');
                    setLayerSel('groups');
                    setGroupSel(id);
                  }}
                />
              }
            >
              <div
                className="routing-canvas-source"
                data-route-source
                data-route-running={sourceRunning ? '' : undefined}
              >
              {layerSel === 'groups' ? (
                profiles ? (
                  <GroupEditor groups={profiles.groups} onChange={patchGroupList} focusId={groupSel || null} />
                ) : null
              ) : currentProfile && profiles ? (
                <>
                  {layerSel === 'global' ? (
                    <div className="rt-notice">
                      {t('全局层是所有模型的兜底：这里配的值会被模型级/路由组按字段覆盖。')}
                    </div>
                  ) : (
                    <div className="rt-notice rt-notice-info">
                      {t('模型名请填客户端的请求名 / 别名（不是上游真名）。后端先按请求名精确匹配，未命中再用全局映射名兜底 —— 用别名暴露、上游真名不同的场景下，两个名字都可以建档案。')}
                      {modelKey ? (
                        <div style={{ marginTop: 6 }}>
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => modelKey && removeModel(modelKey)}
                          >
                            <Trash2 size={13} /> {t('删除「{{name}}」的档案', { name: modelKey })}
                          </Button>
                        </div>
                      ) : null}
                    </div>
                  )}

                  <div style={{ marginTop: 10 }}>
                    <ProfileForm
                      value={currentProfile}
                      onChange={(next) => {
                        if (layerSel === 'global') {
                          setProfiles({ ...profiles, global: next });
                        } else if (modelKey != null) {
                          setModelProfile(modelKey, next);
                        }
                      }}
                      inherited={inheritedProfile}
                      inheritSource={layerSel === 'global' ? t('内置默认') : t('全局')}
                      groupIds={groupIds}
                      provenance={pv?.provenance}
                    />
                  </div>

                  {isProfileEmpty(currentProfile) ? (
                    <div className="rt-state-note rt-state-inherit" style={{ marginTop: 10 }}>
                      {t('本层全部字段都是「继承」：这一层等价于不存在（不产生任何覆盖）。')}
                    </div>
                  ) : null}
                </>
              ) : null}
            </div>
            </RoutingCanvasLayout>

              <div className="rt-savebar">
                <Button
                  variant="primary"
                  onClick={() => void saveProfiles()}
                  disabled={!dirty || savingProfiles || !profiles}
                >
                  {savingProfiles ? <Loader2 size={13} className="spin" /> : null}
                  {t('保存档案')}
                </Button>
                <Button
                  variant="outline"
                  onClick={() => {
                    setProfiles(savedProfiles);
                    addToast(t('已放弃未保存的修改'));
                  }}
                  disabled={!dirty}
                >
                  {t('放弃更改')}
                </Button>
                <Button variant="outline" onClick={() => void copyJson()} disabled={!profiles}>
                  <Copy size={13} /> {t('复制 JSON')}
                </Button>
                <Button variant="outline" size="sm" onClick={() => setShowJson(!showJson)}>
                  {showJson ? t('收起 JSON') : t('查看 JSON')}
                </Button>
                {!profilesEndpointReady ? (
                  <span className="rt-dirty">
                    {t('保存端点未就绪：修改只在本页，刷新即丢 —— 可先「复制 JSON」留档')}
                  </span>
                ) : null}
              </div>

              {showJson && profiles ? (
                <textarea
                  className="form-input rt-json"
                  style={{ marginTop: 10 }}
                  readOnly
                  value={JSON.stringify(profiles, null, 2)}
                />
              ) : null}
            </div>
        </Card>
      </div>

      {SHOW_ROUTING_ENHANCED && (<>
      {/* ④ 预览与来源：候选与命中层 */}
      <div style={{ marginTop: 12 }}>
        <Card title={t('候选与命中层')}>
          <div className="rt-notice">
            {t('预演与真实转发走同一套判定，所以这里的结果就是下一个请求会走的路 —— 界面不会自己再算一遍候选。')}
          </div>
          <div style={{ display: 'flex', gap: 8, marginTop: 10, flexWrap: 'wrap' }}>
            <input
              className="form-input"
              style={{ maxWidth: 320 }}
              placeholder={t('输入模型名（请求名），如 gpt-4o')}
              value={queueModel}
              onChange={(e) => setQueueModel(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void loadQueue();
              }}
            />
            <Button variant="outline" onClick={() => void loadQueue()} disabled={queueLoading || !queueModel.trim()}>
              {queueLoading ? <Loader2 size={13} className="spin" /> : null}
              {t('预演')}
            </Button>
          </div>

          {queue ? (
            <div style={{ marginTop: 12 }}>
              <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                <span className="rt-muted">{t('命中层：')}</span>
                {queue.appliedLayers?.length ? (
                  queue.appliedLayers.map((l: Layer) => <LayerChip key={l} layer={l} size="md" />)
                ) : (
                  <span className="rt-muted">{t('（未命中任何档案层，只走内置默认）')}</span>
                )}
                <span className="rt-muted" style={{ marginLeft: 8 }}>
                  {t('生效策略：')}
                  {queue.strategy}
                  {` · `}
                  {t('粘性：')}
                  {queue.sticky}
                </span>
                {queue.skipped &&
                (queue.skipped.provider_not_allowed || queue.skipped.provider_disabled) ? (
                  <span className="rt-muted">
                    {t('被档案裁掉：提供商不在名单 {{a}} 个 · 提供商被排除 {{b}} 个', {
                      a: queue.skipped.provider_not_allowed ?? 0,
                      b: queue.skipped.provider_disabled ?? 0,
                    })}
                  </span>
                ) : null}
              </div>

              {queue.ordered?.length ? (
                <ul style={{ margin: '8px 0', paddingLeft: 18 }}>
                  {queue.ordered.map(renderCandidate)}
                </ul>
              ) : (
                <EmptyState message={t('没有可用候选')} />
              )}

              {queue.blocked && queue.blocked.length > 0 ? (
                <>
                  <div className="form-hint">{t('被挡在门外：')}</div>
                  <ul style={{ margin: '4px 0', paddingLeft: 18 }}>
                    {queue.blocked.map((c) => (
                      <li key={c.id}>{`${c.name} — ${c.reason ?? ''}`}</li>
                    ))}
                  </ul>
                </>
              ) : null}

              {queue.strategy === 'round_robin' ? (
                <div className="form-hint">
                  {t('组内轮询下实际首挑由游标决定，预演以静态排序为准。')}
                </div>
              ) : null}
            </div>
          ) : null}
        </Card>
      </div>

      {/* ⑤ 能力需求 / 指定组预演与逐字段来源 */}
      <div style={{ marginTop: 12 }}>
        <Card title={t('能力需求 / 指定组预演与逐字段来源')}>
          <div className="rt-notice">
            {t('按请求的能力需求（工具/图片/视频）与指定路由组预演，并列出每个字段的取值来自哪一层 —— 「改了全局为什么没生效」看这张表即可自查。')}
          </div>

          <div style={{ display: 'flex', gap: 8, marginTop: 10, flexWrap: 'wrap', alignItems: 'center' }}>
            <input
              className="form-input"
              style={{ maxWidth: 280 }}
              placeholder={t('模型名（请求名）')}
              value={pvModel}
              onChange={(e) => setPvModel(e.target.value)}
            />
            {NEEDS_FIELDS.map(([k, label]) => (
              <label key={k} style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 12.5 }}>
                <input
                  type="checkbox"
                  checked={pvNeeds[k]}
                  onChange={(e) => setPvNeeds({ ...pvNeeds, [k]: e.target.checked })}
                />
                <span>{t(label)}</span>
              </label>
            ))}
            <input
              className="form-input"
              style={{ maxWidth: 200 }}
              list="rt-group-ids-preview"
              placeholder={t('指定组 id（可选）')}
              value={pvGroup}
              onChange={(e) => setPvGroup(e.target.value)}
            />
            <datalist id="rt-group-ids-preview">
              {groupIds.map((g) => (
                <option key={g} value={g} />
              ))}
            </datalist>
            <Button variant="outline" onClick={() => void runPreview()} disabled={pvLoading || !pvModel.trim()}>
              {pvLoading ? <Loader2 size={13} className="spin" /> : null}
              {t('预演')}
            </Button>
          </div>

          {pvNotReady ? (
            <div className="rt-notice rt-notice-warn" style={{ marginTop: 10 }}>
              {t('预演端点尚未就绪（404/405）。就绪后这里会显示：命中层、逐字段来源、候选与被裁原因 —— 全部取自后端的同一次判定，界面不复算。上方「候选与命中层」已可看到候选与命中层。')}
            </div>
          ) : pv ? (
            <div style={{ marginTop: 12 }}>
              <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                <span className="rt-muted">{t('命中层：')}</span>
                {pv.appliedLayers?.length ? (
                  pv.appliedLayers.map((l: Layer) => <LayerChip key={l} layer={l} size="md" />)
                ) : (
                  <span className="rt-muted">{t('（未命中任何档案层，只走内置默认）')}</span>
                )}
                {pv.groupId ? (
                  <Badge tone="info">{t('生效组：{{id}}', { id: pv.groupId })}</Badge>
                ) : null}
              </div>

              {pv.profile ? (
                <div className="rt-table-wrap" style={{ marginTop: 10 }}>
                  <table className="rt-table">
                    <thead>
                      <tr>
                        <th>{t('字段')}</th>
                        <th>{t('生效值')}</th>
                        <th>{t('来源层')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {PROFILE_FIELDS.map((f) => (
                        <tr key={f}>
                          <td>{t(PROFILE_FIELD_LABELS[f])}</td>
                          <td>{profileFieldText(pv.profile as RoutingProfile, f, t)}</td>
                          <td>
                            <LayerChip
                              layer={
                                pv.provenance
                                  ? pv.provenance[f]
                                  : undefined /* 后端未给来源 → 显示「来源未知」而非推测 */
                              }
                            />
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : null}

              {pv.ordered?.length ? (
                <ul style={{ margin: '10px 0 0', paddingLeft: 18 }}>
                  {pv.ordered.map(renderCandidate)}
                </ul>
              ) : null}
            </div>
          ) : null}
        </Card>
      </div>

      {/* ⑥ 模型覆盖对比（工作台，be-routing） */}
      <div style={{ marginTop: 12 }}>
        <RoutingWorkspace active={mode === 'config'} />
      </div>
      </>)}

      {/* ⑦ 粘滞会话（旧 sticky 档，原样保留）—— 页头「会话粘滞」滚动到这里 */}
      <div style={{ marginTop: 12 }} ref={stickyRef}>
        <RouteSticky />
      </div>
    </div>
  );

  /**
   * 回放左栏的行状态点（A2A RoutingRequestState：成功 / 失败 / 运行中 / 已取消）。
   *
   * `status_code` 为 0 或缺失是「未记录」而不是失败：后端
   * `RequestLog.status_code` 是 `u16` + `#[serde(default)]`，被中断/尚未完成
   * 的老日志会落成 0。旧实现只要 `!= null` 就判 failed，把「不知道」显示成
   * 「失败」（与 Logs.tsx 的 `statusKind` 口径冲突，同一行在两页结论相反）。
   */
  const replayRowState = (r: ReplayLogRow): 'success' | 'failed' | 'running' | 'unknown' => {
    if (r.running) return 'running';
    if (r.status_code == null || r.status_code === 0) return 'unknown';
    return r.status_code >= 200 && r.status_code < 400 ? 'success' : 'failed';
  };

  const replayView = (
    <div className="rt-replay-layout">
      <aside className="rt-replay-rail" aria-label={t('最近请求')}>
        {/* 左栏头：标题 + 匹配数 + 读取中（对齐 A2A routing-request-head） */}
        <div className="rt-replay-rail-title">
          <b>{t('最近请求')}</b>
          <span className="rt-count">{replayMatched}</span>
          {replayLoading ? <Loader2 size={12} className="spin" /> : null}
        </div>
        {/*
          筛选（对齐 A2A routing-request-tools 的四件套 + 翻页）：
            搜索请求 / 请求模型 / 时间范围 / 仅失败。
          A2A 的「仅在途」在本仓没有在途标记，改落为「仅失败」（服务端 status=error 过滤）；
          该项与 A2A 语义不同，已记入交付清单。
        */}
        <div className="rt-replay-tools">
          <input
            className="form-input"
            placeholder={t('搜索请求、模型或账号')}
            aria-label={t('搜索请求、模型或账号')}
            value={replaySearch}
            onChange={(e) => { setReplaySearch(e.target.value); setReplayPage(0); }}
          />
          <select
            className="form-input"
            aria-label={t('请求模型')}
            value={replayModel}
            onChange={(e) => { setReplayModel(e.target.value); setReplayPage(0); }}
          >
            <option value="">{replayModels.length ? t('全部模型') : t('全部模型（窗口内没有请求）')}</option>
            {replayModels.map((m) => (
              <option key={m} value={m}>{m}</option>
            ))}
          </select>
          <select
            className="form-input"
            aria-label={t('已完成请求时间范围')}
            value={String(replayMinutes)}
            onChange={(e) => { setReplayMinutes(Number(e.target.value)); setReplayPage(0); }}
          >
            <option value="0">{t('全部时间')}</option>
            {REPLAY_WINDOWS.map((m) => (
              <option key={m} value={m}>{t('最近 {{n}} 分钟', { n: m })}</option>
            ))}
          </select>
          <label className="rt-replay-only">
            <input
              type="checkbox"
              checked={replayStatus === 'error'}
              disabled={replayRunningOnly}
              onChange={(e) => { setReplayStatus(e.target.checked ? 'error' : ''); setReplayPage(0); }}
            />
            <span>{t('仅失败')}</span>
          </label>
          {/*
            「仅在途」（对齐 A2A routing-request-list 的 `Switch「仅在途」`）：
            在途 = 还没落终态的请求，与「仅失败」互斥（同一行不可能既在途又失败）。
            开时另一个置灰，避免出现「两个都开 = 空列表」的死组合。
          */}
          <label className="rt-replay-only">
            <input
              type="checkbox"
              checked={replayRunningOnly}
              onChange={(e) => {
                setReplayRunningOnly(e.target.checked);
                if (e.target.checked) setReplayStatus('');
                setReplayPage(0);
              }}
            />
            <span>{t('仅在途')}</span>
          </label>
        </div>
        {replayError ? (
          <div className="rt-replay-empty" role="alert">{t('列表读取失败：{{msg}}', { msg: replayError })}</div>
        ) : replayRows.length === 0 ? (
          <div className="rt-replay-empty">
            {replayLoading
              ? t('读取中…')
              : replaySearch || replayModel || replayStatus || replayMinutes > 0 || replayRunningOnly
                ? t('没有匹配的请求')
                : t('暂无可回放的请求')}
          </div>
        ) : (
          replayRows.map((r) => {
            const when = toMillis(r.created_at);
            const state = replayRowState(r);
            return (
              <button
                key={r.id}
                type="button"
                className={`rt-replay-item${replayId === r.id ? ' is-on' : ''}`}
                onClick={() => setReplayId(r.id)}
                aria-pressed={replayId === r.id}
                title={[
                  r.model,
                  when != null ? new Date(when).toLocaleString() : '',
                  r.provider ?? '',
                  r.latency_ms != null ? `${r.latency_ms}ms` : '',
                  r.id,
                ].filter(Boolean).join(' · ')}
              >
                <span className="rt-replay-item-model">
                  <span>{r.model || t('（无模型）')}</span>
                  <span className={`rt-replay-state is-${state}`} aria-label={state}>
                    <i aria-hidden="true" />
                    {state === 'success'
                      ? t('成功')
                      : state === 'running'
                        ? t('在途')
                        : state === 'unknown'
                          ? t('未记录')
                          : t('失败')}
                  </span>
                </span>
                <span className="rt-replay-item-id">
                  {[when != null ? new Date(when).toLocaleTimeString() : null, r.provider, r.request_id || r.id]
                    .filter(Boolean)
                    .join(' · ')}
                </span>
              </button>
            );
          })
        )}
        {/* 翻页（A2A routing-request-foot：上一页 / n / m / 下一页） */}
        <div className="rt-replay-foot">
          <Button
            variant="outline"
            size="sm"
            disabled={!replayCanPrev}
            onClick={() => setReplayPage((p) => Math.max(0, p - 1))}
          >
            {t('上一页')}
          </Button>
          <span>{`${replayPage + 1} / ${replayPages}`}</span>
          <Button
            variant="outline"
            size="sm"
            disabled={!replayCanNext}
            onClick={() => setReplayPage((p) => p + 1)}
          >
            {t('下一页')}
          </Button>
        </div>
      </aside>
      <section className="rt-replay-main">
        {replayId ? (
          <RouteReplayView requestId={replayId} />
        ) : (
          <RouteReplayPanel />
        )}
      </section>
    </div>
  );

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('路由')}</h1>
          <p>
            {t('供应商选路的策略、分层档案（全局 / 按模型 / 路由组）与预演；压缩 / 标题 / 视觉兼容等特殊路由；保存即热生效，无需重启。')}
          </p>
        </div>
        {/* 刷新/重新读取收在下方工具条（与 A2A routing-page-bar 的动作位一致），
            这里不再重复一枚同义按钮 */}
      </div>

      {error && <div className="error-message">{error}</div>}

      {/*
        顶部工具条（对齐 A2A routing-page-bar）：模式分段 + 一行策略摘要 + 右侧动作。
        A2A 的「请求日志」出口在 AIGX 没有独立请求日志页跳转需求（回放左栏即列表），
        故只保留「会话粘滞」与「刷新 / 重新读取」两枚动作。
      */}
      <div className="rt-pagebar">
        <div className="rt-modebar" role="tablist" aria-label={t('路由模式')}>
          <button
            type="button"
            role="tab"
            aria-selected={mode === 'config'}
            className={`rt-mode-btn${mode === 'config' ? ' is-on' : ''}`}
            onClick={() => setMode('config')}
          >
            <SlidersHorizontal size={14} aria-hidden="true" /> {t('路由配置')}
            {dirtyCount > 0 ? (
              <span className="rt-mode-count" title={t('有未保存的修改')}>{dirtyCount}</span>
            ) : null}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode === 'replay'}
            className={`rt-mode-btn${mode === 'replay' ? ' is-on' : ''}`}
            onClick={() => setMode('replay')}
          >
            <History size={14} aria-hidden="true" /> {t('请求回放')}
          </button>
        </div>

        <span className="rt-pagebar-policy">{policyLine}</span>

        <div className="rt-pagebar-actions">
          <Button variant="outline" size="sm" onClick={scrollToSticky}>
            {t('会话粘滞')}
          </Button>
          <Button variant="outline" size="sm" onClick={reloadWithConfirm} disabled={loading}>
            <RefreshCw size={13} /> {mode === 'replay' ? t('刷新') : t('重新读取')}
          </Button>
        </div>
      </div>

      {mode === 'replay' ? replayView : (
      <div className="routing-config-view">
      {/* 画布外壳（对齐 A2A RoutingConfigShell）：账号路由带点阵底 + 连线层；
          特殊请求路由 / 错误处理走 plain —— 去点阵、不挂连线层 */}
      <RoutingConfigShell plain={!canvasActive}>
      <div className="rt-shell">
        <nav className="rt-shell-rail" aria-label={t('路由分区')}>
          {/* rail 头：标题 + 条目总数 + 新增（对齐 A2A RoutingRail 的 SidebarHeader） */}
          <div className="rt-rail-head">
            <b>{t('路由配置')}</b>
            <span className="rt-count">{railItemCount}</span>
            <Button
              variant="outline"
              size="sm"
              className="rt-rail-add-btn"
              title={t('新增路由配置')}
              aria-label={t('新增路由配置')}
              onClick={focusAddModel}
            >
              <Plus size={13} />
            </Button>
          </div>
          <div className="rt-rail-title">{t('默认规则')}</div>
          <button
            type="button"
            // data-route-scope / data-active：画布连线层的「作用域 → 源卡」起点判据
            // （A2A 用 `.routing-config-scope-item[data-active]`，本仓挂在 rail 条目上）
            data-route-scope="global"
            data-active={panel === 'global' && layerSel === 'global' ? '' : undefined}
            className={`rt-nav-item${panel === 'global' && layerSel === 'global' ? ' is-on' : ''}`}
            onClick={() => { setPanel('global'); setLayerSel('global'); }}
          >
            <SlidersHorizontal size={14} className="rt-nav-icon" aria-hidden="true" />
            <span className="rt-nav-label">{t('全局默认')}</span>
            {profiles ? (
              <span className="rt-nav-count">{profileOverrideCount(profiles.global)}</span>
            ) : null}
            {globalDirty ? <span className="rt-nav-dirty" title={t('有未保存的修改')} /> : null}
          </button>
          <button
            type="button"
            className={`rt-nav-item${panel === 'errors' ? ' is-on' : ''}`}
            onClick={() => setPanel('errors')}
          >
            <RefreshCw size={14} className="rt-nav-icon" aria-hidden="true" />
            <span className="rt-nav-label">{t('错误处理')}</span>
            {errorsDirty ? <span className="rt-nav-dirty" title={t('有未保存的修改')} /> : null}
          </button>

          <div className="rt-rail-title">{t('特殊请求路由')}</div>
          {SPECIAL_RAIL.map(({ key, label, icon: Icon }) => (
            <button
              key={key}
              type="button"
              className={`rt-nav-item${panel === key ? ' is-on' : ''}`}
              onClick={() => setPanel(key)}
            >
              <Icon size={14} className="rt-nav-icon" aria-hidden="true" />
              <span className="rt-nav-label">{t(label)}</span>
            </button>
          ))}

          {modelNames.length > 0 ? <div className="rt-rail-title">{t('模型路由')}</div> : null}
          {modelNames.map((name) => (
            <button
              key={`m:${name}`}
              type="button"
              data-route-scope={`m:${name}`}
              data-active={panel === 'global' && layerSel === `m:${name}` ? '' : undefined}
              className={`rt-nav-item${panel === 'global' && layerSel === `m:${name}` ? ' is-on' : ''}`}
              title={name}
              onClick={() => { setPanel('global'); setLayerSel(`m:${name}`); }}
            >
              <span className="rt-nav-label">{name}</span>
              <span className="rt-nav-count">
                {profileOverrideCount(profiles?.models[name] ?? emptyProfile())}
              </span>
              {modelLayerDirty(name) ? (
                <span className="rt-nav-dirty" title={t('有未保存的修改')} />
              ) : null}
            </button>
          ))}

          {(profiles?.groups.length ?? 0) > 0 ? <div className="rt-rail-title">{t('自定义路由组')}</div> : null}
          {(profiles?.groups ?? []).map((g) => (
            <button
              key={`g:${g.id}`}
              type="button"
              data-route-scope={`g:${g.id}`}
              data-active={panel === 'group' && groupSel === g.id ? '' : undefined}
              className={`rt-nav-item${panel === 'group' && groupSel === g.id ? ' is-on' : ''}`}
              title={g.id}
              onClick={() => { setPanel('group'); setLayerSel('groups'); setGroupSel(g.id); }}
            >
              <span className="rt-nav-label">{g.name || g.id}</span>
              <span className="rt-nav-count" title={t('{{n}} 个成员槽位', { n: g.members.length })}>
                {g.members.filter((m) => m.included).length}
              </span>
              {groupLayerDirty(g.id) ? (
                <span className="rt-nav-dirty" title={t('有未保存的修改')} />
              ) : null}
            </button>
          ))}

          <div className="rt-rail-add">
            <input
              ref={addModelRef}
              className="form-input"
              placeholder={t('新增模型档案名')}
              value={newModel}
              onChange={(e) => setNewModel(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') { addModel(); setPanel('global'); } }}
            />
            <Button
              variant="outline"
              size="sm"
              onClick={() => { addModel(); setPanel('global'); }}
              disabled={!newModel.trim()}
            >
              <Plus size={13} /> {t('新增路由配置')}
            </Button>
          </div>
        </nav>

        <div className="rt-shell-main">
          {/* 全局默认 / 模型路由 / 路由组：复用既有配置编辑器（由 layerSel 驱动选层）。
              这里用条件渲染而不是 hidden：plain 视图（特殊请求路由 / 错误处理）必须**真的**
              没有画布 DOM（点阵与连线层都不挂），否则 `hidden` 元素里仍留着一份图例与 SVG。
              草稿存在页面 state 里，卸载配置视图不会丢草稿（切回来仍在）。 */}
          {panel === 'global' || panel === 'group' ? configView : null}
          {panel === 'errors' ? <RoutingErrorsPanel onDirty={setErrorsDirty} /> : null}
          {panel === 'compact' ? (
            <div className="rt-special-split">
              <div className="rt-special-split-main"><CompactRoutes active={panel === 'compact'} /></div>
              <SpecialHits kind="compact" active={panel === 'compact'} onOpenReplay={() => setMode('replay')} />
            </div>
          ) : null}
          {panel === 'title' ? (
            <div className="rt-special-split">
              <div className="rt-special-split-main"><TitleRoutes active={panel === 'title'} /></div>
              <SpecialHits kind="title" active={panel === 'title'} onOpenReplay={() => setMode('replay')} />
            </div>
          ) : null}
          {panel === 'vision' ? (
            <div className="rt-special-split">
              <div className="rt-special-split-main">
                <VisionCompat active={panel === 'vision'} />
                {/* 会话图片压缩/视觉兼容（原「设置 → 聊天」的 ImageCompatPanel，v1.0.93 并入） */}
                <div style={{ marginTop: 12 }}>
                  <ImageCompatPanel />
                </div>
              </div>
              <SpecialHits kind="vision" active={panel === 'vision'} onOpenReplay={() => setMode('replay')} />
            </div>
          ) : null}
        </div>
      </div>
      </RoutingConfigShell>
      </div>
      )}
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}