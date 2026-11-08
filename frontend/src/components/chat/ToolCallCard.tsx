import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ChevronDown, ChevronRight, Wrench, Check, Copy, CircleSlash, AlertTriangle, Loader2,
} from 'lucide-react';
import type { ChatStreamUsage, ToolCallState } from '../../types';
import './ToolCallCard.css';

interface ToolCallCardProps {
  calls: ToolCallState[];
  /** 卡片整体状态（流式/完成/中断/错误） */
  state: 'streaming' | 'done' | 'interrupted' | 'error';
  /** 用量收尾（缺省不渲染） */
  usage?: ChatStreamUsage;
}

type FoldKey = string;

/**
 * 工具调用折叠卡片（#63）。
 *
 * 只消费已聚合的 `ToolCallState[]`（分片拼接在 ChatDebugger 侧完成），
 * 因此线上事件形状变化完全被 `lib/toolCallAdapter` 吸收，本组件不感知协议。
 *
 * 分期渲染：参数/结果各自独立折叠，流式期间默认展开参数、结果区在回灌
 * 层落地前不出现；中断/错误态保留已收到的部分内容并显式标色，不吞信息。
 */
export default function ToolCallCard({ calls, state, usage }: ToolCallCardProps): JSX.Element {
  const { t } = useTranslation();
  // 默认展开流式中的参数（用户要看「正在做什么」），完成态收起省空间
  const [folded, setFolded] = useState<Record<FoldKey, boolean>>({});
  const [copied, setCopied] = useState<FoldKey | null>(null);

  const isFolded = (key: FoldKey): boolean => {
    const v = folded[key];
    if (v !== undefined) return v;
    return state !== 'streaming';
  };
  const toggle = (key: FoldKey): void =>
    setFolded((prev) => ({ ...prev, [key]: !isFolded(key) }));

  const copy = (text: string, key: FoldKey): void => {
    const done = (): void => {
      setCopied(key);
      window.setTimeout(() => setCopied((c) => (c === key ? null : c)), 1500);
    };
    if (navigator.clipboard && window.isSecureContext) {
      void navigator.clipboard.writeText(text).then(done).catch(() => undefined);
    } else {
      done();
    }
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

  const total = usage?.total_tokens
    ?? (usage?.prompt_tokens !== undefined || usage?.completion_tokens !== undefined
      ? (usage?.prompt_tokens ?? 0) + (usage?.completion_tokens ?? 0)
      : undefined);

  return (
    <div className={`tc-card tc-state-${state}`} data-testid="tool-call-card">
      <div className="tc-head">
        <Wrench size={13} />
        <span className="tc-title">{t('工具调用')}</span>
        <span className="tc-count">{calls.length}</span>
        {statusNode()}
      </div>

      {calls.map((call) => {
        const key = `${call.index}:${call.id ?? call.name ?? ''}`;
        const args = call.arguments ?? '';
        const showArgs = !isFolded(`${key}:args`);
        return (
          <div className="tc-item" key={key} data-testid="tool-call-item">
            <div className="tc-item-head">
              <span className="tc-name" title={call.id ?? ''}>
                {call.name || t('名称')}
                {call.id && <code className="tc-id">{call.id}</code>}
              </span>
              {args && (
                <button
                  type="button"
                  className="tc-fold"
                  onClick={() => toggle(`${key}:args`)}
                  title={showArgs ? t('收起') : t('展开')}
                >
                  {showArgs ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                  {t('输入')}
                </button>
              )}
            </div>

            {args && showArgs && (
              <div className="tc-block">
                <div className="tc-block-head">
                  <span>{t('输入')}</span>
                  <button type="button" className="tc-copy" title={t('复制')} onClick={() => copy(args, `${key}:args-copy`)}>
                    {copied === `${key}:args-copy` ? <Check size={11} /> : <Copy size={11} />}
                  </button>
                </div>
                {/* 流式参数分片是增量字符串；未闭合 JSON 原样展示，不做解析 */}
                <pre className="tc-pre" data-testid="tool-call-args">{args}</pre>
              </div>
            )}

            {call.result !== undefined && call.result !== '' && (
              <div className="tc-block tc-block-result">
                <div className="tc-block-head">
                  <span>{t('结果')}</span>
                  <button type="button" className="tc-copy" title={t('复制')} onClick={() => copy(call.result ?? '', `${key}:res-copy`)}>
                    {copied === `${key}:res-copy` ? <Check size={11} /> : <Copy size={11} />}
                  </button>
                </div>
                <pre className="tc-pre" data-testid="tool-call-result">{call.result}</pre>
              </div>
            )}

            {call.result === undefined && state === 'streaming' && (
              <div className="tc-pending">{t('进行中')}…</div>
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
        </div>
      )}
    </div>
  );
}