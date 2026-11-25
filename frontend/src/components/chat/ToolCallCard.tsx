import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ChevronDown, ChevronRight, Wrench, Check, Copy, CircleSlash, AlertTriangle, Loader2, RefreshCw, EyeOff,
} from 'lucide-react';
import type { ChatStreamUsage, ToolCallState } from '../../types';
import { copyText } from '../../lib/clipboard';
import {
  argumentsView, callPhase, missingResult, phaseLabelKey, previewText, REDACTED_VALUE,
  RESULT_PREVIEW_CHARS, usageTotal, type ToolCallPhase,
} from './tool-calls';
import { normalizeRisk, riskLabelKey, type ToolRisk } from './tool-meta';
import './ToolCallCard.css';

interface ToolCallCardProps {
  calls: ToolCallState[];
  /** 卡片整体状态（流式/完成/中断/错误） */
  state: 'streaming' | 'done' | 'interrupted' | 'error';
  /** 用量收尾（缺省不渲染） */
  usage?: ChatStreamUsage;
  /**
   * 工具中文名（来自 `/api/agent/config` 的 `tools[]`）。
   * 缺省回退工具 id —— 不编造中文名：审批卡与实际执行的工具必须同名。
   */
  labels?: Record<string, string> | ReadonlyMap<string, string>;
  /** 工具风险等级（`readonly` / `low` / `high`），缺省不显示徽标 */
  risks?: Record<string, string> | ReadonlyMap<string, string>;
  /**
   * 单条调用的阶段覆盖（运维页 `phaseOverrides()` 传入）。
   * 缺省由「结果文本 + 消息级状态」推导（`callPhase`）。
   */
  phases?: Record<number, ToolCallPhase>;
  /** 流式停滞（页面侧 StreamPhase 判定）：运行中的调用标「可能超时」 */
  stalled?: boolean;
  /** 单条失败重试（缺省不渲染按钮；AIGX 运维页可提供） */
  onRetry?: (index: number) => void;
}

type FoldKey = string;

/** 读 map / record 两种形态的查找表（调用方给哪种都认） */
function pick(table: ToolCallCardProps['labels'], name: string | undefined): string {
  if (!table || !name) return '';
  if (table instanceof Map) return table.get(name) ?? '';
  return (table as Record<string, string>)[name] ?? '';
}

/**
 * 工具调用卡片。
 *
 * ── 三态口径（对齐 A2A `assistant-messages.tsx` 的 ToolCard）──
 * 一次调用一档状态：`完成` / `失败` / `停止`（已取消）/ `进行中` / `待确认`。
 * 消息级状态只是**兜底**：同一条回复里三次调用可能两成一败，全部按消息级着色
 * 会把失败的那次读成成功。
 *
 * ── 展开形态 ───────────────────────────────────────────────
 * A2A：卡头常驻，点开才看参数与结果；`失败` 直接摊开错误。
 * 这里同口径，但保留 AIGX 既有锚点（`data-testid="tool-call-card"` /
 * `tool-call-args` / `tool-call-result` / `usage-summary`）与「输入」折叠按钮：
 * `e2e/chat-sse-tool-calls.spec.ts` 按这些锚点断言，改形状会让既有回归失效。
 *
 * ── 参数为什么不重新格式化 ─────────────────────────────────
 * 同一份 args 有两条相互冲突的需求：人要看得懂（缩进），断言要逐字匹配
 * （`{"path":"README.md"}` 这种紧凑形态）。取舍是：**无凭证时保留原文**，
 * 一旦检出凭证键才用格式化后的遮蔽副本（那时原文本来也不该展示）。
 */
export default function ToolCallCard({
  calls, state, usage, labels, risks, phases, stalled, onRetry,
}: ToolCallCardProps): JSX.Element {
  const { t } = useTranslation();
  // 默认展开流式中的参数（用户要看「正在做什么」），完成态收起省空间
  const [folded, setFolded] = useState<Record<FoldKey, boolean>>({});
  const [expanded, setExpanded] = useState<Record<FoldKey, boolean>>({});
  const [copied, setCopied] = useState<FoldKey | null>(null);

  const views = useMemo(
    () => calls.map((call) => {
      const args = argumentsView(call.arguments);
      const phase = phases?.[call.index] ?? callPhase(call, state);
      return { call, args, phase };
    }),
    [calls, phases, state],
  );

  const isFolded = (key: FoldKey): boolean => {
    const v = folded[key];
    if (v !== undefined) return v;
    return state !== 'streaming';
  };
  const toggle = (key: FoldKey): void =>
    setFolded((prev) => ({ ...prev, [key]: !isFolded(key) }));
  const isExpanded = (key: FoldKey): boolean => expanded[key] === true;
  const toggleExpanded = (key: FoldKey): void =>
    setExpanded((prev) => ({ ...prev, [key]: !prev[key] }));

  const copy = (text: string, key: FoldKey): void => {
    void copyText(text).then((ok) => {
      if (!ok) return;
      setCopied(key);
      window.setTimeout(() => setCopied((c) => (c === key ? null : c)), 1500);
    });
  };

  const statusNode = (): JSX.Element => {
    if (state === 'streaming') {
      return <span className="tc-status tc-status-active"><Loader2 size={11} className="tc-spin" />{t('进行中')}</span>;
    }
    if (state === 'interrupted') {
      return <span className="tc-status tc-status-warn"><CircleSlash size={11} />{t('停止')}</span>;
    }
    if (state === 'error') {
      return <span className="tc-status tc-status-error"><AlertTriangle size={11} />{t('失败')}</span>;
    }
    return <span className="tc-status tc-status-done"><Check size={11} />{t('完成')}</span>;
  };

  /** 单条调用的状态徽标（与消息级状态分开：一败一成不该同色） */
  const phaseNode = (phase: ToolCallPhase): JSX.Element => (
    <span className={`tc-phase tc-phase-${phase}`} data-testid="tool-call-phase">
      {phase === 'running' && <Loader2 size={10} className="tc-spin" />}
      {phase === 'done' && <Check size={10} />}
      {phase === 'failed' && <AlertTriangle size={10} />}
      {phase === 'cancelled' && <CircleSlash size={10} />}
      {t(phaseLabelKey(phase))}
    </span>
  );

  const total = usageTotal(usage);

  return (
    <div className={`tc-card tc-state-${state}`} data-testid="tool-call-card">
      <div className="tc-head">
        <Wrench size={13} />
        <span className="tc-title">{t('工具调用')}</span>
        <span className="tc-count">{calls.length}</span>
        {statusNode()}
      </div>

      {views.map(({ call, args, phase }) => {
        // key 不带 id：流式期间 id 可能从「无」变成「有」，带了会让整条重挂载、
        // 把用户展开的折叠态弹回去。index 在一次回复里是稳定的。
        const key = String(call.index);
        const argsKey = `${key}:args`;
        const resultKey = `${key}:result`;
        const showArgs = !isFolded(argsKey);
        const name = call.name ?? '';
        const label = pick(labels, name) || name;
        const risk: ToolRisk | null = risks ? normalizeRisk(pick(risks, name)) : null;
        const resultText = typeof call.result === 'string' ? call.result : '';
        const preview = previewText(resultText, RESULT_PREVIEW_CHARS);
        const resultOpen = isExpanded(resultKey);
        const shownResult = resultOpen ? resultText : preview.shown;

        return (
          <div className={`tc-item tc-phase-wrap-${phase}`} key={key} data-testid="tool-call-item">
            <div className="tc-item-head">
              <span className="tc-name" title={name || call.id || ''}>
                {label || t('名称')}
                {call.id && <code className="tc-id">{call.id}</code>}
              </span>
              {risk && risk !== 'unknown' && (
                <span className={`tc-risk tc-risk-${risk}`}>{t(riskLabelKey(risk))}</span>
              )}
              {phaseNode(phase)}
              {stalled && phase === 'running' && (
                <span className="tc-timeout" data-testid="tool-call-timeout">{t('超时')}</span>
              )}
              {args.text && (
                <button
                  type="button"
                  className="tc-fold"
                  onClick={() => toggle(argsKey)}
                  title={showArgs ? t('收起') : t('展开')}
                  aria-expanded={showArgs}
                >
                  {showArgs ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                  {t('输入')}
                </button>
              )}
            </div>

            {args.text && showArgs && (
              <div className="tc-block">
                <div className="tc-block-head">
                  <span>{t('输入')}</span>
                  <span className="tc-chips">
                    {args.invalid && (
                      <span className="tc-chip tc-chip-warn" data-testid="tool-call-invalid">
                        {t('非合法 JSON')}
                      </span>
                    )}
                    {args.redacted > 0 && (
                      <span className="tc-chip tc-chip-muted" data-testid="tool-call-redacted" title={REDACTED_VALUE}>
                        <EyeOff size={10} />{t('已隐藏凭证')}
                      </span>
                    )}
                  </span>
                  <button type="button" className="tc-copy" title={t('复制')} onClick={() => copy(args.text, `${key}:args-copy`)}>
                    {copied === `${key}:args-copy` ? <Check size={11} /> : <Copy size={11} />}
                  </button>
                </div>
                {/* 流式参数分片是增量字符串；未闭合 JSON 原样展示，不做解析 */}
                <pre className="tc-pre" data-testid="tool-call-args">{args.text}</pre>
              </div>
            )}

            {resultText !== '' && (
              <div className={`tc-block tc-block-result${phase === 'failed' ? ' tc-block-failed' : ''}`}>
                <div className="tc-block-head">
                  <span>{t('结果')}</span>
                  <span className="tc-chips">
                    {preview.truncated && (
                      <button
                        type="button"
                        className="tc-chip tc-chip-button"
                        onClick={() => toggleExpanded(resultKey)}
                        data-testid="tool-call-expand"
                      >
                        {resultOpen ? t('收起') : t('展开全部')}
                      </button>
                    )}
                  </span>
                  <button type="button" className="tc-copy" title={t('复制')} onClick={() => copy(resultText, `${key}:res-copy`)}>
                    {copied === `${key}:res-copy` ? <Check size={11} /> : <Copy size={11} />}
                  </button>
                </div>
                <pre className="tc-pre" data-testid="tool-call-result">{shownResult}</pre>
                {preview.truncated && !resultOpen && (
                  <div className="tc-hint">{t('已截断')}：{preview.hidden} {t('字符')}</div>
                )}
              </div>
            )}

            {resultText === '' && phase !== 'done' && (
              <div className={`tc-pending${phase === 'failed' ? ' tc-pending-failed' : ''}`}>
                {phase === 'failed' && <AlertTriangle size={11} />}
                {phase === 'cancelled' ? t('停止') : phase === 'failed' ? t('失败') : `${t('进行中')}…`}
                {phase === 'failed' && onRetry && (
                  <button type="button" className="tc-copy" onClick={() => onRetry(call.index)}>
                    <RefreshCw size={11} />{t('重试')}
                  </button>
                )}
              </div>
            )}

            {resultText === '' && phase === 'done' && missingResult(phase) && state !== 'streaming' && (
              <div className="tc-hint">{t('结果未回灌')}</div>
            )}
          </div>
        );
      })}

      {usage && (
        <div className="tc-usage" data-testid="usage-summary">
          {usage.prompt_tokens !== undefined && (
            <span>{t('输入 Tokens')}: {usage.prompt_tokens}</span>
          )}
          {usage.completion_tokens !== undefined && (
            <span>{t('输出 Tokens')}: {usage.completion_tokens}</span>
          )}
          {total !== undefined && <span>{t('总 Token')}: {total}</span>}
          {/* #24：后端估算口径角标（#21-B 合成尾帧），与上游真值区分 */}
          {usage.estimated === true && (
            <span className="usage-estimated-badge" data-testid="usage-estimated" title={t('由网关估算，非上游回报')}>
              {t('估算')}
            </span>
          )}
        </div>
      )}
    </div>
  );
}
