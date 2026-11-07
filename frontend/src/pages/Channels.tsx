import { useState, useEffect, useMemo, useRef, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import {
  Eye, EyeOff, ArrowLeftRight, MoreHorizontal, Search,
  Pencil, Gauge, Power, PowerOff, Loader2, MessageSquare, RotateCcw, Trash2,
  DollarSign, Cloud, Sparkles, Gem, BrainCircuit, Settings2, Plus,
} from 'lucide-react';
import { api } from '../api';
import { useListKeyboard } from '../hooks/useListKeyboard';
import type { ChannelItem as ApiChannelItem } from '../types';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import ChatDebugger from '../components/ChatDebugger';
import ModelMappingEditor from '../components/ModelMappingEditor';
import CostPricingEditor from '../components/CostPricingEditor';
import {
  BALANCE_SCHEME_LABELS,
  PRESET_PROVIDERS,
  presetIconOf,
  type ChannelQuirks,
  type PresetProvider,
} from '../lib/channelPresets';
import { getFilter, setFilter } from '../lib/filterMemory';
import {
  readTableColumns,
  resetTableColumns,
  writeTableColumns,
  type TableColumnDef,
  type TableColumnItem,
  type TableColumnState,
} from '../lib/tableColumns';
import TableColumnSettings from '../components/ui/TableColumnSettings';
import { useColumnResize } from '../components/ui/useColumnResize';
import { usePageSize } from '../components/ui/TableShell';
import { Button, Card, Input, EmptyState, Select, SkeletonTable, Pagination } from '../components/ui';
import './Channels.css';

interface ChannelItem {
  id: string | number;
  /** 展示用短编号（后端按创建顺序注入 1..N；旧数据可能缺失，回退取 id 前 8 位） */
  seq?: number;
  name: string;
  channel_type: string;
  base_url?: string;
  api_key?: string;
  priority: number;
  weight: number;
  status: string;
  models?: string[];
  discovered_models?: string[];
  account_id?: string;
  model_mapping?: Record<string, string>;
  cost_pricing?: Record<string, { input_price?: number; output_price?: number; price_type?: string }>;
  last_error?: string | null;
  last_used_at?: number | null;
  created_at?: number;
  updated_at?: number;
  // new-api 风格字段
  balance?: number;
  credit?: number;
  response_time?: number;
  test_time?: number;
  remark?: string;
  tags?: string[];
  /** 上游特判（预置供应商写入） */
  quirks?: ChannelQuirks | null;
  /** 余额查询方案（空 = OpenAI 计费端点） */
  balance_scheme?: string;
  /** 余额展示单位（USD / CNY / % / quota …；随余额查询响应回填） */
  balance_unit?: string;
}

/**
 * 余额查询专用凭证在 `quirks.headers` 里的约定头名（与后端
 * `channel::balance::credential_field` 一致）。它们不随转发发出，只给余额
 * 查询用；表单里用专用输入框承载，避免用户误以为要填到「额外请求头」。
 */
const BALANCE_CREDENTIAL_HEADERS = [
  'x-balance-access-token',
  'x-balance-user-id',
  'x-balance-token',
  'x-balance-username',
  'x-balance-password',
];

interface ChannelFormState {
  name: string;
  channel_type: string;
  base_url: string;
  api_key: string;
  /** 无密钥模式（参照 cf-ai-gw「永久」复选框思路）：勾选后 API Key 输入框置灰，提交 api_key=null */
  no_key: boolean;
  priority: number | string;
  weight: number | string;
  status: string;
  models: string;
  account_id: string;
  model_mapping: Record<string, string>;
  cost_pricing: Record<string, { input_price?: number; output_price?: number; price_type?: string }>;
  /** 上游特判（预置供应商卡片写入；编辑时回填） */
  quirks: ChannelQuirksForm;
  /** 余额查询方案（空 = OpenAI 计费端点） */
  balance_scheme: string;
  /** 余额查询额外凭证（按 header 名存，提交时并入 quirks.headers） */
  balance_fields: Record<string, string>;
}

/** quirks 的表单形态（与后端 snake_case 字段对齐） */
interface ChannelQuirksForm {
  url_suffix: string;
  /** 每行 `Key: Value` */
  headers: string;
  anthropic_tool_type: string;
  client_emulation: string;
}

// 渠道类型选项 — 与后端 ChannelType 枚举对齐（snake_case）。
// 不含 `account`：账号型渠道由「账号池」添加账号时自动创建，是内部容器，
// 不提供手工新建入口（列表里也不展示，见 Channels 的 allChannels 过滤）。
const CHANNEL_TYPES = [
  { value: 'cloudflare', labelKey: 'Cloudflare Workers AI', isRaw: true },
  { value: 'openai_compatible', labelKey: 'OpenAI 兼容 (DeepSeek/OpenRouter/...)' },
  { value: 'anthropic', labelKey: 'Anthropic 兼容' },
  { value: 'gemini', labelKey: 'Gemini (Google AI)', isRaw: true },
  { value: 'zai', labelKey: 'Zai (智谱AI)', isRaw: true },
];

// 各渠道类型的默认 Base URL（创建时自动填充，减少用户手动输入）
const DEFAULT_BASE_URL = {
  gemini: 'https://generativelanguage.googleapis.com/v1beta',
  zai: 'https://api.z.ai/api/v2',
};

// 各渠道类型的鉴权方式提示
const AUTH_HINT = {
  gemini: '鉴权方式：x-goog-api-key（在 API Key 字段填入 Google AI Studio 密钥）',
  zai: '鉴权方式：Bearer token（在 API Key 字段填入智谱 API Key）',
};

// 类型徽章图标（new-api ProviderBadge 同款思路：图标 + 着色标签）
const TYPE_ICONS: Record<string, typeof Cloud> = {
  cloudflare: Cloud,
  openai_compatible: Sparkles,
  anthropic: BrainCircuit,
  gemini: Gem,
  zai: Sparkles,
};

// 模型名 → 稳定颜色（new-api autoColor 同思路：字符串 hash → HSL 色相）
function modelBadgeColor(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) {
    hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  }
  const hue = hash % 360;
  return `hsl(${hue} 65% 45%)`;
}

/** 余额展示：按单位渲染（USD/CNY 加符号，% 加百分号，quota 原样） */
function formatBalance(value: number, unit?: string): string {
  const u = (unit || '').toUpperCase();
  if (u === '%') return `${value.toFixed(1)}%`;
  if (u === 'USD' || u === '') return `$${value.toFixed(2)}`;
  if (u === 'CNY') return `¥${value.toFixed(2)}`;
  return `${value.toFixed(2)} ${unit}`;
}

// ── 列表交互（对齐 ModelManage：分段筛选/排序、列显隐、客户端分页，全部落盘）──

/** localStorage 键 */
const LS_FILTERS = 'aigx-ch-filters';
const LS_COLS = 'aigx-ch-cols'; // 旧显隐表（公共层 v1 迁移源）
const LS_COLCFG = 'aigx-ch-colcfg'; // 旧数组配置（顺序+对齐；v1 形态喂公共层）
const LS_COLW = 'aigx-ch-colw'; // 旧列宽覆盖（init 时一次性并进 v2 后删键）
const LS_PAGE_SIZE = 'aigx-ch-pagesize'; // 旧每页条数（usePageSize legacy 搬迁）
/** 公共列设置键（tableColumns v2） */
const LS_TCOLS = 'aigx-tbl-cols.channels';
/** 公共每页条数键（usePageSize；旧 aigx-ch-pagesize 搬迁源） */
const LS_PSZ = 'aigx-tbl-pagesize.channels';

const PAGE_SIZES = [20, 50, 100];

function readLs<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    return { ...fallback, ...(JSON.parse(raw) as object) } as T;
  } catch {
    return fallback;
  }
}
// writeLs 已随列设置迁移公共层（writeTableColumns）；readLs 仍被旧筛选读取用

type ChStateFilter = 'all' | 'on' | 'off';
type ChTypeFilter = 'all' | 'cloudflare' | 'openai_compatible' | 'anthropic' | 'gemini' | 'zai';
/** 排序档位（方向即档位语义，同 ModelManage 倍率升/降分开的做法取常用向） */
type ChSortMode = 'default' | 'priority' | 'response' | 'balance' | 'name';
/** 批量操作动作（对齐 ModelManage BatchAction） */
type BatchAction = 'enable' | 'disable' | 'delete';

const CH_STATE_OPTIONS: Array<[ChStateFilter, string]> = [
  ['all', '全部'],
  ['on', '已启用'],
  ['off', '已禁用'],
];
const CH_TYPE_OPTIONS: Array<[ChTypeFilter, string]> = [
  ['all', '全部类型'],
  ['openai_compatible', 'OpenAI 兼容'],
  ['anthropic', 'Anthropic'],
  ['gemini', 'Gemini'],
  ['zai', 'Z.ai'],
  ['cloudflare', 'CF'],
];
const CH_SORT_OPTIONS: Array<[ChSortMode, string]> = [
  ['default', '默认'],
  ['priority', '优先级'],
  ['response', '响应快'],
  ['balance', '余额多'],
  ['name', '名称'],
];

/** 可显隐列（select 勾选列与 actions 操作列锁定，不进面板、不参与拖序） */
type ChColKey = 'id' | 'name' | 'type' | 'status' | 'models' | 'priority' | 'weight' | 'balance' | 'response' | 'testtime';
/** 列声明顺序（面板顺序、colgroup 与表头同源） */
const CH_COL_ORDER: ChColKey[] = ['id', 'name', 'type', 'status', 'models', 'priority', 'weight', 'balance', 'response', 'testtime'];
/** 列设置面板显示的列名（ID 固定英文不译） */
const CH_COL_PANEL_LABEL: Record<ChColKey, string> = {
  id: 'ID',
  name: '名称',
  type: '类型',
  status: '状态',
  models: '模型',
  priority: '优先级',
  weight: '权重',
  balance: '余额',
  response: '响应',
  testtime: '测试时间',
};
/** 列默认宽（colgroup 接管宽度后 CSS 不再声明；与原有 CSS 宽度一致） */
const CH_COL_DEFAULT_W: Record<string, number> = {
  select: 32,
  id: 48,
  name: 170,
  type: 140,
  status: 72,
  models: 200,
  priority: 68,
  weight: 62,
  balance: 150,
  response: 84,
  testtime: 88,
  actions: 132,
};

/** 列对齐三档（对齐 ModelManage ALIGN_OPTIONS） */
type ChAlign = 'left' | 'center' | 'right';

/** 各列默认对齐 = 现有 CSS 的观感（渠道表当前全左对齐），视觉不突变。
 *  声明必须在 CH_COL_DEFS 之前：DEFS 的模块级 map 立即求值，引用后置 const
 *  会 TDZ ReferenceError 打崩 /channels 整页（verifier 2026-11-08 打回项）。 */
const CH_COL_DEFAULT_ALIGN: Record<ChColKey, ChAlign> = {
  id: 'left',
  name: 'left',
  type: 'left',
  status: 'left',
  models: 'left',
  priority: 'left',
  weight: 'left',
  balance: 'left',
  response: 'left',
  testtime: 'left',
};

/** 公共层列声明（键集合与默认值的唯一事实源；CH 数据列全部可显隐可拖序） */
const CH_COL_DEFS: TableColumnDef[] = CH_COL_ORDER.map((key) => ({
  key,
  label: CH_COL_PANEL_LABEL[key],
  align: CH_COL_DEFAULT_ALIGN[key],
  width: CH_COL_DEFAULT_W[key],
}));

/** 读原始 JSON（数组形态不能用 readLs：它会把数组摊成对象） */
function readLsRaw<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return null;
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
}

interface ChannelsProps {
  /** 作为「渠道与账号池」双 Tab 的子页内嵌时隐藏自身标题区 */
  embedded?: boolean;
}

export default function Channels({ embedded = false }: ChannelsProps = {}): JSX.Element {
  const [channels, setChannels] = useState<ChannelItem[]>([]);
  const [loading, setLoading] = useState(true);
  const firstLoadRef = useRef(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [showModal, setShowModal] = useState(false);
  const [editChannel, setEditChannel] = useState<ChannelItem | null>(null);
  const [form, setForm] = useState<ChannelFormState>(defaultForm());
  const [saving, setSaving] = useState(false);
  const [testingId, setTestingId] = useState<string | number | null>(null);
  // 全部测试进行中标记（批量测试期间禁用菜单入口，结束后单条汇总提示）
  const [testingAll, setTestingAll] = useState(false);
  // API Key 输入明文切换（防输错无法核对）
  const [showApiKey, setShowApiKey] = useState(false);
  /** 当前选中的预置供应商 key（仅新增时；编辑回填 null） */
  const [activePreset, setActivePreset] = useState<string | null>(null);
  /** 向导步骤：pick = 选供应商（新增时的第 1 步）；form = 填配置 */
  const [modalStep, setModalStep] = useState<'pick' | 'form'>('form');
  /** 预置供应商搜索（第 1 步卡片网格过滤） */
  const [presetQuery, setPresetQuery] = useState('');

  // ── 全局模型目录候选（映射编辑器源/目标的可搜索下拉）──
  const [globalModels, setGlobalModels] = useState<string[]>([]);
  useEffect(() => {
    api.listManageModels()
      .then((res) => {
        const models = res?.data?.models ?? [];
        const seen = new Map<string, string>();
        const push = (n?: string) => {
          const v = n?.trim();
          if (!v) return;
          const k = v.toLowerCase();
          if (!seen.has(k)) seen.set(k, v);
        };
        models.forEach((m) => {
          push(m.id);
          (m.aliases ?? []).forEach(push);
        });
        setGlobalModels([...seen.values()].sort((a, b) => a.localeCompare(b)));
      })
      .catch(() => { /* 候选加载失败静默降级为仅渠道清单 */ });
  }, []);

  /** 映射编辑器候选：渠道已配置模型 ∪ 上游发现模型 ∪ 全局目录（大小写不敏感去重） */
  const mappingModelOptions = useMemo(() => {
    const seen = new Map<string, string>();
    const push = (n?: string) => {
      const v = n?.trim();
      if (!v) return;
      const k = v.toLowerCase();
      if (!seen.has(k)) seen.set(k, v);
    };
    form.models.split(',').forEach(push);
    (editChannel?.discovered_models ?? []).forEach(push);
    globalModels.forEach(push);
    return [...seen.values()].sort((a, b) => a.localeCompare(b));
  }, [form.models, editChannel, globalModels]);

  // 无密钥复选框联动（同 cf-ai-gw onKeyForeverChange）：勾选 → 输入框置灰清空
  const onNoKeyChange = (): void => {
    setForm((f) => {
      const noKey = !f.no_key;
      return { ...f, no_key: noKey, api_key: noKey ? '' : f.api_key };
    });
  };

  // ── 确认弹窗状态 ──
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // ── 对话调试器状态 ──
  const [showChat, setShowChat] = useState(false);
  const [chatChannel, setChatChannel] = useState<ChannelItem | null>(null);

  // ── 批量操作 / 搜索 / "更多"菜单 ──
  const [selected, setSelected] = useState<Set<string | number>>(new Set());
  // 批量弹窗（对齐 ModelManage：动作单选 + 选中清单 + 失败保留现场）
  const [batchOpen, setBatchOpen] = useState(false);
  const [batchAction, setBatchAction] = useState<BatchAction>('enable');
  const [batchRunning, setBatchRunning] = useState(false);
  const [batchReport, setBatchReport] = useState('');
  // 批量删除需要删除密码（与单条删除同口径，ConfirmDialog requirePassword 的弹窗内形态）
  const [batchPassword, setBatchPassword] = useState('');
  const [moreOpen, setMoreOpen] = useState(false);
  const moreRef = useRef<HTMLDivElement>(null);

  // ── 内联编辑状态 ──
  const [editingField, setEditingField] = useState<{ id: string | number; field: 'priority' | 'weight' } | null>(null);
  const [editValue, setEditValue] = useState<string>('');
  const [updatingBalance, setUpdatingBalance] = useState<Set<string | number>>(new Set());

  // ── 行操作下拉菜单（MoreHorizontal）──
  // 菜单用 fixed 定位挂在 body 层级：Card/table-wrap 的 overflow 会裁掉
  // 行内 absolute 弹层（尤其最后一行向下弹出时），fixed 可逃出所有裁剪容器
  const [rowMenuId, setRowMenuId] = useState<string | number | null>(null);
  const [rowMenuPos, setRowMenuPos] = useState<{ top: number; left: number }>({ top: 0, left: 0 });
  useEffect(() => {
    if (rowMenuId === null) return;
    const handler = (): void => setRowMenuId(null);
    document.addEventListener('mousedown', handler);
    // 表格横向滚动时关闭菜单（fixed 坐标不随滚动联动）
    const scrollHandler = (): void => setRowMenuId(null);
    window.addEventListener('scroll', scrollHandler, true);
    return () => {
      document.removeEventListener('mousedown', handler);
      window.removeEventListener('scroll', scrollHandler, true);
    };
  }, [rowMenuId]);

  /** 打开行菜单：按触发按钮的屏幕坐标计算 fixed 弹出位置（右对齐、下弹出，底部溢出改上弹） */
  const openRowMenu = (e: React.MouseEvent<HTMLButtonElement>, id: string | number): void => {
    e.stopPropagation();
    if (rowMenuId === id) { setRowMenuId(null); return; }
    const rect = e.currentTarget.getBoundingClientRect();
    const menuH = 240; // 菜单预估高度（5 项 + 分隔线）
    const below = window.innerHeight - rect.bottom;
    const top = below < menuH && rect.top > menuH
      ? rect.top - menuH + rect.height // 底部放不下 → 向上弹出
      : rect.bottom + 4;
    setRowMenuPos({ top, left: rect.right });
    setRowMenuId(id);
  };

  // ── 分页（客户端分页：全量拉取后本地过滤/排序/切片，交互对齐模型管理页）──
  // 每页条数走公共 usePageSize（档位校验 + 旧 aigx-ch-pagesize 一次性搬迁）
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = usePageSize(LS_PSZ, PAGE_SIZES, 20, [LS_PAGE_SIZE]);
  const [jumpDraft, setJumpDraft] = useState('');

  // ── 筛选 / 排序 / 列显隐（对齐 ModelManage 的交互面，全部落 localStorage）──
  const chFilters = getFilter<Record<string, string>>(LS_FILTERS, { state: 'all', type: 'all', sort: 'default', search: '' });
  const [search, setSearch] = useState<string>(chFilters.search || '');
  const [stateFilter, setStateFilter] = useState<ChStateFilter>(chFilters.state as ChStateFilter);
  const [typeFilter, setTypeFilter] = useState<ChTypeFilter>(chFilters.type as ChTypeFilter);
  const [sortMode, setSortMode] = useState<ChSortMode>(chFilters.sort as ChSortMode);
  // 列配置已迁公共层（tableColumns v2：顺序/显隐/对齐/列宽统一记忆）。
  // 旧键一次性搬迁：ch-colcfg（数组：顺序+对齐）与 ch-cols（显隐表）合成 v1 形态
  // 喂公共层；ch-colw 列宽覆盖并进 v2；搬完删旧键。
  const [colState, setColState] = useState<TableColumnState>(() => {
    let migrated = readTableColumns(LS_TCOLS, CH_COL_DEFS);
    if (localStorage.getItem(LS_TCOLS) === null) {
      const legacyCfg = readLsRaw<Array<{ key?: unknown; visible?: unknown; align?: unknown }>>(LS_COLCFG);
      const legacyCols = readLsRaw<Partial<Record<ChColKey, boolean>>>(LS_COLS);
      if (legacyCfg || legacyCols) {
        const byKey = new Map<string, { visible?: boolean; align?: unknown }>();
        if (Array.isArray(legacyCfg)) {
          for (const e of legacyCfg) {
            if (e && typeof e === 'object' && typeof e.key === 'string') {
              byKey.set(e.key, { visible: e.visible !== false, align: e.align });
            }
          }
        }
        if (legacyCols && typeof legacyCols === 'object') {
          for (const [k, v] of Object.entries(legacyCols)) {
            if (!byKey.has(k)) byKey.set(k, { visible: v !== false });
          }
        }
        const v1 = {
          columns: CH_COL_ORDER.map((key) => {
            const found = byKey.get(key);
            const def = CH_COL_DEFS.find((d) => d.key === key);
            return {
              key,
              visible: found ? found.visible !== false : true,
              align: (found && typeof found.align === 'string' ? found.align : def?.align ?? 'left') as ChAlign,
            };
          }),
        };
        try {
          migrated = readTableColumns(LS_TCOLS, CH_COL_DEFS, () => JSON.stringify(v1));
        } catch { /* 迁移失败退默认，不炸表 */ }
      }
    }
    const legacyW = readLs<Record<string, number>>(LS_COLW, {});
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
      localStorage.removeItem(LS_COLS);
      localStorage.removeItem(LS_COLW);
    } catch { /* 静默 */ }
    return merged;
  });
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);

  function defaultForm(): ChannelFormState {
    return {
      name: '',
      channel_type: 'openai_compatible',
      base_url: '',
      api_key: '',
      no_key: false,
      priority: 0,
      weight: 1,
      status: 'enabled',
      models: '',
      account_id: '',
      model_mapping: {},
      cost_pricing: {},
      quirks: { url_suffix: '', headers: '', anthropic_tool_type: '', client_emulation: '' },
      balance_scheme: '',
      balance_fields: {},
    };
  }

  /** quirks 对象 → 表单形态（headers 每行 `Key: Value`） */
  function quirksToForm(quirks?: ChannelQuirks | null): ChannelQuirksForm {
    const headers = { ...(quirks?.headers || {}) };
    // 余额凭证走专用输入框，从文本里剔除（提交时再合并回去）
    for (const header of BALANCE_CREDENTIAL_HEADERS) delete headers[header];
    return {
      url_suffix: quirks?.url_suffix || '',
      headers: Object.entries(headers)
        .map(([k, v]) => `${k}: ${v}`)
        .join('\n'),
      anthropic_tool_type: quirks?.anthropic_tool_type || '',
      client_emulation: quirks?.client_emulation || '',
    };
  }

  /** 从渠道 quirks.headers 里抽出余额查询专用凭证（按约定 header 名） */
  function extractBalanceFields(quirks?: ChannelQuirks | null): Record<string, string> {
    const out: Record<string, string> = {};
    const headers = quirks?.headers || {};
    for (const header of BALANCE_CREDENTIAL_HEADERS) {
      if (headers[header]) out[header] = headers[header];
    }
    return out;
  }

  /** 表单形态 → quirks 对象（空则返回 undefined，表示无特判） */
  function formToQuirks(form: ChannelQuirksForm, balanceFields: Record<string, string>): ChannelQuirks | undefined {
    const headers: Record<string, string> = {};
    for (const line of form.headers.split('\n')) {
      const idx = line.indexOf(':');
      if (idx <= 0) continue;
      const name = line.slice(0, idx).trim();
      const value = line.slice(idx + 1).trim();
      if (name && value) headers[name] = value;
    }
    for (const [header, value] of Object.entries(balanceFields)) {
      const trimmed = value.trim();
      if (header && trimmed) headers[header] = trimmed;
    }
    const quirks: ChannelQuirks = {};
    if (form.url_suffix.trim()) quirks.url_suffix = form.url_suffix.trim();
    if (Object.keys(headers).length > 0) quirks.headers = headers;
    if (form.anthropic_tool_type.trim()) quirks.anthropic_tool_type = form.anthropic_tool_type.trim();
    if (form.client_emulation.trim()) quirks.client_emulation = form.client_emulation.trim();
    return Object.keys(quirks).length > 0 ? quirks : undefined;
  }

  /** 选中预置供应商：把名称 / 协议 / 地址 / quirks / 余额方案填进表单 */
  function applyPreset(preset: PresetProvider): void {
    setForm((prev) => ({
      ...prev,
      name: prev.name || preset.name,
      channel_type: preset.protocol,
      base_url: preset.baseUrl,
      no_key: preset.noAuth ?? false,
      quirks: quirksToForm(preset.quirks),
      balance_scheme: preset.balanceScheme || '',
      balance_fields: {},
    }));
    setActivePreset(preset.key);
    // 选中预置后直接进「填配置」步（两步式向导）
    setModalStep('form');
  }

  // 全量拉取（page_size=0）：过滤/排序/分页全在前端，交互对齐模型管理页。
  // 搜索/筛选不再触发请求；写操作后的 loadChannels() 刷新照常。
  useEffect(() => {
    void loadChannels().catch((err: unknown) => {
      setError(err instanceof Error ? err.message : String(err));
    });
  }, []);

  // 筛选条件变化时把页码重置到 1（避免「第 3 页 + 新筛选」导致空页）
  useEffect(() => {
    setPage((prev) => (prev === 1 ? prev : 1));
  }, [search, stateFilter, typeFilter, sortMode, pageSize]);

  // 点击外部关闭"更多"菜单
  useEffect(() => {
    if (!moreOpen) return;
    const handler = (e: MouseEvent): void => {
      if (moreRef.current && !moreRef.current.contains(e.target as Node)) {
        setMoreOpen(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [moreOpen]);

  // 过滤（搜索 + 状态 + 类型）+ 排序 —— 与模型管理页同管线，纯前端
  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    const out = channels.filter((ch) => {
      if (stateFilter === 'on' && ch.status !== 'enabled') return false;
      if (stateFilter === 'off' && ch.status === 'enabled') return false;
      if (typeFilter !== 'all' && ch.channel_type !== typeFilter) return false;
      if (!q) return true;
      return (
        ch.name.toLowerCase().includes(q) ||
        ch.channel_type.toLowerCase().includes(q) ||
        (ch.base_url || '').toLowerCase().includes(q) ||
        (ch.models || []).some((m) => m.toLowerCase().includes(q))
      );
    });
    if (sortMode !== 'default') {
      out.sort((a, b) => {
        if (sortMode === 'priority') return b.priority - a.priority;
        if (sortMode === 'response') {
          return (a.response_time ?? Number.POSITIVE_INFINITY) - (b.response_time ?? Number.POSITIVE_INFINITY);
        }
        if (sortMode === 'balance') {
          return (b.balance ?? Number.NEGATIVE_INFINITY) - (a.balance ?? Number.NEGATIVE_INFINITY);
        }
        return a.name.localeCompare(b.name);
      });
    }
    return out;
  }, [channels, search, stateFilter, typeFilter, sortMode]);

  // 客户端分页（PAGE_SIZES 无 'all' 档，窄化为数值口径）
  const pageSizeNum = pageSize === 'all' ? filtered.length || 1 : pageSize;
  const totalPages = Math.max(1, Math.ceil(filtered.length / pageSizeNum));
  const submitJump = (): void => {
    const n = Number(jumpDraft);
    if (Number.isFinite(n) && n >= 1 && n <= totalPages) setPage(Math.floor(n));
    setJumpDraft('');
  };
  const pageRows = useMemo(
    () => filtered.slice((page - 1) * pageSizeNum, (page - 1) * pageSizeNum + pageSizeNum),
    [filtered, page, pageSizeNum],
  );

  // 档位变化时收敛页码（删渠道后当前页可能越界）
  useEffect(() => {
    setPage((prev) => Math.min(prev, totalPages));
  }, [totalPages]);

  // 筛选/排序/搜索落盘（对齐 ModelManage 的 localStorage 记忆）
  useEffect(() => {
    setFilter(LS_FILTERS, { state: stateFilter, type: typeFilter, sort: sortMode, search });
  }, [stateFilter, typeFilter, sortMode, search]);
  // 每页条数落盘已收敛进 usePageSize（选档时写 LS_PSZ），此处不再重复写

  /** 列 key → 是否可见（tbody 的 ch-hidden 由此来；隐藏列 td 保留但不计入布局） */
  const colVisible = useMemo(() => {
    const map = {} as Record<ChColKey, boolean>;
    for (const item of colState.items) {
      const def = CH_COL_DEFS.find((d) => d.key === item.key);
      map[item.key as ChColKey] = item.visible || def?.locked === true;
    }
    return map;
  }, [colState]);

  /** 列 key → 当前对齐（th/td 的 ta-* 类由此来） */
  const colAlign = useMemo(() => {
    const map = {} as Record<ChColKey, ChAlign>;
    for (const item of colState.items) map[item.key as ChColKey] = item.align;
    return map;
  }, [colState]);

  const colCls = (key: ChColKey): string => (colVisible[key] ? '' : ' ch-hidden');

  /** 列设置变更（面板回调；persist=true 才落盘） */
  const commitColItems = useCallback((next: TableColumnItem[], persist: boolean): void => {
    setColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);

  /** 恢复默认：顺序+显隐+对齐+列宽一并还原（公共层 reset） */
  const resetColConfig = useCallback((): void => {
    setColState(resetTableColumns(LS_TCOLS, CH_COL_DEFS));
  }, []);

  // 列设置浮层：点外部收起
  useEffect(() => {
    if (!colPanelOpen) return;
    const handler = (e: MouseEvent): void => {
      if (colPanelRef.current && !colPanelRef.current.contains(e.target as Node)) {
        setColPanelOpen(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [colPanelOpen]);

  // 窄屏（<520px）：面板以视口为锚（fixed），top 跟随按钮底部，
  // 避免面板右对齐到工具栏按钮时左缘溢出屏外、被 .card 的 overflow:hidden 裁掉手柄列。
  useEffect(() => {
    if (!colPanelOpen || !colPanelRef.current) return;
    const btn = colPanelRef.current.querySelector('button');
    if (!btn) return;
    colPanelRef.current.style.setProperty('--ch-colset-top', `${btn.getBoundingClientRect().bottom + 6}px`);
  }, [colPanelOpen]);

  /** 列宽拖动改公共 hook（pointer events + 双击还原 + 末列无把手） */
  const colResize = useColumnResize({
    widths: Object.fromEntries(
      colState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number]),
    ),
    setWidths: (next) => {
      commitColItems(
        colState.items.map((i) => {
          const w = next[i.key];
          const defW = CH_COL_DEFS.find((d) => d.key === i.key)?.width;
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(CH_COL_DEFS.map((d) => [d.key, d.width])),
    lockedKeys: new Set(CH_COL_DEFS.filter((d) => d.locked).map((d) => d.key)),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });

  /** 可见列全集（勾选列 + 显隐过滤后的数据列 + 操作列；colgroup 与表头同源） */
  const visibleColKeys = useMemo(
    () => ['select', ...colState.items.filter((c) => c.visible).map((c) => c.key as string), 'actions'],
    [colState],
  );

  /** 可见数据列（不含勾选/操作列；表头把手与 colgroup 数据段同源） */
  const visibleDataCols = useMemo(
    () => colState.items.filter((c) => c.visible),
    [colState],
  );

  /** 列名（ID 固定英文不译） */
  const colLabel = (key: ChColKey): string =>
    CH_COL_PANEL_LABEL[key] === 'ID' ? 'ID' : t(CH_COL_PANEL_LABEL[key]);

  /** 点击复制（对齐 ModelManage 的 data-copy 交互） */
  const copyText = (text: string): void => {
    void navigator.clipboard?.writeText(text).then(
      () => addToast(`${t('已复制')}: ${text}`),
      () => addToast(t('复制失败'), 'error'),
    );
  };

  // 键盘导航（cc-haha 列表键盘体系）：/ 聚焦搜索、↑↓/j k 行高亮、Enter 打开编辑
  // 作用域=当前页行（客户端分页后与表格渲染同源）
  const { searchRef, rowRefs, activeIndex, setActiveIndex } = useListKeyboard(
    pageRows.length,
    (i) => { if (pageRows[i]) openEdit(pageRows[i]); },
    !loading && !showModal && !confirmState && !showChat && !moreOpen && rowMenuId === null,
  );

  // 全选/反选（仅当前过滤结果）
  const allSelected = pageRows.length > 0 && pageRows.every((ch) => selected.has(ch.id));
  const toggleAll = (): void => {
    if (allSelected) {
      setSelected(new Set());
    } else {
      setSelected(new Set(pageRows.map((ch) => ch.id)));
    }
  };
  const toggleOne = (id: string | number): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  // 批量操作（对齐 ModelManage 批量弹窗）：动作单选 + 选中清单 + 失败保留现场。
  // 逐条执行并统计失败数：单条失败（权限/密码/网络）不能静默吞掉，
  // 否则用户看到「已删除 N 个」而实际一个都没删。失败时弹窗保留并显示汇总。
  const selectedRows = useMemo(
    () => channels.filter((ch) => selected.has(ch.id)),
    [channels, selected],
  );

  const runBatch = async (): Promise<void> => {
    if (selectedRows.length === 0 || batchRunning) return;
    if (batchAction === 'delete' && !batchPassword) {
      setBatchReport(t('请输入删除密码'));
      return;
    }
    setBatchRunning(true);
    setBatchReport('');
    let fail = 0;
    let firstError = '';
    for (const ch of selectedRows) {
      try {
        if (batchAction === 'delete') {
          await api.deleteChannel(ch.id, batchPassword);
        } else {
          await api.patchChannel(ch.id, { status: batchAction === 'enable' ? 'enabled' : 'disabled' });
        }
      } catch (err) {
        fail += 1;
        if (!firstError) firstError = err instanceof Error ? err.message : String(err);
      }
    }
    await loadChannels();
    setBatchRunning(false);
    if (fail === 0) {
      const n = selectedRows.length;
      addToast(
        batchAction === 'enable'
          ? `${t('已启用')} ${n} ${t('个渠道')}`
          : batchAction === 'disable'
            ? `${t('已停用')} ${n} ${t('个渠道')}`
            : `${t('已删除')} ${n} ${t('个渠道')}`,
        'success',
      );
      setBatchOpen(false);
      setSelected(new Set());
      setBatchPassword('');
    } else {
      setBatchReport(
        `${t('操作失败')} ${fail}/${selectedRows.length} ${t('个渠道')}${firstError ? `：${firstError}` : ''}`,
      );
    }
  };

  // "更多"菜单项
  // 全部测试：并发跑完再单条汇总（避免 N 条 Toast 刷屏，cc-haha PromptInputQueuedCommands 聚合模式）
  const handleTestAll = async (): Promise<void> => {
    setMoreOpen(false);
    if (!channels.length) return;
    setTestingAll(true);
    const results = await Promise.all(channels.map((ch) =>
      api.testChannel(ch.id)
        .then((res) => ({ ok: !!(res as { data?: { success?: boolean } }).data?.success, name: ch.name }))
        .catch(() => ({ ok: false, name: ch.name })),
    ));
    const okCount = results.filter((r) => r.ok).length;
    addToast(t('测试完成：{{ok}}/{{total}} 个渠道连通', { ok: okCount, total: results.length }), okCount === results.length ? 'success' : 'warning');
    setTestingAll(false);
    loadChannels();
  };
  const handleDeleteAllDisabled = (): void => {
    setMoreOpen(false);
    const disabled = channels.filter((ch) => ch.status !== 'enabled');
    if (!disabled.length) {
      addToast(t('没有已禁用的渠道'));
      return;
    }
    setConfirmState({
      title: t('删除所有已禁用渠道'),
      message: t('确定删除全部 {{count}} 个已禁用渠道？', { count: disabled.length }),
      confirmText: t('删除'),
      danger: true,
      requirePassword: true,
      onConfirm: async (password) => {
        const results = await Promise.all(
          disabled.map((ch) => api.deleteChannel(ch.id, password).then(() => true).catch(() => false)),
        );
        const ok = results.filter(Boolean).length;
        addToast(
          ok === disabled.length
            ? `${t('已删除')} ${ok} ${t('个渠道')}`
            : `${t('已删除')} ${ok}/${disabled.length} ${t('个渠道（其余失败：密码错误或权限不足）')}`,
          ok === disabled.length ? 'success' : 'error',
        );
        loadChannels();
      },
    });
  };

  const loadChannels = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listChannels({ page_size: 0 });
      const items: ChannelItem[] = (res?.data ?? []).map((ch: ApiChannelItem) => ({
        id: ch.id as string | number,
        seq: ch.seq,
        name: ch.name,
        channel_type: ch.channel_type || 'openai_compatible',
        base_url: ch.base_url,
        api_key: ch.api_key,
        priority: ch.priority ?? 0,
        weight: ch.weight ?? 1,
        status: ch.status || 'disabled',
        models: ch.models,
        discovered_models: ch.discovered_models,
        model_mapping: ch.model_mapping,
        cost_pricing: ch.cost_pricing || {},
        last_used_at: typeof ch.last_used_at === 'number' ? ch.last_used_at : null,
        // 后端 Option 序列化为 null（非缺省），?? 归一为 undefined，
        // 否则 null !== undefined 判断穿透 → null.toFixed() 运行时崩溃
        response_time: ch.response_time ?? undefined,
        test_time: ch.test_time ?? undefined,
        balance: ch.balance ?? undefined,
        credit: ch.credit ?? undefined,
        quirks: ch.quirks ?? null,
        balance_scheme: ch.balance_scheme || '',
        balance_unit: ch.balance_unit || '',
        created_at: typeof ch.created_at === 'number' ? ch.created_at : undefined,
        updated_at: typeof ch.updated_at === 'number' ? ch.updated_at : undefined,
      }));
      // 账号型渠道不在这里落库：它是账号池（AccountSessions）的内部容器，
      // 由「添加账号」自动创建。这里过滤掉，而不是在渲染层过滤，避免
      // 「批量操作 / 统计 / 空态判断」各处都得记得排除一遍。
      setChannels(items.filter((ch) => ch.channel_type !== 'account'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
      firstLoadRef.current = false;
    }
  };

  const openAdd = (): void => {
    setEditChannel(null);
    setForm(defaultForm());
    setActivePreset(null);
    setPresetQuery('');
    // 新增走两步向导：先选供应商（可跳过直接自定义）
    setModalStep('pick');
    setShowModal(true);
  };

  const openEdit = (ch: ChannelItem): void => {
    setEditChannel(ch);
    setActivePreset(null);
    setForm({
      name: ch.name || '',
      channel_type: ch.channel_type || 'openai_compatible',
      base_url: ch.base_url || '',
      api_key: '',
      // 渠道当前为空密钥 → 默认勾选「无密钥」；有密钥 → 不勾（留空=保持不变）
      no_key: !ch.api_key,
      priority: ch.priority ?? 0,
      weight: ch.weight ?? 1,
      status: ch.status || 'enabled',
      models: (ch.models || []).join(', '),
      account_id: ch.account_id || '',
      model_mapping: ch.model_mapping || {},
      cost_pricing: ch.cost_pricing || {},
      quirks: quirksToForm(ch.quirks),
      balance_scheme: ch.balance_scheme || '',
      // 余额凭证存在 quirks.headers 里，编辑时回填到专用输入框（并从 headers 文本里剔除，
      // 避免同一份值既在文本框又在专用框、提交时互相覆盖）
      balance_fields: extractBalanceFields(ch.quirks),
    });
    // 编辑直达第 2 步（供应商已定）
    setModalStep('form');
    setShowModal(true);
  };

  // 关闭渠道弹窗：保存中禁止关闭（防误触丢请求/丢表单）；正常关闭时重置表单
  const closeModal = (): void => {
    if (saving) return;
    setShowModal(false);
    setEditChannel(null);
    setForm(defaultForm());
  };

  // 构建请求 payload — 与后端 ChannelRequest 对齐
  //
  // api_key 三态（后端 Option<String> 语义）：
  // - 勾选「无密钥」→ null = 显式清除密钥
  // - 不勾且有值 → 新密钥
  // - 不勾且为空 → 缺省不提交 = 编辑保持不变（新增 = 空密钥渠道）
  const buildPayload = () => {
    const payload: Record<string, unknown> = {
      name: form.name,
      channel_type: form.channel_type,
      base_url: form.base_url,
      priority: parseInt(String(form.priority), 10) || 0,
      weight: parseInt(String(form.weight), 10) || 1,
      status: form.status,
      models: form.models.split(',').map((s) => s.trim()).filter(Boolean),
      account_id: form.account_id,
      model_mapping: form.model_mapping,
      cost_pricing: form.cost_pricing,
      // 上游特判：新增时总是提交（无特判 = undefined，后端归一为默认）；
      // 编辑时也总是提交，用户清空表单即等于「清掉特判」
      quirks: formToQuirks(form.quirks, form.balance_fields) ?? {},
      balance_scheme: form.balance_scheme,
    };
    if (form.no_key) {
      payload.api_key = null; // 显式清除
    } else if (form.api_key) {
      payload.api_key = form.api_key; // 设置新密钥
    }
    // 否则不带 api_key 字段 = 不涉及
    return payload;
  };

  const handleSave = async (): Promise<void> => {
    if (!form.name) { setError(t('名称为必填项')); return; }
    if (
      form.channel_type !== 'cloudflare' &&
      !form.base_url
    ) {
      setError(t('非 Cloudflare 渠道需填写 Base URL'));
      return;
    }
    setSaving(true);
    setError('');
    try {
      const payload = buildPayload();
      if (editChannel) {
        await api.updateChannel(editChannel.id, payload);
        addToast(t('渠道更新成功'));
      } else {
        await api.addChannel(payload);
        addToast(t('渠道添加成功'));
      }
      closeModal();
      loadChannels();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async (id: string | number): Promise<void> => {
    setTestingId(id);
    setError('');
    try {
      const res = await api.testChannel(id);
      const data = res.data || {};
      addToast(data.success
        ? `${t('连通')}: ${data.message || ''} (${data.latency_ms || 0}ms)`
        : `${t('失败')}: ${data.message || ''}`);
      loadChannels();
    } catch (err) {
      addToast(`${t('测试失败')}: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setTestingId(null);
    }
  };

  // 手动重置渠道断路器（渠道被熔断后恢复）— 恢复放行可能仍有故障的渠道流量，需确认
  const handleResetCircuit = (id: string | number): void => {
    setConfirmState({
      title: t('重置断路器'),
      message: t('重置后将立即恢复向该渠道放行流量。若渠道仍存在故障，可能再次触发熔断。确定继续？'),
      confirmText: t('重置'),
      onConfirm: async () => {
        setError('');
        try {
          await api.resetChannelCircuit(id);
          addToast(t('断路器已重置'));
          loadChannels();
        } catch (err) {
          addToast(`${t('重置失败')}: ${err instanceof Error ? err.message : String(err)}`, 'error');
        }
      },
    });
  };

  // ── 对话调试器 ──
  // 弹窗内渲染统一的 ChatDebugger 组件（与 Playground 共用同一后端入口
  // /api/channels/chat_test），协议/模型/流式/多模态行为完全一致。
  const openChat = (ch: ChannelItem): void => {
    setChatChannel(ch);
    setShowChat(true);
  };

  const closeChat = (): void => {
    setShowChat(false);
    setChatChannel(null);
  };

  // PATCH 部分更新 — 仅传 status 字段，避免脱敏 api_key 覆盖真实密钥。
  // 停用直接影响生产流量 → 确认弹窗；启用方向直接执行。
  const handleToggle = (ch: ChannelItem): void => {
    const disabling = ch.status === 'enabled';
    const doToggle = async (): Promise<void> => {
      setError('');
      try {
        const newStatus = disabling ? 'disabled' : 'enabled';
        await api.patchChannel(ch.id, { status: newStatus });
        addToast(disabling ? t('渠道已停用') : t('渠道已启用'));
        loadChannels();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    };
    if (disabling) {
      setConfirmState({
        title: t('停用渠道'),
        message: t('停用后该渠道不再接收新请求。确定继续？'),
        confirmText: t('停用'),
        danger: true,
        onConfirm: doToggle,
      });
      return;
    }
    void doToggle();
  };

  const handleDelete = (id: string | number): void => {
    setConfirmState({
      title: t('删除渠道'),
      message: t('确定删除此渠道？'),
      confirmText: t('删除'),
      danger: true,
      requirePassword: true,
      onConfirm: async (password) => {
        setError('');
        try {
          await api.deleteChannel(id, password);
          addToast(t('渠道已删除'));
          loadChannels();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const typeLabel = (val: string): string => {
    const found = CHANNEL_TYPES.find((x) => x.value === val);
    if (!found) return val;
    return found.isRaw ? found.labelKey : t(found.labelKey);
  };

  // ── 内联编辑处理 ──
  // PATCH 部分更新：PUT 端点要求全量字段（name 必填），单字段提交会 422
  const handleInlineUpdate = async (id: string | number, field: 'priority' | 'weight', value: number): Promise<void> => {
    if (isNaN(value)) {
      addToast(t('请输入有效的数字'), 'error');
      return;
    }
    try {
      await api.patchChannel(id, { [field]: value } as Record<string, unknown>);
      addToast(t('更新成功'));
      setEditingField(null);
      void loadChannels();
    } catch (err) {
      addToast(`${t('更新失败')}: ${err instanceof Error ? err.message : String(err)}`, 'error');
    }
  };

  // ── 余额更新 ──
  const handleUpdateBalance = async (id: string | number): Promise<void> => {
    setUpdatingBalance((prev) => new Set(prev).add(id));
    try {
      const res = await api.getChannelBalance(id);
      if (res.success && res.data) {
        const d = res.data;
        const detail = (d.wallets || [])
          .filter((w) => w.balance !== undefined)
          .map((w) => `${w.displayName || w.type}: ${formatBalance(w.balance as number, w.unit)}`)
          .join(' · ');
        addToast(
          detail
            ? `${t('余额已更新')} — ${detail}`
            : t('余额已更新'),
        );
        void loadChannels();
      } else {
        addToast(`${t('获取余额失败')}: ${res.error?.message || ''}`, 'error');
      }
    } catch (err) {
      addToast(`${t('获取余额失败')}: ${err instanceof Error ? err.message : String(err)}`, 'error');
    } finally {
      setUpdatingBalance((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
  };

  // ── 相对时间格式化 ──
  const formatRelativeTime = (timestamp: number): string => {
    const now = Date.now();
    const diff = now - timestamp;
    const seconds = Math.floor(diff / 1000);
    const minutes = Math.floor(seconds / 60);
    const hours = Math.floor(minutes / 60);
    const days = Math.floor(hours / 24);

    if (seconds < 60) return t('刚刚');
    if (minutes < 60) return `${minutes}${t('分钟前')}`;
    if (hours < 24) return `${hours}${t('小时前')}`;
    if (days < 7) return `${days}${t('天前')}`;
    return new Date(timestamp).toLocaleDateString();
  };

  // 余额双徽章（new-api BalanceCell：Used 红 / Remaining 绿）
  const getBalanceVariant = (v: number | undefined, unit?: string): string => {
    if (v === undefined) return 'neutral';
    if (v < 0) return 'danger';
    // 百分比余额（OpenCode / 商汤）：<20% 告警；金额：<$5 告警
    const warn = (unit || '').toUpperCase() === '%' ? 20 : 5;
    return v < warn ? 'warning' : 'success';
  };

  // 仅首次加载显示整页骨架：搜索/翻页时若也早退，搜索框与工具栏会被卸载，
  // 用户输入到一半焦点就丢了。首屏之后改为表格区局部 loading。
  if (loading && firstLoadRef.current) {
    return <SkeletonTable columns={6} rows={6} />;
  }

  return (
    <div>
      {/* PageIntro 标题区（内嵌到「渠道与账号池」双 Tab 时由宿主提供） */}
      {!embedded && (
        <div className="page-header">
          <h1>{t('渠道管理')}</h1>
          <p>{t('管理上游 AI 渠道（支持混用 Cloudflare + 第三方 OpenAI 兼容上游）')}</p>
        </div>
      )}

      {error && <div className="error-message">{error}</div>}

      <Card
        title={`${t('所有渠道')} (${channels.length})`}
        actions={
          <div className="channels-toolbar">
            {/* 搜索框（本地即时过滤，无需回车） */}
            <div className="channels-search">
              <Search size={14} />
              <input
                ref={searchRef}
                placeholder={t('搜索渠道')}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') setPage(1);
                }}
              />
            </div>

            {/* 列设置（顺序 / 显隐 / 对齐浮层，localStorage 记忆，对齐模型管理页） */}
            <div className="ch-colset" ref={colPanelRef}>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setColPanelOpen((v) => !v)}
                title={t('列显示设置')}
                style={{ gap: 6 }}
              >
                <Settings2 size={13} />
                {t('列设置')}
              </Button>
              {colPanelOpen && (
                <TableColumnSettings
                  open
                  onClose={() => setColPanelOpen(false)}
                  defs={CH_COL_DEFS}
                  items={colState.items}
                  onChange={commitColItems}
                  onReset={resetColConfig}
                  anchorRef={colPanelRef}
                />
              )}
            </div>

            {/* "更多"下拉 */}
            <div className="channels-more" ref={moreRef}>
              <Button variant="outline" size="sm" onClick={() => setMoreOpen((v) => !v)}>
                <MoreHorizontal size={13} />
              </Button>
              {moreOpen && (
                <div className="channels-more-menu">
                  <button type="button" onClick={handleTestAll} disabled={testingAll}>{testingAll ? t('测试中…') : t('测试所有渠道')}</button>
                  <button type="button" onClick={handleDeleteAllDisabled}>{t('删除所有已禁用渠道')}</button>
                </div>
              )}
            </div>

            <Button size="sm" onClick={openAdd} style={{ gap: 6 }}>
              <Plus size={13} />
              {t('添加渠道')}
            </Button>
          </div>
        }
      >
        {loading ? (
          /* 局部 loading：搜索/翻页时只替换表格，工具栏与搜索框保持挂载 */
          <SkeletonTable columns={6} rows={5} />
        ) : channels.length === 0 ? (
          <EmptyState
            message={t('暂无渠道')}
            icon="🛰️"
            action={<Button onClick={openAdd}>{t('添加第一个渠道')}</Button>}
          />
        ) : (
          <>
            {/* 筛选/排序分段（对齐模型管理页 mm-filters） */}
            <div className="ch-filters">
              <div className="ch-seg" role="tablist" aria-label={t('状态筛选')}>
                {CH_STATE_OPTIONS.map(([v, label]) => (
                  <button
                    key={v}
                    type="button"
                    className={`ch-seg-btn${stateFilter === v ? ' active' : ''}`}
                    onClick={() => setStateFilter(v)}
                  >
                    {t(label)}
                  </button>
                ))}
              </div>
              <div className="ch-seg" role="tablist" aria-label={t('类型筛选')}>
                {CH_TYPE_OPTIONS.map(([v, label]) => (
                  <button
                    key={v}
                    type="button"
                    className={`ch-seg-btn${typeFilter === v ? ' active' : ''}`}
                    onClick={() => setTypeFilter(v)}
                  >
                    {t(label)}
                  </button>
                ))}
              </div>
              <div className="ch-seg" role="tablist" aria-label={t('排序')}>
                {CH_SORT_OPTIONS.map(([v, label]) => (
                  <button
                    key={v}
                    type="button"
                    className={`ch-seg-btn${sortMode === v ? ' active' : ''}`}
                    onClick={() => setSortMode(v)}
                  >
                    {t(label)}
                  </button>
                ))}
              </div>
              <span className="ch-filters-count">
                {filtered.length === channels.length
                  ? `${filtered.length} ${t('个渠道')}`
                  : `${filtered.length} / ${channels.length} ${t('个渠道')}`}
              </span>
            </div>

            {/* 批量条（勾选后出现，对齐 ModelManage mm-batch-chip：批量操作进弹窗） */}
            {selected.size > 0 && (
              <div className="ch-batch-chip">
                <span>
                  {t('已选')} <strong>{selected.size}</strong> {t('个渠道')}
                </span>
                <button
                  type="button"
                  className="btn btn-outline btn-sm"
                  onClick={() => { setBatchReport(''); setBatchOpen(true); }}
                >
                  {t('批量操作')}
                </button>
                <button
                  type="button"
                  className="ch-batch-clear"
                  title={t('清除选择')}
                  onClick={() => setSelected(new Set())}
                >
                  ×
                </button>
              </div>
            )}

            <div className="table-wrapper channels-table-wrap">
              <table className="channels-table">
                <colgroup>
                  {visibleColKeys.map((k) => (
                    <col
                      key={k}
                      data-col-key={k}
                      style={{ width: `${colResize.effectiveWidth(k) ?? CH_COL_DEFAULT_W[k]}px` }}
                    />
                  ))}
                </colgroup>
                <thead>
                  <tr>
                    <th className="col-select">
                      <input
                        type="checkbox"
                        checked={allSelected}
                        onChange={toggleAll}
                        aria-label={t('全选')}
                      />
                    </th>
                    {visibleDataCols.map((c, idx) => {
                      const colDef = CH_COL_DEFS.find((d) => d.key === c.key);
                      // 末列（操作列前最后一列）不给把手：钉在操作列左缘会顶出横滚条
                      const grip = colResize.gripProps(c.key, {
                        isLast: idx === visibleDataCols.length - 1,
                        locked: colDef?.locked,
                        defaultWidth: colDef?.width,
                      });
                      return (
                        <th
                          key={c.key}
                          className={`col-${c.key} ta-${colAlign[c.key as ChColKey]}`}
                        >
                          {colLabel(c.key as ChColKey)}
                          <span
                            className="ch-col-grip"
                            title={t('拖动调整列宽 · 双击还原')}
                            {...grip}
                          />
                        </th>
                      );
                    })}
                    <th className="col-actions">{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {pageRows.map((ch, i) => {
                    const isEditingPriority = editingField?.id === ch.id && editingField?.field === 'priority';
                    const isEditingWeight = editingField?.id === ch.id && editingField?.field === 'weight';
                    const mappingEntries = Object.entries(ch.model_mapping || {});
                    const TypeIcon = TYPE_ICONS[ch.channel_type] || Sparkles;
                    const models = ch.models || [];

                    return (
                      <tr
                        key={ch.id}
                        ref={(el) => { rowRefs.current[i] = el; }}
                        className={`${selected.has(ch.id) ? 'selected' : ''} ${activeIndex === i ? 'kbd-active' : ''} ${ch.status === 'enabled' ? '' : 'ch-off'}`}
                        onMouseEnter={() => setActiveIndex(i)}
                      >
                        <td className="col-select">
                          <input
                            type="checkbox"
                            checked={selected.has(ch.id)}
                            onChange={() => toggleOne(ch.id)}
                            aria-label={t('选择')}
                          />
                        </td>
                        <td className={`col-id${colCls('id')} ta-${colAlign.id}`} title={String(ch.id)}>
                          {ch.seq ?? String(ch.id).slice(0, 8)}
                        </td>
                        <td className={`col-name${colCls('name')} ta-${colAlign.name}`}>
                          <div className="ch-name ch-copy" title={t('点击复制名称')} onClick={() => copyText(ch.name)}>{ch.name}</div>
                          {(ch.base_url || ch.account_id) && (
                            <div
                              className="ch-sub ch-copy"
                              title={t('点击复制地址')}
                              onClick={() => copyText(ch.base_url || ch.account_id || '')}
                            >
                              {ch.base_url || ch.account_id}
                            </div>
                          )}
                        </td>
                        <td className={`col-type${colCls('type')} ta-${colAlign.type}`}>
                          <span className="ch-type-badge">
                            <TypeIcon size={12} />
                            {typeLabel(ch.channel_type)}
                          </span>
                        </td>
                        <td className={`col-status${colCls('status')} ta-${colAlign.status}`}>
                          <span
                            className={`ch-status-badge ${ch.status === 'enabled' ? 'ok' : 'bad'}`}
                            title={ch.last_error || undefined}
                          >
                            {ch.status === 'enabled' ? t('启用') : t('禁用')}
                          </span>
                        </td>
                        <td className={`col-models${colCls('models')} ta-${colAlign.models}`}>
                          {models.length === 0
                            ? <span className="ch-models-all">{t('全部')}</span>
                            : (
                              <div className="ch-models">
                                {models.slice(0, 3).map((m) => (
                                  <span
                                    key={m}
                                    className="ch-model-badge ch-copy"
                                    style={{ color: modelBadgeColor(m), borderColor: modelBadgeColor(m) }}
                                    title={t('点击复制模型名')}
                                    onClick={() => copyText(m)}
                                  >
                                    {m}
                                  </span>
                                ))}
                                {models.length > 3 && (
                                  <span
                                    className="ch-model-badge ch-models-more"
                                    title={models.slice(3).join(', ')}
                                  >
                                    +{models.length - 3}
                                  </span>
                                )}
                              </div>
                            )}
                          {mappingEntries.length > 0 && (
                            <div
                              className="ch-mapping"
                              title={mappingEntries.map(([k, v]) => `${k} → ${v}`).join('\n')}
                            >
                              <ArrowLeftRight size={11} />
                              {t('映射')} {mappingEntries.length}
                            </div>
                          )}
                        </td>
                        <td className={`col-priority${colCls('priority')} ta-${colAlign.priority}`}>
                          {isEditingPriority ? (
                            <input
                              type="number"
                              value={editValue}
                              onChange={(e) => setEditValue(e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter') {
                                  void handleInlineUpdate(ch.id, 'priority', parseInt(editValue, 10));
                                } else if (e.key === 'Escape') {
                                  setEditingField(null);
                                }
                              }}
                              className="ch-num-input"
                              autoFocus
                            />
                          ) : (
                            <span
                              className="inline-editable"
                              onClick={() => {
                                setEditingField({ id: ch.id, field: 'priority' });
                                setEditValue(String(ch.priority));
                              }}
                              title={t('点击编辑')}
                            >
                              {ch.priority}
                            </span>
                          )}
                        </td>
                        <td className={`col-weight${colCls('weight')} ta-${colAlign.weight}`}>
                          {isEditingWeight ? (
                            <input
                              type="number"
                              value={editValue}
                              onChange={(e) => setEditValue(e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter') {
                                  void handleInlineUpdate(ch.id, 'weight', parseInt(editValue, 10));
                                } else if (e.key === 'Escape') {
                                  setEditingField(null);
                                }
                              }}
                              className="ch-num-input"
                              autoFocus
                            />
                          ) : (
                            <span
                              className="inline-editable"
                              onClick={() => {
                                setEditingField({ id: ch.id, field: 'weight' });
                                setEditValue(String(ch.weight));
                              }}
                              title={t('点击编辑')}
                            >
                              {ch.weight}
                            </span>
                          )}
                        </td>
                        <td className={`col-balance${colCls('balance')} ta-${colAlign.balance}`}>
                          <button
                            className="balance-cell"
                            onClick={() => handleUpdateBalance(ch.id)}
                            disabled={updatingBalance.has(ch.id)}
                            title={t('点击更新余额')}
                          >
                            {updatingBalance.has(ch.id) ? (
                              <Loader2 size={13} className="animate-spin" />
                            ) : (
                              <>
                                <span
                                  className={`ch-balance-badge v-${getBalanceVariant(ch.balance, ch.balance_unit)}`}
                                  title={ch.balance_scheme ? `${BALANCE_SCHEME_LABELS[ch.balance_scheme] || ch.balance_scheme}${ch.balance_unit ? ` · ${ch.balance_unit}` : ''}` : undefined}
                                >
                                  {ch.balance !== undefined
                                    ? formatBalance(ch.balance, ch.balance_unit)
                                    : '—'}
                                </span>
                                {ch.credit !== undefined && (
                                  <span className={`ch-balance-badge v-neutral ch-balance-credit`}>
                                    {formatBalance(ch.credit, ch.balance_unit)}
                                  </span>
                                )}
                              </>
                            )}
                          </button>
                        </td>
                        <td className={`col-response${colCls('response')} ta-${colAlign.response}`}>
                          {ch.response_time !== undefined ? (
                            <span className={`response-time-badge ${ch.response_time < 500 ? 'fast' : ch.response_time < 2000 ? 'medium' : 'slow'}`}>
                              {ch.response_time}ms
                            </span>
                          ) : '—'}
                        </td>
                        <td className={`col-testtime${colCls('testtime')} ta-${colAlign.testtime}`}>
                          {ch.test_time ? (
                            <span title={new Date(ch.test_time * 1000).toLocaleString()}>
                              {formatRelativeTime(ch.test_time * 1000)}
                            </span>
                          ) : '—'}
                        </td>
                        <td className="col-actions">
                          <div className="ch-actions">
                            <button
                              type="button"
                              className="ch-icon-btn"
                              title={t('对话调试')}
                              onClick={() => openChat(ch)}
                            >
                              <MessageSquare size={15} />
                            </button>
                            <button
                              type="button"
                              className="ch-icon-btn"
                              title={t('测试连通性')}
                              onClick={() => handleTest(ch.id)}
                              disabled={testingId === ch.id}
                            >
                              {testingId === ch.id
                                ? <Loader2 size={15} className="animate-spin" />
                                : <Gauge size={15} />}
                            </button>
                            <button
                              type="button"
                              className={`ch-icon-btn ${ch.status === 'enabled' ? 'ch-danger-hover' : ''}`}
                              title={ch.status === 'enabled' ? t('停用') : t('启用')}
                              onClick={() => handleToggle(ch)}
                            >
                              {ch.status === 'enabled' ? <Power size={15} /> : <PowerOff size={15} />}
                            </button>
                            <div className="ch-row-menu">
                              <button
                                type="button"
                                className="ch-icon-btn"
                                title={t('更多操作')}
                                onClick={(e) => openRowMenu(e, ch.id)}
                              >
                                <MoreHorizontal size={15} />
                              </button>
                            </div>
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>

            {/* 分页（公共 Pagination 组件，与 Logs 页统一） */}
            {/* 分页页脚：总数/区间 + 每页档位 + 跳页（对齐模型管理页脚） */}
            <div className="ch-table-foot">
              <span className="ch-foot-info">
                {t('共')} {filtered.length} {t('条')}
                {filtered.length > 0 &&
                  ` · ${t('当前第')} ${(page - 1) * pageSizeNum + 1}–${Math.min(
                    page * pageSizeNum,
                    filtered.length,
                  )} ${t('条')}`}
              </span>
              <div className="ch-foot-right">
                <select
                  className="form-input ch-pagesize"
                  value={String(pageSize)}
                  onChange={(e) => {
                    setPageSize(Number(e.target.value));
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
                <Pagination page={page} totalPages={totalPages} onChange={(p) => setPage(p)} />
                <span className="ch-jump">
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
                    placeholder={String(page)}
                    aria-label={t('跳至页码')}
                  />
                  {t('页')}
                </span>
              </div>
            </div>
          </>
        )}
      </Card>

      {/* 行操作菜单（fixed 挂在 body 层级，逃出 Card/table 的 overflow 裁剪） */}
      {rowMenuId !== null && (() => {
        const ch = channels.find((c) => c.id === rowMenuId);
        if (!ch) return null;
        return (
          <div
            className="ch-row-menu-panel ch-row-menu-fixed"
            style={{ top: rowMenuPos.top, left: rowMenuPos.left }}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
          >
            <button type="button" onClick={() => { setRowMenuId(null); void handleTest(ch.id); }}>
              <Gauge size={14} />
              {t('测试连通性')}
            </button>
            <button type="button" onClick={() => { setRowMenuId(null); void handleUpdateBalance(ch.id); }}>
              <DollarSign size={14} />
              {t('查询余额')}
            </button>
            <button type="button" onClick={() => { setRowMenuId(null); void handleResetCircuit(ch.id); }}>
              <RotateCcw size={14} />
              {t('重置断路器')}
            </button>
            <div className="ch-row-menu-sep" />
            <button type="button" onClick={() => { setRowMenuId(null); openEdit(ch); }}>
              <Pencil size={14} />
              {t('编辑渠道')}
            </button>
            <div className="ch-row-menu-sep" />
            <button
              type="button"
              className="ch-row-menu-danger"
              onClick={() => { setRowMenuId(null); handleDelete(ch.id); }}
            >
              <Trash2 size={14} />
              {t('删除渠道')}
            </button>
          </div>
        );
      })()}

      {showModal && (
        <div className="modal-overlay">
          <form className={`modal ${!editChannel && modalStep === 'pick' ? 'modal-wide' : 'modal-form'}`} onSubmit={(e) => { e.preventDefault(); void handleSave(); }}>
            <div className="modal-header">
              <h3>
                {editChannel ? t('编辑渠道') : t('添加渠道')}
                {!editChannel && (
                  <span className="ch-wizard-step">
                    {modalStep === 'pick' ? t('第 1 步 / 2 · 选供应商') : t('第 2 步 / 2 · 填配置')}
                  </span>
                )}
              </h3>
              <button type="button" className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              {/* 错误提示进弹窗 body（页面级 error 会被遮罩盖住） */}
              {error && <div className="error-message" style={{ marginBottom: 12 }}>{error}</div>}

              {/* ── 第 1 步：选供应商（新增时；对齐账号池添加账号弹框的卡片网格）── */}
              {!editChannel && modalStep === 'pick' && (
                <div className="ch-preset-section">
                  <div className="models-search" style={{ marginBottom: 10 }}>
                    <Search size={14} className="models-search-icon" />
                    <input
                      className="form-input"
                      placeholder={t('搜索预置供应商…')}
                      value={presetQuery}
                      onChange={(e) => setPresetQuery(e.target.value)}
                      aria-label={t('搜索预置供应商')}
                    />
                  </div>
                  <div className="ch-preset-grid" role="listbox" aria-label={t('预置供应商')}>
                    {PRESET_PROVIDERS.filter((preset) => {
                      const q = presetQuery.trim().toLowerCase();
                      if (!q) return true;
                      return (
                        preset.name.toLowerCase().includes(q)
                        || (preset.key?.toLowerCase?.() ?? '').includes(q)
                        || (preset.baseUrl ?? '').toLowerCase().includes(q)
                      );
                    }).map((preset) => {
                      const icon = presetIconOf(preset.key);
                      return (
                        <button
                          type="button"
                          key={preset.key}
                          className={`ch-preset-card ${activePreset === preset.key ? 'active' : ''}`}
                          onClick={() => applyPreset(preset)}
                        >
                          {icon ? (
                            <img className="ch-preset-logo" src={icon} alt="" loading="lazy" />
                          ) : (
                            <span className="ch-preset-initial">{preset.name.slice(0, 1)}</span>
                          )}
                          <span className="ch-preset-name">{preset.name}</span>
                          <span className="ch-preset-url">{preset.baseUrl || t('自填地址')}</span>
                          {preset.hint && <span className="ch-preset-hint">{preset.hint}</span>}
                          <span className="ch-preset-badges">
                            {preset.noAuth && <span className="ch-preset-badge">{t('免密钥')}</span>}
                            {preset.balanceScheme && <span className="ch-preset-badge">{t('余额查询')}</span>}
                          </span>
                        </button>
                      );
                    })}
                  </div>
                  <div className="ch-preset-skip">
                    <Button variant="outline" type="button" onClick={() => setModalStep('form')}>
                      {t('自定义渠道（跳过预置）')}
                    </Button>
                    <span className="ch-preset-skip-hint">
                      {t('账号型渠道（凭证放账号池）也走自定义，类型选 account')}
                    </span>
                  </div>
                </div>
              )}

              {/* ── 第 2 步：填配置 ── */}
              {(editChannel || modalStep === 'form') && !editChannel && (
                <div className="ch-wizard-bar">
                  <span>
                    {t('供应商')}：
                    {activePreset
                      ? PRESET_PROVIDERS.find((p) => p.key === activePreset)?.name
                      : t('自定义')}
                  </span>
                  <Button variant="outline" type="button" onClick={() => setModalStep('pick')}>
                    {t('更换供应商')}
                  </Button>
                </div>
              )}
              <Input
                label={`${t('名称')} *`}
                placeholder={t('例如：OpenAI 官方')}
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                autoFocus
              />
              <Select
                label={t('渠道类型')}
                value={form.channel_type}
                onChange={(e) => {
                  const newType = e.target.value;
                  const isUsingDefault = Object.values(DEFAULT_BASE_URL).includes(form.base_url) || !form.base_url;
                  setForm({
                    ...form,
                    channel_type: newType,
                    base_url: isUsingDefault ? (DEFAULT_BASE_URL[newType as keyof typeof DEFAULT_BASE_URL] || '') : form.base_url,
                  });
                }}
              >
                {CHANNEL_TYPES.map((tp) => (
                  <option key={tp.value} value={tp.value}>
                    {tp.isRaw ? tp.labelKey : t(tp.labelKey)}
                  </option>
                ))}
              </Select>
              {AUTH_HINT[form.channel_type as keyof typeof AUTH_HINT] && (
                <div className="form-hint" style={{ color: 'var(--accent-color)', marginTop: -8 }}>
                  {t(AUTH_HINT[form.channel_type as keyof typeof AUTH_HINT]!)}
                </div>
              )}
                            {form.channel_type === 'cloudflare' ? (
                <Input
                  label={t('Cloudflare 账号 ID')}
                  placeholder={t('Cloudflare 账号 ID')}
                  value={form.account_id}
                  onChange={(e) => setForm({ ...form, account_id: e.target.value })}
                />
              ) : (
                <Input
                  label="Base URL"
                  placeholder="https://cf-ai-gw.pages.dev 或 https://api.Workspace_2B8939.com/v1"
                  value={form.base_url}
                  onChange={(e) => setForm({ ...form, base_url: e.target.value })}
                  hint={t('未带 /v1 时会自动补齐')}
                />
              )}
              <div className="form-group">
                <label>API Key {editChannel && !form.no_key && t('（留空则保持不变）')}</label>
                <label className="no-key-row" title={t('勾选后转发请求不带鉴权头，适用于免鉴权上游或网关侧 IP 白名单')}>
                  <input
                    type="checkbox"
                    checked={form.no_key}
                    onChange={onNoKeyChange}
                  />
                  {t('无密钥（免鉴权上游）')}
                </label>
                <div className="password-input-wrap">
                  <input
                    className="form-input password-input"
                    type={showApiKey ? 'text' : 'password'}
                    disabled={form.no_key}
                    placeholder={form.no_key ? t('无密钥模式') : (editChannel ? t('留空保持当前值') : 'API Key')}
                    value={form.api_key}
                    onChange={(e) => setForm({ ...form, api_key: e.target.value })}
                  />
                  <button
                    type="button"
                    className="password-input-toggle"
                    tabIndex={-1}
                    disabled={form.no_key}
                    onClick={() => setShowApiKey((v) => !v)}
                    aria-label={showApiKey ? t('隐藏') : t('显示')}
                    title={showApiKey ? t('隐藏') : t('显示')}
                  >
                    {showApiKey ? <EyeOff size={14} /> : <Eye size={14} />}
                  </button>
                </div>
              </div>
              {/* 模型清单（白名单）统一收敛到「模型管理」页（/model-manage），
                  这里不再提供逗号输入框与拉取按钮。渠道级「模型映射」与
                  「成本价」是渠道自身配置，仍在此维护（与全局别名区分）。 */}
              {editChannel && (
                <div className="form-group">
                  <label>{t('模型清单')}</label>
                  <div className="ch-model-summary">
                    <div className="ch-model-summary-counts">
                      <span>
                        {t('已配置')} <strong>{(editChannel.models || []).length}</strong> {t('个模型')}
                      </span>
                      <span className="ch-model-summary-sep">·</span>
                      <span>
                        {t('上游发现')} <strong>{(editChannel.discovered_models || []).length}</strong>
                      </span>
                    </div>
                    {(editChannel.models || []).length > 0 && (
                      <div className="ch-models">
                        {(editChannel.models || []).slice(0, 6).map((m) => (
                          <span key={m} className="ch-model-badge">{m}</span>
                        ))}
                        {(editChannel.models || []).length > 6 && (
                          <span
                            className="ch-model-badge ch-models-more"
                            title={(editChannel.models || []).slice(6).join(', ')}
                          >
                            +{(editChannel.models || []).length - 6}
                          </span>
                        )}
                      </div>
                    )}
                    <Link className="btn btn-outline btn-sm" to={`/model-manage?channel_id=${encodeURIComponent(String(editChannel.id))}`}>
                      {t('在模型管理中维护')} →
                    </Link>
                  </div>
                </div>
              )}
              <details className="ch-advanced">
                <summary>{t('高级配置：模型映射 / 成本价 / 上游特判 / 余额查询')}</summary>
                <div className="ch-advanced-body">
              <div className="form-group">
                    <label>{t('模型映射')}</label>
                    <div className="form-hint" style={{ marginBottom: 8 }}>
                      {t('用户请求的模型名 → 实际转发给上游的模型名。优先级：渠道级映射 > 全局映射 > 原名透传。')}
                    </div>
                    <ModelMappingEditor
                      value={form.model_mapping}
                      onChange={(mapping) => setForm((f) => ({ ...f, model_mapping: mapping }))}
                      sourceModelOptions={mappingModelOptions}
                      targetModelOptions={mappingModelOptions}
                      disabled={saving}
                    />
                  </div>
                  <div className="form-group">
                    <label>{t('成本价（可选）')}</label>
                    <div className="form-hint" style={{ marginBottom: 8 }}>
                      {t('按映射后的上游模型名填成本价（每 1k token）。可留空——填了管理员可在日志看利润，不填利润显示「—」。')}
                    </div>
                    <CostPricingEditor
                      value={form.cost_pricing}
                      mappingTargets={Object.values(form.model_mapping)}
                      onChange={(cp) => setForm((f) => ({ ...f, cost_pricing: cp }))}
                      disabled={saving}
                    />
                  </div>
                  <div style={{ display: 'flex', gap: 12 }}>
                    <Input
                      label={t('优先级（越大越优先）')}
                      type="number"
                      value={String(form.priority)}
                      onChange={(e) => setForm({ ...form, priority: e.target.value })}
                      style={{ flex: 1 }}
                    />
                    <Input
                      label={t('权重')}
                      type="number"
                      value={String(form.weight)}
                      onChange={(e) => setForm({ ...form, weight: e.target.value })}
                      style={{ flex: 1 }}
                    />
                  </div>
                  <Select
                    label={t('状态')}
                    value={form.status}
                    onChange={(e) => setForm({ ...form, status: e.target.value })}
                  >
                    <option value="enabled">{t('启用')}</option>
                    <option value="disabled">{t('禁用')}</option>
                  </Select>

                  {/* ── 上游特判（quirks）── */}
                  <details className="ch-quirks-section">
                    <summary>
                      {t('上游特判（预置供应商会自动填好，一般不用手改）')}
                      {formToQuirks(form.quirks, form.balance_fields) && (
                        <span className="ch-quirks-badge">{t('已配置')}</span>
                      )}
                    </summary>
                    <div className="ch-quirks-body">
                      <Input
                        label={t('URL 查询串后缀')}
                        placeholder="?beta=true"
                        value={form.quirks.url_suffix}
                        onChange={(e) => setForm((f) => ({ ...f, quirks: { ...f.quirks, url_suffix: e.target.value } }))}
                        hint={t('原样追加到出站 URL（GLM / MiniMax 的 Claude 兼容端点要求 ?beta=true）')}
                      />
                      <div className="form-group">
                        <label>{t('额外请求头（每行 Key: Value）')}</label>
                        <textarea
                          className="form-input"
                          rows={4}
                          placeholder={'X-CLIENT-TYPE: cline-sdk\nAnthropic-Beta: claude-code-20250219'}
                          value={form.quirks.headers}
                          onChange={(e) => setForm((f) => ({ ...f, quirks: { ...f.quirks, headers: e.target.value } }))}
                        />
                        <div className="form-hint">
                          {t('合并到默认头上（同名覆盖）。Cline 一类上游按客户端形态校验，缺头会 403。')}
                        </div>
                      </div>
                      <Select
                        label={t('Anthropic 工具类型')}
                        value={form.quirks.anthropic_tool_type}
                        onChange={(e) => setForm((f) => ({ ...f, quirks: { ...f.quirks, anthropic_tool_type: e.target.value } }))}
                      >
                        <option value="">{t('不补（默认）')}</option>
                        <option value="custom">{t('补 type: custom（MiniMax）')}</option>
                      </Select>
                      <Select
                        label={t('客户端伪装')}
                        value={form.quirks.client_emulation}
                        onChange={(e) => setForm((f) => ({ ...f, quirks: { ...f.quirks, client_emulation: e.target.value } }))}
                      >
                        <option value="">{t('不伪装（默认）')}</option>
                        <option value="opencode">{t('OpenCode 官方客户端（免费档必需）')}</option>
                      </Select>
                    </div>
                  </details>

                  {/* ── 余额查询方案 ── */}
                  <div className="ch-balance-section">
                    <Select
                      label={t('余额查询方案')}
                      value={form.balance_scheme}
                      onChange={(e) => setForm((f) => ({ ...f, balance_scheme: e.target.value }))}
                    >
                      <option value="">{t('默认（OpenAI 计费端点）')}</option>
                      {Object.entries(BALANCE_SCHEME_LABELS).map(([value, label]) => (
                        <option key={value} value={value}>{label}</option>
                      ))}
                    </Select>
                    {(() => {
                      const preset = PRESET_PROVIDERS.find((p) => p.balanceScheme === form.balance_scheme);
                      const fields = preset?.balanceFields || [];
                      if (!fields.length) return null;
                      return (
                        <div className="ch-balance-fields">
                          {fields.map((field) => (
                            <Input
                              key={field.header}
                              label={field.label}
                              placeholder={field.placeholder}
                              value={form.balance_fields[field.header] || ''}
                              onChange={(e) =>
                                setForm((f) => ({
                                  ...f,
                                  balance_fields: { ...f.balance_fields, [field.header]: e.target.value },
                                }))
                              }
                            />
                          ))}
                        </div>
                      );
                    })()}
                </div>
                </div>
              </details>
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={closeModal} disabled={saving}>{t('取消')}</Button>
              {!editChannel && modalStep === 'pick' ? (
                <Button type="button" onClick={() => setModalStep('form')}>
                  {t('自定义渠道')}
                </Button>
              ) : (
                <Button type="submit" disabled={saving}>
                  {saving ? t('保存中...') : (editChannel ? t('更新') : t('添加'))}
                </Button>
              )}
            </div>
          </form>
        </div>
      )}

      {/* ── 批量操作弹窗（对齐 ModelManage 批量弹窗：动作单选 + 选中清单 + 失败保留现场） ── */}
      {batchOpen && (
        <div className="modal-overlay" onClick={() => !batchRunning && setBatchOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>
                {t('批量操作')} · {t('已选')} {selectedRows.length} {t('个渠道')}
              </h3>
              <button className="modal-close" onClick={() => setBatchOpen(false)} disabled={batchRunning}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <div className="ch-batch-models">
                {selectedRows.map((ch) => (
                  <code key={ch.id} title={ch.base_url || ''}>{ch.name}</code>
                ))}
              </div>
              <div className="ch-batch-actions">
                {(['enable', 'disable', 'delete'] as BatchAction[]).map((v) => (
                  <label key={v} className={`ch-batch-action${batchAction === v ? ' active' : ''}`}>
                    <input
                      type="radio"
                      name="ch-batch-action"
                      checked={batchAction === v}
                      onChange={() => setBatchAction(v)}
                      disabled={batchRunning}
                    />
                    {v === 'enable' ? t('启用') : v === 'disable' ? t('停用') : t('删除')}
                  </label>
                ))}
              </div>
              {batchAction === 'delete' && (
                <>
                  <p className="ch-dialog-note">{t('删除后渠道不可恢复，此操作需要删除密码确认。')}</p>
                  <input
                    className="form-input"
                    type="password"
                    value={batchPassword}
                    onChange={(e) => setBatchPassword(e.target.value)}
                    placeholder={t('删除密码')}
                    autoFocus
                    disabled={batchRunning}
                  />
                </>
              )}
              {batchReport && <div className="error-message">{batchReport}</div>}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setBatchOpen(false)} disabled={batchRunning}>
                {t('取消')}
              </button>
              <button
                className={`btn ${batchAction === 'delete' ? 'btn-danger' : 'btn-primary'}`}
                onClick={() => void runBatch()}
                disabled={batchRunning || selectedRows.length === 0}
              >
                {batchRunning
                  ? t('执行中…')
                  : `${t('执行')}${batchAction === 'enable' ? t('启用') : batchAction === 'disable' ? t('停用') : t('删除')}`}
              </button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

      {showChat && chatChannel && (
        <div className="modal-overlay" onClick={closeChat}>
          <div className="modal modal-chat" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('对话调试')} — {chatChannel.name}</h3>
              <button className="modal-close" onClick={closeChat}>&times;</button>
            </div>
            <div className="modal-body chat-modal-body">
              <ChatDebugger
                channelId={String(chatChannel.id)}
                channelModels={Array.from(new Set([
                  ...(chatChannel.models || []),
                  ...(chatChannel.discovered_models || []),
                ]))}
                initialProtocol={chatChannel.channel_type === 'anthropic' ? 'anthropic' : 'openai'}
                /* 无密钥渠道：浏览器直连上游，请求从本机发出、不经服务器 IP */
                directUpstream={!chatChannel.api_key ? { base_url: chatChannel.base_url, channel_type: chatChannel.channel_type } : undefined}
                compact
              />
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
