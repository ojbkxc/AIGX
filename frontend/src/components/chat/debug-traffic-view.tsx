/**
 * 调试面 · 报文详情的渲染与竞态保护（AIGX 独有：A2A 没有上游原始报文面）。
 *
 * 两件事，都是「调试面长时间用起来才会撞到」的形态：
 *
 * 1. **详情请求乱序**：`DebugTrafficPanel.openDetail` 每次点击都直接
 *    `await api.getDebugTrafficDetail(id)` 然后 `setDetail(...)`，没有「这次响应
 *    还是不是当前选中的那条」的判据。快速点 A → B 时，若 A 的响应后到，界面
 *    会显示 B 的 id 配 A 的报文——调试面板给出错误的原文，比不显示更坏。
 *    修法：单调递增的请求序号 + `isStaleResponse`，晚到的旧响应直接丢弃。
 *
 * 2. **大报文一次性渲染**：`<pre>{detail.requestBody}</pre>` 把整段正文塞进
 *    DOM。后端已按 `maxBodyBytes` 截断（`src/debug_traffic`），但 64KB 级 JSON
 *    在低端机/长会话下仍然卡（每次列表刷新都会重渲染弹层）。修法：按字符
 *    预算首段渲染 + 显式「显示全文」，预算内逐字节不变。
 *
 * 纯函数与组件放在一起：判据（预算、是否过期）必须能被单测钉住，测渲染只能
 * 看文字看不出「显示的是谁的数据」。
 */
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

/** 详情弹层正文的默认渲染预算（字符）。超出的部分折叠，点「显示全文」展开。 */
export const DETAIL_TEXT_BUDGET = 20_000;

/**
 * 按字符预算截断文本。
 *
 * 返回 `{ text, truncated, total }`：`truncated` 为 true 时 `text` 是前 `budget`
 * 个字符（**不**在代理对中间切开——emoji 截半会渲染成乱码方块）。
 */
export function boundedText(
  raw: string | null | undefined,
  budget: number = DETAIL_TEXT_BUDGET,
): { text: string; truncated: boolean; total: number } {
  const s = raw ?? '';
  if (budget <= 0 || s.length <= budget) return { text: s, truncated: false, total: s.length };
  // 别把 UTF-16 代理对切一半：末尾是高代理项时退一格
  let cut = budget;
  const code = s.charCodeAt(cut - 1);
  if (code >= 0xd800 && code <= 0xdbff) cut -= 1;
  return { text: s.slice(0, cut), truncated: true, total: s.length };
}

/**
 * 详情响应是否已经过期（不是当前选中项）。
 *
 * `current` 是组件里最新一次点击分配的序号；`mine` 是发起请求时捕获的序号。
 * 不相等即晚到的旧响应，必须丢弃（否则 A 的报文会覆盖 B 的弹层）。
 */
export function isStaleResponse(current: number, mine: number): boolean {
  return current !== mine;
}

export interface BoundedPreProps {
  /** 原文（可能为 null/空） */
  text: string | null | undefined;
  /** 空态文案（已翻译） */
  emptyLabel?: string;
  /** 字符预算，默认 DETAIL_TEXT_BUDGET */
  budget?: number;
  className?: string;
}

/**
 * 有界 `<pre>`：默认只渲染预算内的前段，显式按钮展开全文。
 *
 * 预算内行为与原 `<pre>{text}</pre>` 一致（同样的换行与等宽排版），所以对
 * 正常大小的报文零视觉变化。
 */
export function BoundedPre({
  text,
  emptyLabel,
  budget = DETAIL_TEXT_BUDGET,
  className,
}: BoundedPreProps): JSX.Element {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const { text: shown, truncated, total } = boundedText(text, expanded ? Infinity : budget);
  const empty = (text ?? '') === '';

  return (
    <div className="debug-bounded-pre">
      <pre className={className}>{empty ? (emptyLabel ?? t('（空）')) : shown}</pre>
      {truncated && (
        // 行内样式：本组件被调试面独立引用（不保证 ChatDebugger.css 已加载），
        // 用 AIGX 既有 CSS 变量取色，避免新增样式文件或跨面耦合
        <div
          className="debug-bounded-pre-more"
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 8,
            marginTop: 4,
            fontSize: 12,
            opacity: 0.8,
            color: 'var(--text-secondary, inherit)',
          }}
        >
          <span>{t('已截断')} · {t('共 {{n}} 字符', { n: total })}</span>
          <button type="button" className="btn btn-outline btn-sm" onClick={() => setExpanded(true)}>
            {t('显示全文')}
          </button>
        </div>
      )}
    </div>
  );
}
