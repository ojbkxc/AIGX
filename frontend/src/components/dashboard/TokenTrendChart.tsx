// Token 趋势双线图（输入/输出分列，自旧 Dashboard.tsx :224 迁入组件化）：
// 读数出口统一走 lib/format（中文万/亿 ⇄ 英文 k/M 随语言联动）。
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { fmtTokens } from '../../lib/format';
import type { TokenTrendPoint } from '../../hooks/useDashboard';

export default function TokenTrendChart({ data, lang }: { data: TokenTrendPoint[]; lang?: string }): JSX.Element | null {
  const { t } = useTranslation();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  if (!data || data.length === 0) return null;
  const w = 600;
  const h = 200;
  const pad = { top: 20, right: 20, bottom: 30, left: 50 };
  const chartW = w - pad.left - pad.right;
  const chartH = h - pad.top - pad.bottom;
  const maxVal = Math.max(...data.map((d) => Math.max(d.input_tokens ?? 0, d.output_tokens ?? 0)), 1);
  const x = (i: number) => pad.left + (i + 0.5) * (chartW / data.length);
  const y = (v: number) => pad.top + chartH - (v / maxVal) * chartH;

  const line = (get: (d: TokenTrendPoint) => number) =>
    data.map((d, i) => `${i === 0 ? 'M' : 'L'} ${x(i)} ${y(get(d))}`).join(' ');

  return (
    <svg viewBox={`0 0 ${w} ${h}`} style={{ width: '100%', height: 'auto' }}>
      {[0, 0.25, 0.5, 0.75, 1].map((r) => (
        <line key={r} x1={pad.left} x2={w - pad.right} y1={pad.top + chartH * r} y2={pad.top + chartH * r} stroke="var(--border-color)" strokeWidth="0.5" />
      ))}
      <path d={line((d) => d.input_tokens ?? 0)} fill="none" stroke="#7ca4f5" strokeWidth="2" />
      <path d={line((d) => d.output_tokens ?? 0)} fill="none" stroke="#34d399" strokeWidth="2" />
      {hoverIdx != null && (
        <line x1={x(hoverIdx)} x2={x(hoverIdx)} y1={pad.top} y2={pad.top + chartH} stroke="var(--text-muted)" strokeDasharray="3,3" />
      )}
      {data.map((_d, i) => (
        <rect
          key={i}
          x={x(i) - chartW / data.length / 2}
          y={pad.top}
          width={chartW / data.length}
          height={chartH}
          fill="transparent"
          onMouseEnter={() => setHoverIdx(i)}
          onMouseLeave={() => setHoverIdx(null)}
        />
      ))}
      {hoverIdx != null && (
        <text x={pad.left} y={pad.top - 6} fill="var(--text-main)" fontSize="10">
          {data[hoverIdx].date} · ↑{fmtTokens(data[hoverIdx].input_tokens, lang)} ↓{fmtTokens(data[hoverIdx].output_tokens, lang)}
        </text>
      )}
      {/* 图例 */}
      <rect x={w - pad.right - 150} y={pad.top - 10} width="10" height="3" fill="#7ca4f5" />
      <text x={w - pad.right - 136} y={pad.top - 6} fill="var(--text-muted)" fontSize="10">{t('输入')}</text>
      <rect x={w - pad.right - 90} y={pad.top - 10} width="10" height="3" fill="#34d399" />
      <text x={w - pad.right - 76} y={pad.top - 6} fill="var(--text-muted)" fontSize="10">{t('输出')}</text>
    </svg>
  );
}
