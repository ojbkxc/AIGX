import { useState, useEffect, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Pagination } from '../components/ui';
import './Pricing.css';

/** 客户端分页每页条数（价格目录 426+ 条，全量渲染过长） */
const PRICE_PAGE_SIZE = 20;

type SubTabKey = 'prices' | 'ratios';

/** 渠道启用状态筛选：all=全部 / enabled=仅启用渠道的模型 / unknown=不在任何渠道里的模型 */
type ChannelFilter = 'all' | 'enabled' | 'unknown';

interface PriceEntry {
  model_name: string;
  input_price: number;
  output_price: number;
  cache_price?: number;
  price_type?: string;
}

interface PriceForm {
  model_name: string;
  input_price: string;
  output_price: string;
  cache_price: string;
  price_type: string;
}

interface RatiosState {
  model_ratio: Record<string, number>;
  group_ratio: Record<string, number>;
}

// 子标签定义 — 参照 deepseek-pp SUB_TABS 模式
const SUB_TABS: Array<{ key: SubTabKey; labelKey: string }> = [
  { key: 'prices', labelKey: '价格目录' },
  { key: 'ratios', labelKey: '倍率配置' },
];

const EMPTY_FORM: PriceForm = { model_name: '', input_price: '', output_price: '', cache_price: '', price_type: 'token' };

export default function Pricing() {
  const { t } = useTranslation();
  const addToast = useToast();
  const [sub, setSub] = useState<SubTabKey>('prices');
  const [error, setError] = useState('');
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // ── 价格目录状态 ──
  const [prices, setPrices] = useState<PriceEntry[]>([]);
  const [priceLoading, setPriceLoading] = useState(true);
  /** 顶部表单：新增态（null）/ 编辑态（被编辑模型名） */
  const [editingModel, setEditingModel] = useState<string | null>(null);
  /** 行内编辑：每行独立编辑态（key = model_name） */
  const [inlineEdit, setInlineEdit] = useState<string | null>(null);
  const [inlineForm, setInlineForm] = useState<PriceForm>(EMPTY_FORM);
  const [priceForm, setPriceForm] = useState<PriceForm>(EMPTY_FORM);
  const [savingPrice, setSavingPrice] = useState(false);
  /** 筛选：模型名搜索 */
  const [query, setQuery] = useState('');
  /** 筛选：渠道启用状态 */
  const [channelFilter, setChannelFilter] = useState<ChannelFilter>('enabled');
  /** 筛选：计价类型（all/token/request） */
  const [typeFilter, setTypeFilter] = useState('all');
  /** 批量选择（复选框） */
  const [selected, setSelected] = useState<Set<string>>(new Set());
  // 价格目录客户端分页（过滤变化时回第 1 页）
  const [pricePage, setPricePage] = useState(1);
  /** 启用渠道的模型集合（从渠道列表推导，控制定价目录默认只展示在售模型） */
  const [enabledModels, setEnabledModels] = useState<Set<string> | null>(null);

  // ── 倍率配置状态 ──
  const [, setRatios] = useState<RatiosState>({ model_ratio: {}, group_ratio: {} });
  const [ratioLoading, setRatioLoading] = useState(true);
  const [ratioText, setRatioText] = useState('');
  const [groupRatioText, setGroupRatioText] = useState('');
  const [savingRatios, setSavingRatios] = useState(false);

  useEffect(() => {
    loadPrices();
    loadRatios();
    loadEnabledModels();
  }, []);

  // ── 价格目录 ──
  const loadPrices = async () => {
    setPriceLoading(true);
    try {
      const res = (await api.listPrices()) as { data?: PriceEntry[] };
      setPrices(res.data || (res as unknown as PriceEntry[]) || []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPriceLoading(false);
    }
  };

  /** 汇总所有启用渠道的模型名（渠道级映射后的上游名也要计入） */
  const loadEnabledModels = async () => {
    try {
      const res = (await api.listChannels()) as { data?: Array<{ status?: string; models?: string[]; model_mapping?: Record<string, string> }> };
      const channels = res.data || [];
      const models = new Set<string>();
      for (const ch of channels) {
        if (ch.status !== 'enabled') continue;
        for (const m of ch.models || []) models.add(m);
        // 映射后的上游名同样是可售模型（计费以映射后名称为准）
        for (const upstream of Object.values(ch.model_mapping || {})) {
          if (typeof upstream === 'string' && upstream) models.add(upstream);
        }
      }
      setEnabledModels(models);
    } catch {
      // 渠道加载失败不阻塞定价页：退化为“不过滤”
      setEnabledModels(null);
    }
  };

  const buildPayload = (f: PriceForm) => ({
    model_name: f.model_name.trim(),
    input_price: Number(f.input_price) || 0,
    output_price: Number(f.output_price) || 0,
    cache_price: Number(f.cache_price) || 0,
    price_type: f.price_type || 'token',
  });

  /** 顶部表单提交：编辑态复用同一 upsert 接口（以 model_name 为键） */
  const handleSavePrice = async () => {
    if (!priceForm.model_name.trim()) {
      setError(t('模型名称为必填项'));
      return;
    }
    setSavingPrice(true);
    setError('');
    try {
      await api.upsertPrice(buildPayload(priceForm));
      addToast(t('定价已保存'));
      setPriceForm(EMPTY_FORM);
      setEditingModel(null);
      loadPrices();
      loadEnabledModels();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingPrice(false);
    }
  };

  /** 取消编辑：清空表单回到新增态 */
  const handleCancelEdit = () => {
    setPriceForm(EMPTY_FORM);
    setEditingModel(null);
  };

  const handleDeletePrice = async (model: string) => {
    setConfirmState({
      title: t('删除定价'),
      message: `${t('确定删除模型')} ${model} ${t('的定价？')}`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deletePrice(model);
          addToast(t('定价已删除'));
          loadPrices();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  /** 批量删除选中定价（逐个调删除接口，全量完成后统一刷新） */
  const handleBatchDelete = () => {
    const models = Array.from(selected);
    if (models.length === 0) return;
    setConfirmState({
      title: t('批量删除定价'),
      message: `${t('确定删除选中的')} ${models.length} ${t('条定价？此操作不可恢复。')}`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        let failed = 0;
        for (const m of models) {
          try {
            await api.deletePrice(m);
          } catch {
            failed += 1;
          }
        }
        if (failed > 0) setError(t('部分定价删除失败'));
        else addToast(t('定价已删除'));
        setSelected(new Set());
        loadPrices();
      },
    });
  };

  /** 顶部表单进入编辑态（填值 + 高亮定位） */
  const handleEditPrice = (p: PriceEntry) => {
    cancelInlineEdit();
    setSelected(new Set());
    setEditingModel(p.model_name);
    setPriceForm({
      model_name: p.model_name || '',
      input_price: p.input_price != null ? String(p.input_price) : '',
      output_price: p.output_price != null ? String(p.output_price) : '',
      cache_price: p.cache_price != null ? String(p.cache_price) : '',
      price_type: p.price_type || 'token',
    });
    document.getElementById('price-form')?.scrollIntoView({ behavior: 'smooth', block: 'center' });
  };

  // ── 行内编辑 ──
  const startInlineEdit = (p: PriceEntry) => {
    handleCancelEdit();
    setSelected(new Set());
    setInlineEdit(p.model_name);
    setInlineForm({
      model_name: p.model_name || '',
      input_price: p.input_price != null ? String(p.input_price) : '',
      output_price: p.output_price != null ? String(p.output_price) : '',
      cache_price: p.cache_price != null ? String(p.cache_price) : '',
      price_type: p.price_type || 'token',
    });
  };

  const cancelInlineEdit = () => {
    setInlineEdit(null);
    setInlineForm(EMPTY_FORM);
  };

  const saveInlineEdit = async () => {
    if (!inlineForm.model_name.trim()) return;
    setError('');
    try {
      await api.upsertPrice(buildPayload(inlineForm));
      addToast(t('定价已保存'));
      setInlineEdit(null);
      setInlineForm(EMPTY_FORM);
      loadPrices();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  // ── 倍率配置 ──
  // 输入合法性标记：为 null 表示合法，否则存非法原因文案
  const [modelError, setModelError] = useState<string | null>(null);
  const [groupError, setGroupError] = useState<string | null>(null);

  const handleRatioBlur = (field: 'model' | 'group', value: string) => {
    try {
      JSON.parse(value || '{}');
      if (field === 'model') setModelError(null);
      else setGroupError(null);
    } catch {
      if (field === 'model') setModelError(t('模型倍率 JSON 格式错误'));
      else setGroupError(t('分组倍率 JSON 格式错误'));
    }
  };

  const loadRatios = async () => {
    setRatioLoading(true);
    try {
      const res = await api.getRatios();
      const r = (res?.data ?? {}) as unknown as RatiosState;
      setRatios(r);
      setRatioText(JSON.stringify(r.model_ratio || {}, null, 2));
      setGroupRatioText(JSON.stringify(r.group_ratio || {}, null, 2));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRatioLoading(false);
    }
  };

  const handleSaveRatios = async () => {
    setSavingRatios(true);
    setError('');
    try {
      let modelRatio: Record<string, number> = {};
      let groupRatio: Record<string, number> = {};
      try {
        modelRatio = JSON.parse(ratioText || '{}');
      } catch {
        setModelError(t('模型倍率 JSON 格式错误'));
        setSavingRatios(false);
        return;
      }
      try {
        groupRatio = JSON.parse(groupRatioText || '{}');
      } catch {
        setGroupError(t('分组倍率 JSON 格式错误'));
        setSavingRatios(false);
        return;
      }
      // payload 与后端 RatioConfig 对齐
      await api.updateRatios({ model_ratio: modelRatio, group_ratio: groupRatio });
      addToast(t('倍率配置已保存'));
      loadRatios();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingRatios(false);
    }
  };

  // ── 筛选 ──
  const q = query.trim().toLowerCase();
  const visiblePrices = useMemo(() => {
    return prices.filter((p) => {
      // 模型名搜索
      if (q && !(p.model_name || '').toLowerCase().includes(q)) return false;
      // 渠道启用状态：enabled=只看启用渠道在售的模型（默认，防止目录被停用模型淹没）
      if (channelFilter === 'enabled' && enabledModels && !enabledModels.has(p.model_name)) return false;
      // 不在任何渠道里（孤儿定价，可据此清理）
      if (channelFilter === 'unknown' && enabledModels && enabledModels.has(p.model_name)) return false;
      // 计价类型
      if (typeFilter !== 'all' && (p.price_type || 'token') !== typeFilter) return false;
      return true;
    });
  }, [prices, q, channelFilter, enabledModels, typeFilter]);
  // 客户端分页：页码越界时钳到最后一页（删除/过滤收缩后不落空页）
  const totalPages = Math.max(1, Math.ceil(visiblePrices.length / PRICE_PAGE_SIZE));
  const safePage = Math.min(pricePage, totalPages);
  const pagePrices = visiblePrices.slice(
    (safePage - 1) * PRICE_PAGE_SIZE,
    safePage * PRICE_PAGE_SIZE,
  );

  const allPageSelected = pagePrices.length > 0 && pagePrices.every((p) => selected.has(p.model_name));

  const toggleSelect = (m: string) => {
    const next = new Set(selected);
    if (next.has(m)) next.delete(m);
    else next.add(m);
    setSelected(next);
  };

  const toggleSelectAll = () => {
    const next = new Set(selected);
    if (allPageSelected) {
      for (const p of pagePrices) next.delete(p.model_name);
    } else {
      for (const p of pagePrices) next.add(p.model_name);
    }
    setSelected(next);
  };

  return (
    <div className="pricing-shell">
      {/* 独立页面（/pricing）：页头标题 + 价格目录/倍率配置子标签 */}
      <div className="page-header">
        <div>
          <h1>{t('定价倍率')}</h1>
          <p>{t('管理模型定价目录与全局倍率配置，支持按 token 计费与分组倍率')}</p>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      {/* 子标签页 — 参照 deepseek-pp SubTabs 模式 */}
      <div className="sub-tabs">
        {SUB_TABS.map((tab) => (
          <button
            key={tab.key}
            className={`sub-tab ${sub === tab.key ? 'active' : ''}`}
            onClick={() => setSub(tab.key)}
          >
            {t(tab.labelKey)}
          </button>
        ))}
      </div>

      <div className="pricing-content">
      {/* 子标签 1：价格目录 */}
        {sub === 'prices' && (
          <div className="card">
            <div className="card-header pricing-filters">
              <h2>{t('模型定价目录')} ({visiblePrices.length}/{prices.length})</h2>
              <div className="pricing-filter-row">
                <input className="form-input" style={{ width: 180 }}
                  placeholder={t('搜索模型名称…')} value={query}
                  onChange={(e) => { setQuery(e.target.value); setPricePage(1); }} />
                <select className="form-input" style={{ width: 150 }} value={channelFilter}
                  onChange={(e) => { setChannelFilter(e.target.value as ChannelFilter); setPricePage(1); }}>
                  <option value="enabled">{t('仅启用渠道的模型')}</option>
                  <option value="all">{t('全部模型')}</option>
                  <option value="unknown">{t('未挂渠道的模型')}</option>
                </select>
                <select className="form-input" style={{ width: 120 }} value={typeFilter}
                  onChange={(e) => { setTypeFilter(e.target.value); setPricePage(1); }}>
                  <option value="all">{t('全部计价类型')}</option>
                  <option value="token">token</option>
                  <option value="request">request</option>
                </select>
              </div>
            </div>
            <div className="card-body">
              {priceLoading ? (
                <div className="loading">{t('加载定价数据')}</div>
              ) : (
                <>
                  {/* 新增/编辑定价表单（置顶：编辑时无需滚到页底） */}
                  <div className={`price-form-row ${editingModel ? 'price-form-editing' : ''}`} id="price-form">
                    <div className="form-group" style={{ flex: '1 1 160px', margin: 0 }}>
                      <label style={{ fontSize: 12 }}>{t('模型名称')}</label>
                      <input className="form-input" placeholder="glm-5.2" value={priceForm.model_name}
                        onChange={(e) => setPriceForm({ ...priceForm, model_name: e.target.value })} />
                    </div>
                    <div className="form-group" style={{ flex: '1 1 110px', margin: 0 }}>
                      <label style={{ fontSize: 12 }}>{t('输入价格')}</label>
                      <input className="form-input" type="number" step="0.0001" placeholder="0.001"
                        value={priceForm.input_price}
                        onChange={(e) => setPriceForm({ ...priceForm, input_price: e.target.value })} />
                    </div>
                    <div className="form-group" style={{ flex: '1 1 110px', margin: 0 }}>
                      <label style={{ fontSize: 12 }}>{t('输出价格')}</label>
                      <input className="form-input" type="number" step="0.0001" placeholder="0.002"
                        value={priceForm.output_price}
                        onChange={(e) => setPriceForm({ ...priceForm, output_price: e.target.value })} />
                    </div>
                    <div className="form-group" style={{ flex: '1 1 110px', margin: 0 }}>
                      <label style={{ fontSize: 12 }}>{t('缓存价格')}</label>
                      <input className="form-input" type="number" step="0.0001" placeholder="0.0005"
                        value={priceForm.cache_price}
                        onChange={(e) => setPriceForm({ ...priceForm, cache_price: e.target.value })} />
                    </div>
                    <div className="form-group" style={{ flex: '1 1 100px', margin: 0 }}>
                      <label style={{ fontSize: 12 }}>{t('计价类型')}</label>
                      <select className="form-input" value={priceForm.price_type}
                        onChange={(e) => setPriceForm({ ...priceForm, price_type: e.target.value })}>
                        <option value="token">token</option>
                        <option value="request">request</option>
                      </select>
                    </div>
                    <button className="btn btn-primary" onClick={handleSavePrice} disabled={savingPrice}>
                      {savingPrice ? t('保存中...') : (editingModel ? t('保存修改') : t('新增定价'))}
                    </button>
                    {editingModel && (
                      <button className="btn btn-outline" onClick={handleCancelEdit}>
                        {t('取消')}
                      </button>
                    )}
                  </div>
                  {editingModel && (
                    <div className="price-editing-hint">
                      {t('正在编辑')}：<code>{editingModel}</code>
                    </div>
                  )}

                  {/* 批量操作条（有选中时显示） */}
                  {selected.size > 0 && (
                    <div className="price-batch-bar">
                      <span>{t('已选择 {{count}} 条', { count: selected.size })}</span>
                      <button className="btn btn-danger btn-sm" onClick={handleBatchDelete}>
                        {t('批量删除')}
                      </button>
                      <button className="btn btn-outline btn-sm" onClick={() => setSelected(new Set())}>
                        {t('取消')}
                      </button>
                    </div>
                  )}

                  <div className="table-wrapper" style={{ marginBottom: 20 }}>
                    <table>
                      <thead>
                        <tr>
                          <th style={{ width: 32 }}>
                            <input type="checkbox" checked={allPageSelected} onChange={toggleSelectAll}
                              aria-label={t('全选本页')} />
                          </th>
                          <th>{t('模型')}</th>
                          <th>{t('输入价格')}</th>
                          <th>{t('输出价格')}</th>
                          <th>{t('缓存价格')}</th>
                          <th>{t('计价类型')}</th>
                          <th>{t('操作')}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {visiblePrices.length === 0 ? (
                          <tr>
                            <td colSpan={7} style={{ textAlign: 'center', color: 'var(--text-muted)' }}>
                              {t('没有匹配的定价')}
                            </td>
                          </tr>
                        ) : pagePrices.map((p) => (
                          inlineEdit === p.model_name ? (
                            // 行内编辑态：各格变输入框，行尾 保存/取消
                            <tr key={p.model_name} className="price-row-editing">
                              <td><input type="checkbox" disabled /></td>
                              <td><strong>{p.model_name}</strong></td>
                              <td><input className="form-input price-inline-input" type="number" step="0.0001" value={inlineForm.input_price}
                                onChange={(e) => setInlineForm({ ...inlineForm, input_price: e.target.value })} /></td>
                              <td><input className="form-input price-inline-input" type="number" step="0.0001" value={inlineForm.output_price}
                                onChange={(e) => setInlineForm({ ...inlineForm, output_price: e.target.value })} /></td>
                              <td><input className="form-input price-inline-input" type="number" step="0.0001" value={inlineForm.cache_price}
                                onChange={(e) => setInlineForm({ ...inlineForm, cache_price: e.target.value })} /></td>
                              <td>
                                <select className="form-input price-inline-input" value={inlineForm.price_type}
                                  onChange={(e) => setInlineForm({ ...inlineForm, price_type: e.target.value })}>
                                  <option value="token">token</option>
                                  <option value="request">request</option>
                                </select>
                              </td>
                              <td>
                                <button className="btn btn-primary btn-sm" style={{ marginRight: 6 }} onClick={saveInlineEdit}>
                                  {t('保存')}
                                </button>
                                <button className="btn btn-outline btn-sm" onClick={cancelInlineEdit}>
                                  {t('取消')}
                                </button>
                              </td>
                            </tr>
                          ) : (
                            <tr key={p.model_name}>
                              <td>
                                <input type="checkbox" checked={selected.has(p.model_name)}
                                  onChange={() => toggleSelect(p.model_name)} aria-label={p.model_name} />
                              </td>
                              <td><strong>{p.model_name}</strong></td>
                              <td className="price-cell">{p.input_price}</td>
                              <td className="price-cell">{p.output_price}</td>
                              <td className="price-cell">{p.cache_price || '—'}</td>
                              <td>
                                <span className="price-type-badge">{p.price_type || 'token'}</span>
                              </td>
                              <td>
                                <button className="btn btn-outline btn-sm" style={{ marginRight: 6 }} onClick={() => startInlineEdit(p)}>
                                  {t('编辑')}
                                </button>
                                <button className="btn btn-outline btn-sm" style={{ marginRight: 6 }} onClick={() => handleEditPrice(p)}>
                                  {t('顶部编辑')}
                                </button>
                                <button className="btn btn-danger btn-sm" onClick={() => handleDeletePrice(p.model_name)}>
                                  {t('删除')}
                                </button>
                              </td>
                            </tr>
                          )
                        ))}
                      </tbody>
                    </table>
                  </div>

                  {/* 客户端分页：页码导航 + 总数 */}
                  {totalPages > 1 && (
                    <div className="pagination-meta">
                      <Pagination page={safePage} totalPages={totalPages} onChange={setPricePage} />
                      <span className="pagination-total">
                        {t('共 {{count}} 条', { count: visiblePrices.length })}
                      </span>
                    </div>
                  )}
                </>
              )}
            </div>
          </div>
        )}

        {/* 子标签 2：倍率配置 */}
        {sub === 'ratios' && (
          <div className="card">
            <div className="card-header">
              <h2>{t('倍率配置')}</h2>
            </div>
            <div className="card-body">
              {ratioLoading ? (
                <div className="loading">{t('加载定价数据')}</div>
              ) : (
                <>
                  <p className="ratio-hint">
                    {t('最终费用 = 基础费用 × 模型倍率 × 分组倍率。缺省倍率为 1.0。')}
                  </p>
                  <div className="ratio-grid">
                    <div className="form-group">
                      <label>{t('模型倍率 (JSON)')}</label>
                      <textarea
                        className="form-input ratio-textarea"
                        rows={10}
                        value={ratioText}
                        onChange={(e) => { setRatioText(e.target.value); setModelError(null); }}
                        onBlur={() => handleRatioBlur('model', ratioText)}
                        style={modelError ? { borderColor: 'rgb(239,68,68)' } : undefined}
                        placeholder='{"glm-5.2": 1, "deepseek-v3": 0.5}'
                      />
                      <span className="form-hint">
                        {t('模型名 → 倍率。例如')} {"{\"gpt-4\": 2, \"claude-3\": 1.5}"}
                      </span>
                      {modelError && (
                        <span className="form-hint" style={{ color: 'rgb(239,68,68)' }}>{modelError}</span>
                      )}
                    </div>
                    <div className="form-group">
                      <label>{t('分组倍率 (JSON)')}</label>
                      <textarea
                        className="form-input ratio-textarea"
                        rows={10}
                        value={groupRatioText}
                        onChange={(e) => { setGroupRatioText(e.target.value); setGroupError(null); }}
                        onBlur={() => handleRatioBlur('group', groupRatioText)}
                        style={groupError ? { borderColor: 'rgb(239,68,68)' } : undefined}
                        placeholder='{"default": 1, "vip": 0.8}'
                      />
                      <span className="form-hint">
                        {t('分组名 → 倍率。例如')} {"{\"default\": 1, \"vip\": 0.8}"}
                      </span>
                      {groupError && (
                        <span className="form-hint" style={{ color: 'rgb(239,68,68)' }}>{groupError}</span>
                      )}
                    </div>
                  </div>

                  <div style={{ marginTop: 16 }}>
                    <button className="btn btn-primary" onClick={handleSaveRatios} disabled={savingRatios}>
                      {savingRatios ? t('保存中...') : t('保存倍率配置')}
                    </button>
                  </div>
                </>
              )}
            </div>
          </div>
        )}
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
