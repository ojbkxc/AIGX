/**
 * 用户侧模型价格表（业主原话：「用户的角度登陆后我都没看每个模型的价格呀」）。
 *
 * ## 数据从哪来（不新建路由，理由写在这里免得后人再造一条）
 *
 * 后端**已有** `GET /api/models/available`（`channels.rs::handle_available_models`，
 * 走 `verify_user`，登录即读），它比 `/v1/models` 多带价格。三个口径要点：
 *
 * 1. **清单本身就等于「能用的模型」**：该端点过 `retain_priced_models`，而它与
 *    数据面门禁 `ensure_model_priced` **同一判据**（`pricing_store.get_price(..).is_some()`）。
 *    所以「表里没有某模型」= 「调它会被拒」（`model_not_priced`），这正是用户要的
 *    真实可售清单 —— 未定价的模型不显示，也**绝不替它编价格**。
 * 2. **免费模型是「有定价、按 0 计费」**（`price_type = "free"`），与「未定价」不同：
 *    它会出现在清单里，价格列显示「免费」而不是 0 —— 0 会让用户以为「填错了」。
 * 3. **按次计价**（`price_type = "count"`）时 `input_price` 是**每次调用价**，
 *    `output_price` 不参与计费。这时不能把两个数并排当「输入/输出单价」展示，
 *    否则用户会按 token 单价心算，算出来的数跟账单差几个数量级。
 *
 * ## 单位
 *
 * 后端 `price_input` / `price_output` / `price_cache` 是**美元 / 1K token**
 * （`pricing::ModelPrice` 字段注释、计费公式 `/1000` 都锚在这个单位上，
 * 后端一个字节都不改）。界面统一换成「¥ / 百万 Token」：值 × 1000（每百万）
 * × `fx_usd_cny`（后端随清单下发的 USD→CNY 汇率，未配置时 1.0）。
 * 换算复用 `lib/pricingUnits.ts`（`pricing-millions` 那波的单一出口），
 * 本文件不再写第二份乘 1000。
 *
 * 汇率缺失/非法时**不假装 1:1**：整表退回「$ / 百万 Token」并显式标注，
 * 免得用户把美元当人民币。
 */
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useQuery } from '@tanstack/react-query';
import { api } from '../../api';
import { Button, EmptyState, Loading } from '../ui';
import { formatPriceNumber, toPerMillion } from '../../lib/pricingUnits';
import type { ModelInfo } from '../../types';

/** 价格条目的计价方式（后端 `price_type` 的三态，`ModelPrice::is_free` 同源） */
type PriceType = 'token' | 'count' | 'free';

/** `/api/models/available` 的价格字段（后端 json! 逐字给出；老后端可能没有） */
interface PricedModel extends ModelInfo {
  price_input?: number;
  price_output?: number;
  price_cache?: number | null;
  price_type?: string;
}

/** 把一行模型规整成可展示的价格读数（纯函数，单测直接喂）。 */
export function priceRowOf(
  m: PricedModel,
  fx: number,
): {
  /** 计价方式（未知/缺省按 token，与后端 `default_price_type` 一致） */
  type: PriceType;
  /** 界面单位符号：¥ 或 $ */
  currency: string;
  /** 输入价（每百万 token；按次计价时为 null） */
  input: string;
  /** 输出价（每百万 token；按次计价时为 null） */
  output: string;
  /** 缓存读价（每百万 token；未配置为 null —— 不是 0） */
  cache: string | null;
  /** 按次计价的单价（每次调用） */
  perCall: string | null;
  /** 该行是否按 0 计费（免费） */
  free: boolean;
} {
  const raw = (m.price_type ?? 'token').toLowerCase();
  // 值域收敛到三态：后端 `upsert_price` 已拒未知值（`pricing/mod.rs` 有校验），
  // 这里兜的是「老后端/脏数据」——未知值按 token 展示而不是显示成「免费」。
  const type: PriceType = raw === 'free' ? 'free' : raw === 'count' ? 'count' : 'token';
  // 汇率非法（0 / NaN / 负）时退回 1.0 并把币种标成 $：宁可显示美元，
  // 也不把 `$0.30` 写成 `¥0.30`（那是 7 倍的错账）。
  const usableFx = Number.isFinite(fx) && fx > 0 ? fx : null;
  const currency = usableFx == null ? '$' : '¥';
  const factor = usableFx ?? 1;
  const toYuanPerMillion = (v: number | null | undefined): number =>
    toPerMillion(Number(v ?? 0)) * factor;

  if (type === 'free') {
    return { type, currency, input: '0', output: '0', cache: null, perCall: null, free: true };
  }
  if (type === 'count') {
    // 按次：input_price 即每次价格（美元/次），不走「每百万」换算（对次计价没有意义）
    return {
      type,
      currency,
      input: '',
      output: '',
      cache: null,
      perCall: formatPriceNumber(Number(m.price_input ?? 0) * factor),
      free: false,
    };
  }
  return {
    type,
    currency,
    input: formatPriceNumber(toYuanPerMillion(m.price_input)),
    output: formatPriceNumber(toYuanPerMillion(m.price_output)),
    // `cache` 未配置 → null（不是 0）：0 是「缓存读免费」，null 是「没有这个价目」。
    cache: m.price_cache == null ? null : formatPriceNumber(toYuanPerMillion(m.price_cache)),
    perCall: null,
    free: false,
  };
}

/** 价格表本体：按模型名搜索 + 排序 + 单位说明。登录用户可见（未定价模型已在后端剔除）。 */
export default function ModelPriceTable(): JSX.Element {
  const { t } = useTranslation();
  const [query, setQuery] = useState('');

  const modelsQ = useQuery({
    queryKey: ['models', 'available', 'price-table'],
    queryFn: async () => {
      const res = await api.listModels();
      return {
        rows: (Array.isArray(res?.data) ? res.data : []) as PricedModel[],
        // 换算口径随清单下发（后端 `price_unit` / `fx_usd_cny`）；
        // 老后端不带这两键时按「每 1K + 汇率 1.0」处理，与那个版本的展示口径一致。
        fx: Number((res as { fx_usd_cny?: number })?.fx_usd_cny ?? 1),
      };
    },
    staleTime: 60_000,
  });

  const rows = useMemo<PricedModel[]>(() => modelsQ.data?.rows ?? [], [modelsQ.data]);
  const fx = modelsQ.data?.fx ?? 1;
  // 汇率不可用时整表标 $（与 priceRowOf 同一判据：fx 非法即美元口径）
  const fxUsable = Number.isFinite(fx) && fx > 0;

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const list = q
      ? rows.filter((m) => m.id.toLowerCase().includes(q) || (m.owned_by ?? '').toLowerCase().includes(q))
      : rows;
    // 稳定排序：先按供应商（同家模型聚在一起），再按名字 —— 用户找「我惯用的那家」
    // 比按字母翻全表快。名字比较用 localeCompare 让 `GLM-4` / `glm-4` 相邻。
    return [...list].sort(
      (a, b) =>
        (a.owned_by ?? '').localeCompare(b.owned_by ?? '') || a.id.localeCompare(b.id),
    );
  }, [rows, query]);

  return (
    <div>
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap', marginBottom: 10 }}>
        <input
          className="input"
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t('搜索模型或供应商')}
          aria-label={t('搜索模型或供应商')}
          style={{ maxWidth: 280 }}
        />
        <span className="docs-note" style={{ margin: 0 }}>
          {t('单位：{{currency}} / 百万 Token（按次计价的模型显示每次价格）', {
            currency: fxUsable ? '¥' : '$',
          })}
        </span>
      </div>

      {modelsQ.isPending ? (
        <Loading text={t('加载模型价格')} />
      ) : modelsQ.isError ? (
        <EmptyState
          message={t('价格表加载失败')}
          action={<Button size="sm" onClick={() => void modelsQ.refetch()}>{t('重试')}</Button>}
        />
      ) : filtered.length === 0 ? (
        <EmptyState
          message={
            query
              ? t('没有匹配「{{q}}」的模型', { q: query })
              : t('暂无可用模型：管理员尚未为任何模型配置价格')
          }
        />
      ) : (
        <>
          <p className="docs-note" style={{ marginTop: 0 }}>
            {t('只列出已定价的模型 —— 未定价的模型调用会被拒绝（错误码 model_not_priced），因此不在此表且不提供报价。')}
          </p>
          <table className="top-rank-table" style={{ width: '100%' }}>
            <thead>
              <tr>
                <th>{t('模型')}</th>
                <th>{t('供应商')}</th>
                <th className="num">{t('输入价')}</th>
                <th className="num">{t('输出价')}</th>
                <th className="num">{t('缓存读价')}</th>
                <th>{t('计价方式')}</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((m) => {
                const p = priceRowOf(m, fx);
                return (
                  <tr key={m.id}>
                    <td
                      style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', maxWidth: 260 }}
                      title={m.id}
                    >
                      {m.id}
                    </td>
                    <td style={{ color: 'var(--text-muted)' }}>{m.owned_by || '—'}</td>
                    {p.type === 'count' ? (
                      <>
                        <td className="num" colSpan={2}>
                          {p.currency}
                          {p.perCall}
                          <span className="pct-hint"> /{t('次')}</span>
                        </td>
                        <td className="num">—</td>
                      </>
                    ) : p.free ? (
                      <td className="num" colSpan={3}>
                        {t('免费')}
                      </td>
                    ) : (
                      <>
                        <td className="num">
                          {p.currency}
                          {p.input}
                        </td>
                        <td className="num">
                          {p.currency}
                          {p.output}
                        </td>
                        <td className="num">{p.cache == null ? '—' : `${p.currency}${p.cache}`}</td>
                      </>
                    )}
                    <td>{t(modelPriceTagLabel(p.type))}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <p className="docs-note">
            {t('行数为 {{n}}：这份清单与网关实际可调用的模型一致（同一判据剔除未定价项）。', {
              n: filtered.length,
            })}
          </p>
        </>
      )}
    </div>
  );
}

/** 计价方式的中文键（值域只这三种，集中一处便于 i18n 扫描与新增时两处同改） */
function modelPriceTagLabel(type: PriceType): string {
  return type === 'free' ? '免费' : type === 'count' ? '按次' : '按 Token';
}