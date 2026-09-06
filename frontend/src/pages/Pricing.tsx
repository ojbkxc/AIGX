import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import './Pricing.css';

type SubTabKey = 'prices' | 'ratios';

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

export default function Pricing() {
  const { t } = useTranslation();
  const addToast = useToast();
  const [sub, setSub] = useState<SubTabKey>('prices');
  const [error, setError] = useState('');
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // ── 价格目录状态 ──
  const [prices, setPrices] = useState<PriceEntry[]>([]);
  const [priceLoading, setPriceLoading] = useState(true);
  const [priceForm, setPriceForm] = useState<PriceForm>({
    model_name: '', input_price: '', output_price: '', cache_price: '', price_type: 'token',
  });
  const [savingPrice, setSavingPrice] = useState(false);

  // ── 倍率配置状态 ──
  const [, setRatios] = useState<RatiosState>({ model_ratio: {}, group_ratio: {} });
  const [ratioLoading, setRatioLoading] = useState(true);
  const [ratioText, setRatioText] = useState('');
  const [groupRatioText, setGroupRatioText] = useState('');
  const [savingRatios, setSavingRatios] = useState(false);

  useEffect(() => {
    loadPrices();
    loadRatios();
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

  const handleSavePrice = async () => {
    if (!priceForm.model_name.trim()) {
      setError(t('模型名称为必填项'));
      return;
    }
    setSavingPrice(true);
    setError('');
    try {
      // payload 与后端 PriceRequest 对齐
      const payload = {
        model_name: priceForm.model_name.trim(),
        input_price: Number(priceForm.input_price) || 0,
        output_price: Number(priceForm.output_price) || 0,
        cache_price: Number(priceForm.cache_price) || 0,
        price_type: priceForm.price_type || 'token',
      };
      await api.upsertPrice(payload);
      addToast(t('定价已保存'));
      setPriceForm({ model_name: '', input_price: '', output_price: '', cache_price: '', price_type: 'token' });
      loadPrices();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingPrice(false);
    }
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

  // ── 倍率配置 ──
  // 输入合法性标记：ratioError 为 null 表示合法，否则存非法原因文案
  const [ratioError, setRatioError] = useState<string | null>(null);

  const handleRatioBlur = (field: 'model' | 'group', value: string) => {
    try {
      JSON.parse(value || '{}');
      setRatioError(null);
    } catch {
      setRatioError(field === 'model' ? t('模型倍率 JSON 格式错误') : t('分组倍率 JSON 格式错误'));
    }
  };

  const loadRatios = async () => {
    setRatioLoading(true);
    try {
      const res = (await api.getRatios()) as { data?: RatiosState } & RatiosState;
      const r = res.data || res || {};
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
        setError(t('模型倍率 JSON 格式错误'));
        setSavingRatios(false);
        return;
      }
      try {
        groupRatio = JSON.parse(groupRatioText || '{}');
      } catch {
        setError(t('分组倍率 JSON 格式错误'));
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

  return (
    <div className="pricing-shell">
      {/* PageIntro 标题区 */}
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
            <div className="card-header">
              <h2>{t('模型定价目录')} ({prices.length})</h2>
            </div>
            <div className="card-body">
              {priceLoading ? (
                <div className="loading">{t('加载定价数据')}</div>
              ) : (
                <>
                  <div className="table-wrapper" style={{ marginBottom: 20 }}>
                    <table>
                      <thead>
                        <tr>
                          <th>{t('模型')}</th>
                          <th>{t('输入价格')}</th>
                          <th>{t('输出价格')}</th>
                          <th>{t('缓存价格')}</th>
                          <th>{t('计价类型')}</th>
                          <th>{t('操作')}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {prices.length === 0 ? (
                          <tr>
                            <td colSpan={6} style={{ textAlign: 'center', color: 'var(--text-muted)' }}>
                              {t('暂无定价配置，未配置的模型将使用倍率计算')}
                            </td>
                          </tr>
                        ) : prices.map((p) => (
                          <tr key={p.model_name}>
                            <td><strong>{p.model_name}</strong></td>
                            <td className="price-cell">{p.input_price}</td>
                            <td className="price-cell">{p.output_price}</td>
                            <td className="price-cell">{p.cache_price || '—'}</td>
                            <td>
                              <span className="price-type-badge">{p.price_type || 'token'}</span>
                            </td>
                            <td>
                              <button className="btn btn-danger btn-sm" onClick={() => handleDeletePrice(p.model_name)}>
                                {t('删除')}
                              </button>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>

                  {/* 新增/编辑定价表单 */}
                  <div className="price-form-row">
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
                      {savingPrice ? t('保存中...') : t('保存定价')}
                    </button>
                  </div>
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
                        onChange={(e) => setRatioText(e.target.value)}
                        onBlur={() => handleRatioBlur('model', ratioText)}
                        style={ratioError ? { borderColor: 'rgb(239,68,68)' } : undefined}
                        placeholder='{"glm-5.2": 1, "deepseek-v3": 0.5}'
                      />
                      <span className="form-hint">
                        {t('模型名 → 倍率。例如')} {"{\"gpt-4\": 2, \"claude-3\": 1.5}"}
                      </span>
                    </div>
                    <div className="form-group">
                      <label>{t('分组倍率 (JSON)')}</label>
                      <textarea
                        className="form-input ratio-textarea"
                        rows={10}
                        value={groupRatioText}
                        onChange={(e) => setGroupRatioText(e.target.value)}
                        onBlur={() => handleRatioBlur('group', groupRatioText)}
                        style={ratioError ? { borderColor: 'rgb(239,68,68)' } : undefined}
                        placeholder='{"default": 1, "vip": 0.8}'
                      />
                      <span className="form-hint">
                        {t('分组名 → 倍率。例如')} {"{\"default\": 1, \"vip\": 0.8}"}
                      </span>
                    </div>
                  </div>

                  {ratioError && (
                    <div style={{ color: 'rgb(239,68,68)', fontSize: 12, marginTop: 8 }}>
                      {ratioError}
                    </div>
                  )}

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
