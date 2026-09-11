import { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Eye, EyeOff, ArrowLeftRight, MoreHorizontal, Search,
  Pencil, Gauge, Power, PowerOff, Loader2, MessageSquare, RotateCcw, Trash2,
  DollarSign, Cloud, Sparkles, Gem, BrainCircuit,
} from 'lucide-react';
import { api } from '../api';
import type { ChannelItem as ApiChannelItem } from '../types';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import ChatDebugger from '../components/ChatDebugger';
import ModelMappingEditor from '../components/ModelMappingEditor';
import CostPricingEditor from '../components/CostPricingEditor';
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
}

interface ChannelFormState {
  name: string;
  channel_type: string;
  base_url: string;
  api_key: string;
  priority: number | string;
  weight: number | string;
  status: string;
  models: string;
  account_id: string;
  model_mapping: Record<string, string>;
  cost_pricing: Record<string, { input_price?: number; output_price?: number; price_type?: string }>;
}

// 渠道类型选项 — 与后端 ChannelType 枚举对齐（snake_case）
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

export default function Channels(): JSX.Element {
  const [channels, setChannels] = useState<ChannelItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [showModal, setShowModal] = useState(false);
  const [editChannel, setEditChannel] = useState<ChannelItem | null>(null);
  const [form, setForm] = useState<ChannelFormState>(defaultForm());
  const [saving, setSaving] = useState(false);
  const [testingId, setTestingId] = useState<string | number | null>(null);
  const [fetchingModels, setFetchingModels] = useState(false);
  // API Key 输入明文切换（防输错无法核对）
  const [showApiKey, setShowApiKey] = useState(false);

  // ── 确认弹窗状态 ──
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // ── 对话调试器状态 ──
  const [showChat, setShowChat] = useState(false);
  const [chatChannel, setChatChannel] = useState<ChannelItem | null>(null);

  // ── 批量操作 / 搜索 / "更多"菜单 ──
  const [selected, setSelected] = useState<Set<string | number>>(new Set());
  const [search, setSearch] = useState('');
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

  // ── 分页（服务端分页：page/pageSize/total）──
  const [page, setPage] = useState(1);
  const [total, setTotal] = useState(0);
  const pageSize = 20;

  function defaultForm(): ChannelFormState {
    return {
      name: '',
      channel_type: 'openai_compatible',
      base_url: '',
      api_key: '',
      priority: 0,
      weight: 1,
      status: 'enabled',
      models: '',
      account_id: '',
      model_mapping: {},
      cost_pricing: {},
    };
  }

  // 挂载时加载一次渠道列表；分页翻页时重新拉取（搜索由服务端过滤）
  useEffect(() => {
    loadChannels().catch((err: unknown) => {
      setError(err instanceof Error ? err.message : String(err));
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [page]);

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

  // 过滤后的渠道列表（服务端搜索已过滤；此处仅兜底前端二次过滤——旧后端兼容）
  const filtered = channels.filter((ch) => {
    if (!search.trim()) return true;
    const q = search.toLowerCase();
    return (
      ch.name.toLowerCase().includes(q) ||
      ch.channel_type.toLowerCase().includes(q) ||
      (ch.base_url || '').toLowerCase().includes(q) ||
      (ch.models || []).some((m) => m.toLowerCase().includes(q))
    );
  });

  // 总页数（服务端 total；旧后端无 total 时退化用当前页条数）
  const totalPages = Math.max(1, Math.ceil(total / pageSize));

  // 全选/反选（仅当前过滤结果）
  const allSelected = filtered.length > 0 && filtered.every((ch) => selected.has(ch.id));
  const toggleAll = (): void => {
    if (allSelected) {
      setSelected(new Set());
    } else {
      setSelected(new Set(filtered.map((ch) => ch.id)));
    }
  };
  const toggleOne = (id: string | number): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  // 批量启用/禁用/删除
  const handleBulkEnable = async (): Promise<void> => {
    const ids = Array.from(selected);
    if (!ids.length) return;
    for (const id of ids) {
      await api.patchChannel(id, { status: 'enabled' }).catch(() => {});
    }
    addToast(`${t('已启用')} ${ids.length} ${t('个渠道')}`);
    setSelected(new Set());
    loadChannels();
  };
  const handleBulkDisable = (): void => {
    const ids = Array.from(selected);
    if (!ids.length) return;
    setConfirmState({
      title: t('批量停用渠道'),
      message: t('确定停用选中的 {{count}} 个渠道？', { count: ids.length }),
      confirmText: t('停用'),
      danger: true,
      onConfirm: async () => {
        for (const id of ids) {
          await api.patchChannel(id, { status: 'disabled' }).catch(() => {});
        }
        addToast(`${t('已停用')} ${ids.length} ${t('个渠道')}`);
        setSelected(new Set());
        loadChannels();
      },
    });
  };
  const handleBulkDelete = (): void => {
    const ids = Array.from(selected);
    if (!ids.length) return;
    setConfirmState({
      title: t('批量删除渠道'),
      message: t('确定删除选中的 {{count}} 个渠道？此操作不可撤销。', { count: ids.length }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        for (const id of ids) {
          await api.deleteChannel(id).catch(() => {});
        }
        addToast(`${t('已删除')} ${ids.length} ${t('个渠道')}`);
        setSelected(new Set());
        loadChannels();
      },
    });
  };

  // "更多"菜单项
  const handleTestAll = async (): Promise<void> => {
    setMoreOpen(false);
    for (const ch of channels) {
      handleTest(ch.id);
    }
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
      onConfirm: async () => {
        for (const ch of disabled) {
          await api.deleteChannel(ch.id).catch(() => {});
        }
        addToast(`${t('已删除')} ${disabled.length} ${t('个渠道')}`);
        loadChannels();
      },
    });
  };

  const loadChannels = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listChannels({ search, page, pageSize });
      const items: ChannelItem[] = (res?.data ?? []).map((ch: ApiChannelItem) => ({
        id: ch.id as string | number,
        seq: ch.seq,
        name: ch.name,
        channel_type: ch.channel_type || ch.type || 'openai_compatible',
        base_url: ch.base_url,
        api_key: ch.api_key,
        priority: ch.priority ?? 0,
        weight: ch.weight ?? 1,
        status: ch.status || (ch.enabled ? 'enabled' : 'disabled'),
        models: ch.models,
        model_mapping: ch.model_mapping,
        cost_pricing: ch.cost_pricing || {},
        last_used_at: typeof ch.last_used_at === 'number' ? ch.last_used_at : null,
        // 后端 Option 序列化为 null（非缺省），?? 归一为 undefined，
        // 否则 null !== undefined 判断穿透 → null.toFixed() 运行时崩溃
        response_time: ch.response_time ?? undefined,
        test_time: ch.test_time ?? undefined,
        balance: ch.balance ?? undefined,
        credit: ch.credit ?? undefined,
        created_at: typeof ch.created_at === 'number' ? ch.created_at : undefined,
        updated_at: typeof ch.updated_at === 'number' ? ch.updated_at : undefined,
      }));
      setChannels(items);
      setTotal(typeof (res as { total?: number }).total === 'number' ? (res as { total: number }).total : items.length);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const openAdd = (): void => {
    setEditChannel(null);
    setForm(defaultForm());
    setShowModal(true);
  };

  const openEdit = (ch: ChannelItem): void => {
    setEditChannel(ch);
    setForm({
      name: ch.name || '',
      channel_type: ch.channel_type || 'openai_compatible',
      base_url: ch.base_url || '',
      api_key: '',
      priority: ch.priority ?? 0,
      weight: ch.weight ?? 1,
      status: ch.status || 'enabled',
      models: (ch.models || []).join(', '),
      account_id: ch.account_id || '',
      model_mapping: ch.model_mapping || {},
      cost_pricing: ch.cost_pricing || {},
    });
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
  const buildPayload = () => ({
    name: form.name,
    channel_type: form.channel_type,
    base_url: form.base_url,
    api_key: form.api_key,
    priority: parseInt(String(form.priority), 10) || 0,
    weight: parseInt(String(form.weight), 10) || 1,
    status: form.status,
    models: form.models.split(',').map((s) => s.trim()).filter(Boolean),
    account_id: form.account_id,
    model_mapping: form.model_mapping,
    cost_pricing: form.cost_pricing,
  });

  const handleSave = async (): Promise<void> => {
    if (!form.name) { setError(t('名称为必填项')); return; }
    if (form.channel_type !== 'cloudflare' && !form.base_url) {
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
        if (!form.api_key) { setError(t('新渠道必填 API Key')); setSaving(false); return; }
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

  // 拉取上游模型列表 — 后端代理转发（避免浏览器 CORS）
  const handleFetchModels = async (): Promise<void> => {
    if (form.channel_type !== 'cloudflare' && !form.base_url.trim()) {
      setError(t('请先填写 Base URL'));
      return;
    }
    setFetchingModels(true);
    setError('');
    try {
      const res = await api.fetchChannelModels({
        channel_type: form.channel_type,
        base_url: form.base_url,
        api_key: form.api_key,
        channel_id: editChannel ? editChannel.id : '',
      });
      const models = res.data?.models || [];
      if (models.length === 0) {
        addToast(t('未拉取到模型（上游未返回模型列表）'));
      } else {
        setForm((f) => ({ ...f, models: models.join(', ') }));
        addToast(`${t('已拉取')} ${models.length} ${t('个模型')}`);
      }
    } catch (err) {
      addToast(`${t('拉取失败')}: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setFetchingModels(false);
    }
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
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteChannel(id);
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
        addToast(t('余额已更新'));
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
  const getBalanceVariant = (v: number | undefined): string => {
    if (v === undefined) return 'neutral';
    if (v < 0) return 'danger';
    if (v < 5) return 'warning';
    return 'success';
  };

  if (loading) return <SkeletonTable columns={6} rows={6} />;

  return (
    <div>
      {/* PageIntro 标题区 */}
      <div className="page-header">
        <h1>{t('渠道管理')}</h1>
        <p>{t('管理上游 AI 渠道（支持混用 Cloudflare + 第三方 OpenAI 兼容上游）')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Card
        title={`${t('所有渠道')} (${total})`}
        actions={
          <div className="channels-toolbar">
            {/* 搜索框（回车触发服务端搜索） */}
            <div className="channels-search">
              <Search size={14} />
              <input
                placeholder={t('搜索渠道')}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    setPage(1);
                    void loadChannels();
                  }
                }}
              />
            </div>

            {/* "更多"下拉 */}
            <div className="channels-more" ref={moreRef}>
              <Button variant="outline" onClick={() => setMoreOpen((v) => !v)}>
                <MoreHorizontal size={16} />
              </Button>
              {moreOpen && (
                <div className="channels-more-menu">
                  <button type="button" onClick={handleTestAll}>{t('测试所有渠道')}</button>
                  <button type="button" onClick={handleDeleteAllDisabled}>{t('删除所有已禁用渠道')}</button>
                </div>
              )}
            </div>

            <Button onClick={openAdd}>{t('+ 添加渠道')}</Button>
          </div>
        }
      >
        {channels.length === 0 ? (
          <EmptyState
            message={t('暂无渠道')}
            icon="🛰️"
            action={<Button onClick={openAdd}>{t('添加第一个渠道')}</Button>}
          />
        ) : (
          <>
            {/* 批量操作栏 */}
            {selected.size > 0 && (
              <div className="channels-bulk">
                <span>{t('已选')} {selected.size} / {filtered.length}</span>
                <Button variant="outline" size="sm" onClick={handleBulkEnable}>{t('启用')}</Button>
                <Button variant="outline" size="sm" onClick={handleBulkDisable}>{t('停用')}</Button>
                <Button variant="danger" size="sm" onClick={handleBulkDelete}>{t('删除')}</Button>
                <Button variant="outline" size="sm" onClick={() => setSelected(new Set())}>{t('取消选择')}</Button>
              </div>
            )}

            <div className="table-wrapper channels-table-wrap">
              <table className="channels-table">
                <thead>
                  <tr>
                    <th className="col-select" style={{ width: 36 }}>
                      <input
                        type="checkbox"
                        checked={allSelected}
                        onChange={toggleAll}
                        aria-label={t('全选')}
                      />
                    </th>
                    <th className="col-id">ID</th>
                    <th className="col-name">{t('名称')}</th>
                    <th className="col-type">{t('类型')}</th>
                    <th className="col-status">{t('状态')}</th>
                    <th className="col-models">{t('模型')}</th>
                    <th className="col-priority">{t('优先级')}</th>
                    <th className="col-weight">{t('权重')}</th>
                    <th className="col-balance">{t('余额')}</th>
                    <th className="col-response">{t('响应')}</th>
                    <th className="col-testtime">{t('测试时间')}</th>
                    <th className="col-actions">{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {filtered.map((ch) => {
                    const isEditingPriority = editingField?.id === ch.id && editingField?.field === 'priority';
                    const isEditingWeight = editingField?.id === ch.id && editingField?.field === 'weight';
                    const mappingEntries = Object.entries(ch.model_mapping || {});
                    const TypeIcon = TYPE_ICONS[ch.channel_type] || Sparkles;
                    const models = ch.models || [];

                    return (
                      <tr key={ch.id} className={selected.has(ch.id) ? 'selected' : ''}>
                        <td className="col-select">
                          <input
                            type="checkbox"
                            checked={selected.has(ch.id)}
                            onChange={() => toggleOne(ch.id)}
                            aria-label={t('选择')}
                          />
                        </td>
                        <td className="col-id" title={String(ch.id)}>
                          {ch.seq ?? String(ch.id).slice(0, 8)}
                        </td>
                        <td className="col-name">
                          <div className="ch-name" title={ch.name}>{ch.name}</div>
                          {(ch.base_url || ch.account_id) && (
                            <div className="ch-sub" title={ch.base_url || ch.account_id}>
                              {ch.base_url || ch.account_id}
                            </div>
                          )}
                        </td>
                        <td className="col-type">
                          <span className="ch-type-badge">
                            <TypeIcon size={12} />
                            {typeLabel(ch.channel_type)}
                          </span>
                        </td>
                        <td className="col-status">
                          <span
                            className={`ch-status-badge ${ch.status === 'enabled' ? 'ok' : 'bad'}`}
                            title={ch.last_error || undefined}
                          >
                            {ch.status === 'enabled' ? t('启用') : t('禁用')}
                          </span>
                        </td>
                        <td className="col-models">
                          {models.length === 0
                            ? <span className="ch-models-all">{t('全部')}</span>
                            : (
                              <div className="ch-models">
                                {models.slice(0, 3).map((m) => (
                                  <span key={m} className="ch-model-badge" style={{ color: modelBadgeColor(m), borderColor: modelBadgeColor(m) }}>
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
                        <td className="col-priority">
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
                        <td className="col-weight">
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
                        <td className="col-balance">
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
                                <span className={`ch-balance-badge v-${getBalanceVariant(ch.balance)}`}>
                                  {ch.balance !== undefined ? `$${ch.balance.toFixed(2)}` : '—'}
                                </span>
                                {ch.credit !== undefined && (
                                  <span className={`ch-balance-badge v-neutral ch-balance-credit`}>
                                    ${ch.credit.toFixed(2)}
                                  </span>
                                )}
                              </>
                            )}
                          </button>
                        </td>
                        <td className="col-response">
                          {ch.response_time !== undefined ? (
                            <span className={`response-time-badge ${ch.response_time < 500 ? 'fast' : ch.response_time < 2000 ? 'medium' : 'slow'}`}>
                              {ch.response_time}ms
                            </span>
                          ) : '—'}
                        </td>
                        <td className="col-testtime">
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
            {totalPages > 1 && (
              <Pagination page={page} totalPages={totalPages} onChange={(p) => setPage(p)} />
            )}
          </>
        )}
      </Card>

      {/* 行操作菜单（fixed 挂在 body 层级，逃出 Card/table 的 overflow 裁剪） */}
      {rowMenuId !== null && (() => {
        const ch = filtered.find((c) => c.id === rowMenuId);
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
          <form className="modal" onSubmit={(e) => { e.preventDefault(); void handleSave(); }}>
            <div className="modal-header">
              <h3>{editChannel ? t('编辑渠道') : t('添加渠道')}</h3>
              <button type="button" className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
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
                  hint={t('未带 /v1 时会自动补齐；例如 cf-ai-gw 填 https://cf-ai-gw.pages.dev 即可')}
                />
              )}
              <div className="form-group">
                <label>API Key {editChannel && t('（留空则保持不变）')}</label>
                <div className="password-input-wrap">
                  <input
                    className="form-input password-input"
                    type={showApiKey ? 'text' : 'password'}
                    placeholder={editChannel ? t('留空保持当前值') : 'API Key'}
                    value={form.api_key}
                    onChange={(e) => setForm({ ...form, api_key: e.target.value })}
                  />
                  <button
                    type="button"
                    className="password-input-toggle"
                    tabIndex={-1}
                    onClick={() => setShowApiKey((v) => !v)}
                    aria-label={showApiKey ? t('隐藏') : t('显示')}
                    title={showApiKey ? t('隐藏') : t('显示')}
                  >
                    {showApiKey ? <EyeOff size={14} /> : <Eye size={14} />}
                  </button>
                </div>
              </div>
              <div className="form-group">
                <label>{t('支持的模型（逗号分隔，留空=全部）')}</label>
                <div style={{ display: 'flex', gap: 8 }}>
                  <Input
                    placeholder="Workspace_2B8939-chat, Workspace_2B8939-coder"
                    value={form.models}
                    onChange={(e) => setForm({ ...form, models: e.target.value })}
                    style={{ flex: 1 }}
                  />
                  <Button
                    type="button"
                    variant="outline"
                    onClick={handleFetchModels}
                    disabled={fetchingModels}
                    title={t('从上游拉取模型列表')}
                    style={{ whiteSpace: 'nowrap', flexShrink: 0 }}
                  >
                    {fetchingModels ? t('拉取中...') : t('拉取模型')}
                  </Button>
                </div>
              </div>
              <div className="form-group">
                <label>{t('模型映射')}</label>
                <div className="form-hint" style={{ marginBottom: 8 }}>
                  {t('用户请求的模型名 → 实际转发给上游的模型名。优先级：渠道级映射 > 全局映射 > 原名透传。')}
                </div>
                <ModelMappingEditor
                  value={form.model_mapping}
                  onChange={(mapping) => setForm((f) => ({ ...f, model_mapping: mapping }))}
                  sourceModelOptions={form.models.split(',').map((s) => s.trim()).filter(Boolean)}
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
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={closeModal} disabled={saving}>{t('取消')}</Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('保存中...') : (editChannel ? t('更新') : t('添加'))}
              </Button>
            </div>
          </form>
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
                channelModels={chatChannel.models || []}
                initialProtocol={chatChannel.channel_type === 'anthropic' ? 'anthropic' : 'openai'}
                compact
              />
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
