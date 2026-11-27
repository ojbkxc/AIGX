import { memo } from 'react';
import type { TFunction } from 'i18next';
import type { PurgeConfigWarning } from '../types';
import { Button } from './ui';
import type { HealthFixSpec } from './purge-fixes';
import { healthFixSpecs } from './purge-fixes';
import './purge-health.css';

/** 严重级 → 徽标文案（i18n key 与中文同形）。 */
export const SEVERITY_LABEL: Record<'error' | 'warn' | 'info', string> = {
  error: '不生效',
  warn: '需确认',
  info: '提示',
};

/**
 * 体检告警按严重级排序（Error → Warn → Info，同级保持后端原序）。
 *
 * 为什么在前端排而不在后端排：后端 `config_warning_items` 的顺序是「检查代码
 * 的执行顺序」，对排查有意义（总开关相关的先出现）；分级只是展示优先级，不该
 * 改动后端产出的语义。前端稳定排序两者兼得。
 *
 * 旧后端不返回 `severity` 时按 `error` 处理——与后端「未登记 code 兜底 Error」
 * 同一立场（宁严不宽）。
 */
export function sortWarningsBySeverity<T extends { severity?: string }>(items: T[]): T[] {
  const rank = (item: T): number => {
    switch (item.severity) {
      case 'warn':
        return 1;
      case 'info':
        return 2;
      default:
        return 0; // error 或未声明
    }
  };
  // Array.prototype.sort 在现代引擎里稳定，同级保持原序
  return [...items].sort((a, b) => rank(a) - rank(b));
}

export interface HealthRow {
  code: string;
  message: string;
  severity: 'error' | 'warn' | 'info';
}

/**
 * 把后端两份产物（结构化 `config_warning_items` + 旧版纯文本 `config_warnings`）
 * 归一成同一形状。结构化优先，旧版补在后面（旧后端只有字符串）。
 */
export function normalizeHealthRows(
  items: PurgeConfigWarning[] | undefined,
  legacy: string[] | undefined,
): HealthRow[] {
  if (items && items.length > 0) {
    return sortWarningsBySeverity(
      items.map((w) => ({
        code: w.code,
        message: w.message,
        severity: w.severity ?? ('error' as const),
      })),
    );
  }
  return (legacy ?? []).map((message) => ({
    code: '',
    message,
    severity: 'error' as const,
  }));
}

/**
 * 体检面板：**每条告警都带一个能点得动的「怎么修」**。
 *
 * 三件事与老版本的区别：
 * 1. Error 级置顶（`sortWarningsBySeverity`），带严重级色条；
 * 2. 每条右侧直接放动作按钮（「填入推荐异源」「去重」「全部启用」…），而不是
 *    统一的「去修复」——那等于让用户自己猜该填什么；
 * 3. 修完的条目多一行「已按建议修改，保存后消失」：**告警本体仍在**，因为
 *    真正的判定在后端（save 后重新体检）。前端把行藏起来就是谎报「已经好了」。
 */
export const PurgeHealthPanel = memo(function PurgeHealthPanel({
  rows,
  fixedCodes,
  fixTargets,
  busy,
  onRunFix,
  onOpenOptions,
  onCopy,
  enabled,
  t,
}: {
  rows: HealthRow[];
  /** 已点过修复的 code（展示「保存后消失」而不是把行藏起来） */
  fixedCodes: readonly string[];
  /** code → 锚点（未登记 code 的兜底跳转） */
  fixTargets: Record<string, string>;
  busy: boolean;
  onRunFix: (code: string, spec: HealthFixSpec) => void;
  /** 该 code 没有可自动执行的动作 → 打开选项/说明 */
  onOpenOptions: (code: string) => void;
  onCopy: () => void;
  enabled: boolean;
  t: TFunction;
}): JSX.Element {
  if (rows.length === 0) {
    return enabled ? (
      <p className="purge-check-ok" role="status" data-testid="purge-health-ok">
        {t('配置体检通过：当前没有「开了但不生效」或自相矛盾的项目。')}
      </p>
    ) : (
      <></>
    );
  }

  const errorCount = rows.filter((row) => row.severity === 'error').length;
  const warnCount = rows.filter((row) => row.severity === 'warn').length;

  return (
    <div className="purge-health" role="alert" data-testid="purge-health">
      <div className="purge-health-head">
        <strong>{t('配置体检')}</strong>
        <span className="purge-hint">
          {errorCount > 0
            ? t('{{n}} 项不生效', { n: errorCount })
            : t('没有不生效项')}
          {warnCount > 0 ? ` · ${t('{{n}} 项需确认', { n: warnCount })}` : ''}
        </span>
        <Button variant="outline" size="sm" data-testid="purge-warning-copy" onClick={onCopy}>
          {t('复制体检结果')}
        </Button>
      </div>
      {rows.map((row, index) => {
        const specs = healthFixSpecs(row.code, fixTargets[row.code]);
        const fixed = row.code !== '' && fixedCodes.includes(row.code);
        return (
          <div
            key={`${row.code}-${index}`}
            className={`purge-warning-row is-${row.severity}`}
            data-health-code={row.code || undefined}
          >
            <span className="purge-warning-badge">{t(SEVERITY_LABEL[row.severity])}</span>
            <span className="purge-health-msg">
              {row.message}
              {fixed ? (
                <span className="purge-health-fixed" data-testid="purge-health-fixed">
                  {t('已按建议修改，保存后消失')}
                </span>
              ) : null}
            </span>
            {specs.map((spec) =>
              spec.kind === 'draft' || spec.anchor ? (
                <Button
                  key={`${row.code}-${spec.label}`}
                  variant="outline"
                  size="sm"
                  disabled={busy}
                  data-testid={`purge-health-fix-${row.code}-${spec.kind}`}
                  onClick={() => onRunFix(row.code, spec)}
                >
                  {t(spec.label)}
                </Button>
              ) : (
                <Button
                  key={`${row.code}-${spec.label}`}
                  variant="outline"
                  size="sm"
                  disabled={busy}
                  onClick={() => onOpenOptions(row.code)}
                >
                  {t(spec.label)}
                </Button>
              ),
            )}
          </div>
        );
      })}
    </div>
  );
});