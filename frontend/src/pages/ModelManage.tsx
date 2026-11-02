import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useSearchParams } from 'react-router-dom';
import {
  AudioLines,
  Boxes,
  ChevronDown,
  ChevronUp,
  Download,
  FlaskConical,
  GripVertical,
  Image as ImageIcon,
  Lightbulb,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Settings2,
  Tag,
  Trash2,
  Wrench,
} from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Loading, EmptyState, Switch, Pagination, Tabs, Combobox } from '../components/ui';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import ModelTestDialog from '../components/ModelTestDialog';
import { fmtCompact } from '../lib/utils';
import type { ManageView, ManagedModel, ModelMetaOverride, ChannelItem } from '../types';
import './ModelManage.css';

// ── 常量 ─────────────────────────────────────────────────────────────

/** 分页档位（对齐 Agent2API table-shell 的 CLIENT_PAGE_SIZES，去掉 all 防卡顿） */
const PAGE_SIZES = [20, 25, 50, 100];
const DEFAULT_PAGE_SIZE = 25;
/** 聚合视图每组默认展示行数，其余折叠（Agent2API GROUP_LIMIT） */
const GROUP_LIMIT = 8;

/** localStorage 键 */
const LS_FILTERS = 'aigx-mm-filters';
const LS_COLS = 'aigx-mm-cols';
const LS_COLCFG = 'aigx-mm-colcfg';
const LS_COLW = 'aigx-mm-colw';
const LS_PAGE_SIZE = 'aigx-mm-pagesize';

type MainTab = 'catalog' | 'meta';
/** 启停筛选（多一档「有映射」，对齐 Agent2API MODEL_STATE_OPTIONS） */
type StateFilter = 'all' | 'on' | 'off' | 'mapped';
type PriceFilter = 'all' | 'priced' | 'unpriced';
type SourceFilter = 'all' | 'channel' | 'account';
/** 排序档位（对齐 Agent2API MODEL_SORT_OPTIONS） */
type SortMode = 'default' | 'ratio-asc' | 'ratio-desc';

/** 可显隐的列（check / model / act 锁定不可关，但可拖动排序） */
type ColKey = 'enable' | 'price' | 'source' | 'caps' | 'alias';
/** 全部表格列（含锁定列）——默认渲染顺序与默认宽度 */
type AnyCol = 'check' | ColKey | 'model' | 'act';
const COL_ORDER: AnyCol[] = ['check', 'enable', 'model', 'price', 'source', 'caps', 'alias', 'act'];
const COL_DEFAULT_W: Record<AnyCol, string> = {
  check: '34px',
  enable: '46px',
  model: '21%',
  price: '15%',
  source: '16%',
  caps: '10%',
  alias: '33%',
  act: '132px',
};
/** 锁定列：恒显示（表不能没有勾选框 / 模型名 / 操作），但可参与拖序 */
const LOCKED_COLS: AnyCol[] = ['check', 'model', 'act'];
/** 列设置面板显示的列名（锁定列也要有名字） */
const COL_PANEL_LABEL: Record<AnyCol, string> = {
  check: '选择',
  enable: '启用',
  model: '模型',
  price: '定价',
  source: '来源',
  caps: '能力',
  alias: '映射',
  act: '操作',
};

/** 列对齐三档（对齐 Agent2API table-col-settings 的 ALIGNS） */
type Align = 'left' | 'center' | 'right';
const ALIGN_OPTIONS: Array<{ value: Align; label: string }> = [
  { value: 'left', label: '左' },
  { value: 'center', label: '居中' },
  { value: 'right', label: '右' },
];
const isAlign = (v: unknown): v is Align => ALIGN_OPTIONS.some((a) => a.value === v);
/** 各列的默认对齐 = 现有 CSS 的观感（勾/启用居中，其余左对齐），视觉不突变 */
const COL_DEFAULT_ALIGN: Record<AnyCol, Align> = {
  check: 'center',
  enable: 'center',
  model: 'left',
  price: 'left',
  source: 'left',
  caps: 'left',
  alias: 'left',
  act: 'left',
};
/** 一列的配置项：**数组顺序就是用户拖出来的列顺序**（对齐上游 ColConfigItem） */
interface ColItem {
  key: AnyCol;
  visible: boolean;
  align: Align;
}

function defaultColItems(): ColItem[] {
  return COL_ORDER.map((key) => ({ key, visible: true, align: COL_DEFAULT_ALIGN[key] }));
}

/**
 * 读列配置：v2（LS_COLCFG，数组含对齐）→ 旧 LS_COLS 显隐表迁移 → 默认。
 * 脏值逐条忽略（对齐上游 normalize）：新出现的键按默认相对位置插入，
 * 存坏了退回默认 —— 列设置读不出来不该让整张表渲染不了。
 */
function readColItems(): ColItem[] {
  const base = defaultColItems();
  const raw = readLS<unknown>(LS_COLCFG, null);
  if (Array.isArray(raw)) {
    const items: ColItem[] = [];
    for (const entry of raw) {
      if (!entry || typeof entry !== 'object') continue;
      const e = entry as { key?: unknown; visible?: unknown; align?: unknown };
      const found = base.find((b) => b.key === e.key);
      if (!found || items.some((i) => i.key === found.key)) continue;
      const locked = LOCKED_COLS.includes(found.key);
      items.push({
        key: found.key,
        visible: locked || e.visible !== false,
        align: isAlign(e.align) ? e.align : found.align,
      });
    }
    // 缺失的键按 COL_ORDER 的相对位置补回（将来加列时老存盘不至于错位）
    for (const b of base) {
      if (items.some((i) => i.key === b.key)) continue;
      const specIdx = COL_ORDER.indexOf(b.key);
      let insertAt = items.length;
      for (let k = 0; k < items.length; k += 1) {
        if (COL_ORDER.indexOf(items[k].key) > specIdx) {
          insertAt = k;
          break;
        }
      }
      items.splice(insertAt, 0, b);
    }
    return items.length === base.length ? items : base;
  }
  // 旧格式迁移：{col: boolean} 只有显隐，顺序与对齐取默认
  const legacy = readLS<Partial<Record<ColKey, boolean>> | null>(LS_COLS, null);
  if (legacy && typeof legacy === 'object' && !Array.isArray(legacy)) {
    return base.map((b) =>
      !LOCKED_COLS.includes(b.key) && legacy[b.key as ColKey] === false ? { ...b, visible: false } : b,
    );
  }
  return base;
}

/** 能力圆点定义（对齐 Agent2API caps 列四枚圆点：工具/图像/音频/思考） */
const CAP_DOTS: Array<{ key: string; match: RegExp; label: string; icon: typeof Wrench }> = [
  { key: 'tool', match: /tool|function/i, label: '工具调用', icon: Wrench },
  { key: 'image', match: /vision|image/i, label: '图像识别', icon: ImageIcon },
  { key: 'audio', match: /audio|speech|tts/i, label: '音频', icon: AudioLines },
  { key: 'reasoning', match: /reason|think/i, label: '思考', icon: Lightbulb },
];

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

function writeLS(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* 隐私模式下静默 */
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
}

/** 元信息覆盖表单：能力四圆点结构化（对齐 Agent2API CapabilityDialog 的声明式编辑） */
interface MetaForm {
  model: string;
  owned_by: string;
  context_length: string;
  /** inherit = 能力标签留空（继承内置推断）；custom = 显式声明下方勾选项 */
  capsMode: 'inherit' | 'custom';
  capTool: boolean;
  capImage: boolean;
  capAudio: boolean;
  capReasoning: boolean;
  /** 四圆点之外的标签（embedding / rerank / …），逗号分隔 */
  extraTags: string;
}

const EMPTY_META_FORM: MetaForm = {
  model: '',
  owned_by: '',
  context_length: '',
  capsMode: 'inherit',
  capTool: false,
  capImage: false,
  capAudio: false,
  capReasoning: false,
  extraTags: '',
};

/** 能力标签 → 表单勾选态（与 CAP_DOTS 同一套正则口径） */
function capsToForm(caps: string[]): Pick<MetaForm, 'capsMode' | 'capTool' | 'capImage' | 'capAudio' | 'capReasoning' | 'extraTags'> {
  if (!caps.length) {
    return { capsMode: 'inherit', capTool: false, capImage: false, capAudio: false, capReasoning: false, extraTags: '' };
  }
  const has = (re: RegExp): boolean => caps.some((c) => re.test(c));
  const extra = caps
    .filter((c) => !/tool|function|vision|image|audio|speech|tts|reason|think|chat/i.test(c))
    .join(', ');
  return {
    capsMode: 'custom',
    capTool: has(/tool|function/i),
    capImage: has(/vision|image/i),
    capAudio: has(/audio|speech|tts/i),
    capReasoning: has(/reason|think/i),
    extraTags: extra,
  };
}

type BatchAction = 'enable' | 'disable' | 'remove-price';

// ── 主组件 ───────────────────────────────────────────────────────────

export default function ModelManage(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [view, setView] = useState<ManageView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

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
  const [navKey, setNavKey] = useState<string>('all');
  const [stateFilter, setStateFilter] = useState<StateFilter>(() => readLS(LS_FILTERS, { state: 'all' }).state as StateFilter);
  const [priceFilter, setPriceFilter] = useState<PriceFilter>(() => readLS(LS_FILTERS, { price: 'all' }).price as PriceFilter);
  const [sourceFilter, setSourceFilter] = useState<SourceFilter>(() => readLS(LS_FILTERS, { source: 'all' }).source as SourceFilter);
  const [sortMode, setSortMode] = useState<SortMode>(() => readLS(LS_FILTERS, { sort: 'default' }).sort as SortMode);
  const [channelFilter, setChannelFilter] = useState('all');
  const [query, setQuery] = useState('');

  useEffect(() => {
    writeLS(LS_FILTERS, { state: stateFilter, price: priceFilter, source: sourceFilter, sort: sortMode });
  }, [stateFilter, priceFilter, sourceFilter, sortMode]);

  // 分页
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState<number>(() => {
    const n = Number(localStorage.getItem(LS_PAGE_SIZE));
    return PAGE_SIZES.includes(n) ? n : DEFAULT_PAGE_SIZE;
  });
  const [jumpDraft, setJumpDraft] = useState('');

  // 批量选择（key = provider|id）
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [batchOpen, setBatchOpen] = useState(false);
  const [batchAction, setBatchAction] = useState<BatchAction>('enable');
  const [batchRunning, setBatchRunning] = useState(false);
  const [batchReport, setBatchReport] = useState('');

  // 列配置：顺序（拖拽）/ 显隐 / 对齐三档 + 列宽（localStorage 记忆，
  // 对齐 Agent2API table-col-settings + 列宽拖动）
  const [colItems, setColItems] = useState<ColItem[]>(() => readColItems());
  const [colWidths, setColWidths] = useState<Record<string, number>>(() => readLS(LS_COLW, {}));
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);
  const colRowsRef = useRef<HTMLDivElement | null>(null);
  const [draggingCol, setDraggingCol] = useState<AnyCol | null>(null);
  const colDragCleanup = useRef<(() => void) | null>(null);

  // 聚合视图的组折叠（展开的组集合；默认折叠）
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());

  // 获取模型弹窗
  const [fetchModal, setFetchModal] = useState(false);
  const [fetchChannels, setFetchChannels] = useState<ChannelItem[]>([]);
  const [fetchingId, setFetchingId] = useState<string | null>(null);
  const [fetchDone, setFetchDone] = useState<Record<string, boolean>>({});
  /** 单渠道失败标记（重试即清除），对照 Agent2API 获取弹窗的状态徽标 */
  const [fetchFailed, setFetchFailed] = useState<Record<string, string>>({});
  const [fetchAllRunning, setFetchAllRunning] = useState(false);

  const [searchParams] = useSearchParams();

  // 弹窗
  const [priceTarget, setPriceTarget] = useState<ManagedModel | null>(null);
  const [priceForm, setPriceForm] = useState<PriceForm>(EMPTY_PRICE_FORM);
  const [priceError, setPriceError] = useState('');
  const [savingPrice, setSavingPrice] = useState(false);

  const [aliasTarget, setAliasTarget] = useState<ManagedModel | null>(null);
  const [aliasForm, setAliasForm] = useState<AliasForm>({ alias: '', target: '' });
  const [aliasError, setAliasError] = useState('');
  const [savingAlias, setSavingAlias] = useState(false);
  const [pendingAlias, setPendingAlias] = useState<string | null>(null);

  const [testTarget, setTestTarget] = useState<ManagedModel | null>(null);

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [pendingRow, setPendingRow] = useState<string | null>(null);

  const [tab, setTab] = useState<MainTab>('catalog');

  // 元信息覆盖
  const [overrides, setOverrides] = useState<Record<string, ModelMetaOverride>>({});
  const [metaModal, setMetaModal] = useState(false);
  const [metaForm, setMetaForm] = useState<MetaForm>(EMPTY_META_FORM);
  const [metaError, setMetaError] = useState('');
  const [savingMeta, setSavingMeta] = useState(false);

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

  const handleToggle = async (row: ManagedModel, enabled: boolean) => {
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
    const input = Number(priceForm.input_price || 0);
    const output = Number(priceForm.output_price || 0);
    const cache = priceForm.cache_price.trim() === '' ? null : Number(priceForm.cache_price);
    if (!Number.isFinite(input) || !Number.isFinite(output) || input < 0 || output < 0) {
      setPriceError(t('价格必须是非负数字'));
      return;
    }
    if (cache != null && (!Number.isFinite(cache) || cache < 0)) {
      setPriceError(t('缓存价格必须是非负数字'));
      return;
    }
    const ratioText = priceForm.model_ratio.trim();
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
      message: t('删除后该模型将按 0 计费（如仍有渠道声明则会放行）。确认删除？') + ` ${row.id}`,
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
    setAliasForm({ alias: presetAlias ?? '', target: row.id });
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
      const res = await api.setModelAlias(alias, target);
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
      ...capsToForm(o.capabilities ?? []),
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
      ...capsToForm(o?.capabilities ?? row.capabilities ?? []),
    });
    setMetaError('');
    setMetaModal(true);
  };

  /** 四圆点勾选 → 提交给后端的标签数组（chat 恒携带，与内置推断口径一致） */
  const metaFormToCaps = (): string[] => {
    if (metaForm.capsMode === 'inherit') return [];
    const caps: string[] = ['chat'];
    if (metaForm.capTool) caps.push('tool');
    if (metaForm.capImage) caps.push('vision');
    if (metaForm.capAudio) caps.push('audio');
    if (metaForm.capReasoning) caps.push('reasoning');
    for (const tag of metaForm.extraTags.split(/[,，]/).map((c) => c.trim()).filter(Boolean)) {
      if (!caps.includes(tag)) caps.push(tag);
    }
    return caps;
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
          if (batchAction === 'remove-price') {
            if (row.price) await api.removeModelPrice(row.id);
            else skip += 1;
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
          batchAction === 'remove-price'
            ? t('已批量删除定价') + (skip > 0 ? `（${skip} ${t('个未定价跳过')}）` : '')
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
    setFetchFailed({});
    try {
      const res = await api.listChannels({ page: 1, page_size: 200 });
      const list = (res?.data ?? []) as ChannelItem[];
      setFetchChannels(list.filter((c) => c.status === 'enabled'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  const fetchOne = async (ch: ChannelItem): Promise<boolean> => {
    const cid = String(ch.id);
    if (fetchingId) return false;
    setFetchingId(cid);
    try {
      await api.fetchChannelModels({ channel_id: cid });
      setFetchDone((d) => ({ ...d, [cid]: true }));
      setFetchFailed((f) => {
        const next = { ...f };
        delete next[cid];
        return next;
      });
      return true;
    } catch (err) {
      setFetchFailed((f) => ({ ...f, [cid]: err instanceof Error ? err.message : String(err) }));
      return false;
    } finally {
      setFetchingId(null);
    }
  };

  /** 逐个点击（拉完刷新目录）；「全部拉取」里传 silent 由批处理统一刷新 */
  const fetchOneClick = async (ch: ChannelItem) => {
    const ok = await fetchOne(ch);
    if (ok) {
      addToast(t('已拉取') + `：${ch.name}`);
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
    try {
      for (const ch of fetchChannels) {
        const done = await fetchOne(ch);
        if (done) ok += 1;
        else fail += 1;
      }
      if (ok > 0) await load();
      addToast(
        `${t('全部拉取完成')}：${ok} ${t('成功')}` + (fail > 0 ? ` · ${fail} ${t('失败')}` : ''),
        fail > 0 ? 'error' : undefined,
      );
    } finally {
      setFetchAllRunning(false);
    }
  };

  // ── 列设置（顺序 / 显隐 / 对齐）/ 列宽拖动 ──
  const visibleCols = useMemo<AnyCol[]>(
    () => colItems.filter((c) => c.visible).map((c) => c.key),
    [colItems],
  );
  /** 列 key → 当前对齐（th/td 的 ta-* 类由此来） */
  const colAlign = useMemo(() => {
    const map = {} as Record<AnyCol, Align>;
    for (const item of colItems) map[item.key] = item.align;
    return map;
  }, [colItems]);

  /** 提交列配置：改内存即重画，写盘只在明确动作时（开关/对齐/松手/重置） */
  const commitColItems = useCallback((next: ColItem[], persist: boolean) => {
    setColItems(next);
    if (persist) writeLS(LS_COLCFG, next);
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

  // 面板收起 / 组件卸载时把拖动会话收干净（对齐上游：卸载补写盘，防内存与 localStorage 漂移）
  const finishColDrag = useCallback(() => {
    if (!colDragCleanup.current) return;
    colDragCleanup.current();
    setColItems((prev) => {
      writeLS(LS_COLCFG, prev);
      return prev;
    });
  }, []);
  useEffect(() => finishColDrag, [finishColDrag]);
  useEffect(() => {
    if (colPanelOpen) return;
    finishColDrag();
  }, [colPanelOpen, finishColDrag]);

  /** 显隐开关：锁定列不可关（表不能没有勾选框/模型名/操作列） */
  const toggleCol = (key: AnyCol) => {
    if (LOCKED_COLS.includes(key)) return;
    commitColItems(
      colItems.map((c) => (c.key === key ? { ...c, visible: !c.visible } : c)),
      true,
    );
  };

  /** 对齐三档 */
  const setColAlignValue = (key: AnyCol, next: Align) => {
    const item = colItems.find((c) => c.key === key);
    if (!item || item.align === next) return;
    commitColItems(
      colItems.map((c) => (c.key === key ? { ...c, align: next } : c)),
      true,
    );
  };

  /**
   * 拖动排序：指针事件（对齐上游，不用 HTML5 拖放）。每跨过一行就重排一次并
   * 重画，但**不写盘**（每跨一行都落一次盘很吵）；松手（或浮层收起）才提交。
   */
  const startColOrderDrag = (key: AnyCol, e: React.PointerEvent<HTMLElement>) => {
    e.preventDefault();
    colDragCleanup.current?.();
    setDraggingCol(key);
    document.body.classList.add('mm-colset-dragging');
    const rows = colRowsRef.current;

    const teardown = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      document.body.classList.remove('mm-colset-dragging');
      colDragCleanup.current = null;
      setDraggingCol(null);
    };
    const onMove = (ev: PointerEvent) => {
      const over = document.elementFromPoint(ev.clientX, ev.clientY)?.closest('[data-col-key]');
      if (!over || !rows || !rows.contains(over)) return;
      const overKey = over.getAttribute('data-col-key') as AnyCol | null;
      if (!overKey || overKey === key) return;
      setColItems((prev) => {
        const from = prev.findIndex((c) => c.key === key);
        const to = prev.findIndex((c) => c.key === overKey);
        if (from < 0 || to < 0) return prev;
        const next = [...prev];
        const [item] = next.splice(from, 1);
        next.splice(to, 0, item);
        return next;
      });
    };
    const onUp = () => {
      teardown();
      setColItems((prev) => {
        writeLS(LS_COLCFG, prev);
        return prev;
      });
    };
    colDragCleanup.current = teardown;
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  };

  /** 恢复默认：顺序、显隐、对齐一起回到列声明（列宽一并还原） */
  const resetColConfig = () => {
    commitColItems(defaultColItems(), true);
    setColWidths({});
    writeLS(LS_COLW, {});
  };

  /** 列宽拖动：拖表头右缘把手改宽（写回 colgroup），双击把手还原 */
  const startColDrag = (key: AnyCol, e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const th = (e.target as HTMLElement).closest('th');
    const startW = th?.getBoundingClientRect().width ?? 100;
    const startX = e.clientX;
    let latest = startW;
    const move = (ev: MouseEvent) => {
      latest = Math.max(56, Math.round(startW + ev.clientX - startX));
      setColWidths((prev) => ({ ...prev, [key]: latest }));
    };
    const up = () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
      setColWidths((prev) => {
        writeLS(LS_COLW, prev);
        return prev;
      });
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  };

  const resetColWidth = (key: AnyCol) => {
    setColWidths((prev) => {
      const next = { ...prev };
      delete next[key];
      writeLS(LS_COLW, next);
      return next;
    });
  };

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
  }, [view, navKey, stateFilter, priceFilter, sourceFilter, channelFilter, query, sortMode]);

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

  const totalPages = Math.max(1, Math.ceil(filtered.length / pageSize));
  const currentPage = Math.min(page, totalPages);
  const pageRows = useMemo(
    () => filtered.slice((currentPage - 1) * pageSize, currentPage * pageSize),
    [filtered, currentPage, pageSize],
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
  }, [navKey, stateFilter, priceFilter, sourceFilter, channelFilter, query, sortMode]);

  // 渠道页「在模型管理中维护」跳转：预选该渠道
  useEffect(() => {
    const cid = searchParams.get('channel_id');
    if (cid) {
      setChannelFilter(cid);
      setSourceFilter('all');
    }
  }, [searchParams]);

  const toggleSelectAllPage = () => {
    const keys = (groupedMode ? filtered : pageRows).map(rowKey);
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

  const submitJump = () => {
    const n = Number(jumpDraft);
    if (Number.isInteger(n) && n >= 1 && n <= totalPages) setPage(n);
    setJumpDraft('');
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
      case 'enable':
        return (
          <td key={col} className={`mm-c-enable ta-${colAlign.enable}`}>
            <Switch
              checked={row.enabled}
              onChange={(next) => void handleToggle(row, next)}
              disabled={pendingRow === key}
              ariaLabel={`${row.id} ${t('启停')}`}
              className="mm-row-switch"
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
      case 'price':
        return (
          <td key={col} className={`mm-c-price ta-${colAlign.price}`}>
            {row.price ? (
              isFreePrice(row.price) ? (
                <span className="mm-price-free" title={t('价格已配置为 0，调用不计费')}>
                  {t('免费')}
                </span>
              ) : (
                <span
                  className="mm-price"
                  title={t('单位：美元。按量价为每 1k tokens 输入/输出价；按次价为每次调用价。')}
                >
                  {row.price.price_type === 'count'
                    ? `$${fmtPrice(row.price.input_price)}/${t('次')}`
                    : `$${fmtPrice(row.price.input_price)}/$${fmtPrice(row.price.output_price)}`}
                  {row.model_ratio !== 1 && <span className="mm-price-ratio">{row.model_ratio}x</span>}
                </span>
              )
            ) : (
              <span className="mm-price-empty">{t('未定价')}</span>
            )}
          </td>
        );
      case 'source':
        return (
          <td key={col} className={`mm-c-source ta-${colAlign.source}`}>
            <div className="mm-sources">
              {row.sources.slice(0, 3).map((s, i) => (
                <span
                  key={`${s.kind}-${s.channel_id}-${s.account_id ?? ''}-${i}`}
                  className={`mm-src-tag${s.kind === 'account' ? '' : ' provider'}`}
                  title={`${s.kind === 'account' ? t('账号') : t('渠道')}：${s.name}`}
                >
                  {s.name}
                </span>
              ))}
              {row.sources.length > 3 && (
                <span
                  className="mm-src-more"
                  title={row.sources.map((s) => s.name).join('\n')}
                >
                  +{row.sources.length - 3}
                </span>
              )}
              {row.sources.length === 0 && <span className="mm-src-none">—</span>}
            </div>
          </td>
        );
      case 'caps':
        return (
          <td key={col} className={`mm-c-caps ta-${colAlign.caps}`}>
            <button
              type="button"
              className="mm-caps-btn"
              title={t('查看 / 覆盖能力声明（继承内置推断，覆盖后以下游声明为准）')}
              onClick={() => openMetaForRow(row)}
            >
              {CAP_DOTS.map((c) => {
                const on = (row.capabilities ?? []).some((cap) => c.match.test(cap));
                const Icon = c.icon;
                return (
                  <span
                    key={c.key}
                    className={`mm-cap-dot${on ? ' on' : ''}`}
                    title={`${t(c.label)}：${on ? t('支持') : t('未声明')}`}
                  >
                    <Icon size={10} />
                  </span>
                );
              })}
              <span className="mm-cap-ctx">
                {row.context_length ? fmtCompact(row.context_length) : '—'}
              </span>
            </button>
          </td>
        );
      case 'alias':
        return (
          <td key={col} className={`mm-c-alias ta-${colAlign.alias}`}>
            <div className="mm-alias-cell">
              {(row.alias_entries?.length
                ? row.alias_entries
                : row.aliases.map((a) => ({ alias: a, target: row.id, enabled: true }))
              ).map((entry) => (
                <span
                  key={entry.alias}
                  className={`mm-alias${entry.enabled ? '' : ' is-off'}`}
                  title={
                    entry.enabled
                      ? `${entry.alias} → ${entry.target}`
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
                    {entry.alias}
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
                    ? t('该模型没有可用来源（启用账号或渠道），无法测试')
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
            </div>
          </td>
        );
      default:
        return null;
    }
  };

  const COL_HEAD: Record<AnyCol, { label: string; tip?: string }> = {
    check: { label: '' },
    enable: { label: t('启用') },
    model: { label: t('模型') },
    price: { label: t('定价'), tip: t('单位：美元（USD），按量价为每 1k tokens') },
    source: { label: t('来源') },
    caps: { label: t('能力'), tip: t('工具 / 图像 / 音频 / 思考 + 上下文长度；点击覆盖') },
    alias: { label: t('映射'), tip: t('对外名 → 本模型；开关关掉后请求原样透传') },
    act: { label: t('操作') },
  };

  const renderHeadCell = (col: AnyCol): JSX.Element => {
    const def = COL_HEAD[col];
    return (
      <th
        key={col}
        className={`mm-h-${col} ta-${colAlign[col] ?? COL_DEFAULT_ALIGN[col]}`}
        title={def.tip}
      >
        {col === 'check' ? (
          <input
            type="checkbox"
            aria-label={t('全选')}
            checked={
              (groupedMode ? filtered : pageRows).length > 0 &&
              (groupedMode ? filtered : pageRows).every((r) => selected.has(rowKey(r)))
            }
            onChange={toggleSelectAllPage}
          />
        ) : (
          def.label
        )}
        {col !== 'act' && (
          <span
            className="mm-col-grip"
            title={t('拖动调整列宽 · 双击还原')}
            onMouseDown={(e) => startColDrag(col, e)}
            onDoubleClick={() => resetColWidth(col)}
          />
        )}
      </th>
    );
  };

  const renderRow = (row: ManagedModel): JSX.Element => {
    const key = rowKey(row);
    return (
      <tr key={key} className={`${row.enabled ? '' : 'mm-off'}${selected.has(key) ? ' mm-sel' : ''}`}>
        {visibleCols.map((col) => renderCell(col, row))}
      </tr>
    );
  };

  // ── 渲染 ──
  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('模型管理')}</h1>
          <p>{t('渠道与账号池的模型统一在此管理：测试、启停、定价、映射。')}</p>
        </div>
        <button className="btn btn-outline btn-sm" onClick={() => void load()} disabled={loading} style={{ gap: 6 }}>
          <RefreshCw size={13} />
          {loading ? t('加载中...') : t('刷新')}
        </button>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Tabs
        items={[
          { key: 'catalog', label: t('模型目录') },
          { key: 'meta', label: t('元信息覆盖') },
        ]}
        active={tab}
        onChange={(k) => setTab(k as MainTab)}
        ariaLabel={t('模型管理')}
      />

      {tab === 'meta' ? (
        <div className="mm-panel">
          <div className="mm-titlebar">
            <div className="mm-title">
              <span className="mm-title-name">{t('元信息覆盖')}</span>
              <span className="mm-meta">{t('覆盖内置推断的归属方 / 上下文长度 / 能力声明')}</span>
            </div>
            <div className="mm-head-actions">
              <button className="btn btn-outline btn-sm" onClick={openMetaCreate} style={{ gap: 6 }}>
                <Plus size={13} />
                {t('新增覆盖')}
              </button>
            </div>
          </div>
          {Object.keys(overrides).length === 0 ? (
            <EmptyState
              icon="🏷️"
              message={t('暂无覆盖项。内置推断已覆盖常见模型，需要自定义归属/上下文/能力时在此添加')}
            />
          ) : (
            <div className="mm-table-wrap">
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
                          <button type="button" className="mm-act-btn" onClick={() => openMetaEdit(model)} title={t('编辑')}>
                            <Pencil size={11} />
                          </button>
                          <button type="button" className="mm-act-btn danger icon-only" onClick={() => removeMeta(model)} title={t('删除')}>
                            <Trash2 size={11} />
                          </button>
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
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

            <div className="mm-rail-title">{t('渠道供应商')}</div>
            {channelRails.length === 0 ? (
              <div className="mm-rail-empty">{t('暂无启用的渠道')}</div>
            ) : (
              channelRails.map((c) => (
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
            {builtinRails.length === 0 ? (
              <div className="mm-rail-empty">{t('暂无账号池供应商')}</div>
            ) : (
              builtinRails.map((g) => (
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
            <div className="mm-rail-empty">{t('暂无自定义提供商')}</div>
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
                <button
                  type="button"
                  className="btn btn-primary btn-sm"
                  onClick={() => void openFetchModal()}
                  style={{ gap: 6 }}
                  title={t('从上游拉取模型列表')}
                >
                  <Download size={13} />
                  {t('获取模型')}
                </button>
                <div className="mm-colset" ref={colPanelRef}>
                  <button
                    type="button"
                    className={`btn btn-outline btn-sm${colPanelOpen ? ' mm-colset-open' : ''}`}
                    onClick={() => setColPanelOpen((v) => !v)}
                    style={{ gap: 6 }}
                    title={t('列显示设置')}
                  >
                    <Settings2 size={13} />
                    {t('列设置')}
                  </button>
                  {colPanelOpen && (
                    <div className="mm-colset-panel" role="group" aria-label={t('列设置')}>
                      {/* 表头与数据行同一套网格：手柄 / 列名 / 显示 / 对齐（对齐上游面板骨架） */}
                      <div className="mm-colset-grid mm-colset-head">
                        <span />
                        <span>{t('列')}</span>
                        <span>{t('显示')}</span>
                        <span>{t('对齐')}</span>
                      </div>
                      <div className="mm-colset-rows" ref={colRowsRef}>
                        {colItems.map((item) => {
                          const locked = LOCKED_COLS.includes(item.key);
                          const name = t(COL_PANEL_LABEL[item.key]);
                          return (
                            <div
                              key={item.key}
                              data-col-key={item.key}
                              className={`mm-colset-grid mm-colset-row${draggingCol === item.key ? ' dragging' : ''}`}
                            >
                              <span
                                className="mm-colset-grip"
                                title={t('按住拖动调整列顺序')}
                                onPointerDown={(e) => startColOrderDrag(item.key, e)}
                              >
                                <GripVertical size={13} />
                              </span>
                              <span className="mm-colset-name" title={name}>
                                {name}
                              </span>
                              <Switch
                                className="mm-colset-switch"
                                checked={item.visible}
                                disabled={locked}
                                onChange={() => toggleCol(item.key)}
                                ariaLabel={`${t('显示列')} ${name}`}
                                title={item.visible ? t('这一列正在显示') : t('这一列已隐藏')}
                              />
                              <div
                                className="mm-seg mm-seg-mini"
                                role="group"
                                aria-label={t('「{name}」列的对齐', { name })}
                              >
                                {ALIGN_OPTIONS.map((a) => (
                                  <button
                                    key={a.value}
                                    type="button"
                                    className={`mm-seg-btn${item.align === a.value ? ' active' : ''}`}
                                    onClick={() => setColAlignValue(item.key, a.value)}
                                  >
                                    {t(a.label)}
                                  </button>
                                ))}
                              </div>
                            </div>
                          );
                        })}
                      </div>
                      <div className="mm-colset-foot">
                        <span className="mm-colset-tip">{t('拖动手柄调整顺序')}</span>
                        <button type="button" className="mm-colset-reset" onClick={resetColConfig}>
                          {t('恢复默认（含列宽）')}
                        </button>
                      </div>
                    </div>
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
                  placeholder={t('搜索模型 ID / 别名 / 来源…')}
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
                <option value="channel">{t('仅渠道')}</option>
                <option value="account">{t('仅账号池')}</option>
              </select>
              <select
                className="form-input mm-mini-select"
                value={channelFilter}
                onChange={(e) => setChannelFilter(e.target.value)}
                aria-label={t('渠道筛选')}
              >
                <option value="all">{t('全部渠道')}</option>
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

            {/* 批量条（勾选后出现，对齐 Agent2API batch-chip） */}
            {selected.size > 0 && (
              <div className="mm-batch-chip">
                <span>
                  {t('已选')} <strong>{selected.size}</strong> {t('个')}
                </span>
                <button type="button" className="btn btn-outline btn-sm" onClick={() => { setBatchReport(''); setBatchOpen(true); }}>
                  {t('批量操作')}
                </button>
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
                      : t('暂无模型。请先在渠道管理或账号池里配置上游。')
                }
              />
            ) : (
              <>
                <div className="mm-table-wrap">
                  <table className="mm-table">
                    <colgroup>
                      {visibleCols.map((col) => (
                        <col key={col} style={{ width: colWidths[col] ? `${colWidths[col]}px` : COL_DEFAULT_W[col] }} />
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
                                {shown.map(renderRow)}
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
                        : pageRows.map(renderRow)}
                    </tbody>
                  </table>
                </div>

                {/* 分页页脚（对齐 Agent2API TableFooter：总数 / 每页档位 / 跳页） */}
                {!groupedMode && (
                  <div className="mm-table-foot">
                    <span className="mm-foot-info">
                      {t('共')} {filtered.length} {t('条')}
                      {filtered.length > 0 &&
                        ` · ${t('当前第')} ${(currentPage - 1) * pageSize + 1}–${Math.min(
                          currentPage * pageSize,
                          filtered.length,
                        )} ${t('条')}`}
                    </span>
                    <div className="mm-foot-right">
                      <select
                        className="form-input mm-pagesize"
                        value={pageSize}
                        onChange={(e) => {
                          const n = Number(e.target.value);
                          setPageSize(n);
                          localStorage.setItem(LS_PAGE_SIZE, String(n));
                          setPage(1);
                        }}
                        aria-label={t('每页条数')}
                      >
                        {PAGE_SIZES.map((n) => (
                          <option key={n} value={n}>
                            {n} / {t('页')}
                          </option>
                        ))}
                      </select>
                      <Pagination page={currentPage} totalPages={totalPages} onChange={setPage} />
                      <span className="mm-jump">
                        {t('跳至')}
                        <input
                          className="form-input"
                          value={jumpDraft}
                          onChange={(e) => setJumpDraft(e.target.value.replace(/\D/g, ''))}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter') submitJump();
                            if (e.key === 'Escape') setJumpDraft('');
                          }}
                          onBlur={submitJump}
                          placeholder={String(currentPage)}
                          aria-label={t('跳至页码')}
                        />
                        {t('页')}
                      </span>
                    </div>
                  </div>
                )}
              </>
            )}
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
                  </>
                ) : (
                  <p className="mm-dialog-note" style={{ marginTop: 8 }}>
                    {t('能力标签按模型名内置推断（chat / embedding / image / audio…），下游按推断值展示。')}
                  </p>
                )}
              </div>
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setMetaModal(false)} disabled={savingMeta}>
                {t('取消')}
              </button>
              <button className="btn btn-primary" onClick={() => void saveMeta()} disabled={savingMeta}>
                {savingMeta ? t('保存中...') : t('保存')}
              </button>
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
                </select>
              </div>
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
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setPriceTarget(null)} disabled={savingPrice}>
                {t('取消')}
              </button>
              <button className="btn btn-primary" onClick={() => void savePrice()} disabled={savingPrice}>
                {savingPrice ? t('保存中...') : t('保存')}
              </button>
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
                  options={modelIdOptions}
                  placeholder={t('搜索或输入目标模型名')}
                  onChange={(v) => setAliasForm({ ...aliasForm, target: v })}
                  disabled={savingAlias}
                />
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
              <button className="btn btn-outline" onClick={() => setAliasTarget(null)} disabled={savingAlias}>
                {t('取消')}
              </button>
              <button className="btn btn-primary" onClick={() => void saveAlias()} disabled={savingAlias}>
                {savingAlias ? t('保存中...') : t('保存')}
              </button>
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
                {t('从上游 /models 接口拉取模型清单（写入渠道的「上游发现」快照）。账号型渠道请在账号池里维护模型白名单。')}
              </p>
              {fetchChannels.length === 0 ? (
                <EmptyState icon="📡" message={t('暂无启用的可拉取渠道')} />
              ) : (
                <>
                  <div className="mm-fetch-toolbar">
                    <span className="mm-fetch-summary">
                      {t('共')} {fetchChannels.length} {t('个渠道')}
                      {Object.keys(fetchDone).length > 0 &&
                        ` · ${t('成功')} ${Object.keys(fetchDone).length}`}
                      {Object.keys(fetchFailed).length > 0 &&
                        ` · ${t('失败')} ${Object.keys(fetchFailed).length}`}
                    </span>
                    <button
                      type="button"
                      className="btn btn-outline btn-sm"
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
                    </button>
                  </div>
                  <div className="mm-fetch-list">
                    {fetchChannels.map((ch) => {
                      const cid = String(ch.id);
                      const busy = fetchingId === cid || (fetchAllRunning && fetchingId === null && !fetchDone[cid] && !fetchFailed[cid]);
                      return (
                        <div key={cid} className="mm-fetch-row">
                          <div className="mm-fetch-info">
                            <strong>{ch.name}</strong>
                            <span className="mm-model-sub">
                              {ch.channel_type} · {t('已发现')} {(ch.discovered_models ?? []).length}
                            </span>
                            {fetchFailed[cid] && (
                              <span className="mm-fetch-err" title={fetchFailed[cid]}>
                                {fetchFailed[cid].length > 60 ? `${fetchFailed[cid].slice(0, 60)}…` : fetchFailed[cid]}
                              </span>
                            )}
                          </div>
                          {fetchDone[cid] ? (
                            <span className="mm-badge-ok">{t('已拉取')}</span>
                          ) : (
                            <button
                              type="button"
                              className="btn btn-outline btn-sm"
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
                            </button>
                          )}
                        </div>
                      );
                    })}
                  </div>
                </>
              )}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setFetchModal(false)}>
                {t('关闭')}
              </button>
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
                    ['remove-price', '删除定价'],
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
              {batchReport && <div className="error-message">{batchReport}</div>}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setBatchOpen(false)} disabled={batchRunning}>
                {t('取消')}
              </button>
              <button
                className={`btn ${batchAction === 'remove-price' ? 'btn-danger' : 'btn-primary'}`}
                onClick={() => void runBatch()}
                disabled={batchRunning || selectedRows.length === 0}
              >
                {batchRunning ? t('执行中…') : `${t('执行')}${t(batchAction === 'enable' ? '启用' : batchAction === 'disable' ? '禁用' : '删除定价')}`}
              </button>
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

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}

/** 分组片段（key 需要挂在显式组件上，React 19 前 <> 不支持 key） */
function FragmentGroup({ children }: { children: React.ReactNode }): JSX.Element {
  return <>{children}</>;
}
