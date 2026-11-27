/**
 * 破甲 · 异源递进的「一键填入推荐」面板。
 *
 * ── 为什么单独一个组件 ────────────────────────────────────────
 * `fallback_models` 是一个**空数组**，而「该填什么」需要后端把模型管理视图
 * （启用渠道 + 账号池 + 定价）与主模型的厂商归属比一遍才算得出来。这段 UI 要
 * 展示候选、被排除项、池子统计三块，塞进 5.6k 行的 PurgeConsole 只会更难维护；
 * 放这里之后父组件只负责「把推荐结果填进草稿」。
 *
 * ── 交互约定 ─────────────────────────────────────────────────
 * - **只改草稿，不直接保存**：与「设为结构化模板」同口径（用户点保存才落库）。
 *   自动改线上配置是危险的——管理员可能刚手填了一批自己的模型。
 * - 已填内容与推荐不一致时**不静默覆盖**：按钮文案变成「替换为推荐」，并在
 *   面板上列出「当前 vs 推荐」的差异，让用户看清楚再点。
 * - 「为什么不推荐它」始终可见（被排除项可折叠），否则列表看着像漏了模型。
 */
import { memo, useState } from 'react';
import type { TFunction } from 'i18next';
import type {
  PurgeFallbackExcluded,
  PurgeFallbackSuggestion,
} from '../types';
import { Button } from './ui';

/** 排除原因的展示名（code → 短标签）。与后端 `Exclusion` 一一对应。 */
const EXCLUSION_LABELS: Record<string, string> = {
  same_vendor: '同厂商',
  same_model: '与主模型同名',
  unpriced: '未定价',
  no_carrier: '无启用承载',
  not_chat: '非对话模型',
  disabled: '已关闭',
};

export interface FallbackSuggestPanelProps {
  t: TFunction;
  /** 后端推荐（`null` = 旧后端 / 还没加载出来） */
  suggestion?: PurgeFallbackSuggestion | null;
  /** 当前草稿里的 `fallback_models` */
  current: readonly string[];
  /** 当前用于判定的主模型（面板要显示它，否则「异源」无从判断） */
  mainModel?: string;
  /** 推荐不可用（如后端返回空 / 主模型未知但池子为空时仍可点，这里只挡按钮） */
  disabled?: boolean;
  onApply: (models: string[]) => void;
  onRefresh?: () => void;
  /** 刷新中（按钮转圈，防连点） */
  refreshing?: boolean;
}

/** 推荐与当前填写的差异（面板上「点下去会发生什么」）。 */
export function fallbackDiff(
  current: readonly string[],
  recommended: readonly string[],
): { added: string[]; kept: string[]; removed: string[] } {
  const lower = new Set(current.map((m) => m.trim().toLowerCase()).filter(Boolean));
  const recommendedLower = new Set(recommended.map((m) => m.trim().toLowerCase()));
  return {
    added: recommended.filter((m) => !lower.has(m.toLowerCase())),
    kept: recommended.filter((m) => lower.has(m.toLowerCase())),
    removed: current.filter((m) => m.trim() && !recommendedLower.has(m.trim().toLowerCase())),
  };
}

const ExcludedList = memo(function ExcludedList({
  t,
  items,
}: {
  t: TFunction;
  items: PurgeFallbackExcluded[];
}): JSX.Element {
  // 按原因分组：同一原因下往往是一批模型，「3 条未定价」比逐条列三行更省地方
  const groups = new Map<string, PurgeFallbackExcluded[]>();
  for (const item of items) {
    const list = groups.get(item.code) ?? [];
    list.push(item);
    groups.set(item.code, list);
  }
  return (
    <ul className="purge-fallback-excluded">
      {[...groups.entries()].map(([code, list]) => (
        <li key={code}>
          <span className="purge-fallback-excluded-label">
            {t(EXCLUSION_LABELS[code] ?? code)}
          </span>
          <span className="purge-fallback-excluded-models">
            {list
              .slice(0, 4)
              .map((item) => item.model)
              .join('、')}
            {list.length > 4 ? t(' 等 {{n}} 个', { n: list.length }) : ''}
          </span>
        </li>
      ))}
    </ul>
  );
});

export const FallbackSuggestPanel = memo(function FallbackSuggestPanel({
  t,
  suggestion,
  current,
  mainModel,
  disabled,
  onApply,
  onRefresh,
  refreshing,
}: FallbackSuggestPanelProps): JSX.Element {
  const [showExcluded, setShowExcluded] = useState(false);
  if (!suggestion) {
    return (
      <p className="purge-hint" data-testid="purge-fallback-suggest-missing">
        {t('后端未返回候选推荐（旧版本）。直接在下面手填模型名也能用。')}
      </p>
    );
  }
  const recommended = suggestion.models ?? [];
  const diff = fallbackDiff(current, recommended);
  const dirty = diff.added.length > 0 || diff.removed.length > 0;
  const empty = recommended.length === 0;
  const pools = suggestion.pools;

  return (
    <div className="purge-fallback-suggest" data-testid="purge-fallback-suggest">
      <div className="purge-fallback-head">
        <span>
          {t('主模型：{{model}}', { model: mainModel || suggestion.main_model || '—' })}
          {suggestion.main_vendor ? ` · ${suggestion.main_vendor}` : ''}
        </span>
        {!suggestion.main_known ? (
          <span className="purge-fallback-warn" data-testid="purge-fallback-main-unknown">
            {t('主模型不在启用清单里：推荐按「已定价 + 有承载 + 可对话」挑，未做同厂商排除')}
          </span>
        ) : null}
      </div>

      <p className="purge-hint" data-testid="purge-fallback-pools">
        {t(
          '可挑池：启用 {{enabled}} / 已定价 {{priced}} / 可接力 {{eligible}} / 真异源 {{cross}}（共 {{total}} 行）',
          {
            enabled: pools.enabled,
            priced: pools.enabled_priced,
            eligible: pools.eligible,
            cross: pools.cross_vendor,
            total: pools.models_total,
          },
        )}
      </p>

      {empty ? (
        <p className="purge-empty" data-testid="purge-fallback-suggest-empty">
          {t(
            '当前没有可推荐的异源模型。原因通常是：除主模型外没有「已定价 + 有启用承载」的别家模型。先到模型管理给候选配价/开渠道，再点刷新。',
          )}
        </p>
      ) : (
        <ul className="purge-fallback-list">
          {suggestion.candidates.map((candidate) => (
            <li key={candidate.model}>
              <strong title={candidate.model}>{candidate.model}</strong>
              <span className="purge-fallback-vendor">{candidate.vendor}</span>
              {candidate.free ? (
                <span className="purge-fallback-badge">{t('免费')}</span>
              ) : null}
              {candidate.preview ? (
                <span className="purge-fallback-badge" data-testid="purge-fallback-preview">
                  {t('预览版')}
                </span>
              ) : null}
              <span className="purge-hint">{candidate.reason}</span>
            </li>
          ))}
        </ul>
      )}

      <div className="purge-actions">
        <Button
          size="sm"
          disabled={disabled || empty}
          data-testid="purge-fallback-apply"
          onClick={() => onApply(recommended)}
        >
          {dirty && current.length > 0
            ? t('替换为推荐异源（{{n}} 个）', { n: recommended.length })
            : t('一键填入推荐异源（{{n}} 个）', { n: recommended.length })}
        </Button>
        {onRefresh ? (
          <Button
            variant="outline"
            size="sm"
            disabled={refreshing}
            data-testid="purge-fallback-refresh"
            onClick={onRefresh}
          >
            {refreshing ? t('重算中…') : t('重算（按最新渠道/定价）')}
          </Button>
        ) : null}
        {suggestion.excluded.length > 0 ? (
          <Button
            variant="outline"
            size="sm"
            data-testid="purge-fallback-toggle-excluded"
            onClick={() => setShowExcluded((v) => !v)}
          >
            {showExcluded
              ? t('收起排除清单（{{n}}）', { n: suggestion.excluded.length })
              : t('为什么没推荐其它模型（{{n}}）', { n: suggestion.excluded.length })}
          </Button>
        ) : null}
      </div>

      {/* 点下去会发生什么，先写清楚（尤其是会丢掉当前已填的模型） */}
      {dirty && current.length > 0 ? (
        <p className="purge-hint" data-testid="purge-fallback-diff">
          {t('当前 {{kept}} 个已在推荐里；将新增 {{added}} 个、移除 {{removed}} 个。', {
            kept: diff.kept.length,
            added: diff.added.length,
            removed: diff.removed.length,
          })}
          {diff.removed.length > 0 ? ` ${t('移除的是：')}${diff.removed.slice(0, 3).join('、')}` : ''}
        </p>
      ) : null}

      {showExcluded ? <ExcludedList t={t} items={suggestion.excluded} /> : null}
    </div>
  );
});