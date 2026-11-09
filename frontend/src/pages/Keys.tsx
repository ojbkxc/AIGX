import { useState, useEffect, useMemo, useRef, useCallback, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { useSearchParams } from 'react-router-dom';
import {
  Columns3,
  Eye, EyeOff, MoreHorizontal, Search, Pencil, Power, PowerOff,
  Loader2, RotateCcw, Trash2, Copy, Check, RefreshCcw,
} from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { fmtQuota, isAdmin } from '../lib/utils';
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
import { TablePaginationFooter, usePageSize, CLIENT_PAGE_SIZES } from '../components/ui/TableShell';
import { Tooltip } from '../components/ui';
import { useListKeyboard } from '../hooks/useListKeyboard';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import KeysDocsPanel from '../components/KeysDocsPanel';
import { Button, Card, Input, EmptyState, Select, SkeletonTable, MultiSelect, Tabs } from '../components/ui';
import './Keys.css';

/** 客户端分页每页条数（与 Pricing 页一致） */
const LS_PSZ = 'aigx-tbl-pagesize.keys';
const LS_TCOLS = 'aigx-tbl-cols.keys';
/** 密钥表列声明（select/actions 锁定） */
const KEYS_COL_DEFS: TableColumnDef[] = [
  { key: 'select', label: '选择', align: 'center', width: 32, locked: true },
  { key: 'name', label: '名称', width: 150 },
  { key: 'user', label: '用户', width: 150 },
  { key: 'status', label: '状态', align: 'center', width: 80 },
  { key: 'key', label: '密钥', width: 220 },
  { key: 'quota', label: '额度', align: 'right', width: 130 },
  { key: 'group', label: '分组', width: 100 },
  { key: 'models', label: '模型', width: 160 },
  { key: 'expires', label: '过期', width: 150 },
  { key: 'created', label: '创建时间', width: 150 },
  { key: 'actions', label: '操作', width: 130, locked: true },
];
const DEFAULT_PAGE_SIZE = 20;

/** 页面 Tab：密钥管理 / 接入文档（文档 2026-11-07 自独立页并入，07 E.3） */
type KeysTab = 'keys' | 'docs';

interface TokenItem {
  id: string | number;
  name: string;
  /** 所属用户邮箱（管理员视角带出归属；管理员级令牌为空串） */
  user_email?: string;
  group?: string;
  allowed_models?: string[] | string;
  ip_limit?: string[];
  quota_limit?: number | null;
  used_quota?: number;
  expires_at?: number | null;
  status?: string;
  is_active?: boolean;
  created_at?: number;
  plain_key?: string;
  key?: string;
  api_key?: string;
}

interface GroupItem {
  name?: string;
}

interface KeyFormState {
  name: string;
  group: string;
  allowed_models: string;
  /** 过期时间：表单内为 datetime-local 字符串（空串=永不过期），提交时转 Unix 秒 */
  expires_at: string;
  quota_limit: string;
  ip_limit: string;
  status: string;
  /** 自定义密钥值（cf-ai-gw 同款语义）：创建时可选，留空随机生成 sk-xxx */
  custom_key: string;
}

interface GeneratedKeyState {
  key?: string;
  api_key?: string;
  [field: string]: unknown;
}

interface RotatedKeyState {
  name: string;
  key: string;
}

const EMPTY_FORM: KeyFormState = {
  name: '',
  group: 'default',
  allowed_models: '',
  expires_at: '',
  quota_limit: '',
  ip_limit: '',
  status: 'active',
  custom_key: '',
};

/** Unix 秒 → datetime-local 字符串（本地时区，取分钟精度） */
function tsToLocalInput(ts: number | null | undefined): string {
  if (ts == null || ts <= 0) return '';
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  if (Number.isNaN(d.getTime())) return '';
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** datetime-local 字符串 → Unix 秒（空串/非法返回 null） */
function localInputToTs(v: string): number | null {
  if (!v.trim()) return null;
  const ms = new Date(v).getTime();
  if (Number.isNaN(ms)) return null;
  return Math.floor(ms / 1000);
}

/** 模型名 → 稳定颜色（与渠道页 autoColor 同思路：字符串 hash → HSL 色相） */
function modelBadgeColor(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) {
    hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  }
  const hue = hash % 360;
  return `hsl(${hue} 65% 45%)`;
}

export default function Keys(): JSX.Element {
  // Tab 走 URL 参数（?tab=docs），与渠道页/模型管理页的合并 Tab 同构：
  // 刷新/书签直达、文档 Tab 的链接可分享（/keys?tab=docs）
  const [params, setParams] = useSearchParams();
  const tabParam = params.get('tab');
  const tab: KeysTab = tabParam === 'docs' ? 'docs' : 'keys';
  const changeTab = (next: KeysTab): void => {
    if (next === tab) return;
    const nextParams = new URLSearchParams(params);
    if (next === 'keys') nextParams.delete('tab');
    else nextParams.set('tab', next);
    setParams(nextParams, { replace: true });
  };

  const [tokens, setTokens] = useState<TokenItem[]>([]);
  const [groups, setGroups] = useState<GroupItem[]>([]);
  /** 模型白名单候选（对话/运维页同源：/api/models/available） */
  const [modelOptions, setModelOptions] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const [showModal, setShowModal] = useState(false);
  // 明文 key 展开状态（列表脱敏；明文经「查看/复制」按需取回）
  const [revealedKeys, setRevealedKeys] = useState<Record<string | number, boolean>>({});
  // 按需取回的明文缓存（管理员列表脱敏，点击查看/复制时经 GET /api/tokens/:id/key 取回）
  const [plainKeys, setPlainKeys] = useState<Record<string | number, string>>({});
  // 正在取明文的令牌 ID（查看按钮 loading 态）
  const [fetchingKeyId, setFetchingKeyId] = useState<string | number | null>(null);
  // 复制成功打勾回显（new-api ApiKeyCell：Copy → Check 短暂切换）
  const [copiedKeyId, setCopiedKeyId] = useState<string | number | null>(null);
  const [editing, setEditing] = useState<TokenItem | null>(null);
  const [form, setForm] = useState<KeyFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [generatedKey, setGeneratedKey] = useState<GeneratedKeyState | null>(null);

  // 令牌轮换后展示的新密钥（一次性显示，提示用户立即保存）
  const [rotatedKey, setRotatedKey] = useState<RotatedKeyState | null>(null);

  // ── 批量选择 / 搜索 / 分页 / 行操作菜单（对齐渠道页） ──
  const [selected, setSelected] = useState<Set<string | number>>(new Set());
  const [search, setSearch] = useState('');
  // 管理员视角切换（需求 4）：管理员进入本页**默认**查看全系统令牌，
  // 可显式切回「只看自己的」；普通用户恒 false（后端对普通用户无视 all=1
  // 只返回本人，且下方切换按钮仅管理员可见）。
  const [viewAll, setViewAll] = useState(isAdmin());
  // 防止 load() 依赖 viewAll 造成初始双请求：仅由切换控件显式触发重新拉取
  const viewAllRef = useRef(isAdmin());
  // 客户端分页：搜索过滤变化时回第 1 页；每页条数公共 usePageSize（CLIENT 档位含 'all'）
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = usePageSize(LS_PSZ, CLIENT_PAGE_SIZES, DEFAULT_PAGE_SIZE);
  // 列设置已迁公共层（tableColumns v2）
  const [colState, setColState] = useState<TableColumnState>(() => readTableColumns(LS_TCOLS, KEYS_COL_DEFS));
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);
  const commitKeysCols = useCallback((next: TableColumnItem[], persist: boolean) => {
    setColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);
  const resetKeysCols = useCallback(() => setColState(resetTableColumns(LS_TCOLS, KEYS_COL_DEFS)), []);
  const appliedCols = useMemo(() => applyTableColumns(colState, KEYS_COL_DEFS), [colState]);
  const visibleKeySet = useMemo(() => new Set(appliedCols.map((c) => c.key)), [appliedCols]);
  const colResize = useColumnResize({
    widths: Object.fromEntries(colState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number])),
    setWidths: (next) => {
      commitKeysCols(
        colState.items.map((i) => {
          const w = next[i.key];
          const defW = KEYS_COL_DEFS.find((d) => d.key === i.key)?.width;
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(KEYS_COL_DEFS.map((d) => [d.key, d.width])),
    lockedKeys: new Set(KEYS_COL_DEFS.filter((d) => d.locked).map((d) => d.key)),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });
  // 菜单用 fixed 定位挂在 body 层级：Card/table-wrap 的 overflow 会裁掉
  // 行内 absolute 弹层（尤其最后一行向下弹出时），fixed 可逃出所有裁剪容器
  const [rowMenuId, setRowMenuId] = useState<string | number | null>(null);
  const [rowMenuPos, setRowMenuPos] = useState<{ top: number; left: number }>({ top: 0, left: 0 });
  useEffect(() => {
    if (rowMenuId === null) return;
    const handler = (): void => setRowMenuId(null);
    document.addEventListener('mousedown', handler);
    // 页面/表格滚动时关闭菜单（fixed 坐标不随滚动联动）
    const scrollHandler = (): void => setRowMenuId(null);
    window.addEventListener('scroll', scrollHandler, true);
    return () => {
      document.removeEventListener('mousedown', handler);
      window.removeEventListener('scroll', scrollHandler, true);
    };
  }, [rowMenuId]);

  /** 打开行菜单：按触发按钮屏幕坐标计算 fixed 弹出位置（右对齐、下弹出，底部溢出改上弹） */
  const openRowMenu = (e: React.MouseEvent<HTMLButtonElement>, id: string | number): void => {
    e.stopPropagation();
    if (rowMenuId === id) { setRowMenuId(null); return; }
    const rect = e.currentTarget.getBoundingClientRect();
    const menuH = 200; // 菜单预估高度（4 项 + 分隔线）
    const below = window.innerHeight - rect.bottom;
    const top = below < menuH && rect.top > menuH
      ? rect.top - menuH + rect.height
      : rect.bottom + 4;
    setRowMenuPos({ top, left: rect.right });
    setRowMenuId(id);
  };

  useEffect(() => {
    // 管理员默认全量加载（与 viewAll 初值一致）；普通用户 isAdmin() 为 false，
    // 与改动前行为完全一致（只拉自己的令牌）
    void load(isAdmin());
  }, []);

  // 模型白名单候选：与聊天页 / AI 运维页同源（网关聚合的可用模型）。
  // 与 keys 列表分开拉，失败静默降级——候选拿不到时 MultiSelect 仍允许自由填。
  useEffect(() => {
    let mounted = true;
    api.listModels()
      .then((res) => {
        if (!mounted) return;
        const raw = Array.isArray(res) ? res : res?.data;
        const list = Array.isArray(raw)
          ? raw
              .map((m) => (typeof m === 'string' ? m : (m as { id?: string })?.id ?? ''))
              .filter(Boolean)
              .sort()
          : [];
        setModelOptions(list);
      })
      .catch(() => undefined);
    return () => {
      mounted = false;
    };
  }, []);

  const load = async (all = false) => {
    setLoading(true);
    setError('');
    try {
      const [tokenRes, groupRes] = await Promise.all([
        api.listTokens(all),
        isAdmin() ? api.listGroups().catch(() => null) : Promise.resolve(null),
      ]);
      setTokens(Array.isArray(tokenRes?.data) ? (tokenRes.data as unknown as TokenItem[]) : []);
      if (groupRes) setGroups(Array.isArray(groupRes?.data) ? groupRes.data : []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  // 管理员切换「只看自己的 / 查看全部」：显式触发重新拉取
  const toggleViewAll = (): void => {
    const next = !viewAllRef.current;
    viewAllRef.current = next;
    setViewAll(next);
    setSelected(new Set());
    setPage(1);
    void load(next);
  };

  // 前端过滤（后端令牌接口无 search 参数，全量拉回后本地匹配——名称/分组/密钥前缀）
  const q = search.trim().toLowerCase();
  const filtered = useMemo(() => {
    if (!q) return tokens;
    return tokens.filter((tk) =>
      (tk.name || '').toLowerCase().includes(q)
      || (tk.group || '').toLowerCase().includes(q)
      || (tk.key || '').toLowerCase().includes(q));
  }, [tokens, q]);

  // 客户端分页：'all' 档恒 1 页；其余按每页条数取整（空集也至少 1 页，避免 0 页）。
  // 原实现 'all' 档把总页数算成 filtered.length，会渲染出 N 个页码按钮——一并修正。
  const pageSizeNum = pageSize === 'all' ? Math.max(1, filtered.length) : pageSize;
  const totalPages = pageSize === 'all' ? 1 : Math.max(1, Math.ceil(filtered.length / pageSize));
  // 渲染用钳位页：page 越界时按末页渲染（显示层兜底）
  const safePage = Math.min(page, totalPages);
  const pageItems = pageSize === 'all' ? filtered : filtered.slice((safePage - 1) * pageSizeNum, safePage * pageSizeNum);

  // 页码真复位：删除/筛选收缩令总页数减少后，把 page state 夹回末页。
  // 原先只有 safePage 渲染钳位（page 本身仍停在旧值），再次增删会跳到意外页。
  useEffect(() => {
    setPage((p) => (p > totalPages ? totalPages : p));
  }, [totalPages]);

  // 键盘导航（cc-haha 列表键盘体系）：/ 聚焦搜索、↑↓/j k 行高亮、Enter 打开编辑
  const { searchRef, rowRefs, activeIndex, setActiveIndex } = useListKeyboard(
    pageItems.length,
    (i) => { if (pageItems[i]) openEdit(pageItems[i]); },
    !loading && !showModal && !confirmState && !rotatedKey && rowMenuId === null,
  );

  // 全选/反选（仅当前过滤结果）
  const allSelected = filtered.length > 0 && filtered.every((tk) => selected.has(tk.id));
  const toggleAll = (): void => {
    if (allSelected) setSelected(new Set());
    else setSelected(new Set(filtered.map((tk) => tk.id)));
  };
  const toggleOne = (id: string | number): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const openCreate = () => {
    setEditing(null);
    setForm(EMPTY_FORM);
    setGeneratedKey(null);
    setShowModal(true);
  };

  const openEdit = (tk: TokenItem) => {
    setEditing(tk);
    setForm({
      name: tk.name || '',
      group: tk.group || 'default',
      allowed_models: Array.isArray(tk.allowed_models)
        ? tk.allowed_models.join(', ')
        : (tk.allowed_models || ''),
      expires_at: tsToLocalInput(tk.expires_at),
      quota_limit: tk.quota_limit != null ? String(tk.quota_limit) : '',
      // 编辑时预填现有 IP 白名单（None=不限展示为空），避免保存时把
      // 未展示的字段清空——后端只要 Some 就覆盖。
      ip_limit: Array.isArray(tk.ip_limit) ? tk.ip_limit.join(', ') : '',
      status: tk.status || (tk.is_active === false ? 'disabled' : 'active'),
      // 编辑态无自定义密钥值（密钥值只在创建时可选）
      custom_key: '',
    });
    setGeneratedKey(null);
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditing(null);
    setGeneratedKey(null);
  };

  const handleSave = async (e: FormEvent) => {
    e.preventDefault();
    if (!form.name.trim()) {
      addToast(t('密钥名称为必填项'), 'error');
      return;
    }
    setSaving(true);
    setError('');
    try {
      const payload: Record<string, unknown> = {
        name: form.name.trim(),
        group: form.group || 'default',
        status: form.status,
      };
      // 模型白名单/IP 白名单：创建必发；编辑仅在用户改动时发——
      // 后端契约 Some 即覆盖，恒发空数组会把已有白名单清掉。
      const allowedModels = form.allowed_models
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      const ipLimit = form.ip_limit
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      // 可清除字段按「与原值比较」决定是否提交：原来用「非空才发」，
      // 用户把白名单/额度/过期时间清空后保存，后端因字段缺省视为「不修改」，
      // 限制依然生效且界面无任何提示。
      const origModels = Array.isArray(editing?.allowed_models)
        ? editing!.allowed_models
        : [];
      if (
        !editing
        || allowedModels.length > 0
        || JSON.stringify(origModels.slice().sort()) !== JSON.stringify(allowedModels.slice().sort())
      ) {
        payload.allowed_models = allowedModels;
      }
      const origIps = Array.isArray(editing?.ip_limit) ? editing!.ip_limit : [];
      if (
        !editing
        || ipLimit.length > 0
        || JSON.stringify(origIps.slice().sort()) !== JSON.stringify(ipLimit.slice().sort())
      ) {
        // 留空=显式清除；后端 update/generate 均把 [] 归一化为 None（不限）。
        // 注意历史坑：未归一化的版本会把 [] 判成「拒绝所有 IP」。
        payload.ip_limit = ipLimit;
      }
      const expiresTs = localInputToTs(form.expires_at);
      if (expiresTs != null) {
        payload.expires_at = expiresTs;
      } else if (editing && editing.expires_at != null && !form.expires_at.trim()) {
        // 显式清空过期时间：后端 null = 永不过期
        payload.expires_at = null;
      }
      // 自定义密钥值（仅创建）：非空才发，留空后端随机生成（cf-ai-gw 语义）
      if (!editing && form.custom_key.trim()) {
        payload.custom_key = form.custom_key.trim();
      }
      const quotaNum = Number(form.quota_limit);
      if (form.quota_limit.trim()) {
        if (!Number.isFinite(quotaNum)) {
          // 非数字输入：finally 会复位 saving，此处提示后直接中止提交
          addToast(t('额度上限必须为数字'), 'error');
          return;
        }
        payload.quota_limit = quotaNum;
      } else if (editing && editing.quota_limit != null) {
        // 清空额度上限 = 改为不限（后端 0 表示不限）
        payload.quota_limit = 0;
      }

      if (editing) {
        await api.updateToken(editing.id, payload);
        addToast(t('令牌已更新'));
      } else {
        const res = await api.addToken(payload);
        const created = (res?.data ?? {}) as unknown as GeneratedKeyState;
        // 契约：创建响应额外返回 plain_key（一次性明文），优先展示明文而非脱敏密钥
        if (created && (created.plain_key || created.key || created.api_key)) {
          setGeneratedKey({ ...created, key: String(created.plain_key || created.key || created.api_key) });
        }
        addToast(t('令牌创建成功'));
      }
      await load(viewAllRef.current);
      // 编辑保存后关闭弹窗；创建路径保持弹窗以展示一次性密钥
      if (editing) closeModal();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = (id: string | number) => {
    setConfirmState({
      title: t('删除令牌'),
      message: t('确定删除此令牌？使用该令牌的调用将立即失败。'),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteToken(id);
          addToast(t('令牌已删除'));
          await load(viewAllRef.current);
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  // 批量删除（对齐渠道页批量操作栏）
  const handleBulkDelete = (): void => {
    const ids = Array.from(selected);
    if (!ids.length) return;
    setConfirmState({
      title: t('批量删除令牌'),
      message: t('确定删除选中的 {{count}} 个令牌？使用这些令牌的调用将立即失败。', { count: ids.length }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        // 逐个删除，失败的计数反馈（原先全吞，部分失败仍报成功）
        let failed = 0;
        for (const id of ids) {
          try {
            await api.deleteToken(id);
          } catch {
            failed += 1;
          }
        }
        if (failed > 0) {
          addToast(t('已删除 {{n}} 个，{{m}} 个失败', { n: ids.length - failed, m: failed }), 'error');
        } else {
          addToast(t('已删除 {{count}} 个令牌', { count: ids.length }));
        }
        setSelected(new Set());
        await load(viewAllRef.current);
      },
    });
  };

  // 批量启停
  const handleBulkStatus = (status: 'active' | 'disabled'): void => {
    const ids = Array.from(selected);
    if (!ids.length) return;
    const disabling = status === 'disabled';
    const run = async (): Promise<void> => {
      // 逐条统计成败：原来把失败全部 `.catch(() => {})` 吞掉后无条件提示
      // 「已启用/禁用 N 个」，实际可能全失败 —— 用户会以为改成功了。
      let ok = 0;
      for (const id of ids) {
        try {
          await api.updateToken(id, { status });
          ok += 1;
        } catch {
          /* 计数后统一提示 */
        }
      }
      if (ok === ids.length) {
        addToast(disabling
          ? t('已禁用 {{count}} 个令牌', { count: ok })
          : t('已启用 {{count}} 个令牌', { count: ok }));
      } else {
        addToast(
          t('{{ok}} 个成功，{{fail}} 个失败', { ok, fail: ids.length - ok }),
          'error',
        );
      }
      setSelected(new Set());
      await load(viewAllRef.current);
    };
    if (disabling) {
      setConfirmState({
        title: t('批量禁用令牌'),
        message: t('确定禁用选中的 {{count}} 个令牌？使用这些令牌的调用将立即失败。', { count: ids.length }),
        confirmText: t('禁用'),
        danger: true,
        onConfirm: run,
      });
      return;
    }
    void run();
  };

  const handleToggleStatus = (tk: TokenItem) => {
    const disabling = tk.status !== 'disabled';
    const newStatus = disabling ? 'disabled' : 'active';
    // 禁用令牌立即影响生产调用，需要确认；启用方向可直接执行
    if (disabling) {
      setConfirmState({
        title: t('禁用令牌'),
        message: t('确定禁用此令牌？使用该令牌的调用将立即失败。'),
        confirmText: t('禁用'),
        danger: true,
        onConfirm: async () => {
          setError('');
          try {
            await api.updateToken(tk.id, { status: newStatus });
            addToast(t('已禁用'));
            await load(viewAllRef.current);
          } catch (err) {
            setError(err instanceof Error ? err.message : String(err));
          }
        },
      });
      return;
    }
    void (async () => {
      setError('');
      try {
        await api.updateToken(tk.id, { status: newStatus });
        addToast(t('已启用'));
        await load(viewAllRef.current);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    })();
  };

  const handleResetUsed = (id: string | number) => {
    setConfirmState({
      title: t('重置已用'),
      message: t('确定重置已用额度为 0？'),
      confirmText: t('重置已用'),
      danger: false,
      onConfirm: async () => {
        setError('');
        try {
          await api.resetTokenUsed(id);
          addToast(t('已重置已用额度'));
          await load(viewAllRef.current);
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  // 令牌轮换：生成新密钥，旧密钥立即失效；新密钥仅此一次展示
  const handleRotate = (tk: TokenItem) => {
    setConfirmState({
      title: t('轮换令牌密钥'),
      message: t('轮换将生成新密钥，旧密钥立即失效。确定继续？'),
      confirmText: t('轮换'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          const res = await api.rotateToken(tk.id);
          const data = (res?.data ?? {}) as unknown as GeneratedKeyState;
          // 后端返回新密钥明文（plain_key / key / api_key）
          const newKey = data.plain_key || data.key || data.api_key;
          if (newKey) {
            setRotatedKey({ name: tk.name, key: String(newKey) });
          }
          addToast(t('令牌已轮换，请立即保存新密钥'));
          await load(viewAllRef.current);
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const copyToClipboard = (text: string) => {
    // navigator.clipboard 仅在安全上下文（HTTPS/localhost）可用；AIGX 常以
    // http://IP:9527 部署，需降级 execCommand 方案，否则复制静默失败。
    const fallbackCopy = (): boolean => {
      try {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        const ok = document.execCommand('copy');
        document.body.removeChild(ta);
        return ok;
      } catch {
        return false;
      }
    };
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(() => {
        addToast(t('已复制到剪贴板'));
      }).catch(() => {
        if (!fallbackCopy()) addToast(t('复制失败，请手动选择复制'), 'error');
        else addToast(t('已复制到剪贴板'));
      });
    } else if (fallbackCopy()) {
      addToast(t('已复制到剪贴板'));
    } else {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  // 取回令牌明文：本地已有则直接用；否则请求后端（失败时提示）
  const fetchPlainKey = async (tk: TokenItem): Promise<string | null> => {
    const cached = plainKeys[tk.id] || tk.plain_key;
    if (cached) return cached;
    try {
      const res = await api.getTokenKey(tk.id);
      const key = res?.data?.plain_key || res?.data?.key;
      if (key) {
        setPlainKeys((prev) => ({ ...prev, [tk.id]: String(key) }));
        return String(key);
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : t('获取密钥失败'), 'error');
      return null;
    }
    return null;
  };

  // 复制按钮：new-api ApiKeyCell 风格 —— 复制成功后 Copy 图标短暂变绿勾
  const handleCopyKey = (tk: TokenItem): void => {
    void fetchPlainKey(tk).then((key) => {
      if (!key) return;
      copyToClipboard(key);
      setCopiedKeyId(tk.id);
      window.setTimeout(() => setCopiedKeyId(null), 1500);
    });
  };

  const isExpired = (tk: TokenItem): boolean => {
    if (!tk.expires_at) return false;
    return Number(tk.expires_at) < Math.floor(Date.now() / 1000);
  };

  // 相对时间（new-api ApiKeyTimestampCell：相对时间 + hover 绝对时间）
  const formatRelativeTime = (ts: number): string => {
    const diff = Date.now() - ts * 1000;
    const seconds = Math.floor(diff / 1000);
    const minutes = Math.floor(seconds / 60);
    const hours = Math.floor(minutes / 60);
    const days = Math.floor(hours / 24);
    if (seconds < 60) return t('刚刚');
    if (minutes < 60) return `${minutes}${t('分钟前')}`;
    if (hours < 24) return `${hours}${t('小时前')}`;
    if (days < 30) return `${days}${t('天前')}`;
    return new Date(ts * 1000).toLocaleDateString();
  };

  // 模型白名单数组（兼容字符串形式）
  const modelsOf = (tk: TokenItem): string[] => {
    if (Array.isArray(tk.allowed_models)) return tk.allowed_models;
    if (typeof tk.allowed_models === 'string' && tk.allowed_models.trim()) {
      return tk.allowed_models.split(',').map((s) => s.trim()).filter(Boolean);
    }
    return [];
  };

  if (loading) return <SkeletonTable columns={11} rows={6} />;

  return (
    <div>
      <div className="page-header">
        <h1>{t('API 密钥')}</h1>
        <p>{t('创建与管理 API 密钥，以及接入文档（网关地址、协议端点与可用模型）')}</p>
      </div>

      {/* 密钥 / 接入文档双 Tab（文档 2026-11-07 自独立页并入，07 E.3）：
          密钥与文档是同一件事的两半——拿 key → 怎么填，合并后少一个顶级菜单。
          文档 Tab 对所有登录用户可见（本页走 ProtectedLayout）；面板内部
          按角色分层（普通用户 Base URL 用站点 origin，管理员额外看监听口径）。 */}
      <Tabs<KeysTab>
        className="keys-tabs"
        ariaLabel={t('API 密钥')}
        items={[
          { key: 'keys', label: t('密钥管理') },
          { key: 'docs', label: t('接入文档') },
        ]}
        active={tab}
        onChange={changeTab}
      />

      <div hidden={tab !== 'keys'}>
        {error && <div className="error-message">{error}</div>}

        <Card
        title={`${viewAll ? t('全部令牌') : t('我的令牌')} (${tokens.length})`}
        actions={
          <div className="keys-toolbar">
            <div className="keys-search">
              <Search size={14} />
              <input
                ref={searchRef}
                placeholder={t('搜索令牌')}
                value={search}
                onChange={(e) => { setSearch(e.target.value); setPage(1); }}
              />
            </div>
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
                  defs={KEYS_COL_DEFS}
                  items={colState.items}
                  onChange={commitKeysCols}
                  onReset={resetKeysCols}
                  anchorRef={colPanelRef}
                />
              )}
            </div>
            {isAdmin() && (
              <Button variant="outline" size="sm" onClick={toggleViewAll}>
                {viewAll ? t('只看自己的') : t('查看全部用户')}
              </Button>
            )}
            <Button onClick={openCreate}>{t('+ 创建令牌')}</Button>
          </div>
        }
      >
        {tokens.length === 0 ? (
          <EmptyState message={t('暂无 API 令牌')} icon="🔑" action={<Button onClick={openCreate}>{t('创建第一个令牌')}</Button>} />
        ) : (
          <>
            {/* 批量操作栏（对齐渠道页） */}
            {selected.size > 0 && (
              <div className="keys-bulk">
                <span>{t('已选')} {selected.size} / {filtered.length}</span>
                <Button variant="outline" size="sm" onClick={() => handleBulkStatus('active')}>{t('启用')}</Button>
                <Button variant="outline" size="sm" onClick={() => handleBulkStatus('disabled')}>{t('禁用')}</Button>
                <Button variant="danger" size="sm" onClick={handleBulkDelete}>{t('删除')}</Button>
                <Button variant="outline" size="sm" onClick={() => setSelected(new Set())}>{t('取消选择')}</Button>
              </div>
            )}

            <div className="table-wrapper keys-table-wrap">
              <table className="keys-table">
                <colgroup>
                  {appliedCols.map((c) => (
                    <col key={c.key} data-col-key={c.key} style={c.width != null ? { width: `${c.width}px` } : undefined} />
                  ))}
                </colgroup>
                <thead>
                  <tr>
                    {appliedCols.map((c, idx) => {
                      const grip = colResize.gripProps(c.key, {
                        isLast: idx === appliedCols.length - 1,
                        locked: c.locked,
                        defaultWidth: c.width,
                      });
                      return (
                        <th
                          key={c.key}
                          className={`col-${c.key}${c.align === 'right' ? ' ta-right' : ''}`}
                          style={c.width != null ? { width: `${c.width}px` } : undefined}
                        >
                          {c.key === 'select' ? (
                            <input
                              type="checkbox"
                              checked={allSelected}
                              onChange={toggleAll}
                              aria-label={t('全选')}
                            />
                          ) : (
                            t(KEYS_COL_DEFS.find((d) => d.key === c.key)?.label ?? c.key)
                          )}
                          {c.key !== 'select' && c.key !== 'actions' && (
                            <span className="as-col-grip" title={t('拖动调整列宽 · 双击还原')} {...grip} />
                          )}
                        </th>
                      );
                    })}
                  </tr>
                </thead>
                <tbody>
                  {pageItems.map((tk, i) => {
                    const expired = isExpired(tk);
                    const disabled = tk.status === 'disabled' || tk.is_active === false;
                    const models = modelsOf(tk);
                    const used = Number(tk.used_quota || 0);
                    const limit = tk.quota_limit;
                    // 额度进度（new-api Quota Cell：剩余/总量 + 进度条着色）
                    const pct = limit ? Math.max(0, Math.min(100, ((limit - used) / limit) * 100)) : null;
                    const maskShown = tk.key || '••••••••••••';
                    return (
                      <tr
                        key={tk.id}
                        ref={(el) => { rowRefs.current[i] = el; }}
                        className={`${selected.has(tk.id) ? 'selected' : ''} ${activeIndex === i ? 'kbd-active' : ''}`}
                        onMouseEnter={() => setActiveIndex(i)}
                      >
                        <td className="col-select">
                          <input
                            type="checkbox"
                            checked={selected.has(tk.id)}
                            onChange={() => toggleOne(tk.id)}
                            aria-label={t('选择')}
                          />
                        </td>
{visibleKeySet.has('name') && (
                        <td className="col-name">
                          <div className="tk-name" title={tk.name}>{tk.name}</div>
                        </td>
                        )}
{/* 归属列在「只看自己」模式下恒为本人邮箱，是纯噪声——此处仍保持渲染
    以维持表头/单元格列数对齐（表头由 appliedCols 驱动，不受 viewAll 影响）；
    仅在空值时用 — 占位，视觉上不引入新信息。 */}
{visibleKeySet.has('user') && (
                        <td className="col-user">
                          {viewAll && tk.user_email
                            ? <span className="tk-user-email" title={tk.user_email}>{tk.user_email}</span>
                            : <span className="tk-user-none">—</span>}
                        </td>
                        )}
{visibleKeySet.has('status') && (
                        <td className="col-status">
                          <span className={`tk-status-badge ${expired ? 'warn' : disabled ? 'bad' : 'ok'}`}>
                            {expired ? t('已过期') : disabled ? t('禁用') : t('启用')}
                          </span>
                        </td>
                        )}
{visibleKeySet.has('key') && (
                        <td className="col-key">
                          <div className="tk-key-cell">
                            <code className="tk-key-code" title={revealedKeys[tk.id] ? (plainKeys[tk.id] || '') : maskShown}>
                              {revealedKeys[tk.id]
                                ? (plainKeys[tk.id] || tk.plain_key || t('（获取失败）'))
                                : maskShown}
                            </code>
                            <button
                              type="button"
                              className="tk-icon-btn"
                              title={revealedKeys[tk.id] ? t('隐藏密钥') : t('查看密钥')}
                              disabled={fetchingKeyId === tk.id}
                              onClick={() => {
                                if (!revealedKeys[tk.id] && !plainKeys[tk.id] && !tk.plain_key) {
                                  setFetchingKeyId(tk.id);
                                  void fetchPlainKey(tk).finally(() => setFetchingKeyId(null));
                                }
                                setRevealedKeys((prev) => ({ ...prev, [tk.id]: !prev[tk.id] }));
                              }}
                            >
                              {fetchingKeyId === tk.id
                                ? <Loader2 size={13} className="animate-spin" />
                                : (revealedKeys[tk.id] ? <EyeOff size={13} /> : <Eye size={13} />)}
                            </button>
                            <button
                              type="button"
                              className="tk-icon-btn"
                              title={copiedKeyId === tk.id ? t('已复制到剪贴板') : t('复制密钥')}
                              onClick={() => handleCopyKey(tk)}
                            >
                              {copiedKeyId === tk.id
                                ? <Check size={13} style={{ color: 'var(--success-color, #10b981)' }} />
                                : <Copy size={13} />}
                            </button>
                          </div>
                        </td>
                        )}
{visibleKeySet.has('quota') && (
                        <td className="col-quota">
                          {limit ? (
                            <div className="tk-quota" title={`${t('已用')} ${fmtQuota(used)} / ${t('上限')} ${fmtQuota(limit)}`}>
                              <div className="tk-quota-nums">
                                <span>{fmtQuota(used)}</span>
                                <span className="tk-quota-total">/ {fmtQuota(limit)}</span>
                              </div>
                              <div className="tk-quota-bar">
                                <div
                                  className={`tk-quota-fill ${pct !== null && pct <= 10 ? 'low' : pct !== null && pct <= 30 ? 'mid' : 'high'}`}
                                  style={{ width: `${pct ?? 0}%` }}
                                />
                              </div>
                            </div>
                          ) : (
                            <span className="tk-quota-free" title={`${t('已用')} ${fmtQuota(used)}`}>
                              {fmtQuota(used)} / ∞
                            </span>
                          )}
                        </td>
                        )}
{visibleKeySet.has('group') && (
                        <td className="col-group">
                          <span className="tk-group-badge">{tk.group || 'default'}</span>
                        </td>
                        )}
{visibleKeySet.has('models') && (
                        <td className="col-models">
                          {models.length === 0
                            ? <span className="tk-models-all">{t('全部')}</span>
                            : (
                              // 白名单过长只显示前 2 个 + "+N"，悬停 title 看完整列表
                              <div className="tk-models" title={models.join(', ')}>
                                {models.slice(0, 2).map((m) => (
                                  <span key={m} className="tk-model-badge" style={{ color: modelBadgeColor(m), borderColor: modelBadgeColor(m) }}>
                                    {m}
                                  </span>
                                ))}
                                {models.length > 2 && (
                                  <span className="tk-model-badge tk-models-more">
                                    +{models.length - 2}
                                  </span>
                                )}
                              </div>
                            )}
                        </td>
                        )}
{visibleKeySet.has('expires') && (
                        <td className="col-expires">
                          {tk.expires_at
                            ? (
                              <span
                                title={new Date(tk.expires_at * 1000).toLocaleString()}
                                style={expired ? { color: 'var(--danger-color, #ef4444)' } : undefined}
                              >
                                {new Date(tk.expires_at * 1000).toLocaleDateString()}
                              </span>
                            )
                            : <span className="tk-never">{t('永不过期')}</span>}
                        </td>
                        )}
{visibleKeySet.has('created') && (
                        <td className="col-created">
                          {tk.created_at
                            ? (
                              <span
                                title={new Date(tk.created_at > 1e12 ? tk.created_at : tk.created_at * 1000).toLocaleString()}
                              >
                                {formatRelativeTime(tk.created_at > 1e12 ? Math.floor(tk.created_at / 1000) : tk.created_at)}
                              </span>
                            )
                            : '—'}
                        </td>
                        )}
                        <td className="col-actions">
                          <div className="tk-actions">
                            <button
                              type="button"
                              className={`tk-icon-btn ${!disabled ? 'tk-danger-hover' : ''}`}
                              title={disabled ? t('启用') : t('禁用')}
                              onClick={() => handleToggleStatus(tk)}
                            >
                              {disabled ? <PowerOff size={15} /> : <Power size={15} />}
                            </button>
                            <button
                              type="button"
                              className="tk-icon-btn"
                              title={t('编辑令牌')}
                              onClick={() => openEdit(tk)}
                            >
                              <Pencil size={15} />
                            </button>
                            <div className="tk-row-menu">
                              <button
                                type="button"
                                className="tk-icon-btn"
                                title={t('更多操作')}
                                onClick={(e) => openRowMenu(e, tk.id)}
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

            {/* 客户端分页：页码导航 + 总数（全选/批量仍作用于过滤集，与分页前语义一致） */}
            <TablePaginationFooter
              total={filtered.length}
              page={safePage}
              totalPages={totalPages}
              size={pageSize}
              sizes={CLIENT_PAGE_SIZES}
              onSizeChange={(n) => { setPageSize(n); setPage(1); }}
              onPageChange={setPage}
            />

            {filtered.length === 0 && search && (
              <EmptyState message={t('无匹配结果')} icon="🔍" />
            )}
          </>
        )}
      </Card>
      </div>

      {/* 接入文档 Tab：角色分层见 KeysDocsPanel 头注（全员可见 Base URL/协议/
          模型清单；网关监听口径仅管理员） */}
      <div hidden={tab !== 'docs'}>
        <KeysDocsPanel />
      </div>

      {/* 行操作菜单（fixed 挂在 body 层级，逃出 Card/table 的 overflow 裁剪） */}
      {rowMenuId !== null && (() => {
        const tk = filtered.find((c) => c.id === rowMenuId);
        if (!tk) return null;
        return (
          <div
            className="tk-row-menu-panel tk-row-menu-fixed"
            style={{ top: rowMenuPos.top, left: rowMenuPos.left }}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
          >
            <button type="button" onClick={() => { setRowMenuId(null); handleCopyKey(tk); }}>
              <Copy size={14} />
              {t('复制密钥')}
            </button>
            {tk.quota_limit && (
              <button type="button" onClick={() => { setRowMenuId(null); handleResetUsed(tk.id); }}>
                <RefreshCcw size={14} />
                {t('重置已用')}
              </button>
            )}
            <button type="button" onClick={() => { setRowMenuId(null); handleRotate(tk); }}>
              <RotateCcw size={14} />
              {t('轮换')}
            </button>
            <div className="tk-row-menu-sep" />
            <button
              type="button"
              className="tk-row-menu-danger"
              onClick={() => { setRowMenuId(null); handleDelete(tk.id); }}
            >
              <Trash2 size={14} />
              {t('删除令牌')}
            </button>
          </div>
        );
      })()}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

      {/* 令牌轮换后新密钥展示模态框：新密钥仅此一次显示 */}
      {rotatedKey && (
        <div className="modal-overlay" onClick={() => setRotatedKey(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('令牌已轮换')} — {rotatedKey.name}</h3>
              <button type="button" className="modal-close" onClick={() => setRotatedKey(null)}>&times;</button>
            </div>
            <div className="modal-body">
              <div className="success-message">{t('轮换成功！请立即保存新密钥，此为最后一次显示。')}</div>
              <div className="form-group">
                <label>{t('新 API 密钥')}</label>
                <div className="generated-key-box">
                  <code className="generated-key">{rotatedKey.key}</code>
                </div>
                <p className="key-warning">{t('旧密钥已立即失效，请将新密钥保存到安全位置，关闭后将无法再次查看。')}</p>
              </div>
              <Button onClick={() => copyToClipboard(rotatedKey.key)} style={{ width: '100%' }}>
                {t('复制到剪贴板')}
              </Button>
            </div>
            <div className="modal-footer">
              <Button onClick={() => setRotatedKey(null)}>{t('我已保存')}</Button>
            </div>
          </div>
        </div>
      )}

      {showModal && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={handleSave}>
            <div className="modal-header">
              <h3>{editing ? t('编辑令牌') : t('创建 API 令牌')}</h3>
              <button type="button" className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              {generatedKey && !editing ? (
                <div>
                  <div className="success-message">{t('令牌创建成功！')}</div>
                  <div className="form-group">
                    <label>{t('您的 API 密钥')}</label>
                    <div className="generated-key-box">
                      <code className="generated-key">{generatedKey.key || generatedKey.api_key}</code>
                    </div>
                    <p className="key-warning">{t('请立即复制此密钥，关闭后将无法再次查看。')}</p>
                  </div>
                  <Button onClick={() => copyToClipboard(String(generatedKey.key || generatedKey.api_key))} style={{ width: '100%' }}>
                    {t('复制到剪贴板')}
                  </Button>
                </div>
              ) : (
                <>
                  <Input
                    label={`${t('名称')} *`}
                    placeholder={t('例如：开发环境令牌')}
                    value={form.name}
                    onChange={(e) => setForm({ ...form, name: e.target.value })}
                    autoFocus
                  />
                  {!editing && (
                    <Input
                      label={`${t('密钥值')} ${t('(可选，留空则随机生成 sk-...)')}`}
                      placeholder="sk-..."
                      value={form.custom_key}
                      onChange={(e) => setForm({ ...form, custom_key: e.target.value })}
                    />
                  )}
                  <Select
                    label={t('分组')}
                    value={form.group}
                    onChange={(e) => setForm({ ...form, group: e.target.value })}
                  >
                    <option value="default">default</option>
                    {groups.filter((g) => g.name && g.name !== 'default').map((g) => (
                      <option key={g.name} value={g.name}>{g.name}</option>
                    ))}
                  </Select>
                  {/* 模型白名单：候选来自网关可用模型（与聊天/运维页同源），
                      也允许自由填。空 = 不限制，与 Agent2API 口径一致。 */}
                  <div className="form-group">
                    <label>
                      {`${t('模型白名单')} ${t('(留空则允许全部)')}`}
                    </label>
                    <MultiSelect
                      value={form.allowed_models
                        .split(',')
                        .map((s) => s.trim())
                        .filter(Boolean)}
                      onChange={(list) =>
                        setForm({ ...form, allowed_models: list.join(', ') })
                      }
                      options={modelOptions}
                      placeholder={
                        editing
                          ? t('留空表示不修改')
                          : 'glm-5.2, deepseek-v3, kimi-k2.6'
                      }
                      emptyHint={t('暂无可用模型（先在模型管理里启用）')}
                    />
                  </div>
                  <Input
                    label={`${t('过期时间')} ${t('(留空则永不过期)')}`}
                    type="datetime-local"
                    placeholder={editing ? t('留空表示不修改') : ''}
                    value={form.expires_at}
                    onChange={(e) => setForm({ ...form, expires_at: e.target.value })}
                  />
                  <Input
                    label={`${t('额度上限')} ${t('(留空则无上限)')}`}
                    type="number"
                    placeholder={editing ? t('留空表示不修改') : t('keysPlaceholderQuotaLimit')}
                    value={form.quota_limit}
                    onChange={(e) => setForm({ ...form, quota_limit: e.target.value })}
                  />
                  <Input
                    label={`${t('IP 限制')} ${t('(逗号分隔，留空则不限制)')}`}
                    placeholder={editing ? t('留空表示不修改') : t('keysPlaceholderIpLimit')}
                    value={form.ip_limit}
                    onChange={(e) => setForm({ ...form, ip_limit: e.target.value })}
                  />
                  <Select
                    label={t('状态')}
                    value={form.status}
                    onChange={(e) => setForm({ ...form, status: e.target.value })}
                  >
                    <option value="active">{t('启用')}</option>
                    <option value="disabled">{t('禁用')}</option>
                  </Select>
                </>
              )}
            </div>
            <div className="modal-footer">
              {generatedKey && !editing ? (
                <Button onClick={closeModal}>{t('完成')}</Button>
              ) : (
                <>
                  <Button variant="outline" onClick={closeModal} disabled={saving}>{t('取消')}</Button>
                  <Button type="submit" disabled={saving}>
                    {saving ? t('保存中...') : (editing ? t('保存') : t('创建'))}
                  </Button>
                </>
              )}
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
