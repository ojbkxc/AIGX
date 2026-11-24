import { useCallback, useEffect, useMemo, useRef, useState, memo } from 'react';
import { useTranslation } from 'react-i18next';
import { useSearchParams } from 'react-router-dom';
import {
  AudioLines,
  Boxes,
  ChevronDown,
  ChevronUp,
  Download,
  DownloadCloud,
  FlaskConical,
  Image as ImageIcon,
  Lightbulb,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Settings2,
  SlidersHorizontal,
  Tag,
  Trash2,
  Video,
  Wrench,
} from 'lucide-react';
import { api } from '../api';

import { useToast } from '../components/Toast';
import { Button, Loading, EmptyState, Switch, Tabs, Combobox, Tooltip } from '../components/ui';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import ModelTestDialog from '../components/ModelTestDialog';
import ModelCapabilityDialog from '../components/ModelCapabilityDialog';
import RatioPanel from '../components/RatioPanel';
import { fmtCompact } from '../lib/utils';
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
import { usePageSize, TablePaginationFooter, CLIENT_PAGE_SIZES } from '../components/ui/TableShell';
import type { AliasEntry, ManageView, ManagedModel, ModelMetaOverride, ChannelItem, AccountSessionItem } from '../types';
import './ModelManage.css';

// ── 常量 ─────────────────────────────────────────────────────────────

/** 分页档位（#67 对齐 Agent2API table-shell 的 CLIENT_PAGE_SIZES：[20,50,100,200,'all']，默认 50） */
const DEFAULT_PAGE_SIZE = 50;
/** 聚合视图每组默认展示行数，其余折叠（Agent2API GROUP_LIMIT） */
const GROUP_LIMIT = 8;

/** localStorage 键 */
const LS_FILTERS = 'aigx-mm-filters';
const LS_COLCFG = 'aigx-mm-colcfg';
const LS_COLW = 'aigx-mm-colw';
const LS_PAGE_SIZE = 'aigx-mm-pagesize';
/** 公共列设置键（tableColumns v2） */
const LS_TCOLS = 'aigx-tbl-cols.models';
/** 公共每页条数键（usePageSize；旧 aigx-mm-pagesize 搬迁源） */
const LS_PSZ = 'aigx-tbl-pagesize.models';
/** 获取模型弹窗「模型来源账号」选择记忆（渠道 id → 账号 id） */
const LS_FETCH_ACCPICK = 'aigx-mm-fetch-acctpick';

/** 公共层列声明（定义在 COL_DEFAULT_ALIGN 之后；见下方 MM_COL_DEFS） */

type MainTab = 'catalog' | 'ratio';
/** 启停筛选（多一档「有映射」，对齐 Agent2API MODEL_STATE_OPTIONS） */
type StateFilter = 'all' | 'on' | 'off' | 'mapped';
/**
 * 可见性筛选（对齐 Agent2API MODEL_VISIBILITIES：all / visible / hidden / undeclared，
 * 默认 `visible`）。语义照 A2A 的注释：`undeclared`（这家没有判据）**归入可见桶**
 * —— 「没依据就不藏」，所以默认档永远不会因为缺判据而把行筛空。
 */
type VisibilityFilter = 'all' | 'visible' | 'hidden' | 'undeclared';
type PriceFilter = 'all' | 'priced' | 'unpriced';
type SourceFilter = 'all' | 'channel' | 'account';
/** 排序档位（对齐 Agent2API MODEL_SORT_OPTIONS） */
type SortMode = 'default' | 'ratio-asc' | 'ratio-desc';

/** 可显隐的列（check / model / act 锁定不可关，但可拖动排序） */
type ColKey = 'rate' | 'price' | 'visibility' | 'source' | 'budget' | 'caps' | 'alias';
/**
 * 全部表格列（含锁定列）——默认渲染顺序与默认宽度**逐列对齐 Agent2API
 * models-panel-state.ts 的 COLUMNS**（check / model / rate / pricing / visibility /
 * source / budget / caps / alias / act，顺序即默认渲染顺序）。
 *
 * `visibility` 列与筛选档位都按 A2A 落地：后端 `ManageModel.visibility`（`"hidden"` /
 * `"shown"`，缺省 = 无判据）由 `src/model/manage.rs` 从元信息覆盖读出来，前端按
 * hidden → 不展示、shown/visible → 可见、其余 → 未声明归一，与 A2A 同一口径。
 */
type AnyCol = 'check' | ColKey | 'model' | 'act';
const COL_ORDER: AnyCol[] = ['check', 'model', 'rate', 'price', 'visibility', 'source', 'budget', 'caps', 'alias', 'act'];
/** 列宽：弹性列对齐 Agent2API page-gateway.css（`.c-*`），**操作列回到 px 定宽**。
 *  A2A 的 act=11% 放得下是因为它操作列按钮更少更小；AIGX 是三颗按钮（测试/定价/删除），
 *  11%≈90px 装不下 ~145px 内容 → 「测试」向左溢出进映射列、真人点不到（verify 实测）。
 *  act=170px 定宽（td 内宽 150px ≥ 按钮 ~145px）；% 列之和 ≈ (表宽−act)/表宽
 *  （822px 视口下 act 170 → % 之和 79%），余量给唯一的弹性富余列 alias。见 §9。 */
/**
 * 列宽：**逐列照 Agent2API page-gateway.css 的 `.c-*`**（v3.5.1 实测值，合计 100%）：
 * check 3.5 / model 14.5 / rate 6 / pricing 10.5 / visibility 6 / source 5.5 /
 * budget 9.5 / caps 11 / alias 25 / act 8.5。
 *
 * **act 例外，用 170px 定宽**：8.5%（≈70px）放不下本仓三颗按钮（测试 / 计价 /
 * 删除 ≈145px），内容会向左溢出进映射列把「测试」盖住（真人点不到）。A2A 的 act
 * 只有两颗按钮所以 8.5% 够；这是 AIGX 多一颗按钮的必然结果，不是数值抄错。
 * 其余列的超宽部分由 `.mm-table-wrap` 横向滚动兜住 —— 与 A2A「列宽是绝对量、
 * 装不下就滚」同一条取舍（见 ui/table-columns.js 模块头）。
 */
const COL_DEFAULT_W: Record<AnyCol, string> = {
  check: '3.5%',
  model: '14.5%',
  rate: '6%',
  price: '10.5%',
  visibility: '6%',
  source: '5.5%',
  budget: '9.5%',
  caps: '11%',
  alias: '25%',
  // 唯一不由 A2A 数值决定的列：A2A 的 act 只有「测试/删除」两颗按钮，8.5%（≈70px）
  // 够用；AIGX 多一颗「计价」，三颗 ≈145px，8.5% 会让内容向左溢出进映射列、
  // 「测试」被 chip 盖住（真人点不到，Playwright 报 subtree intercepts pointer
  // events）。这里改回文件头已记录过的定宽方案：170px —— td 内宽 150px ≥ 145px。
  act: '170px',
};
/** 锁定列：恒显示（表不能没有勾选框 / 模型名 / 操作），但可参与拖序 */
const LOCKED_COLS: AnyCol[] = ['check', 'model', 'act'];
/** 列设置面板显示的列名（锁定列也要有名字） */
const COL_PANEL_LABEL: Record<AnyCol, string> = {
  check: '选择',
  model: '上游模型',
  rate: '倍率',
  price: '计价',
  visibility: '可见性',
  source: '来源',
  budget: '上下文 / 输出',
  caps: '能力',
  alias: '模型映射',
  act: '操作',
};

/** 列对齐三档（对齐 Agent2API table-col-settings 的 ALIGNS） */
type Align = 'left' | 'center' | 'right';
/** 各列的默认对齐 = 现有 CSS 的观感（勾居中，其余左对齐），视觉不突变 */
const COL_DEFAULT_ALIGN: Record<AnyCol, Align> = {
  check: 'center',
  model: 'left',
  rate: 'left',
  price: 'left',
  visibility: 'left',
  source: 'left',
  budget: 'left',
  caps: 'left',
  alias: 'left',
  // A2A 的 act 列 align: 'right'（表头 `className='r'`、单元格 cellClass 透传）
  act: 'right',
};

/** 公共层列声明：默认宽沿用 COL_DEFAULT_W（px 列参与列宽拖拽记忆，百分比列不参与） */
const MM_COL_DEFS: TableColumnDef[] = COL_ORDER.map((key) => {
  const raw = COL_DEFAULT_W[key];
  const px = raw.endsWith('px') ? Number(raw.slice(0, -2)) : undefined;
  return {
    key,
    label: COL_PANEL_LABEL[key],
    align: COL_DEFAULT_ALIGN[key],
    ...(px != null && Number.isFinite(px) ? { width: px } : {}),
    ...(LOCKED_COLS.includes(key) ? { locked: true } : {}),
  };
});

/**
 * 能力圆点定义（对齐 Agent2API caps 列圆点：工具/图像/音频/视频/思考）。
 * match 命中 = 「可用」；capabilities 存在但该圆点未命中 = 「不可用」（灰实底）；
 * capabilities 完全缺失 = 「未测」（虚线圈）—— 三态判定见 renderCapDots。
 */
const CAP_DOTS: Array<{ key: string; match: RegExp; label: string; icon: typeof Wrench }> = [
  { key: 'tool', match: /tool|function/i, label: '工具调用', icon: Wrench },
  { key: 'image', match: /vision|image/i, label: '图像识别', icon: ImageIcon },
  { key: 'video', match: /video/i, label: '视频识别', icon: Video },
  { key: 'audio', match: /audio|speech|tts/i, label: '音频', icon: AudioLines },
  { key: 'reasoning', match: /reason|think/i, label: '思考', icon: Lightbulb },
];

/** 清单来源三态/四档（对齐 Agent2API per-model `source`：manual/remote/builtin/modelsdev） */
const SOURCE_DOTS: Record<string, { label: string; short: string; className: string }> = {
  remote: { label: '远程拉取', short: '远程', className: 'mm-src-dot remote' },
  builtin: { label: '内置', short: '内置', className: 'mm-src-dot builtin' },
  manual: { label: '手动登记', short: '手动', className: 'mm-src-dot manual' },
  // models.dev 社区目录补齐（A2A models-cells 的独立来源档：能力/上下文由社区目录回填）
  modelsdev: { label: 'models.dev 社区目录', short: '社区', className: 'mm-src-dot modelsdev' },
};

/**
 * 归一化一行的可见性（与 Agent2API `models-panel-state.ts::visibilityOf` 同一取值域）。
 *
 * `"hidden"` → hidden（不展示）；`"shown"` / `"visible"` → visible（可见）；
 * 其余（缺字段 / 空串 / 拼错）→ undeclared（没判据）。**绝不把「没有判据」当成可见**
 * —— 那会让管理页看起来把所有没判断的家都判成了「会展示」，而后端只对四家有判据。
 */
function visibilityOf(row: { visibility?: string } | null | undefined): 'visible' | 'hidden' | 'undeclared' {
  const raw = typeof row?.visibility === 'string' ? row.visibility.trim().toLowerCase() : '';
  if (raw === 'hidden') return 'hidden';
  if (raw === 'shown' || raw === 'visible') return 'visible';
  return 'undeclared';
}

/** 取一行的清单来源档：后端总会给一档；旧后端空串时按承载方兜底到手动/内置 */
function modelSourceKey(row: { source?: string; sources: { kind: string; source?: string }[] }): string {
  if (row.source && SOURCE_DOTS[row.source]) return row.source;
  if (row.sources.some((s) => s.source === 'manual')) return 'manual';
  return 'builtin';
}

/**
 * 能力圆点三态判定：
 *   caps 缺失/空数组 → 未声明（unset，虚线圈）；
 *   有声明且命中 → 可用（on，品牌淡底）；有声明但未命中 → 不可用（off，灰实底）。
 */
function capDotState(caps: string[] | undefined, match: RegExp): '' | 'on' | 'off' {
  if (!caps || caps.length === 0) return '';
  return caps.some((c) => match.test(c)) ? 'on' : 'off';
}

/**
 * 能力圆点的**取数来源**（#51 P1#4，消费后端 cap_sources.capabilities）：
 * 命中该 dot 的能力标签（vision/video/tools/reasoning…）在 cap_sources 里的档
 * —— builtin（内置推断）/ modelsdev（社区目录补齐）/ user（用户覆盖）/ ''（无标记）。
 */
function capDotSource(
  caps: string[] | undefined,
  capSources: ManagedModel['cap_sources'],
  match: RegExp,
): string {
  const tag = caps?.find((c) => match.test(c));
  if (!tag) return '';
  return String(capSources?.capabilities?.[tag] ?? '');
}

/** cap_sources 档位 → 人类可读中文（悬停可信度展示） */
const CAP_SRC_LABEL: Record<string, string> = {
  builtin: '内置推断',
  modelsdev: '社区目录补齐',
  user: '用户覆盖',
};

// ── 小工具 ───────────────────────────────────────────────────────────

function fmtPrice(n: number | null | undefined): string {
  if (n == null) return '—';
  // 去尾零：0.0010 → 0.001，列宽紧张时不被截断
  if (n < 0.01) return String(parseFloat(n.toFixed(4)));
  if (n < 1) return String(parseFloat(n.toFixed(3)));
  return n.toFixed(2);
}

/** 定价条目是否全 0（免费模型）——与「未定价」（price 为 null）区分开展示 */
function isFreePrice(p: { input_price: number; output_price: number; cache_price?: number | null; price_type: string }): boolean {
  // 计价方式=免费是显式第三态，与「价格恰好为 0」等价展示
  if (p.price_type === 'free') return true;
  if (p.price_type === 'count') return p.input_price === 0;
  return p.input_price === 0 && p.output_price === 0 && (p.cache_price == null || p.cache_price === 0);
}

/** provider 单调色（Agent2API monogram：哈希取色相，首尾字母做字形） */
function providerHue(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i += 1) h = (h * 31 + id.charCodeAt(i)) % 360;
  return h;
}

function monogramChar(label: string): string {
  const latin = label.match(/[A-Za-z]/);
  if (latin) return latin[0].toUpperCase();
  return label.trim().slice(-1) || '·';
}

function readLS<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? { ...fallback, ...JSON.parse(raw) } : fallback;
  } catch {
    return fallback;
  }
}

// ── 表单类型 ─────────────────────────────────────────────────────────

interface PriceForm {
  input_price: string;
  output_price: string;
  cache_price: string;
  price_type: string;
  model_ratio: string;
}

const EMPTY_PRICE_FORM: PriceForm = {
  input_price: '',
  output_price: '',
  cache_price: '',
  price_type: 'token',
  model_ratio: '',
};

interface AliasForm {
  alias: string;
  target: string;
  reasoning_effort: string;
}

/** 元信息覆盖表单：能力四圆点结构化（对齐 Agent2API CapabilityDialog 的声明式编辑） */
interface MetaForm {
  model: string;
  owned_by: string;
  context_length: string;
  /** 后端下发的最大输出 token（models.dev limit.output）；缺省 = 未声明 */
  max_output_tokens?: number | null;
  /** inherit = 能力标签留空（继承内置推断）；custom = 显式声明下方勾选项 */
  capsMode: 'inherit' | 'custom';
  capTool: boolean;
  capImage: boolean;
  capVideo: boolean;
  capAudio: boolean;
  capReasoning: boolean;
  /** 四圆点之外的标签（embedding / rerank / …），逗号分隔 */
  extraTags: string;
  /** 思考等级候选（逗号分隔，对齐 Agent2API 能力弹窗的 reasoningLevels 编辑） */
  reasoningLevels: string;
  /**
   * 可见性判据覆盖（对齐 Agent2API 的 `visibility` 键：`hidden` / `shown`，缺省 = 无判据）。
   * `'inherit'` = 不覆盖（落到 None = 没判据，前端显示「未声明」），不伪造「可见」。
   */
  visibilityMode: 'inherit' | 'shown' | 'hidden';
}

const EMPTY_META_FORM: MetaForm = {
  model: '',
  owned_by: '',
  context_length: '',
  capsMode: 'inherit',
  capTool: false,
  capImage: false,
  capVideo: false,
  capAudio: false,
  capReasoning: false,
  extraTags: '',
  reasoningLevels: '',
  visibilityMode: 'inherit',
};

/** 可见性覆盖字段 → 弹窗三态（对齐 A2A 取值域：只认 hidden / shown，其余 = 没判据） */
function metaFormVisibilityMode(raw: string | null | undefined): MetaForm['visibilityMode'] {
  const value = typeof raw === 'string' ? raw.trim().toLowerCase() : '';
  if (value === 'hidden') return 'hidden';
  if (value === 'shown' || value === 'visible') return 'shown';
  return 'inherit';
}

/** 能力标签 → 表单勾选态（与 CAP_DOTS 同一套正则口径） */
function capsToForm(caps: string[]): Pick<MetaForm, 'capsMode' | 'capTool' | 'capImage' | 'capVideo' | 'capAudio' | 'capReasoning' | 'extraTags'> {
  if (!caps.length) {
    return { capsMode: 'inherit', capTool: false, capImage: false, capVideo: false, capAudio: false, capReasoning: false, extraTags: '' };
  }
  const has = (re: RegExp): boolean => caps.some((c) => re.test(c));
  const extra = caps
    .filter((c) => !/tool|function|vision|image|video|audio|speech|tts|reason|think|chat/i.test(c))
    .join(', ');
  return {
    capsMode: 'custom',
    capTool: has(/tool|function/i),
    capImage: has(/vision|image/i),
    capVideo: has(/video/i),
    capAudio: has(/audio|speech|tts/i),
    capReasoning: has(/reason|think/i),
    extraTags: extra,
  };
}

type BatchAction = 'enable' | 'disable' | 'remove-price' | 'set-reasoning' | 'remove-custom';

/* ── 思考等级候选（对齐 Agent2API models-reasoning：三级回落） ── */

/** 兜底候选表（与上游 FALLBACK_LEVELS 同源；正常路径用模型声明/覆盖档位） */
const FALLBACK_REASONING_LEVELS: readonly string[] = ['off', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'];

/** 「自定义等级」下拉哨兵：选中只是把输入框露出来，不是真等级（对齐上游 CUSTOM_LEVEL） */
const CUSTOM_LEVEL = '__custom__';

/**
 * 一条映射能绑的候选等级（三级回落，对齐上游 levels()）：
 *   ① 目标模型的覆盖声明（overrides[id].reasoning_levels）；
 *   ② 没声明 → 本地兜底表。
 * 已声明但为空 = 明确「这家不认识任何档位」，不回落（返回空表）。
 */
function reasoningLevelCandidates(overrides: Record<string, ModelMetaOverride>, modelId: string): string[] {
  const declared = overrides[modelId]?.reasoning_levels;
  if (Array.isArray(declared)) return declared.filter((l): l is string => typeof l === 'string' && l.trim().length > 0);
  return [...FALLBACK_REASONING_LEVELS];
}

// ── 主组件 ───────────────────────────────────────────────────────────

/**
 * 模型管理表行（memo 行级隔离）：筛选/多选只重渲染相关行。
 * 单元格渲染经 rowCallbacks ref 透传（内容逐渲染刷新、ref 身份恒定），
 * row/visibleCols/isSelected 为行级 primitive prop——任一行状态变化不波及其余行。
 */
interface MmRowCallbacks {
  renderCell: (col: AnyCol, row: ManagedModel) => JSX.Element | null;
}

interface MmRowProps {
  row: ManagedModel;
  visibleCols: AnyCol[];
  isSelected: boolean;
  callbacks: React.MutableRefObject<MmRowCallbacks>;
}

const MmRow = memo(function MmRow({ row, visibleCols, isSelected, callbacks }: MmRowProps): JSX.Element {
  return (
    <tr className={`${row.enabled ? '' : 'mm-off'}${isSelected ? ' mm-sel' : ''}`}>
      {visibleCols.map((col) => callbacks.current.renderCell(col, row))}
    </tr>
  );
});

export default function ModelManage(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [view, setView] = useState<ManageView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  /** 「刷新模型目录」按钮在途（A2A POST /api/models/refresh） */
  const [refreshingModels, setRefreshingModels] = useState(false);
  /** 能力位三态弹窗（provider × model） */
  const [capTarget, setCapTarget] = useState<ManagedModel | null>(null);

  /** 手动全量刷新模型清单（上游目录 / models.dev）；刷新后回读视图 */
  const refreshCatalog = async (): Promise<void> => {
    setRefreshingModels(true);
    try {
      const res = await api.refreshModels();
      const data = res?.data;
      if (data) {
        const refreshed = Number(data.refreshed ?? 0);
        const failed = Number(data.failed ?? 0);
        const message = data.message || '';
        if (failed > 0) {
          addToast(t('刷新完成：成功 {{ok}}，失败 {{failed}}', { ok: refreshed, failed }), 'error');
        } else if (message) {
          addToast(message);
        } else {
          addToast(t('模型目录已刷新（{{n}} 批）', { n: refreshed }));
        }
      }
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setRefreshingModels(false);
    }
  };

  /** 全量模型 ID 候选（别名弹窗目标模型下拉）：目录 id ∪ 别名，大小写不敏感去重 */
  const modelIdOptions = useMemo(() => {
    const seen = new Map<string, string>();
    const push = (name?: string) => {
      const n = name?.trim();
      if (!n) return;
      const k = n.toLowerCase();
      if (!seen.has(k)) seen.set(k, n);
    };
    view?.models.forEach((m) => {
      push(m.id);
      m.aliases.forEach(push);
    });
    return [...seen.values()].sort((a, b) => a.localeCompare(b));
  }, [view]);

  // ── 筛选（落盘记忆，对齐 Agent2API models-filters） ──
  const mmFilters = getFilter<Record<string, string>>(LS_FILTERS, { state: 'all', visibility: 'visible', price: 'all', source: 'all', sort: 'default', navKey: 'all', query: '' });
  const [navKey, setNavKey] = useState<string>(mmFilters.navKey || 'all');
  const [stateFilter, setStateFilter] = useState<StateFilter>(mmFilters.state as StateFilter);
  /**
   * 可见性筛选（默认 `visible`，与 Agent2API `DEFAULT_FILTERS.visibility` 同值）。
   *
   * 存坏值按 A2A 的同一口径回落到 `visible`（不是「全部」）：A2A 的注释里解释了为什么
   * 默认是 visible 而不是 all —— 上游目录是超集，里面混着官方客户端不展示的模型。
   */
  const [visibilityFilter, setVisibilityFilter] = useState<VisibilityFilter>(
    (['all', 'visible', 'hidden', 'undeclared'] as VisibilityFilter[]).includes(mmFilters.visibility as VisibilityFilter)
      ? (mmFilters.visibility as VisibilityFilter)
      : 'visible',
  );
  const [priceFilter, setPriceFilter] = useState<PriceFilter>(mmFilters.price as PriceFilter);
  const [sourceFilter, setSourceFilter] = useState<SourceFilter>(mmFilters.source as SourceFilter);
  const [sortMode, setSortMode] = useState<SortMode>(mmFilters.sort as SortMode);
  const [channelFilter, setChannelFilter] = useState('all');
  const [query, setQuery] = useState<string>(mmFilters.query || '');
  /** 左栏 provider 搜索（对齐 Agent2API prov-rail 的搜索框；只过滤左栏，不动表格筛选） */
  const [railQuery, setRailQuery] = useState('');

  // 筛选变更即落盘（navKey 分组 / query 搜索同记，对齐 models-filters 的 provider / search）
  useEffect(() => {
    setFilter(LS_FILTERS, { state: stateFilter, visibility: visibilityFilter, price: priceFilter, source: sourceFilter, sort: sortMode, navKey, query });
  }, [stateFilter, visibilityFilter, priceFilter, sourceFilter, sortMode, navKey, query]);

  // 分页（每页条数走公共 usePageSize，旧 aigx-mm-pagesize 一次性搬迁）
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = usePageSize(LS_PSZ, CLIENT_PAGE_SIZES, DEFAULT_PAGE_SIZE, [LS_PAGE_SIZE]);

  // 批量选择（key = provider|id）
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [batchOpen, setBatchOpen] = useState(false);
  const [batchAction, setBatchAction] = useState<BatchAction>('enable');
  const [batchRunning, setBatchRunning] = useState(false);
  const [batchReport, setBatchReport] = useState('');
  const [batchReasoning, setBatchReasoning] = useState('');

  // 列配置已迁公共层（tableColumns v2：顺序/显隐/对齐/列宽统一记忆）。
  // 旧键一次性搬迁：mm-colcfg（数组：顺序+显隐+对齐）转 v1 形态喂公共层；
  // mm-colw 列宽覆盖并进 v2；搬完删旧键。
  const [colState, setColState] = useState<TableColumnState>(() => {
    let migrated = readTableColumns(LS_TCOLS, MM_COL_DEFS);
    if (localStorage.getItem(LS_TCOLS) === null) {
      const legacyCfg = readLS<unknown>(LS_COLCFG, null);
      if (Array.isArray(legacyCfg)) {
        const v1 = {
          columns: legacyCfg
            .filter((e): e is { key: string; visible?: unknown; align?: unknown } =>
              Boolean(e) && typeof e === 'object' && typeof (e as { key?: unknown }).key === 'string'
              // 已删列（如旧「启用」列）的残留键丢在迁移前，别喂给公共层造成幽灵列
              && MM_COL_DEFS.some((d) => d.key === (e as { key: string }).key))
            .map((e) => ({
              key: e.key,
              visible: e.visible !== false,
              align: (typeof e.align === 'string' ? e.align : 'left') as Align,
            })),
        };
        try {
          migrated = readTableColumns(LS_TCOLS, MM_COL_DEFS, () => JSON.stringify(v1));
        } catch { /* 迁移失败退默认，不炸表 */ }
      }
    }
    const legacyW = readLS<Record<string, number>>(LS_COLW, {});
    if (Object.keys(legacyW).length === 0) return migrated;
    const merged: TableColumnState = {
      ...migrated,
      items: migrated.items.map((i) =>
        legacyW[i.key] != null && i.width == null ? { ...i, width: legacyW[i.key] } : i,
      ),
    };
    writeTableColumns(LS_TCOLS, merged);
    try {
      localStorage.removeItem(LS_COLCFG);
      localStorage.removeItem(LS_COLW);
    } catch { /* 静默 */ }
    return merged;
  });
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);

  // 聚合视图的组折叠（展开的组集合；默认折叠）
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());

  // 获取模型弹窗
  const [fetchModal, setFetchModal] = useState(false);
  const [fetchChannels, setFetchChannels] = useState<ChannelItem[]>([]);
  const [fetchingId, setFetchingId] = useState<string | null>(null);
  const [fetchDone, setFetchDone] = useState<Record<string, boolean>>({});
  /** 每渠道本次拉取顺带清理的失效映射条数（后端 removed_mappings；0/缺省不展示） */
  const [fetchCleaned, setFetchCleaned] = useState<Record<string, number>>({});
  /** 单渠道失败标记（重试即清除），对照 Agent2API 获取弹窗的状态徽标 */
  const [fetchFailed, setFetchFailed] = useState<Record<string, string>>({});
  const [fetchAllRunning, setFetchAllRunning] = useState(false);
  /** 账号池型渠道的可选账号（A2A「模型来源账号」下拉：用哪个账号去拉目录） */
  const [fetchAccounts, setFetchAccounts] = useState<AccountSessionItem[]>([]);
  /** 每渠道选中的模型来源账号（落盘记忆，key=渠道 id） */
  const [fetchAccountPick, setFetchAccountPick] = useState<Record<string, string>>(
    readLS<Record<string, string>>(LS_FETCH_ACCPICK, {}),
  );
  /** 按账号拉取的在途渠道 id */
  const [fetchingAccountCid, setFetchingAccountCid] = useState<string | null>(null);

  // 添加自定义模型弹窗
  const [addModelOpen, setAddModelOpen] = useState(false);
  const [addModelProvider, setAddModelProvider] = useState('');
  const [addModelId, setAddModelId] = useState('');
  const [addModelError, setAddModelError] = useState('');
  const [addingModel, setAddingModel] = useState(false);

  const [searchParams] = useSearchParams();

  // 弹窗
  const [priceTarget, setPriceTarget] = useState<ManagedModel | null>(null);
  const [priceForm, setPriceForm] = useState<PriceForm>(EMPTY_PRICE_FORM);
  const [priceError, setPriceError] = useState('');
  const [savingPrice, setSavingPrice] = useState(false);

  const [aliasTarget, setAliasTarget] = useState<ManagedModel | null>(null);
  const [aliasForm, setAliasForm] = useState<AliasForm>({ alias: '', target: '', reasoning_effort: '' });
  const [aliasError, setAliasError] = useState('');
  const [savingAlias, setSavingAlias] = useState(false);
  const [pendingAlias, setPendingAlias] = useState<string | null>(null);

  /** 当前映射行的「同家」模型清单（A2A 映射 target 锁定同家清单，避免建出跨家调不通的映射）。
   *  为空（该家仅此一行 / 旧后端）时回退到全局候选并允许自由输入。 */
  const aliasProviderOptions = useMemo(() => {
    const provider = aliasTarget?.provider;
    if (!provider) return [] as string[];
    const seen = new Set<string>();
    const out: string[] = [];
    for (const m of view?.models ?? []) {
      if (m.provider !== provider) continue;
      const k = m.id.toLowerCase();
      if (seen.has(k)) continue;
      seen.add(k);
      out.push(m.id);
    }
    return out.sort((a, b) => a.localeCompare(b));
  }, [view, aliasTarget]);

  const [testTarget, setTestTarget] = useState<ManagedModel | null>(null);

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [pendingRow, setPendingRow] = useState<string | null>(null);

  const [tab, setTab] = useState<MainTab>('catalog');

  // ── 自定义模型（对齐 Agent2API addCustomModel / removeCustomModel）──
  // 「新建自定义模型」与「添加模型」两个弹窗已合并为同一个（provider 可搜下拉 + 模型 ID），
  // 两个入口（左栏沉底 / 标题栏）都打开它。状态在下方 addModel* 一组里。


  // ── 批量映射前缀（对齐 Agent2API 批量加映射的「前缀 + 同名合并」选项） ──
  const [prefixModal, setPrefixModal] = useState(false);
  const [prefixForm, setPrefixForm] = useState({ prefix: '', sep: '-', merge: true });
  const [prefixError, setPrefixError] = useState('');
  const [savingPrefix, setSavingPrefix] = useState(false);

  // 元信息覆盖
  const [overrides, setOverrides] = useState<Record<string, ModelMetaOverride>>({});
  const [metaModal, setMetaModal] = useState(false);
  const [metaForm, setMetaForm] = useState<MetaForm>(EMPTY_META_FORM);
  const [metaError, setMetaError] = useState('');
  const [savingMeta, setSavingMeta] = useState(false);
  // 覆盖项管理弹窗（原「元信息覆盖」Tab 并入目录后，列表 + 删除入口收在这里）
  const [overridesModal, setOverridesModal] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listManageModels();
      setView(res?.data ?? null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const loadOverrides = useCallback(async () => {
    try {
      const res = await api.listModelMeta();
      setOverrides(res?.data ?? {});
    } catch {
      // 覆盖列表加载失败不阻塞模型目录
    }
  }, []);

  useEffect(() => {
    void loadOverrides();
  }, [loadOverrides]);

  /** 写接口都回读完整视图，就地替换即可 */
  const acceptView = useCallback((next: ManageView | undefined) => {
    if (next) setView(next);
  }, []);

  /**
   * 「默认绑定」（id→id 自映射）开关：写入仍走 `POST /api/models/state`（SPEC §2.1）。
   * 这就是 A2A 里模型自身的 `enabled`——行级启用 = 默认绑定开着或任一条别名开着。
   */
  const toggleDefaultBinding = async (row: ManagedModel, enabled: boolean) => {
    const key = `${row.provider}|${row.id}`;
    if (pendingRow) return;
    setPendingRow(key);
    try {
      const res = await api.setModelEnabled(row.provider, row.id, enabled);
      acceptView(res?.data);
      addToast(enabled ? t('已启用') : t('已禁用'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setPendingRow(null);
    }
  };

  /** 点击复制（模型 id / 映射名，Agent2API data-copy 同款交互） */
  const copyText = (text: string) => {
    void navigator.clipboard?.writeText(text).then(
      () => addToast(t('已复制') + `：${text}`),
      () => addToast(t('复制失败'), 'error'),
    );
  };

  // ── 定价弹窗 ──
  const openPrice = (row: ManagedModel) => {
    setPriceTarget(row);
    setPriceError('');
    const p = row.price;
    setPriceForm({
      input_price: p ? String(p.input_price ?? '') : '',
      output_price: p ? String(p.output_price ?? '') : '',
      cache_price: p && p.cache_price != null ? String(p.cache_price) : '',
      price_type: p?.price_type || 'token',
      model_ratio: row.model_ratio !== 1 ? String(row.model_ratio) : '',
    });
  };

  const savePrice = async () => {
    if (!priceTarget) return;
    setPriceError('');
    const free = priceForm.price_type === 'free';
    // 免费：价格与倍率都不参与计费（后端 cost_with_name 对 free 直接返回 0），
    // 统一提交 0/null，避免残留旧价格让列表误显示成「$x/1k tokens」。
    const input = free ? 0 : Number(priceForm.input_price || 0);
    const output = free ? 0 : Number(priceForm.output_price || 0);
    const cache = free || priceForm.cache_price.trim() === '' ? null : Number(priceForm.cache_price);
    if (!Number.isFinite(input) || !Number.isFinite(output) || input < 0 || output < 0) {
      setPriceError(t('价格必须是非负数字'));
      return;
    }
    if (cache != null && (!Number.isFinite(cache) || cache < 0)) {
      setPriceError(t('缓存价格必须是非负数字'));
      return;
    }
    const ratioText = free ? '' : priceForm.model_ratio.trim();
    const ratio = ratioText === '' ? undefined : Number(ratioText);
    if (ratio !== undefined && (!Number.isFinite(ratio) || ratio < 0)) {
      setPriceError(t('模型倍率必须是非负数字'));
      return;
    }
    setSavingPrice(true);
    try {
      const res = await api.upsertModelPrice({
        model: priceTarget.id,
        input_price: input,
        output_price: output,
        cache_price: cache,
        price_type: priceForm.price_type,
        model_ratio: ratio,
      });
      acceptView(res?.data);
      addToast(t('价格已保存'));
      setPriceTarget(null);
    } catch (err) {
      setPriceError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingPrice(false);
    }
  };

  const removePrice = (row: ManagedModel) => {
    setConfirmState({
      title: t('删除定价'),
      message: t('删除后该模型将按 0 计费（如仍有供应商声明则会放行）。确认删除？') + ` ${row.id}`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        const res = await api.removeModelPrice(row.id);
        acceptView(res?.data);
        addToast(t('已删除定价'));
      },
    });
  };

  // ── 别名弹窗 ──
  const openAlias = (row: ManagedModel, presetAlias?: string) => {
    setAliasTarget(row);
    setAliasError('');
    // 预填已有思考等级（编辑已有别名时）
    const existing = presetAlias
      ? row.alias_entries?.find((e) => e.alias === presetAlias)
      : undefined;
    setAliasForm({
      alias: presetAlias ?? '',
      target: row.id,
      reasoning_effort: existing?.reasoning_effort ?? '',
    });
  };

  const toggleAlias = async (alias: string, enabled: boolean) => {
    setPendingAlias(alias);
    try {
      const res = await api.setModelAliasState(alias, enabled);
      acceptView(res?.data);
      addToast(enabled ? t('映射已启用') : t('映射已关闭'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setPendingAlias(null);
    }
  };

  const saveAlias = async () => {
    setAliasError('');
    const alias = aliasForm.alias.trim();
    const target = aliasForm.target.trim();
    if (!alias) {
      setAliasError(t('别名不能为空'));
      return;
    }
    if (!target) {
      setAliasError(t('目标模型不能为空'));
      return;
    }
    setSavingAlias(true);
    try {
      // 三态：非空=设置，空串=清除，undefined=不动
      const effort = aliasForm.reasoning_effort.trim();
      const res = await api.setModelAlias(
        alias,
        target,
        undefined,
        effort || undefined
      );
      acceptView(res?.data);
      addToast(t('映射已保存') + `：${alias} → ${target}`);
      setAliasTarget(null);
    } catch (err) {
      setAliasError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingAlias(false);
    }
  };

  const removeAlias = (alias: string) => {
    setConfirmState({
      title: t('删除映射'),
      message: t('删除后客户端将无法再通过该别名调用') + `：${alias}`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        const res = await api.removeModelAlias(alias);
        acceptView(res?.data);
        addToast(t('已删除映射'));
      },
    });
  };

  // ── 元信息覆盖 ──
  const openMetaCreate = () => {
    setMetaForm({ ...EMPTY_META_FORM });
    setMetaError('');
    setMetaModal(true);
  };

  const openMetaEdit = (model: string) => {
    const o = overrides[model];
    if (!o) return;
    setMetaForm({
      model,
      owned_by: o.owned_by ?? '',
      context_length: o.context_length != null ? String(o.context_length) : '',
      reasoningLevels: (o.reasoning_levels ?? []).join(', '),
      ...capsToForm(o.capabilities ?? []),
      visibilityMode: metaFormVisibilityMode(o.visibility),
    });
    setMetaError('');
    setMetaModal(true);
  };

  /** 目录行点「能力 / 上下文」单元格 → 打开覆盖弹窗并预填当前推断值
   *  （对齐 Agent2API 能力弹窗的「继承 / 覆盖」语义：留空 = 继承内置推断）。 */
  const openMetaForRow = (row: ManagedModel) => {
    const o = overrides[row.id];
    setMetaForm({
      model: row.id,
      owned_by: o?.owned_by ?? row.owned_by ?? '',
      context_length:
        o?.context_length != null
          ? String(o.context_length)
          : row.context_length != null
            ? String(row.context_length)
            : '',
      reasoningLevels: (o?.reasoning_levels ?? []).join(', '),
      ...capsToForm(o?.capabilities ?? row.capabilities ?? []),
      visibilityMode: metaFormVisibilityMode(o?.visibility ?? row.visibility),
    });
    setMetaError('');
    setMetaModal(true);
  };

  /** 五圆点勾选 → 提交给后端的标签数组（chat 恒携带，与内置推断口径一致） */
  const metaFormToCaps = (): string[] => {
    if (metaForm.capsMode === 'inherit') return [];
    const caps: string[] = ['chat'];
    if (metaForm.capTool) caps.push('tool');
    if (metaForm.capImage) caps.push('vision');
    if (metaForm.capVideo) caps.push('video');
    if (metaForm.capAudio) caps.push('audio');
    if (metaForm.capReasoning) caps.push('reasoning');
    for (const tag of metaForm.extraTags.split(/[,，]/).map((c) => c.trim()).filter(Boolean)) {
      if (!caps.includes(tag)) caps.push(tag);
    }
    return caps;
  };

  /** 思考等级候选输入 → 数组（中文逗号也兼容；空 = 不覆盖，交由后端继承） */
  const metaFormToLevels = (): string[] | undefined => {
    const levels = metaForm.reasoningLevels
      .split(/[,，]/)
      .map((s) => s.trim())
      .filter(Boolean);
    return levels.length ? levels : undefined;
  };

  const saveMeta = async () => {
    setMetaError('');
    const model = metaForm.model.trim();
    const ownedBy = metaForm.owned_by.trim();
    if (!model) { setMetaError(t('模型 ID 为必填项')); return; }
    if (!ownedBy) { setMetaError(t('归属方为必填项')); return; }
    let ctx: number | null = null;
    if (metaForm.context_length.trim()) {
      ctx = Number(metaForm.context_length.trim());
      if (!Number.isFinite(ctx) || ctx < 0) {
        setMetaError(t('上下文长度需为非负整数'));
        return;
      }
    }
    setSavingMeta(true);
    try {
      await api.setModelMeta(model, {
        owned_by: ownedBy,
        context_length: ctx,
        capabilities: metaFormToCaps(),
        reasoning_levels: metaFormToLevels(),
        // 可见性覆盖：只有选了可见 / 不展示才提交（inherit = 不覆盖，落 None = 没判据）
        visibility:
          metaForm.visibilityMode === 'hidden' ? 'hidden'
            : metaForm.visibilityMode === 'shown' ? 'shown'
              : undefined,
      });
      addToast(t('元信息覆盖已保存'));
      setMetaModal(false);
      await loadOverrides();
      await load();
    } catch (err) {
      setMetaError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingMeta(false);
    }
  };

  const removeMeta = (model: string) => {
    setConfirmState({
      title: t('删除元信息覆盖'),
      message: t('删除后将回落到内置推断') + `：${model}`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        await api.deleteModelMeta(model);
        addToast(t('已删除'));
        await loadOverrides();
        await load();
      },
    });
  };

  // ── 自定义模型（保存/删除失败只弹 toast，不崩页面） ──

  /** 判定一行是否为自定义模型：来源里带 manual 标记，或该 provider 是自定义家 */
  const isCustomRow = (row: ManagedModel): boolean => {
    if (row.sources.some((s) => s.source === 'manual')) return true;
    return customProviders.some((p) => p.id === row.provider);
  };

  /** 打开「添加自定义模型」弹窗（原「新建自定义模型」入口已合并到这一个弹窗） */
  const openAddModel = () => {
    setAddModelProvider('');
    setAddModelId('');
    setAddModelError('');
    setAddModelOpen(true);
  };

  const deleteCustom = (row: ManagedModel) => {
    setConfirmState({
      title: t('删除自定义模型'),
      message: t('删除后客户端将无法再调用该模型') + `：${row.id}`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        await api.removeCustomModel(row.provider, row.id);
        addToast(t('已删除自定义模型'));

        await load();
      },
    });
  };

  // ── 批量映射前缀（对齐 Agent2API 批量加映射：前缀 + 同名合并到多家） ──

  const openPrefixBatch = () => {
    if (selectedRows.length === 0) return;
    setPrefixForm({ prefix: '', sep: '-', merge: true });
    setPrefixError('');
    setPrefixModal(true);
  };

  const savePrefixBatch = async () => {
    setPrefixError('');
    const prefix = prefixForm.prefix.trim();
    if (!prefix) {
      setPrefixError(t('前缀不能为空'));
      return;
    }
    setSavingPrefix(true);
    try {
      const targets = selectedRows.map((m) => m.id);
      const res = await api.batchAddAliasPrefix(prefix, {
        sep: prefixForm.sep,
        merge: prefixForm.merge,
        targets,
      });
      acceptView(res ?? undefined);
      addToast(t('已批量添加映射') + `：${prefix}<sep><模型ID>`);
      setPrefixModal(false);
      setSelected(new Set());
    } catch (err) {
      // 报错留在弹窗里供重试
      setPrefixError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingPrefix(false);
    }
  };

  // ── 批量操作 ──
  const rowKey = (m: ManagedModel) => `${m.provider}|${m.id}`;

  const toggleSelect = (key: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const selectedRows = useMemo(
    () => (view?.models ?? []).filter((m) => selected.has(rowKey(m))),
    [view, selected],
  );

  const runBatch = async () => {
    if (selectedRows.length === 0 || batchRunning) return;
    setBatchRunning(true);
    setBatchReport('');
    let fail = 0;
    let skip = 0;
    let firstError = '';
    try {
      for (const row of selectedRows) {
        try {
          if (batchAction === 'remove-custom') {
            // #51 P1#5：删除「自定义模型登记」（对齐 A2A 批量删模型），仅对自定义/手动行；
            // 复用行内 deleteCustom 的 removeCustomModel(provider, id) 端点。
            if (isCustomRow(row)) await api.removeCustomModel(row.provider, row.id);
            else skip += 1;
          } else if (batchAction === 'remove-price') {
            if (row.price) await api.removeModelPrice(row.id);
            else skip += 1;
          } else if (batchAction === 'set-reasoning') {
            // 收集该模型所有别名，调用批量设等级端点
            const aliases = (row.alias_entries?.map((e) => e.alias) ?? row.aliases).filter(Boolean);
            if (aliases.length === 0) {
              skip += 1;
              continue;
            }
            const res = await api.setAliasReasoning(aliases, batchReasoning);
            const written = res?.data?.written ?? 0;
            if (written === 0) skip += 1;
            if (res?.data?.view) acceptView(res.data.view);
          } else {
            await api.setModelEnabled(row.provider, row.id, batchAction === 'enable');
          }
        } catch (err) {
          fail += 1;
          if (!firstError) firstError = err instanceof Error ? err.message : String(err);
        }
      }
      await load();
      if (fail === 0) {
        addToast(
          batchAction === 'remove-custom'
            ? t('已批量删除自定义模型') + (skip > 0 ? `（${skip} ${t('个非自定义跳过')}）` : '')
            : batchAction === 'remove-price'
            ? t('已批量删除定价') + (skip > 0 ? `（${skip} ${t('个未定价跳过')}）` : '')
            : batchAction === 'set-reasoning'
              ? t('已批量设置思考等级') + (skip > 0 ? `（${skip} ${t('个无别名跳过')}）` : '')
              : batchAction === 'enable'
                ? t('已批量启用')
                : t('已批量禁用'),
        );
        setBatchOpen(false);
        setSelected(new Set());
      } else {
        setBatchReport(
          `${t('成功')} ${selectedRows.length - fail - skip} · ${t('失败')} ${fail}` +
            (skip > 0 ? ` · ${t('跳过')} ${skip}` : '') +
            (firstError ? ` — ${firstError}` : ''),
        );
      }
    } finally {
      setBatchRunning(false);
    }
  };

  // ── 获取模型（逐渠道拉取上游模型清单）──
  const openFetchModal = async () => {
    setFetchModal(true);
    setFetchDone({});
    setFetchCleaned({});
    setFetchFailed({});
    try {
      const res = await api.listChannels({ page: 1, page_size: 200 });
      const list = (res?.data ?? []) as ChannelItem[];
      setFetchChannels(list.filter((c) => c.status === 'enabled'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
    // 账号池型渠道的「模型来源账号」候选（失败不阻塞渠道级拉取，静默降级为空）
    try {
      const accRes = await api.listAccountSessions({ page: 1, page_size: 500 });
      setFetchAccounts((accRes?.data ?? []) as AccountSessionItem[]);
    } catch {
      setFetchAccounts([]);
    }
  };

  /** 记住「模型来源账号」选择（落盘，A2A 同款「哪个账号去拉 + 落盘记忆」） */
  useEffect(() => {
    setFilter(LS_FETCH_ACCPICK, fetchAccountPick);
  }, [fetchAccountPick]);

  /** 按选定账号拉取该家目录（A2A「模型来源账号」映射：syncAccountCatalog 只刷缓存、
   *  不写白名单、不清映射，与渠道级「全部账号」拉取并存）。 */
  const fetchOneAccount = async (ch: ChannelItem, accountId: string) => {
    const cid = String(ch.id);
    if (fetchingAccountCid) return;
    const acc = fetchAccounts.find((a) => a.id === accountId);
    setFetchingAccountCid(cid);
    try {
      const res = await api.syncAccountCatalog({ account_id: accountId, apply: false, cleanup: false });
      const data = res?.data;
      const ok = Number(data?.ok_count ?? 0);
      const fail = Number(data?.error_count ?? 0);
      const firstErr = data?.errors?.[0]?.error;
      if (fail > 0 && ok === 0) {
        setFetchFailed((f) => ({ ...f, [cid]: firstErr || t('拉取失败') }));
      } else {
        setFetchDone((d) => ({ ...d, [cid]: true }));
        setFetchFailed((f) => {
          const next = { ...f };
          delete next[cid];
          return next;
        });
        addToast(t('已按账号拉取模型目录') + `：${acc?.name ?? accountId}`);
        await load();
      }
    } catch (err) {
      setFetchFailed((f) => ({ ...f, [cid]: err instanceof Error ? err.message : String(err) }));
    } finally {
      setFetchingAccountCid(null);
    }
  };

  const saveAddModel = async () => {
    setAddModelError('');
    const provider = addModelProvider.trim();
    const id = addModelId.trim();
    if (!provider) {
      setAddModelError(t('请选择 provider'));
      return;
    }
    if (!id) {
      setAddModelError(t('模型 ID 不能为空'));
      return;
    }
    setAddingModel(true);
    try {
      const res = await api.addCustomModel(provider, id);
      acceptView(res?.data);
      addToast(t('已添加自定义模型') + `：${provider}/${id}`);
      setAddModelOpen(false);
      setAddModelProvider('');
      setAddModelId('');
    } catch (err) {
      setAddModelError(err instanceof Error ? err.message : String(err));
    } finally {
      setAddingModel(false);
    }
  };

  /** 拉单渠道；返回本次顺带清理的失效映射条数（后端 removed_mappings，缺省 0） */
  const fetchOne = async (ch: ChannelItem): Promise<{ ok: boolean; removed: number }> => {
    const cid = String(ch.id);
    if (fetchingId) return { ok: false, removed: 0 };
    setFetchingId(cid);
    try {
      const res = await api.fetchChannelModels({ channel_id: cid });
      const removed = Number(res?.data?.removed_mappings ?? 0);
      setFetchDone((d) => ({ ...d, [cid]: true }));
      setFetchCleaned((d) => ({ ...d, [cid]: removed }));
      setFetchFailed((f) => {
        const next = { ...f };
        delete next[cid];
        return next;
      });
      return { ok: true, removed };
    } catch (err) {
      setFetchFailed((f) => ({ ...f, [cid]: err instanceof Error ? err.message : String(err) }));
      return { ok: false, removed: 0 };
    } finally {
      setFetchingId(null);
    }
  };

  /** 逐个点击（拉完刷新目录）；「全部拉取」里传 silent 由批处理统一刷新 */
  const fetchOneClick = async (ch: ChannelItem) => {
    const { ok, removed } = await fetchOne(ch);
    if (ok) {
      // 清理反馈：后端顺带清了失效映射时展示条数，0 条不显示
      addToast(
        t('已拉取') + `：${ch.name}`
        + (removed > 0 ? ` · ${t('已清理失效映射 {{n}} 条', { n: removed })}` : ''),
      );
      await load();
    } else {
      addToast(t('拉取失败'), 'error');
    }
  };

  /** 全部拉取：串行逐渠道（避免同时打爆上游），完成后统一刷新并汇报成败数 */
  const fetchAll = async () => {
    if (fetchAllRunning || fetchChannels.length === 0) return;
    setFetchAllRunning(true);
    let ok = 0;
    let fail = 0;
    let cleaned = 0;
    try {
      for (const ch of fetchChannels) {
        const done = await fetchOne(ch);
        if (done.ok) {
          ok += 1;
          cleaned += done.removed;
        } else {
          fail += 1;
        }
      }
      if (ok > 0) await load();
      addToast(
        `${t('全部拉取完成')}：${ok} ${t('成功')}` + (fail > 0 ? ` · ${fail} ${t('失败')}` : '')
        + (cleaned > 0 ? ` · ${t('清理失效映射 {{n}} 条', { n: cleaned })}` : ''),
        fail > 0 ? 'error' : undefined,
      );
    } finally {
      setFetchAllRunning(false);
    }
  };

  // ── 列设置（顺序 / 显隐 / 对齐）/ 列宽拖动（公共层 tableColumns v2）──
  const appliedCols = useMemo(() => applyTableColumns(colState, MM_COL_DEFS), [colState]);
  const visibleCols = useMemo<AnyCol[]>(
    () => appliedCols.map((c) => c.key as AnyCol),
    [appliedCols],
  );
  /** 列 key → 当前对齐（th/td 的 ta-* 类由此来） */
  const colAlign = useMemo(() => {
    const map = {} as Record<AnyCol, Align>;
    for (const item of colState.items) map[item.key as AnyCol] = item.align;
    return map;
  }, [colState]);

  /** 提交列配置（面板回调；persist=true 才落盘） */
  const commitColItems = useCallback((next: TableColumnItem[], persist: boolean) => {
    setColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);

  useEffect(() => {
    if (!colPanelOpen) return;
    const onDown = (e: MouseEvent) => {
      if (colPanelRef.current && !colPanelRef.current.contains(e.target as Node)) {
        setColPanelOpen(false);
      }
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [colPanelOpen]);

  // 窄屏（<520px）：面板以视口为锚（fixed），top 跟随按钮底部，
  // 避免面板右对齐到工具栏按钮时左缘溢出屏外、被 .card 的 overflow:hidden 裁掉手柄列。
  useEffect(() => {
    if (!colPanelOpen || !colPanelRef.current) return;
    const btn = colPanelRef.current.querySelector('button');
    if (!btn) return;
    colPanelRef.current.style.setProperty('--mm-colset-top', `${btn.getBoundingClientRect().bottom + 6}px`);
  }, [colPanelOpen]);

  /** 恢复默认：顺序+显隐+对齐+列宽一并还原（公共层 reset） */
  const resetColConfig = useCallback(() => {
    setColState(resetTableColumns(LS_TCOLS, MM_COL_DEFS));
  }, []);

  /** 列宽拖动改公共 hook（pointer events + 双击还原） */
  const colResize = useColumnResize({
    widths: Object.fromEntries(
      colState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number]),
    ),
    setWidths: (next) => {
      commitColItems(
        colState.items.map((i) => {
          const w = next[i.key];
          const defW = MM_COL_DEFS.find((d) => d.key === i.key)?.width;
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(MM_COL_DEFS.map((d) => [d.key, d.width])),
    // 锁定列 = 「列设置里恒显示」，**不是**「不能拖宽」：Agent2API 的 locked 只影响显隐，
    // 列宽那一层对每一列都给把手（见 ui/table-columns.js 的「末列也有把手」）。早先这里传
    // 了 LOCKED_COLS，model / act 两列因此拖不动，与 A2A 行为不一致。
    lockedKeys: new Set<string>(),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });

  // ── 过滤 + 排序 + 分组 + 分页 ──
  const filtered = useMemo(() => {
    const list = view?.models ?? [];
    const q = query.trim().toLowerCase();
    const navIsChannel = navKey.startsWith('ch:');
    const navChannelId = navIsChannel ? navKey.slice(3) : '';
    const navIsProvider = navKey.startsWith('pv:');
    const navProvider = navIsProvider ? navKey.slice(3) : '';
    const out = list.filter((m) => {
      if (navIsChannel && !m.sources.some((s) => s.channel_id === navChannelId)) return false;
      if (navIsProvider && m.provider !== navProvider) return false;
      if (stateFilter === 'on' && !m.enabled) return false;
      if (stateFilter === 'off' && m.enabled) return false;
      if (stateFilter === 'mapped' && (m.alias_entries?.length ?? m.aliases.length) === 0) return false;
      // 可见性筛选（对齐 Agent2API 的口径）：`visible` 这一档**包含 undeclared**
      // —— 没判据的家按「不藏」处理，否则默认档会因为「这家没有这个信息」而筛空。
      if (visibilityFilter === 'hidden' && visibilityOf(m) !== 'hidden') return false;
      if (visibilityFilter === 'undeclared' && visibilityOf(m) !== 'undeclared') return false;
      if (visibilityFilter === 'visible' && !(visibilityOf(m) === 'visible' || visibilityOf(m) === 'undeclared')) return false;
      if (priceFilter === 'priced' && !m.price) return false;
      if (priceFilter === 'unpriced' && m.price) return false;
      if (sourceFilter === 'channel' && !m.sources.some((s) => s.kind === 'channel')) return false;
      if (sourceFilter === 'account' && !m.sources.some((s) => s.kind === 'account')) return false;
      if (channelFilter !== 'all' && !m.sources.some((s) => s.channel_id === channelFilter)) return false;
      if (!q) return true;
      const hay = [m.id, m.owned_by, m.provider_label, ...m.aliases, ...m.sources.map((s) => s.name)]
        .join(' ')
        .toLowerCase();
      return hay.includes(q);
    });
    // 排序：默认保持后端序（provider_label → id）；倍率档在同启用态内排
    if (sortMode !== 'default') {
      const dir = sortMode === 'ratio-asc' ? 1 : -1;
      out.sort((a, b) => {
        if (a.enabled !== b.enabled) return a.enabled ? -1 : 1;
        return (a.model_ratio - b.model_ratio) * dir || a.id.localeCompare(b.id);
      });
    }
    return out;
  }, [view, navKey, stateFilter, visibilityFilter, priceFilter, sourceFilter, channelFilter, query, sortMode]);

  /** 聚合模式：左栏「全部」且无搜索词 → 按提供商分组 + 组内折叠（Agent2API 同款）。
   *  任何筛选/排序/搜索生效时回落到平铺分页表。 */
  const groupedMode = navKey === 'all' && !query.trim();

  const groups = useMemo(() => {
    if (!groupedMode) return [];
    const map = new Map<string, { label: string; rows: ManagedModel[] }>();
    for (const m of filtered) {
      const hit = map.get(m.provider);
      if (hit) hit.rows.push(m);
      else map.set(m.provider, { label: m.provider_label, rows: [m] });
    }
    return Array.from(map.entries()).map(([id, g]) => ({ id, ...g }));
  }, [filtered, groupedMode]);

  // 客户端分页（CLIENT_PAGE_SIZES 含 'all' 档：全部 → 取全量）
  const pageSizeNum = pageSize === 'all' ? filtered.length || 1 : pageSize;
  const totalPages = Math.max(1, Math.ceil(filtered.length / pageSizeNum));
  const currentPage = Math.min(page, totalPages);
  const pageRows = useMemo(
    () => filtered.slice((currentPage - 1) * pageSizeNum, currentPage * pageSizeNum),
    [filtered, currentPage, pageSizeNum],
  );

  // 左栏数据
  const channelRails = useMemo(() => {
    const map = new Map<string, { id: string; name: string; count: number }>();
    for (const m of view?.models ?? []) {
      for (const s of m.sources) {
        if (s.kind !== 'channel' || !s.channel_id) continue;
        const hit = map.get(s.channel_id);
        if (hit) hit.count += 1;
        else map.set(s.channel_id, { id: s.channel_id, name: s.name || s.channel_id, count: 1 });
      }
    }
    return Array.from(map.values()).sort((a, b) => a.name.localeCompare(b.name));
  }, [view]);

  const builtinRails = useMemo(() => (view?.providers ?? []).filter((g) => g.account), [view]);

  /** 自定义提供商分组：providers 里 account=false 但 id 带 custom 前缀的分组（后端就绪后自动出现） */
  const customProviders = useMemo(
    () =>
      (view?.providers ?? []).filter(
        (g) => !g.account && (g.id.startsWith('custom:') || g.id === 'custom'),
      ),
    [view],
  );

  /** 左栏按 provider 名过滤（只动左栏导航，不影响右侧表格的筛选/搜索） */
  const railFilter = useMemo(() => {
    const q = railQuery.trim().toLowerCase();
    const match = (s: string): boolean => !q || s.toLowerCase().includes(q);
    return {
      active: q.length > 0,
      channels: channelRails.filter((c) => match(c.name)),
      builtins: builtinRails.filter((g) => match(g.label)),
      customs: customProviders.filter((p) => match(p.label)),
    };
  }, [railQuery, channelRails, builtinRails, customProviders]);

  const providerStats = useMemo(() => {
    const groupsList = view?.providers ?? [];
    return {
      total: groupsList.reduce((n, g) => n + g.total, 0),
      providers: groupsList.length,
      unpriced: view?.unpriced ?? 0,
    };
  }, [view]);

  /** 面板头统计：总数 / 已启用 / 有映射（对齐 Agent2API mm-meta） */
  const headStats = useMemo(() => {
    const list = filtered;
    return {
      total: list.length,
      enabled: list.filter((m) => m.enabled).length,
      mapped: list.filter((m) => (m.alias_entries?.length ?? m.aliases.length) > 0).length,
    };
  }, [filtered]);

  /** 面板头标题：当前视图名 */
  const viewTitle = useMemo(() => {
    if (navKey === 'all') return t('全部');
    if (navKey.startsWith('ch:')) {
      const cid = navKey.slice(3);
      return channelRails.find((c) => c.id === cid)?.name ?? cid;
    }
    if (navKey.startsWith('pv:')) {
      const pid = navKey.slice(3);
      return (view?.providers ?? []).find((g) => g.id === pid)?.label ?? pid;
    }
    return t('全部');
  }, [navKey, channelRails, view, t]);

  const selectNav = (key: string): void => {
    setNavKey(key);
    if (key.startsWith('ch:')) setChannelFilter(key.slice(3));
    else if (channelFilter !== 'all') setChannelFilter('all');
  };

  const channelOptions = useMemo(() => {
    const map = new Map<string, string>();
    for (const m of view?.models ?? []) {
      for (const s of m.sources) {
        if (!map.has(s.channel_id)) map.set(s.channel_id, s.name);
      }
    }
    return Array.from(map.entries()).sort((a, b) => a[1].localeCompare(b[1]));
  }, [view]);

  useEffect(() => {
    setPage(1);
    setSelected(new Set());
  }, [navKey, stateFilter, visibilityFilter, priceFilter, sourceFilter, channelFilter, query, sortMode]);

  // 渠道页「在模型管理中维护」跳转：预选该渠道
  useEffect(() => {
    const cid = searchParams.get('channel_id');
    if (cid) {
      setChannelFilter(cid);
      setSourceFilter('all');
    }
  }, [searchParams]);

  // 旧 /pricing 重定向到 ?tab=ratio：按 URL 参数切 Tab（只认已注册的 Tab，
  // 避免手改 URL 把 tab 置成非法值导致整页空白）。?tab=meta 已并入目录
  // （由 App 重定向到 ?tab=catalog），这里忽略未知值回落到当前 tab。
  useEffect(() => {
    const want = searchParams.get('tab');
    if (want && (['catalog', 'ratio'] as string[]).includes(want)) {
      setTab(want as MainTab);
    }
  }, [searchParams]);

  /**
   * 表头全选：作用域是**当前筛选出的全部行（跨分页）**，与 Agent2API allPickNode 一致
   * （见 renderHeadCell 里那颗勾选框的 title）。聚合视图下 filtered 已经是分组展开前的全集。
   */
  const toggleSelectAllFiltered = () => {
    const keys = filtered.map(rowKey);
    setSelected((prev) => {
      const next = new Set(prev);
      const allSelected = keys.length > 0 && keys.every((k) => next.has(k));
      for (const k of keys) {
        if (allSelected) next.delete(k);
        else next.add(k);
      }
      return next;
    });
  };

  const toggleGroup = (gid: string) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(gid)) next.delete(gid);
      else next.add(gid);
      return next;
    });
  };

  // ── 悬空映射（对齐 Agent2API models-dangling：目标模型不在清单里 → 挂不到任何行）──
  // 后端给了 dangling_mappings / mappings 带标记就用后端事实；旧后端没给时前端
  // 按「mappings 的 target 不在任何模型行 id 里」自行推断（宁可列出待确认，
  // 也不让这批映射既看不见又删不掉）。
  const danglingMappings = useMemo<Array<{ alias: string; target: string; fromBackend: boolean }>>(() => {
    const v = view;
    if (!v) return [];
    // #51 P1#8：后端口径是 `dangling`（`ManageView.dangling`，含 provider 维度），
    // 旧前端误读 `dangling_mappings` 导致读不到后端事实、只能自行推断。优先读 `dangling`，
    // 兼容旧后端/旧字段名回落 `dangling_mappings`，最后才按 mappings 推断。
    const backend = Array.isArray(v.dangling)
      ? v.dangling
      : Array.isArray(v.dangling_mappings)
        ? v.dangling_mappings
        : null;
    if (backend) {
      return backend
        .filter((m) => m && typeof m.alias === 'string')
        .map((m) => ({ alias: m.alias, target: String(m.target || ''), fromBackend: true }));
    }
    const liveIds = new Set(v.models.map((m) => m.id.toLowerCase()));
    const out: Array<{ alias: string; target: string; fromBackend: boolean }> = [];
    // 兜底推断：后端 `mappings` 是**逐条数组**（MappingView[]），旧扁平对象在
    // `mappingsFlat`。两种形态都读，绝不对数组做 Object.entries（那会拿到下标串）。
    const entries: Array<{ alias: string; target: string; dangling?: boolean }> = Array.isArray(
      v.mappings,
    )
      ? v.mappings.map((m) => ({
          alias: m.alias,
          target: String(m.target || ''),
          dangling: m.dangling === true,
        }))
      : // 对象形态：新后端把扁平表放在 `mappingsFlat`；旧后端与 e2e 静态桩则是
        // `mappings` 本身就是对象（扁平表）。两处都要读 —— 只认 mappingsFlat 会
        // 把「对象形态的 mappings」当空表，悬空区一条都推断不出来（实测
        // `.mm-dangling` 恒不渲染，e2e 因此红）。
        Object.entries(
          (v.mappingsFlat && typeof v.mappingsFlat === 'object' && v.mappingsFlat) ||
            (v.mappings && !Array.isArray(v.mappings) && typeof v.mappings === 'object'
              ? (v.mappings as Record<string, string>)
              : {}),
        ).map(([alias, target]) => ({
          alias,
          target: String(target || ''),
          dangling: false,
        }));
    for (const { alias, target, dangling } of entries) {
      if (!alias) continue;
      if (dangling) {
        out.push({ alias, target, fromBackend: true });
        continue;
      }
      if (target && !liveIds.has(target.toLowerCase())) {
        out.push({ alias, target, fromBackend: false });
      }
    }
    return out.sort((a, b) => a.alias.localeCompare(b.alias));
  }, [view]);

  /** 删除一条悬空映射（复用既有 removeModelAlias；删到不剩时区块自己消失） */
  const removeDangling = (alias: string, target: string): void => {
    setConfirmState({
      title: t('删除未挂载的映射'),
      message: t('确定删除映射「{{alias}} → {{target}}」？它指向的上游模型已不在清单里。删除后该对外名即可重新使用。', { alias, target }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        const res = await api.removeModelAlias(alias);
        acceptView(res?.data);
        addToast(t('已删除映射'));
      },
    });
  };

  // ── 单元格渲染 ──
  const renderCell = (col: AnyCol, row: ManagedModel): JSX.Element | null => {
    const key = rowKey(row);
    switch (col) {
      case 'check':
        return (
          <td key={col} className={`mm-c-check ta-${colAlign.check}`}>
            <input
              type="checkbox"
              aria-label={t('选择') + ' ' + row.id}
              checked={selected.has(key)}
              onChange={() => toggleSelect(key)}
            />
          </td>
        );
      case 'model':
        return (
          <td key={col} className={`mm-c-model ta-${colAlign.model}`}>
            <span
              className="mm-mid"
              title={t('点击复制模型 ID')}
              onClick={() => copyText(row.id)}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === 'Enter') copyText(row.id);
              }}
            >
              {row.id}
            </span>
            <span className="mm-model-sub">
              {row.provider_label}
              {row.context_length ? ` · ${fmtCompact(row.context_length)}` : ''}
            </span>
          </td>
        );
      case 'rate': {
        // 「倍率」列：**照 Agent2API models-cells.tsx 的 rate 分支落**——渲染清单上的
        // `credits` 文本（formatCredits），没有就读 `—`。
        //
        // 本仓的定价倍率 `model_ratio` 是另一个字段（后端 `ratio_config.model_ratio`，
        // 语义是「价格倍率」而不是上游 credits）。严格 1:1 对齐时这一列只认 credits，
        // model_ratio 仍在计价格里显示 ×N —— 两个字段语义不同，留哪个见「AIGX 独有项」。
        //
        // credits 目前不在 ManagedModel 声明里（后端也没下发）：按未知键读，读到什么
        // 显示什么，读不到落 `—`，不猜。
        const credits = (row as unknown as { credits?: unknown }).credits;
        const rateText = typeof credits === 'string' && credits.trim() ? credits.trim() : '';
        return (
          <td key={col} className={`mm-c-rate ta-${colAlign.rate}`}>
            {rateText
              ? <span className="mm-price" title={rateText}>{rateText}</span>
              : <span className="mm-price-empty" title={t('这条清单没有倍率读数')}>—</span>}
          </td>
        );
      }
      case 'visibility': {
        // 「可见性」列：三档 + 一档「没有判据」（照 A2A visibilityCell 的口径：
        // hidden → 不展示、shown/visible → 可见、其余 → 没有判据）。
        //
        // 取值域与 A2A 逐字一致（见 visibilityOf）；后端 `ManageModel.visibility`
        // 由元信息覆盖读出，缺字段/未声明走最后一档。
        const vis = visibilityOf(row);
        if (vis === 'hidden') {
          return (
            <td key={col} className={`mm-c-visibility ta-${colAlign.visibility}`}>
              <span
                className="mm-src-tag model-source"
                title={t('上游能列出这个模型，但官方客户端不展示它')}
              >
                <i className="mm-src-dot manual" />
                {t('不展示')}
              </span>
            </td>
          );
        }
        if (vis === 'visible') {
          return (
            <td key={col} className={`mm-c-visibility ta-${colAlign.visibility}`}>
              <span className="mm-src-tag model-source" title={t('官方客户端会展示这个模型')}>
                <i className="mm-src-dot builtin" />
                {t('可见')}
              </span>
            </td>
          );
        }
        return (
          <td key={col} className={`mm-c-visibility ta-${colAlign.visibility}`}>
            <span
              className="mm-price-empty"
              title={t('这条记录没有可见性判据（这家的目录没有给出这一维；A2A 只对部分内置家有）')}
            >
              —
            </span>
          </td>
        );
      }
      case 'price':
        return (
          <td
            key={col}
            className={`mm-c-price ta-${colAlign.price}`}
            role="button"
            tabIndex={0}
            title={`${t('点击编辑定价')} · ${t('单位：美元。按量价为每 1k tokens 输入/输出价；按次价为每次调用价。')}`}
            onClick={() => openPrice(row)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                openPrice(row);
              }
            }}
          >
            {row.price ? (
              isFreePrice(row.price) ? (
                <span className="mm-price-free" title={t('价格已配置为 0，调用不计费')}>
                  {t('免费')}
                </span>
              ) : (
                // 两行制（对齐 A2A pricing 单元格：第一行读数、第二行是「这个读数怎么来的」）：
                // A2A 第二行给计价来源 + 倍率；本仓没有 pricing.source 这一维，第二行就只放
                // 定价倍率（`model_ratio`）—— 它是本仓唯一会修正最终价格的因子，属于「AIGX 独有项」，
                // 见交付说明第 ③ 段（倍率列本身已按 A2A 改用 credits，这里是它原来的位置）。
                <span className="mm-price" style={{ display: 'inline-flex', flexDirection: 'column', alignItems: 'flex-start', lineHeight: 1.25 }}>
                  <span>
                    {row.price.price_type === 'count'
                      ? `$${fmtPrice(row.price.input_price)}/${t('次')}`
                      : `$${fmtPrice(row.price.input_price)}/$${fmtPrice(row.price.output_price)}`}
                  </span>
                  {row.model_ratio !== 1 && (
                    <span className="mm-price-ratio" style={{ fontSize: 10.5, opacity: 0.85 }} title={`${t('定价倍率')}：×${row.model_ratio}`}>
                      ×{row.model_ratio}
                    </span>
                  )}
                </span>
              )
            ) : (
              <span className="mm-price-empty">{t('未定价')}</span>
            )}
          </td>
        );
      case 'source': {
        // 行内展示「清单来源」三态/四档（manual/remote/builtin/modelsdev，对齐 A2A models-cells）；
        // 承载方（渠道/账号名）收进 td 的 title（A2A 承载方另处）。
        const srcKey = modelSourceKey(row);
        const srcMeta = SOURCE_DOTS[srcKey] ?? SOURCE_DOTS.builtin;
        const srcLabel =
          srcKey === 'remote' ? t('远程')
            : srcKey === 'manual' ? t('手动')
              : srcKey === 'modelsdev' ? t('社区')
                : t('内置');
        const carriers = row.sources.map((s) => s.name).filter(Boolean).join('、');
        return (
          <td
            key={col}
            className={`mm-c-source ta-${colAlign.source}`}
            title={
              carriers ? `${srcLabel} · ${t('承载方')}：${carriers}` : `${srcLabel} · ${t('无承载方')}`
            }
          >
            <span className="mm-src-tag model-source">
              <i className={srcMeta.className} />
              {srcLabel}
            </span>
          </td>
        );
      }
      case 'budget':
        return (
          <td key={col} className={`mm-c-budget ta-${colAlign.budget}`}>
            {(() => {
              const flags = row.capability_flags;
              // 上下文 = 能力位 ✓ 覆盖 → 元信息 → 空；输出同理。
              // 2026-11-24：输出侧此前只认 capability_flags.maxOutputTokens，而
              // 后端从不下发那个键（元信息结构里根本没有 output 维度），线上 136
              // 个模型该列右半恒为「未声明」。现在后端透出 max_output_tokens
              // （models.dev `limit.output`），这里直接读它。
              const inTok = flags?.maxInputTokens ?? row.context_length ?? null;
              const outTok = row.max_output_tokens ?? flags?.maxOutputTokens ?? null;
              const ctxSrc = String(row.cap_sources?.context_length ?? '');
              const outSrc = String(row.cap_sources?.max_output_tokens ?? '');
              const hint =
                `${t('上下文窗口')} ${inTok == null ? t('未声明') : inTok}` +
                ` / ${t('最大输出 Token')} ${outTok == null ? t('未声明') : outTok}` +
                ` —— ${t('点击修改模型能力')}` +
                (ctxSrc === 'modelsdev' ? t('；其中 上下文窗口 由 models.dev 补齐') : '') +
                (outSrc === 'modelsdev' ? t('；最大输出 Token 由 models.dev 补齐') : '');
              return (
                <button
                  type="button"
                  className="mm-caps-btn"
                  title={hint}
                  onClick={() => openMetaForRow(row)}
                >
                  <span className={`mm-cap-ctx${inTok == null ? '' : ' filled'}${ctxSrc === 'modelsdev' ? ' from-modelsdev' : ''}`}>
                    {inTok != null ? fmtCompact(inTok) : '—'}
                  </span>
                  <span className="mm-cap-ctx">/</span>
                  <span className={`mm-cap-ctx${outTok == null ? '' : ' filled'}`}>
                    {outTok != null ? fmtCompact(outTok) : '—'}
                  </span>
                </button>
              );
            })()}
          </td>
        );
      case 'caps':
        return (
          <td key={col} className={`mm-c-caps ta-${colAlign.caps}`}>
            <button
              type="button"
              className="mm-caps-btn"
              title={t('目录级元信息覆盖：按模型 ID，覆盖该模型在所有来源的默认展示（归属 / 上下文 / 能力）')}
              onClick={() => openMetaForRow(row)}
            >
              {CAP_DOTS.map((c) => {
                const state = capDotState(row.capabilities, c.match);
                // #51 P1#4：消费后端 cap_sources.capabilities，圆点按取数来源上色 + 悬停标注可信度
                const src = capDotSource(row.capabilities, row.cap_sources, c.match);
                const Icon = c.icon;
                const base = `${t(c.label)}：${
                  state === 'on'
                    ? t('可用')
                    : state === 'off'
                      ? t('不可用')
                      : t('未测')
                }`;
                return (
                  <Tooltip
                    key={c.key}
                    label={src ? `${base} · ${t('来源')}：${t(CAP_SRC_LABEL[src] ?? src)}` : base}
                  >
                    <span className={`mm-cap-dot${state ? ` ${state}` : ''}${src ? ` src-${src}` : ''}`}>
                      <Icon size={10} />
                    </span>
                  </Tooltip>
                );
              })}
              {/* models.dev 补齐标记：能力来自社区目录补齐时加内侧描边小点 */}
              {row.sources.some((s) => s.source === 'modelsdev') && (
                <span className="mm-cap-mark" title={t('能力由 models.dev 社区目录补齐')} />
              )}
            </button>
            {/* 三态编辑入口：按 (provider, model) 显式声明能力位，
                与上面的「目录级元信息覆盖」（按模型 id 的全局默认声明）职责不同 ——
                前者只管 {provider} 这一条链，后者管该模型在所有链上的默认展示 */}
            <button
              type="button"
              className="mm-cap-edit"
              title={t('按当前提供商（{{p}}）编辑能力位：只作用于这条链，继承 / 是 / 否 三态', { p: row.provider_label })}
              onClick={() => setCapTarget(row)}
              aria-label={t('按当前提供商编辑能力位')}
            >
              <SlidersHorizontal size={11} />
            </button>
          </td>
        );
      case 'alias': {
        const aliasEntries: AliasEntry[] = row.alias_entries?.length
          ? row.alias_entries
          : row.aliases.map((a): AliasEntry => ({ alias: a, target: row.id, enabled: true, reasoning_effort: undefined }));
        // 默认绑定（id→id 自映射）那颗开关 = 行级启用（model_rules，SPEC §2.1）。
        // A2A 里它就是「模型自身的 enabled」，常驻在映射列首颗；关掉它且无可生效别名时整行压淡。
        // 后端未落地新字段时回落到旧的 row.enabled，避免整列被误判为关闭。
        const defaultOn = row.default_enabled ?? row.enabled;
        const defaultBusy = pendingRow === key;
        return (
          <td key={col} className={`mm-c-alias ta-${colAlign.alias}`}>
            <div className="mm-alias-cell">
              {/* 默认绑定 chip：开关 + `<id> → <id>` + 「默认」小标（对标 A2A .binding-default） */}
              <span
                className={`mm-alias mm-binding-default${defaultOn ? '' : ' is-off'}`}
                title={defaultOn ? `${row.id} → ${row.id}` : `${row.id} → ${row.id}（${t('已关闭，请求原样透传')}）`}
              >
                <Switch
                  checked={defaultOn}
                  onChange={(next) => void toggleDefaultBinding(row, next)}
                  disabled={defaultBusy}
                  ariaLabel={`${t('默认绑定')} ${row.id}`}
                  className="mm-alias-switch"
                />
                <span className="mm-alias-name mm-binding-name" role="presentation">
                  <span className="mm-alias-text">{row.id}</span>
                  <span className="mm-alias-arrow" aria-hidden="true">→</span>
                  <span className="mm-alias-target">{row.id}</span>
                </span>
                <span className="mm-binding-default-tag">{t('默认')}</span>
              </span>
              {aliasEntries.map((entry) => (
                <span
                  key={entry.alias}
                  className={`mm-alias${entry.enabled ? '' : ' is-off'}`}
                  title={
                    entry.enabled
                      ? `${entry.alias} → ${entry.target}${entry.reasoning_effort ? ` · 思考: ${entry.reasoning_effort}` : ''}`
                      : `${entry.alias} → ${entry.target}（${t('已关闭，请求原样透传')}）`
                  }
                >
                  <Switch
                    checked={entry.enabled}
                    onChange={(next) => void toggleAlias(entry.alias, next)}
                    disabled={pendingAlias === entry.alias}
                    ariaLabel={`${entry.enabled ? t('关闭') : t('启用')}映射 ${entry.alias}`}
                    className="mm-alias-switch"
                  />
                  <span
                    className="mm-alias-name"
                    title={t('点击复制对外名；双击编辑映射')}
                    onClick={() => copyText(entry.alias)}
                    onDoubleClick={() => openAlias(row, entry.alias)}
                    role="button"
                    tabIndex={0}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') openAlias(row, entry.alias);
                    }}
                  >
                    <span className="mm-alias-text">{entry.alias}</span>
                    {/* 对外名 → 目标名：目标名常显（弱化色/小字号），不再只藏在 tooltip 里 */}
                    <span className="mm-alias-arrow" aria-hidden="true">→</span>
                    <span className="mm-alias-target">{entry.target}</span>
                    {entry.reasoning_effort && (
                      <span className="mm-alias-reason" title={`映射思考等级: ${entry.reasoning_effort}`}>
                        · {entry.reasoning_effort}
                      </span>
                    )}
                  </span>
                  <button
                    type="button"
                    className="mm-alias-x"
                    title={t('删除映射')}
                    onClick={() => removeAlias(entry.alias)}
                  >
                    ×
                  </button>
                </span>
              ))}
              <button
                type="button"
                className="mm-alias-add"
                onClick={() => openAlias(row)}
                title={t('添加映射：对外名 → 本模型')}
              >
                <Plus size={10} /> {t('映射')}
              </button>
            </div>
          </td>
        );
      }
      case 'act':
        return (
          <td key={col} className={`mm-c-act ta-${colAlign.act}`}>
            <div className="mm-actions">
              <button
                type="button"
                className="mm-act-btn"
                disabled={row.sources.length === 0}
                title={
                  row.sources.length === 0
                    ? t('该模型没有可用来源（启用账号或供应商），无法测试')
                    : t('沿真实链路发一条最小对话，验证可用性与延迟')
                }
                onClick={() => setTestTarget(row)}
              >
                <FlaskConical size={11} /> {t('测试')}
              </button>
              <button
                type="button"
                className="mm-act-btn"
                onClick={() => openPrice(row)}
                title={t('编辑定价')}
              >
                <Tag size={11} /> {t('定价')}
              </button>
              {row.price && (
                <button
                  type="button"
                  className="mm-act-btn danger icon-only"
                  onClick={() => removePrice(row)}
                  title={t('删除定价')}
                >
                  <Trash2 size={11} />
                </button>
              )}
              {/* 自定义模型：行内删除（对齐 Agent2API removeCustomModel） */}
              {isCustomRow(row) && (
                <button
                  type="button"
                  className="mm-act-btn danger icon-only"
                  onClick={() => deleteCustom(row)}
                  title={t('删除自定义模型')}
                >
                  <Trash2 size={11} />
                </button>
              )}
            </div>
          </td>
        );
      default:
        return null;
    }
  };

  // 行级 memo 交互面：ref 对象身份恒定（内容逐渲染刷新），
  // 传 ref 本体给 MmRow —— 传 .current 会拿到逐渲染新对象，memo 全失效
  const rowCallbacks = useRef<MmRowCallbacks>({} as MmRowCallbacks);
  rowCallbacks.current = { renderCell };

  const COL_HEAD: Record<AnyCol, { label: string; tip?: string }> = {
    check: { label: '' },
    model: { label: t('上游模型') },
    rate: { label: t('倍率'), tip: t('按提供商分组排序，已启用模型优先；未知倍率排在末尾') },
    price: {
      label: t('计价'),
      // A2A 的 th title 逐字（单位信息在单元格自己的 title 里，见 renderCell 的 price 分支）
      tip: t('点击任意单元格可覆盖单价或设置参考模型'),
    },
    // A2A 的 th title（逐字）：这一列在本仓尚无后端数据源，单元格落 `—` 并另挂一句说明
    visibility: {
      label: t('可见性'),
      tip: t('这个模型在官方客户端里是否会被展列；不展示的那些默认已关闭（含默认绑定）'),
    },
    source: { label: t('来源') },
    budget: {
      label: t('上下文 / 输出'),
      tip: t('数字带虚线底 = 由 models.dev 补齐；右上角小点 = 你手动覆盖过；— = 未声明。点任意一格可改。'),
    },
    caps: {
      label: t('能力'),
      tip: t('圆点带内侧描边 = 由 models.dev 补齐；右上角小点 = 你手动覆盖过；虚线圈 = 未声明。悬停某一枚可看逐项出处。'),
    },
    alias: {
      label: t('模型映射'),
      tip: t('每条映射 = 对外名 → 上游模型；原始 ID 与别名有独立开关，默认绑定永远存在（没有映射时是合成的那条）。'),
    },
    act: { label: t('操作') },
  };

  const renderHeadCell = (col: AnyCol): JSX.Element => {
    const def = COL_HEAD[col];
    // 「倍率」表头可点排序（对齐 A2A models-page 的 `<TableHead data-col='rate'
    //   sortDirection=… sortLabel=… onSortChange=…>`）：点一下升序、再点降序。
    // 排序档位复用本页既有的 sortMode（default / rate-asc / rate-desc），所以表头与
    // 筛选行里那三档「默认 / 倍率升序 / 倍率降序」分段控件**同源**，不会各排一套。
    const rateSorting = col === 'rate';
    const rateDir: 'asc' | 'desc' | null =
      sortMode === 'ratio-asc' ? 'asc' : sortMode === 'ratio-desc' ? 'desc' : null;
    const rateSortLabel = rateDir === 'asc'
      ? t('倍率升序，点击切换为降序')
      : rateDir === 'desc'
        ? t('倍率降序，点击切换为升序')
        : t('按倍率升序排列');
    return (
      <th
        key={col}
        className={`mm-h-${col} ta-${colAlign[col] ?? COL_DEFAULT_ALIGN[col]}`}
        title={def.tip}
        aria-sort={rateSorting ? (rateDir === 'asc' ? 'ascending' : rateDir === 'desc' ? 'descending' : 'none') : undefined}
      >
        {col === 'check' ? (
          <input
            type="checkbox"
            aria-label={t('全选')}
            // 表头那颗勾选框的作用域 = **当前筛选出的全部行**（跨分页），与 A2A
            // allPickNode 同一口径（它的 title 原文就是「全选当前筛选出的模型（跨分页）；再点取消」）。
            // 早先只勾「当前页」，用户在分页视图里点它再删，会以为清掉了筛选结果。
            title={t('全选当前筛选出的模型（跨分页）；再点取消')}
            checked={filtered.length > 0 && filtered.every((r) => selected.has(rowKey(r)))}
            onChange={toggleSelectAllFiltered}
          />
        ) : rateSorting ? (
          <button
            type="button"
            // 表头排序按钮**不用新 CSS 类**（本波不动 .css）：外观内联到既有观感 ——
            // 透明底、继承字重、箭头只在有方向时着色，未排序时淡出。
            style={{
              border: 0,
              background: 'transparent',
              padding: 0,
              font: 'inherit',
              color: 'inherit',
              cursor: 'pointer',
              display: 'inline-flex',
              alignItems: 'center',
              gap: 3,
            }}
            title={rateSortLabel}
            aria-label={rateSortLabel}
            onClick={() => setSortMode(rateDir === 'asc' ? 'ratio-desc' : 'ratio-asc')}
          >
            {def.label}
            <span
              aria-hidden="true"
              style={{ fontSize: 10, opacity: rateDir ? 1 : 0.35, lineHeight: 1 }}
            >
              {rateDir === 'desc' ? '↓' : '↑'}
            </span>
          </button>
        ) : (
          def.label
        )}
        {col !== 'act' && (() => {
          const idx = visibleCols.indexOf(col);
          const colDef = MM_COL_DEFS.find((d) => d.key === col);
          // 末列也有把手（对齐 Agent2API table-columns.js 的 `.col-grip-end`）：
          // 默认 right:-4px 钉在表格最右缘会顶出一条 4px 横滚，末列因此改 right:0 往内让。
          const isLast = idx === visibleCols.length - 1;
          // locked 只影响列设置里的「恒显示」，不给把手就是另一回事了（见上面 lockedKeys 的注释）
          const grip = colResize.gripProps(col, {
            isLast: false,
            locked: false,
            defaultWidth: colDef?.width,
          });
          return (
            <span
              className="mm-col-grip"
              title={t('拖动调整列宽 · 双击还原')}
              style={isLast ? { right: 0 } : undefined}
              {...grip}
            />
          );
        })()}
      </th>
    );
  };

  // ── 渲染 ──
  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('模型管理')}</h1>
          <p>{t('供应商与账号池的模型统一在此管理：测试、启停、定价、映射。')}</p>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <Tooltip label={t('从上游目录 / models.dev 重新拉取模型清单（不影响已保存的映射与定价）')}>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void refreshCatalog()}
              disabled={refreshingModels || loading}
              style={{ gap: 6 }}
            >
              {refreshingModels ? <Loader2 size={13} className="spin" /> : <DownloadCloud size={13} />}
              {t('刷新模型目录')}
            </Button>
          </Tooltip>
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading} style={{ gap: 6 }}>
            <RefreshCw size={13} />
            {loading ? t('加载中...') : t('刷新')}
          </Button>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Tabs
        items={[
          { key: 'catalog', label: t('模型目录') },
          { key: 'ratio', label: t('定价倍率') },
        ]}
        active={tab}
        onChange={(k) => setTab(k as MainTab)}
        ariaLabel={t('模型管理')}
      />

      {tab === 'ratio' ? (
        <RatioPanel />
      ) : (
        <div className="mm-layout">
          {/* 左栏：分组导航（对齐 Agent2API prov-rail） */}
          <nav className="mm-rail" aria-label={t('提供商分组')}>
            <button
              type="button"
              className={`mm-rail-item${navKey === 'all' ? ' active' : ''}`}
              onClick={() => selectNav('all')}
            >
              <span className="mm-mono mute">
                <Boxes size={13} />
              </span>
              <span className="mm-rail-name">{t('全部')}</span>
              <span className="mm-rail-count">{providerStats.total}</span>
            </button>

            <div className="mm-rail-search">
              <Search size={12} className="mm-rail-search-icon" />
              <input
                className="form-input"
                value={railQuery}
                onChange={(e) => setRailQuery(e.target.value)}
                placeholder={t('搜索 provider…')}
                aria-label={t('搜索 provider')}
              />
            </div>

            <div className="mm-rail-title">{t('供应商')}</div>
            {railFilter.channels.length === 0 ? (
              <div className="mm-rail-empty">
                {railFilter.active ? t('无匹配的供应商') : t('暂无启用的供应商')}
              </div>
            ) : (
              railFilter.channels.map((c) => (
                <button
                  key={c.id}
                  type="button"
                  className={`mm-rail-item${navKey === `ch:${c.id}` ? ' active' : ''}`}
                  onClick={() => selectNav(`ch:${c.id}`)}
                  title={`${c.name} · ${c.count} ${t('个模型')}`}
                >
                  <span
                    className="mm-mono"
                    style={{
                      background: `hsla(${providerHue(c.id)}, 65%, 55%, 0.16)`,
                      color: `hsl(${providerHue(c.id)}, 55%, 42%)`,
                    }}
                  >
                    {monogramChar(c.name)}
                  </span>
                  <span className="mm-rail-name">{c.name}</span>
                  <span className="mm-rail-count">{c.count}</span>
                </button>
              ))
            )}

            <div className="mm-rail-title">{t('账号池提供商')}</div>
            {railFilter.builtins.length === 0 ? (
              <div className="mm-rail-empty">
                {railFilter.active ? t('无匹配的供应商') : t('暂无账号池供应商')}
              </div>
            ) : (
              railFilter.builtins.map((g) => (
                <button
                  key={g.id}
                  type="button"
                  className={`mm-rail-item${navKey === `pv:${g.id}` ? ' active' : ''}`}
                  onClick={() => selectNav(`pv:${g.id}`)}
                  title={`${g.label} (${g.enabled}/${g.total})`}
                >
                  <span
                    className="mm-mono"
                    style={{
                      background: `hsla(${providerHue(g.id)}, 65%, 55%, 0.16)`,
                      color: `hsl(${providerHue(g.id)}, 55%, 42%)`,
                    }}
                  >
                    {monogramChar(g.label)}
                  </span>
                  <span className="mm-rail-name">{g.label}</span>
                  <span className="mm-rail-count">
                    {g.enabled}/{g.total}
                  </span>
                </button>
              ))
            )}

            <div className="mm-rail-title">{t('自定义提供商')}</div>
            {railFilter.customs.length === 0 ? (
              <div className="mm-rail-empty">
                {railFilter.active ? t('无匹配的供应商') : t('暂无自定义提供商')}
              </div>
            ) : (
              railFilter.customs.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  className={`mm-rail-item${navKey === `pv:${p.id}` ? ' active' : ''}`}
                  onClick={() => selectNav(`pv:${p.id}`)}
                  title={`${p.label} · ${p.total} ${t('个模型')}`}
                >
                  <span
                    className="mm-mono"
                    style={{
                      background: `hsla(${providerHue(p.id)}, 65%, 55%, 0.16)`,
                      color: `hsl(${providerHue(p.id)}, 55%, 42%)`,
                    }}
                  >
                    {monogramChar(p.label)}
                  </span>
                  <span className="mm-rail-name">{p.label}</span>
                  <span className="mm-rail-count">{p.total}</span>
                </button>
              ))
            )}
            {/* 新建自定义模型（对齐 Agent2API「+新建」沉底动作）；与标题栏「添加模型」同一个弹窗 */}
            <button type="button" className="mm-rail-add" onClick={openAddModel}>
              <Plus size={12} /> {t('新建自定义模型')}
            </button>
          </nav>

          {/* 右侧面板 */}
          <div className="mm-panel">
            {/* 第一行：标题 + 统计 + 动作 */}
            <div className="mm-titlebar">
              <div className="mm-title">
                <span className="mm-title-name">{viewTitle}</span>
                <span className="mm-meta">
                  {headStats.total} {t('个模型')} · {t('已启用')} {headStats.enabled} · {t('有映射')}{' '}
                  {headStats.mapped}
                  {providerStats.unpriced > 0 && ` · ${t('未定价')} ${providerStats.unpriced}`}
                </span>
              </div>
              <div className="mm-head-actions">
                <Button
                  variant="primary"
                  size="sm"
                  onClick={() => void openFetchModal()}
                  style={{ gap: 6 }}
                  title={t('从上游拉取模型列表')}
                >
                  <Download size={13} />
                  {t('获取模型')}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={openAddModel}
                  style={{ gap: 6 }}
                  title={t('手动登记自定义模型')}
                >
                  <Plus size={13} />
                  {t('添加模型')}
                </Button>
                {/* 原「元信息覆盖」Tab 的列表 + 新增入口收进这个弹窗（Tab 已并入目录） */}
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setOverridesModal(true)}
                  style={{ gap: 6 }}
                  title={t('查看 / 新增 / 删除元信息覆盖（归属方、上下文长度、能力声明）')}
                >
                  <SlidersHorizontal size={13} />
                  {t('覆盖管理')}
                </Button>
                <div className="mm-colset" ref={colPanelRef}>
                  <Button
                    variant="outline"
                    size="sm"
                    className={colPanelOpen ? 'mm-colset-open' : undefined}
                    onClick={() => setColPanelOpen((v) => !v)}
                    style={{ gap: 6 }}
                    title={t('列显示设置')}
                  >
                    <Settings2 size={13} />
                    {t('列设置')}
                  </Button>
                  {colPanelOpen && (
                    <TableColumnSettings
                      open
                      onClose={() => setColPanelOpen(false)}
                      defs={MM_COL_DEFS}
                      items={colState.items}
                      onChange={commitColItems}
                      onReset={resetColConfig}
                      anchorRef={colPanelRef}
                    />
                  )}
                </div>
              </div>
            </div>

            {/* 第二行：筛选 */}
            <div className="mm-filters">
              <div className="mm-seg" role="tablist" aria-label={t('状态筛选')}>
                {(
                  [
                    ['all', '全部'],
                    ['on', '已启用'],
                    ['off', '已禁用'],
                    ['mapped', '有映射'],
                  ] as Array<[StateFilter, string]>
                ).map(([v, label]) => (
                  <button
                    key={v}
                    type="button"
                    className={`mm-seg-btn${stateFilter === v ? ' active' : ''}`}
                    onClick={() => setStateFilter(v)}
                  >
                    {t(label)}
                  </button>
                ))}
              </div>
              {/* 可见性筛选：档位与默认值照 Agent2API MODEL_VISIBILITY_OPTIONS /
                  DEFAULT_FILTERS（all / visible / hidden / undeclared，默认 visible）。
                  「可见」这一档含未声明 —— 「没判据就不藏」，否则默认档会因为「这家没有这个信息」
                  而把行筛空。档位在「状态」之后、搜索框之前，与 A2A 筛选行的摆放一致。 */}
              <div className="mm-seg" role="tablist" aria-label={t('可见性筛选')}>
                {(
                  [
                    ['all', '全部'],
                    ['visible', '可见'],
                    ['hidden', '不展示'],
                    ['undeclared', '未声明'],
                  ] as Array<[VisibilityFilter, string]>
                ).map(([v, label]) => (
                  <button
                    key={v}
                    type="button"
                    className={`mm-seg-btn${visibilityFilter === v ? ' active' : ''}`}
                    title={
                      v === 'visible'
                        ? t('官方客户端会展示的模型；没有可见性判据的家也归在这一档（没依据就不藏）')
                        : v === 'hidden'
                          ? t('上游能列出这个模型，但官方客户端不展示它')
                          : v === 'undeclared'
                            ? t('这家的目录没有给出可见性判据')
                            : undefined
                    }
                    onClick={() => setVisibilityFilter(v)}
                  >
                    {t(label)}
                  </button>
                ))}
              </div>
              <div className="mm-seg" role="tablist" aria-label={t('排序')}>
                {(
                  [
                    ['default', '默认'],
                    ['ratio-asc', '倍率升序'],
                    ['ratio-desc', '倍率降序'],
                  ] as Array<[SortMode, string]>
                ).map(([v, label]) => (
                  <button
                    key={v}
                    type="button"
                    className={`mm-seg-btn${sortMode === v ? ' active' : ''}`}
                    onClick={() => setSortMode(v)}
                  >
                    {t(label)}
                  </button>
                ))}
              </div>
              <div className="models-search mm-search">
                <Search size={13} className="models-search-icon" />
                <input
                  className="form-input"
                  placeholder={t('搜索模型 ID / 名称 / 映射名 / 来源…')}
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  aria-label={t('搜索模型')}
                />
              </div>
              <select
                className="form-input mm-mini-select"
                value={sourceFilter}
                onChange={(e) => setSourceFilter(e.target.value as SourceFilter)}
                aria-label={t('来源筛选')}
              >
                <option value="all">{t('全部来源')}</option>
                <option value="channel">{t('仅供应商')}</option>
                <option value="account">{t('仅账号池')}</option>
              </select>
              <select
                className="form-input mm-mini-select"
                value={channelFilter}
                onChange={(e) => setChannelFilter(e.target.value)}
                aria-label={t('供应商筛选')}
              >
                <option value="all">{t('全部供应商')}</option>
                {channelOptions.map(([cid, name]) => (
                  <option key={cid} value={cid}>
                    {name}
                  </option>
                ))}
              </select>
              <select
                className="form-input mm-mini-select"
                value={priceFilter}
                onChange={(e) => setPriceFilter(e.target.value as PriceFilter)}
                aria-label={t('定价筛选')}
              >
                <option value="all">{t('全部定价')}</option>
                <option value="priced">{t('已定价')}</option>
                <option value="unpriced">{t('未定价')}</option>
              </select>
            </div>

            {/* 悬空映射提醒（数据健康提示，不随搜索/筛选缩水——对齐上游 DanglingMappings） */}
            {danglingMappings.length > 0 && (
              <section className="mm-dangling" aria-label={t('未挂载的模型映射')}>
                <div className="mm-dg-head">
                  <span className="mm-dg-title">{t('未挂载的模型映射')}</span>
                  <span className="mm-dg-count">{danglingMappings.length}</span>
                </div>
                <p className="mm-dg-hint">
                  {t('这些映射指向的上游模型已不在当前清单里，挂不到任何模型行上：用对应别名发请求会失败。删除后该对外名即可重新使用。')}
                </p>
                <ul className="mm-dg-list">
                  {danglingMappings.map(({ alias, target }) => {
                    const isDefault = alias === target;
                    return (
                      <li key={`${alias}\u0001${target}`} className="mm-dg-item">
                        <code
                          className="mm-dg-alias"
                          title={t('点击复制：{{name}}', { name: alias })}
                          onClick={() => copyText(alias)}
                          role="button"
                          tabIndex={0}
                          onKeyDown={(e) => { if (e.key === 'Enter') copyText(alias); }}
                        >
                          {alias}
                        </code>
                        {isDefault ? (
                          <span className="mm-dg-tag">{t('默认绑定')}</span>
                        ) : (
                          <>
                            <span className="mm-dg-arrow" aria-hidden="true"> → </span>
                            <code
                              className="mm-dg-target"
                              title={t('点击复制：{{name}}', { name: target })}
                              onClick={() => copyText(target)}
                              role="button"
                              tabIndex={0}
                              onKeyDown={(e) => { if (e.key === 'Enter') copyText(target); }}
                            >
                              {target}
                            </code>
                          </>
                        )}
                        <span className="mm-dg-state" title={t('上游已经不再提供这个模型（可能已下线或改了 ID）')}>
                          {t('目标已下线')}
                        </span>
                        <button
                          type="button"
                          className="mm-act-btn danger"
                          title={t('删除这条映射')}
                          onClick={() => removeDangling(alias, target)}
                        >
                          {t('删除')}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </section>
            )}

            {/* 批量条（勾选后出现，对齐 Agent2API batch-chip） */}
            {selected.size > 0 && (
              <div className="mm-batch-chip">
                <span>
                  {t('已选')} <strong>{selected.size}</strong> {t('个')}
                </span>
                <Button variant="outline" size="sm" onClick={() => { setBatchReport(''); setBatchOpen(true); }}>
                  {t('批量操作')}
                </Button>
                {/* 批量映射前缀：给选中模型统一加「前缀+sep+原ID」的对外名 */}
                <Button variant="outline" size="sm" onClick={openPrefixBatch}>
                  {t('批量映射前缀')}
                </Button>
                <button type="button" className="mm-batch-clear" title={t('清除选择')} onClick={() => setSelected(new Set())}>
                  ×
                </button>
              </div>
            )}

            {loading ? (
              <Loading text={t('加载模型目录')} />
            ) : filtered.length === 0 ? (
              <EmptyState
                icon="📦"
                message={
                  query
                    ? t('没有匹配「') + query + t('」的模型')
                    : stateFilter !== 'all' || priceFilter !== 'all' || navKey !== 'all'
                      ? t('没有匹配的模型')
                      : t('暂无模型。请先在供应商或账号池里配置上游。')
                }
              />
            ) : (
              <>
                <div className="mm-table-wrap table-wrapper">
                  <table
                    className="mm-table"
                    // 表格宽度下限（对齐 Agent2API table-columns.js 的 tableMinWidthOf）：
                    // 拖宽一列之后不再从别处扣 —— 其余列停在 56px 兜底份额上，多出来的部分
                    // 交给外层横向滚动。不写下限时其余列会被压成 0 宽，表头直接消失。
                    style={{ minWidth: visibleCols.reduce((sum, col) => (
                      sum + (colResize.effectiveWidth(col) ?? (MM_COL_DEFS.find((d) => d.key === col)?.width ?? 56))
                    ), 0) }}
                  >
                    <colgroup>
                      {visibleCols.map((col) => (
                        <col
                          key={col}
                          data-col-key={col}
                          style={{
                            width: colResize.effectiveWidth(col) != null
                              ? `${colResize.effectiveWidth(col)}px`
                              : COL_DEFAULT_W[col],
                          }}
                        />
                      ))}
                    </colgroup>
                    <thead>
                      <tr>{visibleCols.map(renderHeadCell)}</tr>
                    </thead>
                    <tbody>
                      {groupedMode
                        ? groups.map((g) => {
                            const expanded = expandedGroups.has(g.id);
                            const shown = expanded ? g.rows : g.rows.slice(0, GROUP_LIMIT);
                            const hidden = g.rows.length - shown.length;
                            return (
                              <FragmentGroup key={g.id}>
                                <tr className="mm-tr-group">
                                  <td colSpan={visibleCols.length}>
                                    <span
                                      className="mm-mono"
                                      style={{
                                        background: `hsla(${providerHue(g.id)}, 65%, 55%, 0.16)`,
                                        color: `hsl(${providerHue(g.id)}, 55%, 42%)`,
                                      }}
                                    >
                                      {monogramChar(g.label)}
                                    </span>
                                    <span className="mm-group-label">{g.label}</span>
                                    <span className="mm-group-count">{g.rows.length}</span>
                                  </td>
                                </tr>
                                {shown.map((row) => (
                                  <MmRow
                                    key={rowKey(row)}
                                    row={row}
                                    visibleCols={visibleCols}
                                    isSelected={selected.has(rowKey(row))}
                                    callbacks={rowCallbacks}
                                  />
                                ))}
                                {hidden > 0 && (
                                  <tr className="mm-tr-more">
                                    <td colSpan={visibleCols.length}>
                                      <button type="button" className="mm-more-btn" onClick={() => toggleGroup(g.id)}>
                                        <ChevronDown size={12} /> {t('展开其余')} {hidden} {t('个模型')}
                                      </button>
                                    </td>
                                  </tr>
                                )}
                                {expanded && g.rows.length > GROUP_LIMIT && (
                                  <tr className="mm-tr-more">
                                    <td colSpan={visibleCols.length}>
                                      <button type="button" className="mm-more-btn" onClick={() => toggleGroup(g.id)}>
                                        <ChevronUp size={12} /> {t('收起')}
                                      </button>
                                    </td>
                                  </tr>
                                )}
                              </FragmentGroup>
                            );
                          })
                        : pageRows.map((row) => (
                            <MmRow
                              key={rowKey(row)}
                              row={row}
                              visibleCols={visibleCols}
                              isSelected={selected.has(rowKey(row))}
                              callbacks={rowCallbacks}
                            />
                          ))}
                    </tbody>
                  </table>
                </div>

                {/* 分页页脚：统一走公共 TablePaginationFooter（总数 / 每页档位 / 页码 / 跳页） */}
                {!groupedMode && (
                  <TablePaginationFooter
                    total={filtered.length}
                    page={currentPage}
                    totalPages={totalPages}
                    size={pageSize}
                    sizes={CLIENT_PAGE_SIZES}
                    onSizeChange={setPageSize}
                    onPageChange={setPage}
                  />
                )}
              </>
            )}
          </div>
        </div>
      )}

      {/* ── 覆盖管理弹窗（原「元信息覆盖」Tab 的列表/新增/删除入口，Tab 已并入目录） ── */}
      {overridesModal && (
        <div className="modal-overlay" onClick={() => setOverridesModal(false)}>
          <div className="modal mm-overrides-modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('元信息覆盖')}</h3>
              <button className="modal-close" onClick={() => setOverridesModal(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <p className="mm-dialog-note">
                {t('覆盖内置推断的归属方 / 上下文长度 / 能力声明')}
              </p>
              <div className="mm-overrides-toolbar">
                <Button variant="primary" size="sm" onClick={openMetaCreate} style={{ gap: 6 }}>
                  <Plus size={13} />
                  {t('新增覆盖')}
                </Button>
              </div>
              {Object.keys(overrides).length === 0 ? (
                <EmptyState
                  icon="🏷️"
                  message={t('暂无覆盖项。内置推断已覆盖常见模型，需要自定义归属/上下文/能力时在此添加')}
                />
              ) : (
                <div className="mm-table-wrap mm-overrides-table">
                  <table className="mm-table">
                    <thead>
                      <tr>
                        <th>{t('模型')}</th>
                        <th>{t('归属方')}</th>
                        <th>{t('上下文长度')}</th>
                        <th>{t('能力')}</th>
                        <th style={{ width: 96 }}>{t('操作')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {Object.entries(overrides).map(([model, o]) => (
                        <tr key={model}>
                          <td>
                            <span className="mm-mid">{model}</span>
                          </td>
                          <td>{o.owned_by}</td>
                          <td>{o.context_length ?? '—'}</td>
                          <td>
                            <div className="mm-sources">
                              {(o.capabilities ?? []).map((c) => (
                                <span key={c} className="mm-src-tag provider">{c}</span>
                              ))}
                            </div>
                          </td>
                          <td>
                            <div className="mm-actions">
                              <Tooltip label={t('编辑')}>
                                <button type="button" className="mm-act-btn" onClick={() => openMetaEdit(model)}>
                                  <Pencil size={11} />
                                </button>
                              </Tooltip>
                              <Tooltip label={t('删除')}>
                                <button type="button" className="mm-act-btn danger icon-only" onClick={() => removeMeta(model)}>
                                  <Trash2 size={11} />
                                </button>
                              </Tooltip>
                            </div>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setOverridesModal(false)}>
                {t('关闭')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 元信息覆盖弹窗 ── */}
      {metaModal && (
        <div className="modal-overlay" onClick={() => setMetaModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('元信息覆盖')}</h3>
              <button className="modal-close" onClick={() => setMetaModal(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {metaError && <div className="error-message">{metaError}</div>}
              <p className="mm-dialog-note">
                {t('这些声明展示给下游客户端（/v1/models 的 owned_by / context_length / capabilities），不影响网关转发与路由。')}
              </p>
              <div className="form-group">
                <label>{t('模型 ID')} *</label>
                <input
                  className="form-input"
                  value={metaForm.model}
                  onChange={(e) => setMetaForm({ ...metaForm, model: e.target.value })}
                  placeholder="gpt-4o"
                  disabled={savingMeta}
                />
              </div>
              <div className="form-group">
                <label>{t('归属方')} *</label>
                <input
                  className="form-input"
                  value={metaForm.owned_by}
                  onChange={(e) => setMetaForm({ ...metaForm, owned_by: e.target.value })}
                  placeholder="openai"
                  disabled={savingMeta}
                />
              </div>
              <div className="form-group">
                <label>{t('上下文窗口')}</label>
                <input
                  className="form-input"
                  value={metaForm.context_length}
                  onChange={(e) => setMetaForm({ ...metaForm, context_length: e.target.value })}
                  placeholder={t('留空继承内置推断')}
                  disabled={savingMeta}
                />
              </div>
              <div className="form-group">
                <label>{t('可见性')}</label>
                {/* 对齐 Agent2API 的 `visibility` 键（hidden / shown / 无判据）。
                    「不覆盖」= 不写该键，落后端 `None` = 没判据，管理页显示「未声明」，
                    **不**当成「可见」来伪造 —— 与 A2A「没依据就不藏」同一条纪律。 */}
                <div className="mm-caps-mode" role="tablist" aria-label={t('可见性判据')}>
                  <button
                    type="button"
                    className={`mm-seg-btn${metaForm.visibilityMode === 'inherit' ? ' active' : ''}`}
                    title={t('不写覆盖项：这家目录没有给出可见性判据，管理页按「未声明」展示')}
                    onClick={() => setMetaForm({ ...metaForm, visibilityMode: 'inherit' })}
                    disabled={savingMeta}
                  >
                    {t('不覆盖')}
                  </button>
                  <button
                    type="button"
                    className={`mm-seg-btn${metaForm.visibilityMode === 'shown' ? ' active' : ''}`}
                    title={t('官方客户端会展示这个模型')}
                    onClick={() => setMetaForm({ ...metaForm, visibilityMode: 'shown' })}
                    disabled={savingMeta}
                  >
                    {t('可见')}
                  </button>
                  <button
                    type="button"
                    className={`mm-seg-btn${metaForm.visibilityMode === 'hidden' ? ' active' : ''}`}
                    title={t('上游能列出这个模型，但官方客户端不展示它')}
                    onClick={() => setMetaForm({ ...metaForm, visibilityMode: 'hidden' })}
                    disabled={savingMeta}
                  >
                    {t('不展示')}
                  </button>
                </div>
              </div>
              <div className="form-group">
                <label>{t('能力声明')}</label>
                <div className="mm-caps-mode" role="tablist" aria-label={t('能力声明模式')}>
                  <button
                    type="button"
                    className={`mm-seg-btn${metaForm.capsMode === 'inherit' ? ' active' : ''}`}
                    onClick={() => setMetaForm({ ...metaForm, capsMode: 'inherit' })}
                    disabled={savingMeta}
                  >
                    {t('继承内置推断')}
                  </button>
                  <button
                    type="button"
                    className={`mm-seg-btn${metaForm.capsMode === 'custom' ? ' active' : ''}`}
                    onClick={() => setMetaForm({ ...metaForm, capsMode: 'custom' })}
                    disabled={savingMeta}
                  >
                    {t('显式声明')}
                  </button>
                </div>
                {metaForm.capsMode === 'custom' ? (
                  <>
                    <div className="mm-caps-toggles">
                      {(
                        [
                          ['capTool', '工具调用', Wrench],
                          ['capImage', '图像识别', ImageIcon],
                          ['capVideo', '视频识别', Video],
                          ['capAudio', '音频', AudioLines],
                          ['capReasoning', '思考', Lightbulb],
                        ] as Array<[keyof MetaForm, string, typeof Wrench]>
                      ).map(([key, label, Icon]) => {
                        const on = Boolean(metaForm[key]);
                        return (
                          <button
                            key={String(key)}
                            type="button"
                            className={`mm-cap-toggle${on ? ' on' : ''}`}
                            aria-pressed={on}
                            disabled={savingMeta}
                            onClick={() => setMetaForm({ ...metaForm, [key]: !on })}
                          >
                            <Icon size={12} /> {t(label)}
                          </button>
                        );
                      })}
                    </div>
                    <input
                      className="form-input"
                      value={metaForm.extraTags}
                      onChange={(e) => setMetaForm({ ...metaForm, extraTags: e.target.value })}
                      placeholder={t('其他标签（逗号分隔）：embedding, rerank…')}
                      disabled={savingMeta}
                      style={{ marginTop: 8 }}
                    />
                    {/* 思考等级候选（对齐 Agent2API 能力弹窗的 reasoningLevels 编辑） */}
                    <input
                      className="form-input"
                      value={metaForm.reasoningLevels}
                      onChange={(e) => setMetaForm({ ...metaForm, reasoningLevels: e.target.value })}
                      placeholder={t('思考等级候选（逗号分隔）：off, low, medium, high…')}
                      disabled={savingMeta}
                      style={{ marginTop: 8 }}
                    />
                  </>
                ) : (
                  <p className="mm-dialog-note" style={{ marginTop: 8 }}>
                    {t('能力标签按模型名内置推断（chat / embedding / image / audio…），下游按推断值展示。')}
                  </p>
                )}
              </div>
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setMetaModal(false)} disabled={savingMeta}>
                {t('取消')}
              </Button>
              <Button variant="primary" onClick={() => void saveMeta()} disabled={savingMeta}>
                {savingMeta ? t('保存中...') : t('保存')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 定价弹窗 ── */}
      {priceTarget && (
        <div className="modal-overlay" onClick={() => setPriceTarget(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('编辑定价')}</h3>
              <button className="modal-close" onClick={() => setPriceTarget(null)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {priceError && <div className="error-message">{priceError}</div>}
              <div className="form-group">
                <label>{t('模型')}</label>
                <input className="form-input" value={priceTarget.id} disabled />
              </div>
              <div className="form-group">
                <label>{t('计价方式')}</label>
                <select
                  className="form-input"
                  value={priceForm.price_type}
                  onChange={(e) => setPriceForm({ ...priceForm, price_type: e.target.value })}
                  disabled={savingPrice}
                >
                  <option value="token">{t('按 token 计价')}</option>
                  <option value="count">{t('按次计价')}</option>
                  <option value="free">{t('免费')}</option>
                </select>
              </div>
              {priceForm.price_type === 'free' ? (
                <div className="form-group">
                  <p style={{ margin: 0, color: 'var(--text-muted)' }}>
                    {t('价格已配置为 0，调用不计费')}
                  </p>
                  <p style={{ margin: '6px 0 0', color: 'var(--text-muted)' }}>
                    {t('免费模型仍有定价条目，因此保留在客户端模型清单里；未定价模型则不会出现在清单中。')}
                  </p>
                </div>
              ) : (
                <>
              <div className="form-group">
                <label>
                  {priceForm.price_type === 'count' ? t('每次价格') : t('输入价格（每 1k token）')}
                </label>
                <input
                  className="form-input"
                  value={priceForm.input_price}
                  onChange={(e) => setPriceForm({ ...priceForm, input_price: e.target.value })}
                  placeholder="0.0005"
                  disabled={savingPrice}
                />
              </div>
              {priceForm.price_type !== 'count' && (
                <>
                  <div className="form-group">
                    <label>{t('输出价格（每 1k token）')}</label>
                    <input
                      className="form-input"
                      value={priceForm.output_price}
                      onChange={(e) => setPriceForm({ ...priceForm, output_price: e.target.value })}
                      placeholder="0.0015"
                      disabled={savingPrice}
                    />
                  </div>
                  <div className="form-group">
                    <label>{t('缓存价格（可选）')}</label>
                    <input
                      className="form-input"
                      value={priceForm.cache_price}
                      onChange={(e) => setPriceForm({ ...priceForm, cache_price: e.target.value })}
                      placeholder="0.0001"
                      disabled={savingPrice}
                    />
                  </div>
                </>
              )}
              <div className="form-group">
                <label>{t('模型倍率（可选，默认 1.0）')}</label>
                <input
                  className="form-input"
                  value={priceForm.model_ratio}
                  onChange={(e) => setPriceForm({ ...priceForm, model_ratio: e.target.value })}
                  placeholder="1.0"
                  disabled={savingPrice}
                />
              </div>
                </>
              )}
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setPriceTarget(null)} disabled={savingPrice}>
                {t('取消')}
              </Button>
              <Button variant="primary" onClick={() => void savePrice()} disabled={savingPrice}>
                {savingPrice ? t('保存中...') : t('保存')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 别名弹窗 ── */}
      {aliasTarget && (
        <div className="modal-overlay" onClick={() => setAliasTarget(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>
                {aliasForm.alias.trim() && aliasTarget.aliases.includes(aliasForm.alias.trim())
                  ? t('编辑映射')
                  : t('添加映射')}
              </h3>
              <button className="modal-close" onClick={() => setAliasTarget(null)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {aliasError && <div className="error-message">{aliasError}</div>}
              <div className="form-group">
                <label>{t('对外映射名（客户端调用的名字）')} *</label>
                <input
                  className="form-input"
                  value={aliasForm.alias}
                  onChange={(e) => setAliasForm({ ...aliasForm, alias: e.target.value })}
                  placeholder="gpt-4o"
                  disabled={savingAlias}
                  autoFocus
                />
              </div>
              <div className="form-group">
                <label>{t('转发到上游模型')} *</label>
                <Combobox
                  value={aliasForm.target}
                  options={aliasProviderOptions.length ? aliasProviderOptions : modelIdOptions}
                  allowFreeText={aliasProviderOptions.length === 0}
                  placeholder={
                    aliasProviderOptions.length
                      ? t('从当前提供商的模型清单中选择目标')
                      : t('搜索或输入目标模型名')
                  }
                  onChange={(v) => setAliasForm({ ...aliasForm, target: v })}
                  disabled={savingAlias}
                />
                {aliasProviderOptions.length > 0 && (
                  <span className="form-hint">
                    {t('目标限定在当前提供商的模型清单内，避免建出跨家调不通的映射。')}
                  </span>
                )}
              </div>
              <div className="form-group">
                <label>{t('映射级思考等级（不覆盖 = 跟随客户端）')}</label>
                {(() => {
                  // 候选三级回落（模型声明 > 兜底表），弹窗打开时目标模型即锁定
                  const candidates = reasoningLevelCandidates(overrides, aliasForm.target.trim());
                  const current = aliasForm.reasoning_effort.trim();
                  const isCustom = current !== '' && !candidates.includes(current);
                  const selectValue = current === '' ? '' : isCustom ? CUSTOM_LEVEL : current;
                  return (
                    <>
                      <select
                        className="form-input"
                        value={selectValue}
                        onChange={(e) => {
                          const v = e.target.value;
                          setAliasForm({
                            ...aliasForm,
                            // 哨兵 = 进入自定义编辑：空值时先给一个可编辑的种子值
                            // （保持空会让 select 弹回「不覆盖」，输入框永远露不出来），
                            // 已有值则原样保留（切档不丢用户输入）
                            reasoning_effort: v === CUSTOM_LEVEL ? (current || 'custom') : v,
                          });
                        }}
                        disabled={savingAlias}
                      >
                        <option value="">{t('不覆盖（跟随客户端请求）')}</option>
                        {candidates.map((level) => (
                          <option key={level} value={level}>{level}</option>
                        ))}
                        {/* 已有等级不在候选里时也要能显示出来（否则 select 显示空白） */}
                        {isCustom && <option value={CUSTOM_LEVEL}>{t('自定义等级…（{{level}}）', { level: current })}</option>}
                        {!isCustom && <option value={CUSTOM_LEVEL}>{t('自定义等级…')}</option>}
                      </select>
                      {selectValue === CUSTOM_LEVEL && (
                        <input
                          className="form-input"
                          style={{ marginTop: 6 }}
                          value={aliasForm.reasoning_effort}
                          onChange={(e) => setAliasForm({ ...aliasForm, reasoning_effort: e.target.value })}
                          placeholder={t('自定义等级（例如 custom-high）')}
                          disabled={savingAlias}
                          autoFocus
                        />
                      )}
                      <span className="form-hint">
                        {candidates.length
                          ? t('仅当客户端未在请求里显式指定 reasoning_effort 时叠加；候选来自该模型的元信息覆盖声明，未声明时用通用档位。')
                          : t('该模型声明了空档位表（上游不认识任何等级），建议留空不覆盖。')}
                      </span>
                    </>
                  );
                })()}
              </div>
              <div className="mm-alias-preview">
                {aliasForm.alias.trim() && aliasForm.target.trim() ? (
                  <>
                    {t('下游请求')} <code>{aliasForm.alias.trim()}</code> → {t('转发')}{' '}
                    <code>{aliasForm.target.trim()}</code>
                  </>
                ) : (
                  <span className="mm-alias-preview-empty">
                    {t('填上对外名与目标模型后，这里会显示实际转发链路')}
                  </span>
                )}
              </div>
              <p className="mm-dialog-note">
                {t('映射是全局的：客户端请求对外名时，网关按映射改名为目标模型再转发。同一对外名可被多条映射使用（主备切换）。')}
              </p>
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setAliasTarget(null)} disabled={savingAlias}>
                {t('取消')}
              </Button>
              <Button variant="primary" onClick={() => void saveAlias()} disabled={savingAlias}>
                {savingAlias ? t('保存中...') : t('保存')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 批量映射前缀弹窗（对齐 Agent2API 批量加映射的前缀选项） ── */}
      {prefixModal && (
        <div className="modal-overlay" onClick={() => !savingPrefix && setPrefixModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>
                {t('批量映射前缀')} · {t('已选')} {selectedRows.length} {t('个模型')}
              </h3>
              <button className="modal-close" onClick={() => setPrefixModal(false)} disabled={savingPrefix}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {prefixError && <div className="error-message">{prefixError}</div>}
              <div className="mm-batch-models">
                {selectedRows.map((m) => (
                  <code key={rowKey(m)}>{m.id}</code>
                ))}
              </div>
              <div className="form-group">
                <label>{t('对外名前缀')} *</label>
                <input
                  className="form-input"
                  value={prefixForm.prefix}
                  onChange={(e) => setPrefixForm({ ...prefixForm, prefix: e.target.value })}
                  placeholder="gw"
                  disabled={savingPrefix}
                  autoFocus
                />
              </div>
              <div className="form-group">
                <label>{t('分隔符')}</label>
                <select
                  className="form-input"
                  value={prefixForm.sep}
                  onChange={(e) => setPrefixForm({ ...prefixForm, sep: e.target.value })}
                  disabled={savingPrefix}
                >
                  <option value="-">-</option>
                  <option value="/">/</option>
                  <option value=".">.</option>
                  <option value="__">__</option>
                </select>
              </div>
              <label className="mm-batch-action">
                <input
                  type="checkbox"
                  checked={prefixForm.merge}
                  onChange={(e) => setPrefixForm({ ...prefixForm, merge: e.target.checked })}
                  disabled={savingPrefix}
                />
                {t('同名合并：同前缀+同后缀可指向多个目标（主备）')}
              </label>
              <div className="mm-alias-preview">
                <span>
                  {t('预览')}：{prefixForm.prefix.trim() || '<前缀>'}
                  {prefixForm.sep}
                  {'<模型ID>'}
                </span>
              </div>
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setPrefixModal(false)} disabled={savingPrefix}>
                {t('取消')}
              </Button>
              <Button variant="primary" onClick={() => void savePrefixBatch()} disabled={savingPrefix}>
                {savingPrefix ? t('执行中…') : t('执行')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 获取模型弹窗 ── */}
      {fetchModal && (
        <div className="modal-overlay" onClick={() => setFetchModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('获取模型')}</h3>
              <button className="modal-close" onClick={() => setFetchModal(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <p className="mm-dialog-note">
                {t('从上游拉取模型清单（写入供应商的「上游发现」快照）。账号池型供应商按其账号逐号拉取目录（只更新目录与缓存，模型白名单仍在账号池维护）。')}
              </p>
              {fetchChannels.length === 0 ? (
                <EmptyState icon="📡" message={t('暂无启用的可拉取供应商')} />
              ) : (
                <>
                  <div className="mm-fetch-toolbar">
                    <span className="mm-fetch-summary">
                      {t('共')} {fetchChannels.length} {t('个供应商')}
                      {Object.keys(fetchDone).length > 0 &&
                        ` · ${t('成功')} ${Object.keys(fetchDone).length}`}
                      {Object.keys(fetchFailed).length > 0 &&
                        ` · ${t('失败')} ${Object.keys(fetchFailed).length}`}
                    </span>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={fetchingId !== null || fetchAllRunning}
                      onClick={() => void fetchAll()}
                    >
                      {fetchAllRunning || fetchingId !== null ? (
                        <>
                          <Loader2 size={12} className="spin" /> {t('拉取中...')}
                        </>
                      ) : (
                        Object.keys(fetchDone).length > 0 ? t('重新获取全部') : t('全部拉取')
                      )}
                    </Button>
                  </div>
                  <div className="mm-fetch-list">
                    {fetchChannels.map((ch) => {
                      const cid = String(ch.id);
                      const busy = fetchingId === cid || (fetchAllRunning && fetchingId === null && !fetchDone[cid] && !fetchFailed[cid]);
                      const accs = ch.channel_type === 'account'
                        ? fetchAccounts.filter((a) => a.channel_id === cid)
                        : [];
                      const accPick = fetchAccountPick[cid] ?? '';
                      return (
                        <div key={cid} className="mm-fetch-row">
                          <div className="mm-fetch-info">
                            <strong>{ch.name}</strong>
                            <span className="mm-model-sub">
                              {ch.channel_type === 'account' ? t('账号池') : ch.channel_type} · {t('已发现')} {(ch.discovered_models ?? []).length}
                            </span>
                            {accs.length > 0 && (
                              <div className="mm-fetch-acct">
                                <select
                                  className="form-input mm-mini-select"
                                  value={accPick}
                                  aria-label={t('模型来源账号')}
                                  onChange={(e) =>
                                    setFetchAccountPick((prev) => ({ ...prev, [cid]: e.target.value }))
                                  }
                                >
                                  <option value="">{t('模型来源账号：全部账号')}</option>
                                  {accs.map((a) => (
                                    <option key={a.id} value={a.id}>
                                      {a.name || a.id}
                                    </option>
                                  ))}
                                </select>
                                {accPick && (
                                  <Button
                                    variant="outline"
                                    size="sm"
                                    disabled={fetchingAccountCid !== null}
                                    onClick={() => void fetchOneAccount(ch, accPick)}
                                  >
                                    {fetchingAccountCid === cid ? (
                                      <>
                                        <Loader2 size={12} className="spin" /> {t('拉取中...')}
                                      </>
                                    ) : (
                                      t('按账号拉取')
                                    )}
                                  </Button>
                                )}
                              </div>
                            )}
                            {fetchFailed[cid] && (
                              <span className="mm-fetch-err" title={fetchFailed[cid]}>
                                {fetchFailed[cid].length > 60 ? `${fetchFailed[cid].slice(0, 60)}…` : fetchFailed[cid]}
                              </span>
                            )}
                          </div>
                          {fetchDone[cid] ? (
                            <span className="mm-badge-ok">
                              {t('已拉取')}
                              {(fetchCleaned[cid] ?? 0) > 0
                                ? ` · ${t('清理失效映射 {{n}} 条', { n: fetchCleaned[cid] })}`
                                : ''}
                            </span>
                          ) : (
                            <Button
                              variant="outline"
                              size="sm"
                              disabled={fetchingId !== null || fetchAllRunning}
                              onClick={() => void fetchOneClick(ch)}
                            >
                              {busy ? (
                                <>
                                  <Loader2 size={12} className="spin" /> {t('拉取中...')}
                                </>
                              ) : fetchFailed[cid] ? (
                                t('重试')
                              ) : (
                                t('拉取')
                              )}
                            </Button>
                          )}
                        </div>
                      );
                    })}
                  </div>
                </>
              )}
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setFetchModal(false)}>
                {t('关闭')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 添加自定义模型弹窗 ── */}
      {addModelOpen && (
        <div className="modal-overlay" onClick={() => setAddModelOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('添加自定义模型')}</h3>
              <button className="modal-close" onClick={() => setAddModelOpen(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <p className="mm-dialog-note">
                {t('手动登记上游未发现（或暂未接入供应商）的模型，使其出现在模型清单里，可定价 / 测试 / 启停。不编造能力位元数据。')}
              </p>
              {addModelError && <div className="error-message">{addModelError}</div>}
              <div className="form-group">
                <label>{t('Provider')} *</label>
                <Combobox
                  value={addModelProvider}
                  options={Array.from(
                    new Set([
                      ...(view?.providers ?? []).map((p) => p.id),
                      'openai',
                      'anthropic',
                      'deepseek',
                      'zhipu',
                      'gemini',
                      'moonshot',
                      'qwen',
                      'custom',
                    ]),
                  ).sort((a, b) => a.localeCompare(b))}
                  placeholder={t('搜索或输入 provider（如 openai / zhipu / custom）')}
                  onChange={(v) => setAddModelProvider(v)}
                  disabled={addingModel}
                />
                <span className="form-hint">
                  {t('provider 标识（默认 custom）；自定义模型登记后可在目录中启停、定价与映射。')}
                </span>
              </div>
              <div className="form-group">
                <label>{t('模型 ID')} *</label>
                <input
                  className="form-input"
                  value={addModelId}
                  onChange={(e) => setAddModelId(e.target.value)}
                  placeholder="gpt-4o / claude-3-5-sonnet / deepseek-chat"
                  disabled={addingModel}
                  autoFocus
                />
              </div>
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setAddModelOpen(false)} disabled={addingModel}>
                {t('取消')}
              </Button>
              <Button variant="primary" onClick={() => void saveAddModel()} disabled={addingModel}>
                {addingModel ? t('添加中...') : t('添加')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 批量操作弹窗（对齐 Agent2API models-batch-dialog） ── */}
      {batchOpen && (
        <div className="modal-overlay" onClick={() => !batchRunning && setBatchOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>
                {t('批量操作')} · {t('已选')} {selectedRows.length} {t('个模型')}
              </h3>
              <button className="modal-close" onClick={() => setBatchOpen(false)} disabled={batchRunning}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <div className="mm-batch-models">
                {selectedRows.map((m) => (
                  <code key={rowKey(m)}>{m.id}</code>
                ))}
              </div>
              <div className="mm-batch-actions">
                {(
                  [
                    ['enable', '启用'],
                    ['disable', '禁用'],
                    ['set-reasoning', '设思考等级'],
                    ['remove-price', '删除定价'],
                    ['remove-custom', '删除自定义模型'],
                  ] as Array<[BatchAction, string]>
                ).map(([v, label]) => (
                  <label key={v} className={`mm-batch-action${batchAction === v ? ' active' : ''}`}>
                    <input
                      type="radio"
                      name="mm-batch-action"
                      checked={batchAction === v}
                      onChange={() => setBatchAction(v)}
                      disabled={batchRunning}
                    />
                    {t(label)}
                  </label>
                ))}
              </div>
              {batchAction === 'remove-price' && (
                <p className="mm-dialog-note">{t('未定价的模型会被跳过；删除后按 0 计费。')}</p>
              )}
              {batchAction === 'remove-custom' && (
                <p className="mm-dialog-note">{t('仅删除自定义 / 手动登记的行，内置与远程目录模型会被跳过；此操作不可撤销。')}</p>
              )}
              {batchAction === 'set-reasoning' && (
                <div className="form-group">
                  <label>{t('思考等级（留空 = 清除已有等级）')}</label>
                  <input
                    className="form-input"
                    value={batchReasoning}
                    onChange={(e) => setBatchReasoning(e.target.value)}
                    placeholder="off / minimal / low / medium / high / xhigh"
                    disabled={batchRunning}
                    autoFocus
                  />
                  <span className="form-hint">
                    {t('仅对选中模型的别名设置思考等级；无别名的模型会被跳过')}
                  </span>
                </div>
              )}
              {batchReport && <div className="error-message">{batchReport}</div>}
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setBatchOpen(false)} disabled={batchRunning}>
                {t('取消')}
              </Button>
              <Button
                variant={batchAction === 'remove-price' || batchAction === 'remove-custom' ? 'danger' : 'primary'}
                onClick={() => void runBatch()}
                disabled={batchRunning || selectedRows.length === 0}
              >
                {batchRunning ? t('执行中…') : `${t('执行')}${t(batchAction === 'enable' ? '启用' : batchAction === 'disable' ? '禁用' : batchAction === 'set-reasoning' ? '设思考等级' : batchAction === 'remove-custom' ? '删除自定义模型' : '删除定价')}`}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 模型测试弹窗 ── */}
      {testTarget && (
        <ModelTestDialog
          target={{ provider: testTarget.provider, providerLabel: testTarget.provider_label, model: testTarget }}
          onClose={() => setTestTarget(null)}
        />
      )}

      {/* ── 能力位三态弹窗（provider × model，A2A model-capability）── */}
      {capTarget && (
        <ModelCapabilityDialog
          model={capTarget}
          onClose={() => setCapTarget(null)}
          onSaved={() => void load()}
        />
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}

/** 分组片段（key 需要挂在显式组件上，React 19 前 <> 不支持 key） */
function FragmentGroup({ children }: { children: React.ReactNode }): JSX.Element {
  return <>{children}</>;
}
