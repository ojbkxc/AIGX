/**
 * ModelPricingPage — 用户侧「模型价格」页（每百万 Token 口径 + 官方价对照与折扣）。
 *
 * ## 数据来源与契约
 *
 * 依赖 `GET /api/user/models`（由另一位成员 `user-consumption-view` 注册路由；
 * 现状：后端尚未注册，`src/main.rs:2093` 只有注释，故本页**按最可能 shape 渲染并兜底**）。
 * 约定的 shape（单位**每 1K token**，与 `pricing::ModelPrice` 同口径）：
 *
 * ```json
 * { "success": true, "data": [ { "model": "gpt-4o", "input_price": 0.0025,
 *   "output_price": 0.01, "cache_price": 0.00125, "price_type": "token" } ] }
 * ```
 *
 * 为了在接口未上线时不白屏，本页按顺序回落：
 * 1. `GET /api/user/models`（首选，见上）
 * 2. `GET /api/models/available`（**已上线**，返回 `price_input` / `price_output` /
 *    `price_type`，同样是每 1K；`cache_price` 缺）——见 `src/api/admin/channels.rs:handle_available_models`
 * 3. 两者都失败 → 显示错误态，不静默显示空表
 *
 * ## 官方价
 *
 * 后端**没有** official / list_price / msrp / reference_price 字段（证据见
 * `lib/officialPricing.ts` 头注）；models.dev 快照也没有价格键。所以官方价来自
 * `lib/officialPricing.ts`（内置种子 + 管理员在本机填的覆盖表）：填了才显示折扣，
 * 没填显示「未登记」。**绝不凭空造官方价。**
 *
 * ## 未定价
 *
 * 价格缺失或全 0 一律标「未定价（不可用）」，绝不显示成「0 元」。
 */
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Search, RefreshCw, Info } from 'lucide-react';
import { api } from '../api';
import { API_BASE, request } from '../api/client';
import { Button, Card, EmptyState, Loading, Select } from '../components/ui';
import { TablePaginationFooter, usePageSize, CLIENT_PAGE_SIZES } from '../components/ui/TableShell';
import { isAdmin } from '../lib/utils';
import {
  discountRatio,
  formatDiscount,
  formatPerMillion,
  formatPriceNumber,
  toPerMillion,
} from '../lib/pricingUnits';
import {
  clearOfficialPrice,
  getOfficialPrices,
  normalizeModelName,
  saveOfficialPrice,
  type OfficialPrice,
} from '../lib/officialPricing';
import './ModelPricingPage.css';

const LS_PSZ = 'aigx-tbl-pagesize.model-pricing';

/** 页面视图行（单位：每 1K token，与后端同口径；展示时再换算）。 */
interface PriceRow {
  model: string;
  input_price: number | null;
  output_price: number | null;
  cache_price: number | null;
  price_type: string;
}

/** `GET /api/user/models` 的最小形状（见文件头注）。 */
interface UserModelEntry {
  model?: string;
  id?: string;
  input_price?: number;
  output_price?: number;
  cache_price?: number | null;
  price_type?: string;
}

/** `GET /api/models/available` 的形状（已上线端点）。 */
interface AvailableModelEntry {
  id?: string;
  price_input?: number;
  price_output?: number;
  // 后端 `handle_available_models` 已返回 `price_cache`（每 1K，未配置=null）；
  // 回落路径此前漏读它，导致缓存价列恒显示「—」。
  price_cache?: number | null;
  price_type?: string;
}

/** 把接口条目归一成 PageRow；缺字段一律 null（未定价，不是 0）。 */
function toRow(entry: UserModelEntry | AvailableModelEntry): PriceRow | null {
  const name = ('model' in entry && entry.model) || ('id' in entry && entry.id) || '';
  if (!name) return null;
  const num = (v: unknown): number | null => {
    const n = Number(v);
    return Number.isFinite(n) && n >= 0 ? n : null;
  };
  return {
    model: String(name),
    input_price: num((entry as UserModelEntry).input_price ?? (entry as AvailableModelEntry).price_input),
    output_price: num((entry as UserModelEntry).output_price ?? (entry as AvailableModelEntry).price_output),
    cache_price: num((entry as UserModelEntry).cache_price ?? (entry as AvailableModelEntry).price_cache),
    price_type: String((entry as UserModelEntry).price_type ?? (entry as AvailableModelEntry).price_type ?? 'token'),
  };
}

/** 未定价判据：缺价、或按量价全 0 且无缓存价。与后端「免费/未定价」两态区分。 */
function isUnpriced(row: PriceRow): boolean {
  if (row.price_type === 'free') return false;
  if (row.price_type === 'count') return row.input_price == null;
  const input = row.input_price ?? 0;
  const output = row.output_price ?? 0;
  const cache = row.cache_price ?? 0;
  return input === 0 && output === 0 && cache === 0;
}

/** 价格页的排序/筛选档位。 */
type PriceFilter = 'all' | 'priced' | 'unpriced' | 'discounted';

export default function ModelPricingPage(): JSX.Element {
  const { t } = useTranslation();
  const admin = isAdmin();
  const [rows, setRows] = useState<PriceRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  /** 数据来源标记：user-models（首选）/ available（已上线端点回落） */
  const [source, setSource] = useState<'user-models' | 'available' | ''>('');
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<PriceFilter>('all');
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = usePageSize(LS_PSZ, CLIENT_PAGE_SIZES, 50);
  /** 官方价表（内置种子 + 本机覆盖）。管理员改完用它重渲染。 */
  const [official, setOfficial] = useState<Record<string, OfficialPrice>>(() => getOfficialPrices());
  /** 正在编辑官方价的行（模型名）；null = 无 */
  const [editingOfficial, setEditingOfficial] = useState<string | null>(null);
  const [officialDraft, setOfficialDraft] = useState({ input: '', output: '', cache: '' });

  const load = async (): Promise<void> => {
    setLoading(true);
    setError('');
    // 1) 首选：用户侧聚合（每 1K，含 cache_price）
    try {
      const res = await request<{ success?: boolean; data?: UserModelEntry[] }>(
        'GET',
        `${API_BASE}/user/models`
      );
      const list = Array.isArray(res?.data) ? res.data : null;
      if (list) {
        setRows(list.map(toRow).filter((r): r is PriceRow => r !== null));
        setSource('user-models');
        setLoading(false);
        return;
      }
    } catch {
      // 404 / 未注册：走回落，不报错
    }
    // 2) 回落：已上线的 /api/models/available（同样每 1K，无 cache_price）
    try {
      const res = await api.listModels();
      const list = (res?.data ?? []) as AvailableModelEntry[];
      setRows(list.map(toRow).filter((r): r is PriceRow => r !== null));
      setSource('available');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setSource('');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const officialByName = useMemo(() => {
    const map: Record<string, OfficialPrice> = {};
    for (const [k, v] of Object.entries(official)) map[normalizeModelName(k)] = v;
    return map;
  }, [official]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return rows.filter((row) => {
      if (q && !row.model.toLowerCase().includes(q)) return false;
      const unpriced = isUnpriced(row);
      if (filter === 'priced') return !unpriced;
      if (filter === 'unpriced') return unpriced;
      if (filter === 'discounted') {
        if (unpriced) return false;
        const off = officialByName[normalizeModelName(row.model)];
        return off?.input != null && row.input_price != null;
      }
      return true;
    });
  }, [rows, query, filter, officialByName]);

  // 筛选/搜索变化时回第 1 页（避免停在超出新结果集的页码）
  useEffect(() => {
    setPage(1);
  }, [query, filter]);

  const total = filtered.length;
  const size = pageSize === 'all' ? total || 1 : pageSize;
  const totalPages = Math.max(1, Math.ceil(total / size));
  const safePage = Math.min(page, totalPages);
  const pageRows =
    pageSize === 'all' ? filtered : filtered.slice((safePage - 1) * size, safePage * size);

  const startEditOfficial = (model: string): void => {
    const cur = officialByName[normalizeModelName(model)];
    setEditingOfficial(model);
    setOfficialDraft({
      input: cur?.input != null ? String(cur.input) : '',
      output: cur?.output != null ? String(cur.output) : '',
      cache: cur?.cache != null ? String(cur.cache) : '',
    });
  };

  const commitOfficial = (model: string): void => {
    const num = (raw: string): number | null => {
      const text = raw.trim();
      if (text === '') return null;
      const n = Number(text);
      return Number.isFinite(n) && n >= 0 ? n : null;
    };
    saveOfficialPrice(model, {
      input: num(officialDraft.input),
      output: num(officialDraft.output),
      cache: num(officialDraft.cache),
    });
    setOfficial(getOfficialPrices());
    setEditingOfficial(null);
  };

  const resetOfficial = (model: string): void => {
    clearOfficialPrice(model);
    setOfficial(getOfficialPrices());
  };

  /** 单列折扣展示：our(每百万) vs official(每百万)。 */
  const renderDiscount = (our: number | null, off: number | null): JSX.Element => {
    if (our == null || off == null) {
      return <span className="mpp-muted">—</span>;
    }
    const ratio = discountRatio(our, off);
    if (ratio == null) return <span className="mpp-muted">—</span>;
    if (ratio >= 1) {
      return <span className="mpp-muted">{t('无折扣')}</span>;
    }
    return <span className="mpp-discount">{formatDiscount(ratio)}</span>;
  };

  if (loading) return <Loading text={t('加载模型价格…')} />;

  return (
    <div className="mpp">
      <div className="page-header">
        <h1>{t('模型价格')}</h1>
        <p>{t('全部单位：元 / 百万 Token（后端按每 1K token 计费，这里已换算成百万口径，便于与官方价直接对比）')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {source === 'available' && (
        <div className="mpp-notice">
          <Info size={14} />
          <span>
            {t('当前展示的数据来自已上线的模型清单接口，缓存读价格暂不可见；用户侧价格接口上线后会自动切换。')}
          </span>
        </div>
      )}

      <Card
        title={`${t('可用模型')} (${total})`}
        actions={
          <div className="mpp-toolbar">
            <div className="mpp-search">
              <Search size={14} />
              <input
                placeholder={t('搜索模型名')}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                aria-label={t('搜索模型名')}
              />
            </div>
            <Select
              value={filter}
              onChange={(e) => setFilter(e.target.value as PriceFilter)}
              aria-label={t('筛选')}
              style={{ width: 140 }}
            >
              <option value="all">{t('全部')}</option>
              <option value="priced">{t('已定价')}</option>
              <option value="unpriced">{t('未定价')}</option>
              <option value="discounted">{t('有官方价对照')}</option>
            </Select>
            <Button variant="outline" size="sm" onClick={() => void load()}>
              <RefreshCw size={14} /> {t('刷新')}
            </Button>
          </div>
        }
      >
        {total === 0 ? (
          <EmptyState
            message={rows.length === 0 ? t('暂无可展示的模型') : t('无匹配结果')}
            icon="💰"
          />
        ) : (
          <>
            <div className="table-wrapper">
              <table className="mpp-table">
                <thead>
                  <tr>
                    <th>{t('模型')}</th>
                    <th className="ta-right">{t('输入（元 / 百万）')}</th>
                    <th className="ta-right">{t('输出（元 / 百万）')}</th>
                    <th className="ta-right">{t('缓存读（元 / 百万）')}</th>
                    <th className="ta-right">{t('官方输入价')}</th>
                    <th className="ta-right">{t('折扣')}</th>
                    {admin && <th className="ta-right">{t('官方价维护')}</th>}
                  </tr>
                </thead>
                <tbody>
                  {pageRows.map((row) => {
                    const unpriced = isUnpriced(row);
                    const off = officialByName[normalizeModelName(row.model)] ?? null;
                    const editing = editingOfficial === row.model;
                    return (
                      <tr key={row.model} className={unpriced ? 'mpp-row-unpriced' : undefined}>
                        <td>
                          <code className="mpp-model">{row.model}</code>
                          {unpriced && <span className="mpp-tag-unpriced">{t('未定价（不可用）')}</span>}
                          {row.price_type === 'free' && <span className="mpp-tag-free">{t('免费')}</span>}
                        </td>
                        <td className="ta-right">
                          {unpriced ? (
                            <span className="mpp-muted">{t('未定价（不可用）')}</span>
                          ) : row.price_type === 'count' ? (
                            <span>{formatPerMillion(row.input_price)} / {t('次')}</span>
                          ) : (
                            <span>{formatPerMillion(row.input_price)}</span>
                          )}
                        </td>
                        <td className="ta-right">
                          {unpriced || row.price_type === 'count' ? (
                            <span className="mpp-muted">—</span>
                          ) : (
                            <span>{formatPerMillion(row.output_price)}</span>
                          )}
                        </td>
                        <td className="ta-right">
                          {unpriced || row.price_type === 'count' || row.cache_price == null ? (
                            <span className="mpp-muted">—</span>
                          ) : (
                            <span>{formatPerMillion(row.cache_price)}</span>
                          )}
                        </td>
                        <td className="ta-right">
                          {off?.input != null ? (
                            <span className="mpp-official">{formatPriceNumber(off.input)}</span>
                          ) : (
                            <span className="mpp-muted">{t('未登记')}</span>
                          )}
                        </td>
                        <td className="ta-right">
                          {unpriced ? (
                            <span className="mpp-muted">—</span>
                          ) : (
                            renderDiscount(
                              row.input_price == null ? null : toPerMillion(row.input_price),
                              off?.input ?? null
                            )
                          )}
                        </td>
                        {admin && (
                          <td className="ta-right">
                            {editing ? (
                              <div className="mpp-official-edit">
                                <input
                                  type="number"
                                  min="0"
                                  step="0.01"
                                  placeholder={t('输入价')}
                                  value={officialDraft.input}
                                  onChange={(e) => setOfficialDraft({ ...officialDraft, input: e.target.value })}
                                />
                                <input
                                  type="number"
                                  min="0"
                                  step="0.01"
                                  placeholder={t('输出价')}
                                  value={officialDraft.output}
                                  onChange={(e) => setOfficialDraft({ ...officialDraft, output: e.target.value })}
                                />
                                <input
                                  type="number"
                                  min="0"
                                  step="0.01"
                                  placeholder={t('缓存读')}
                                  value={officialDraft.cache}
                                  onChange={(e) => setOfficialDraft({ ...officialDraft, cache: e.target.value })}
                                />
                                <Button size="sm" variant="primary" onClick={() => commitOfficial(row.model)}>
                                  {t('保存')}
                                </Button>
                                <Button size="sm" variant="outline" onClick={() => setEditingOfficial(null)}>
                                  {t('取消')}
                                </Button>
                              </div>
                            ) : (
                              <div className="mpp-official-actions">
                                <Button size="sm" variant="outline" onClick={() => startEditOfficial(row.model)}>
                                  {off ? t('修改官方价') : t('填官方价')}
                                </Button>
                                {off && (
                                  <Button size="sm" variant="outline" onClick={() => resetOfficial(row.model)}>
                                    {t('清除')}
                                  </Button>
                                )}
                              </div>
                            )}
                          </td>
                        )}
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
            <TablePaginationFooter
              total={total}
              page={safePage}
              totalPages={totalPages}
              size={pageSize}
              sizes={CLIENT_PAGE_SIZES}
              onSizeChange={(n) => {
                setPageSize(n);
                setPage(1);
              }}
              onPageChange={setPage}
            />
          </>
        )}
      </Card>

      <p className="mpp-foot-note">
        {t('官方价是可选的参考值：管理员填了才显示折扣，没填显示「未登记」。折扣 = 我们价 ÷ 官方价，例如 0.1 显示为「1 折」。')}
      </p>
    </div>
  );
}