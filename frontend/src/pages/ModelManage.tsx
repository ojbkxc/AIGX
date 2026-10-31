import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Boxes, Search, Trash2, Plus, RefreshCw, Tag, Pencil } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Card, Loading, EmptyState, Badge, Switch, Pagination, Tabs } from '../components/ui';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { fmtCompact } from '../lib/utils';
import type { ManageView, ManagedModel, ModelMetaOverride } from '../types';
import './ModelManage.css';

/** 每页行数（模型目录动辄数百条，全量渲染卡顿） */
const PAGE_SIZE = 25;

/** 主 Tab */
type MainTab = 'catalog' | 'meta';

/** 启停筛选 */
type StateFilter = 'all' | 'on' | 'off';
/** 定价筛选 */
type PriceFilter = 'all' | 'priced' | 'unpriced';

const CAP_LABELS: Array<{ match: string; label: string }> = [
  { match: 'vision', label: '视觉' },
  { match: 'image', label: '图像' },
  { match: 'audio', label: '音频' },
  { match: 'tool', label: '工具' },
  { match: 'function', label: '函数' },
  { match: 'reasoning', label: '推理' },
  { match: 'embedding', label: '向量' },
];

function fmtPrice(n: number | null | undefined): string {
  if (n == null || n === 0) return '—';
  if (n < 0.01) return n.toFixed(4);
  if (n < 1) return n.toFixed(3);
  return n.toFixed(2);
}

/** 价格表单（字符串态，提交时解析） */
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

/** 别名表单 */
interface AliasForm {
  alias: string;
  target: string;
}

export default function ModelManage(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [view, setView] = useState<ManageView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  // 筛选
  const [provider, setProvider] = useState('all');
  const [stateFilter, setStateFilter] = useState<StateFilter>('all');
  const [priceFilter, setPriceFilter] = useState<PriceFilter>('all');
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(1);

  // 弹窗
  const [priceTarget, setPriceTarget] = useState<ManagedModel | null>(null);
  const [priceForm, setPriceForm] = useState<PriceForm>(EMPTY_PRICE_FORM);
  const [priceError, setPriceError] = useState('');
  const [savingPrice, setSavingPrice] = useState(false);

  const [aliasTarget, setAliasTarget] = useState<ManagedModel | null>(null);
  const [aliasForm, setAliasForm] = useState<AliasForm>({ alias: '', target: '' });
  const [aliasError, setAliasError] = useState('');
  const [savingAlias, setSavingAlias] = useState(false);

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  /** 正在切换启停的行（provider|id），防重复点击 */
  const [pendingRow, setPendingRow] = useState<string | null>(null);

  // ── 主 Tab ──
  const [tab, setTab] = useState<MainTab>('catalog');

  // ── 元信息覆盖（owned_by / 上下文长度 / 能力）──
  const [overrides, setOverrides] = useState<Record<string, ModelMetaOverride>>({});
  const [metaModal, setMetaModal] = useState(false);
  const [metaForm, setMetaForm] = useState({
    model: '',
    owned_by: '',
    context_length: '',
    capabilities: '',
  });
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
  const openAlias = (row: ManagedModel) => {
    setAliasTarget(row);
    setAliasError('');
    setAliasForm({ alias: '', target: row.id });
  };

  const saveAlias = async () => {
    setAliasError('');
    const alias = aliasForm.alias.trim();
    if (!alias) {
      setAliasError(t('别名不能为空'));
      return;
    }
    setSavingAlias(true);
    try {
      const res = await api.setModelAlias(alias, aliasForm.target.trim());
      acceptView(res?.data);
      addToast(t('别名已保存'));
      setAliasTarget(null);
    } catch (err) {
      setAliasError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingAlias(false);
    }
  };

  const removeAlias = (alias: string) => {
    setConfirmState({
      title: t('删除别名'),
      message: t('删除后客户端将无法再通过该别名调用') + `：${alias}`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        const res = await api.removeModelAlias(alias);
        acceptView(res?.data);
        addToast(t('已删除别名'));
      },
    });
  };

  // ── 元信息覆盖 ──
  const openMetaCreate = () => {
    setMetaForm({ model: '', owned_by: '', context_length: '', capabilities: '' });
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
      capabilities: (o.capabilities ?? []).join(', '),
    });
    setMetaError('');
    setMetaModal(true);
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
        capabilities: metaForm.capabilities
          .split(/[,，]/)
          .map((c) => c.trim())
          .filter(Boolean),
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

  // ── 过滤 + 分页 ──
  const filtered = useMemo(() => {
    const list = view?.models ?? [];
    const q = query.trim().toLowerCase();
    return list.filter((m) => {
      if (provider !== 'all' && m.provider !== provider) return false;
      if (stateFilter === 'on' && !m.enabled) return false;
      if (stateFilter === 'off' && m.enabled) return false;
      if (priceFilter === 'priced' && !m.price) return false;
      if (priceFilter === 'unpriced' && m.price) return false;
      if (!q) return true;
      const hay = [
        m.id,
        m.owned_by,
        m.provider_label,
        ...m.aliases,
        ...m.sources.map((s) => s.name),
      ]
        .join(' ')
        .toLowerCase();
      return hay.includes(q);
    });
  }, [view, provider, stateFilter, priceFilter, query]);

  useEffect(() => {
    setPage(1);
  }, [provider, stateFilter, priceFilter, query]);

  const totalPages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const currentPage = Math.min(page, totalPages);
  const pageRows = filtered.slice((currentPage - 1) * PAGE_SIZE, currentPage * PAGE_SIZE);

  const providerStats = useMemo(() => {
    const groups = view?.providers ?? [];
    return {
      total: groups.reduce((n, g) => n + g.total, 0),
      providers: groups.length,
      unpriced: view?.unpriced ?? 0,
    };
  }, [view]);

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('模型管理')}</h1>
          <p>
            {t(
              '渠道与账号池的模型统一在此管理：启停、定价、别名。渠道表单里不再重复配置模型。'
            )}
          </p>
        </div>
        <button
          className="btn btn-outline btn-sm"
          onClick={() => void load()}
          disabled={loading}
          style={{ gap: 6 }}
        >
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
        onChange={setTab}
        ariaLabel={t('模型管理')}
      />

      {tab === 'meta' ? (
        <Card
          title={t('元信息覆盖')}
          actions={
            <button className="btn btn-outline btn-sm" onClick={openMetaCreate} style={{ gap: 6 }}>
              <Plus size={13} />
              {t('新增覆盖')}
            </button>
          }
        >
          {Object.keys(overrides).length === 0 ? (
            <EmptyState
              icon="🏷️"
              message={t('暂无覆盖项。内置推断已覆盖常见模型，需要自定义归属/上下文/能力时在此添加')}
            />
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('模型')}</th>
                    <th>{t('归属方')}</th>
                    <th>{t('上下文长度')}</th>
                    <th>{t('能力')}</th>
                    <th style={{ width: 110 }}>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(overrides).map(([model, o]) => (
                    <tr key={model}>
                      <td><strong>{model}</strong></td>
                      <td>{o.owned_by}</td>
                      <td>{o.context_length ?? '—'}</td>
                      <td>
                        <div className="mm-caps">
                          {(o.capabilities ?? []).map((c) => (
                            <Badge key={c} tone="neutral">{c}</Badge>
                          ))}
                        </div>
                      </td>
                      <td>
                        <div className="mm-actions">
                          <button
                            type="button"
                            className="btn btn-outline btn-sm"
                            onClick={() => openMetaEdit(model)}
                            title={t('编辑')}
                          >
                            <Pencil size={12} />
                          </button>
                          <button
                            type="button"
                            className="btn btn-danger btn-sm"
                            onClick={() => removeMeta(model)}
                            title={t('删除')}
                          >
                            <Trash2 size={12} />
                          </button>
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      ) : (
      <div className="mm-layout">
        {/* 左栏：provider 分组 */}
        <nav className="mm-rail" aria-label={t('提供商分组')}>
          <div className="mm-rail-title">{t('提供商')}</div>
          <button
            type="button"
            className={`mm-rail-item${provider === 'all' ? ' active' : ''}`}
            onClick={() => setProvider('all')}
          >
            <Boxes size={14} />
            <span className="mm-rail-name">{t('全部')}</span>
            <span className="mm-rail-count">{providerStats.total}</span>
          </button>
          {(view?.providers ?? []).map((g) => (
            <button
              key={g.id}
              type="button"
              className={`mm-rail-item${provider === g.id ? ' active' : ''}`}
              onClick={() => setProvider(g.id)}
              title={`${g.label} (${g.enabled}/${g.total})`}
            >
              <span className={`mm-rail-dot${g.account ? ' account' : ''}`} />
              <span className="mm-rail-name">{g.label}</span>
              <span className="mm-rail-count">
                {g.enabled}/{g.total}
              </span>
            </button>
          ))}
        </nav>

        {/* 右侧：表格 */}
        <div className="mm-main">
          <Card bodyClassName="">
            <div className="mm-toolbar">
              <div className="models-search" style={{ flex: 1, minWidth: 180 }}>
                <Search size={14} className="models-search-icon" />
                <input
                  className="form-input"
                  placeholder={t('搜索模型 / 别名 / 渠道 / 账号…')}
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  aria-label={t('搜索模型')}
                />
              </div>
              <select
                className="form-input"
                value={stateFilter}
                onChange={(e) => setStateFilter(e.target.value as StateFilter)}
                aria-label={t('启停筛选')}
                style={{ maxWidth: 120 }}
              >
                <option value="all">{t('全部状态')}</option>
                <option value="on">{t('已启用')}</option>
                <option value="off">{t('已禁用')}</option>
              </select>
              <select
                className="form-input"
                value={priceFilter}
                onChange={(e) => setPriceFilter(e.target.value as PriceFilter)}
                aria-label={t('定价筛选')}
                style={{ maxWidth: 120 }}
              >
                <option value="all">{t('全部定价')}</option>
                <option value="priced">{t('已定价')}</option>
                <option value="unpriced">{t('未定价')}</option>
              </select>
              <span className="mm-summary">
                {t('显示')} {filtered.length} / {providerStats.total} ·{' '}
                {t('未定价')} {providerStats.unpriced}
              </span>
            </div>

            {loading ? (
              <Loading text={t('加载模型目录')} />
            ) : filtered.length === 0 ? (
              <EmptyState
                icon="📦"
                message={
                  query || stateFilter !== 'all' || priceFilter !== 'all' || provider !== 'all'
                    ? t('没有匹配的模型')
                    : t('暂无模型。请先在渠道管理或账号池里配置上游。')
                }
              />
            ) : (
              <>
                <table className="mm-table">
                  <thead>
                    <tr>
                      <th style={{ width: 52 }}>{t('启用')}</th>
                      <th>{t('模型')}</th>
                      <th>{t('来源')}</th>
                      <th style={{ width: 150 }}>{t('价格')}</th>
                      <th style={{ width: 130 }}>{t('能力')}</th>
                      <th style={{ width: 170 }}>{t('别名')}</th>
                      <th style={{ width: 150 }}>{t('操作')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {pageRows.map((row) => {
                      const key = `${row.provider}|${row.id}`;
                      return (
                        <tr key={key}>
                          <td>
                            <Switch
                              checked={row.enabled}
                              onChange={(next) => void handleToggle(row, next)}
                              disabled={pendingRow === key}
                              ariaLabel={`${row.id} ${t('启停')}`}
                            />
                          </td>
                          <td>
                            <div className="mm-model-id">{row.id}</div>
                            <div className="mm-model-sub">
                              {row.provider_label}
                              {row.context_length
                                ? ` · ${t('上下文')} ${fmtCompact(row.context_length)}`
                                : ''}
                            </div>
                          </td>
                          <td>
                            <div className="mm-sources">
                              {row.sources.map((s, i) => (
                                <span
                                  key={`${s.kind}-${s.channel_id}-${s.account_id ?? ''}-${i}`}
                                  className={`mm-source-chip${s.kind === 'account' ? ' account' : ''}`}
                                  title={`${s.kind === 'account' ? t('账号') : t('渠道')}：${s.name}`}
                                >
                                  {s.kind === 'account' ? '👤' : '🔗'} {s.name}
                                </span>
                              ))}
                            </div>
                          </td>
                          <td>
                            {row.price ? (
                              <span className="mm-price">
                                {row.price.price_type === 'count'
                                  ? `${t('¥/次')} ${fmtPrice(row.price.input_price)}`
                                  : `${fmtPrice(row.price.input_price)} / ${fmtPrice(
                                      row.price.output_price
                                    )}`}
                                {row.model_ratio !== 1 && (
                                  <span className="mm-price-ratio">
                                    {t('倍率')} {row.model_ratio}
                                  </span>
                                )}
                              </span>
                            ) : (
                              <span className="mm-price-empty">{t('未定价')}</span>
                            )}
                          </td>
                          <td>
                            <div className="mm-caps">
                              {CAP_LABELS.filter((c) =>
                                (row.capabilities ?? []).some((cap) =>
                                  cap.toLowerCase().includes(c.match)
                                )
                              ).map((c) => (
                                <Badge key={c.match} tone="neutral">
                                  {c.label}
                                </Badge>
                              ))}
                            </div>
                          </td>
                          <td>
                            <div className="mm-sources">
                              {row.aliases.map((a) => (
                                <span key={a} className="mm-source-chip">
                                  {a}
                                  <button
                                    type="button"
                                    className="btn btn-outline btn-sm"
                                    style={{ padding: '0 4px', height: 18 }}
                                    title={t('删除别名')}
                                    onClick={() => removeAlias(a)}
                                  >
                                    ×
                                  </button>
                                </span>
                              ))}
                              <button
                                type="button"
                                className="btn btn-outline btn-sm"
                                style={{ padding: '0 6px', height: 22 }}
                                onClick={() => openAlias(row)}
                                title={t('新增别名')}
                              >
                                <Plus size={11} />
                              </button>
                            </div>
                          </td>
                          <td>
                            <div className="mm-actions">
                              <button
                                type="button"
                                className="btn btn-outline btn-sm"
                                onClick={() => openPrice(row)}
                                title={t('编辑定价')}
                              >
                                <Tag size={12} /> {t('定价')}
                              </button>
                              {row.price && (
                                <button
                                  type="button"
                                  className="btn btn-danger btn-sm"
                                  onClick={() => removePrice(row)}
                                  title={t('删除定价')}
                                >
                                  <Trash2 size={12} />
                                </button>
                              )}
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
                <div className="mm-pagination">
                  <Pagination
                    page={currentPage}
                    totalPages={totalPages}
                    onChange={setPage}
                  />
                </div>
              </>
            )}
          </Card>
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
                <label>{t('上下文长度（可选）')}</label>
                <input
                  className="form-input"
                  value={metaForm.context_length}
                  onChange={(e) => setMetaForm({ ...metaForm, context_length: e.target.value })}
                  placeholder="128000"
                  disabled={savingMeta}
                />
              </div>
              <div className="form-group">
                <label>{t('能力标签（逗号分隔，可选）')}</label>
                <input
                  className="form-input"
                  value={metaForm.capabilities}
                  onChange={(e) => setMetaForm({ ...metaForm, capabilities: e.target.value })}
                  placeholder="chat, vision"
                  disabled={savingMeta}
                />
              </div>
            </div>
            <div className="modal-footer">
              <button
                className="btn btn-outline"
                onClick={() => setMetaModal(false)}
                disabled={savingMeta}
              >
                {t('取消')}
              </button>
              <button
                className="btn btn-primary"
                onClick={() => void saveMeta()}
                disabled={savingMeta}
              >
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
                  onChange={(e) =>
                    setPriceForm({ ...priceForm, price_type: e.target.value })
                  }
                  disabled={savingPrice}
                >
                  <option value="token">{t('按 token 计价')}</option>
                  <option value="count">{t('按次计价')}</option>
                </select>
              </div>
              <div className="form-group">
                <label>
                  {priceForm.price_type === 'count'
                    ? t('每次价格')
                    : t('输入价格（每 1k token）')}
                </label>
                <input
                  className="form-input"
                  value={priceForm.input_price}
                  onChange={(e) =>
                    setPriceForm({ ...priceForm, input_price: e.target.value })
                  }
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
                      onChange={(e) =>
                        setPriceForm({ ...priceForm, output_price: e.target.value })
                      }
                      placeholder="0.0015"
                      disabled={savingPrice}
                    />
                  </div>
                  <div className="form-group">
                    <label>{t('缓存价格（可选）')}</label>
                    <input
                      className="form-input"
                      value={priceForm.cache_price}
                      onChange={(e) =>
                        setPriceForm({ ...priceForm, cache_price: e.target.value })
                      }
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
                  onChange={(e) =>
                    setPriceForm({ ...priceForm, model_ratio: e.target.value })
                  }
                  placeholder="1.0"
                  disabled={savingPrice}
                />
              </div>
            </div>
            <div className="modal-footer">
              <button
                className="btn btn-outline"
                onClick={() => setPriceTarget(null)}
                disabled={savingPrice}
              >
                {t('取消')}
              </button>
              <button
                className="btn btn-primary"
                onClick={() => void savePrice()}
                disabled={savingPrice}
              >
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
              <h3>{t('新增别名')}</h3>
              <button className="modal-close" onClick={() => setAliasTarget(null)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {aliasError && <div className="error-message">{aliasError}</div>}
              <div className="form-group">
                <label>{t('别名（客户端调用的名字）')} *</label>
                <input
                  className="form-input"
                  value={aliasForm.alias}
                  onChange={(e) => setAliasForm({ ...aliasForm, alias: e.target.value })}
                  placeholder="gpt-4o"
                  disabled={savingAlias}
                />
              </div>
              <div className="form-group">
                <label>{t('目标模型')} *</label>
                <input
                  className="form-input"
                  value={aliasForm.target}
                  onChange={(e) => setAliasForm({ ...aliasForm, target: e.target.value })}
                  disabled={savingAlias}
                />
              </div>
              <p style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                {t('别名是全局映射：客户端请求别名时，网关按映射改名为目标模型再转发。')}
              </p>
            </div>
            <div className="modal-footer">
              <button
                className="btn btn-outline"
                onClick={() => setAliasTarget(null)}
                disabled={savingAlias}
              >
                {t('取消')}
              </button>
              <button
                className="btn btn-primary"
                onClick={() => void saveAlias()}
                disabled={savingAlias}
              >
                {savingAlias ? t('保存中...') : t('保存')}
              </button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
