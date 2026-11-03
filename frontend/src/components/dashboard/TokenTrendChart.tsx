// Token 趋势双线图（输入/输出分列）：nice-step 纵轴 + 每刻度读数、hover 多行
// 气泡（日期 / ↑↓ 读数 / 合计），窄屏自动收窄左轴留白。
// 读数出口统一走 lib/format（中文万/亿 ⇄ 英文 k/M 随语言联动）。
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { axisMax, fmtAxis, fmtInt, fmtTokens } from '../../lib/format';
import { useMeasuredWidth } from './TrendBars';
import type { TokenTrendPoint } from '../../hooks/useDashboard';

export default function TokenTrendChart({ data, lang }: { data: TokenTrendPoint[]; lang?: string }): JSX.Element | null {
  const { t } = useTranslation();
  const [ref, measured] = useMeasuredWidth<HTMLDivElement>();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  if (!data || data.length === 0) return null;

  const w = Math.max(measured || 640, 320);
  const h = 220;
  const pad = { top: 26, right: 16, bottom: 26, left: 54 };
  const chartW = Math.max(40, w - pad.left - pad.right);
  const chartH = h - pad.top - pad.bottom;
  const peak = Math.max(...data.map((d) => Math.max(d.input_tokens ?? 0, d.output_tokens ?? 0)), 0);
  const yMax = axisMax(peak) || 1;
  const x = (i: number) => pad.left + (i + 0.5) * (chartW / data.length);
  const y = (v: number) => pad.top + chartH - (v / yMax) * chartH;

  const line = (get: (d: TokenTrendPoint) => number) =>
    data.map((d, i) => `${i === 0 ? 'M' : 'L'} ${x(i)} ${y(get(d))}`).join(' ');

  const grid = [0, 1, 2, 3, 4].map((i) => {
    const v = (yMax * i) / 4;
    return { key: `g${i}`, y: Math.round(y(v) * 10) / 10, text: fmtAxis(v, yMax, lang) };
  });

  // 横轴刻度：最多 6 个且必含最后一天
  const labelStep = Math.max(1, Math.ceil(data.length / 6));
  const marks: number[] = [];
  for (let i = 0; i < data.length; i += labelStep) marks.push(i);
  if (marks[marks.length - 1] !== data.length - 1) marks.push(data.length - 1);

  const hover = hoverIdx != null ? data[hoverIdx] : null;
  const tipLines = hover
    ? [
        hover.date || '',
        `↑ ${fmtTokens(hover.input_tokens, lang)} · ↓ ${fmtTokens(hover.output_tokens, lang)}`,
        `${t('合计')} ${fmtTokens((hover.input_tokens ?? 0) + (hover.output_tokens ?? 0), lang)}`,
      ]
    : [];
  const tipW = Math.min(chartW - 10, 200);
  const tipX = hoverIdx != null ? Math.min(x(hoverIdx) + 10, pad.left + chartW - tipW) : 0;

  return (
    <div ref={ref} style={{ width: '100%' }}>
      <svg viewBox={`0 0 ${w} ${h}`} style={{ width: '100%', height: 'auto', display: 'block' }}
        onMouseLeave={() => setHoverIdx(null)}>
        {grid.map((g) => (
          <g key={g.key}>
            <line x1={pad.left} y1={g.y} x2={pad.left + chartW} y2={g.y} stroke="var(--border-color)" strokeWidth="1" />
            <text x={pad.left - 8} y={g.y + 3} textAnchor="end" fill="var(--text-muted)" fontSize="9">{g.text}</text>
          </g>
        ))}
        <path d={line((d) => d.input_tokens ?? 0)} fill="none" stroke="#7ca4f5" strokeWidth="2" />
        <path d={line((d) => d.output_tokens ?? 0)} fill="none" stroke="#34d399" strokeWidth="2" />
        {data.map((d, i) => (
          <g key={`p${i}`}>
            <circle cx={x(i)} cy={y(d.input_tokens ?? 0)} r={hoverIdx === i ? 4 : 2.4} fill="#7ca4f5" />
            <circle cx={x(i)} cy={y(d.output_tokens ?? 0)} r={hoverIdx === i ? 4 : 2.4} fill="#34d399" />
          </g>
        ))}
        {marks.map((mi) => {
          const only = data.length === 1;
          const last = !only && mi === data.length - 1;
          return (
            <text
              key={`m${mi}`}
              x={only ? pad.left + chartW / 2 : last ? pad.left + chartW : x(mi)}
              y={h - 8}
              textAnchor={only ? 'middle' : last ? 'end' : mi === 0 ? 'start' : 'middle'}
              fill="var(--text-muted)" fontSize="9"
            >
              {(data[mi].date || '').slice(5).replace('-', '/')}
            </text>
          );
        })}
        {/* hover 热区：整列铺满 */}
        {data.map((_d, i) => (
          <rect
            key={`h${i}`}
            x={pad.left + (chartW / data.length) * i}
            y={pad.top}
            width={chartW / data.length}
            height={chartH}
            fill="transparent"
            onMouseEnter={() => setHoverIdx(i)}
          />
        ))}
        {hoverIdx != null && (
          <g pointerEvents="none">
            <line x1={x(hoverIdx)} x2={x(hoverIdx)} y1={pad.top} y2={pad.top + chartH}
              stroke="var(--accent-color)" strokeDasharray="4 3" opacity={0.6} />
            <rect x={tipX} y={pad.top} width={tipW} height={16 + tipLines.length * 13} rx={7}
              fill="var(--card-bg)" stroke="var(--border-color)" opacity={0.97} />
            {tipLines.map((lineText, li) => (
              <text key={li} x={tipX + 8} y={pad.top + 14 + li * 13}
                fill={li === 0 ? 'var(--text-main)' : 'var(--text-muted)'}
                fontSize={li === 0 ? 11 : 10} fontWeight={li === 0 ? 600 : 400}>
                {lineText}
              </text>
            ))}
          </g>
        )}
        {/* 图例 + 区间合计 */}
        <g fontSize="10" fill="var(--text-muted)">
          <rect x={w - 186} y={pad.top - 16} width="10" height="3" fill="#7ca4f5" />
          <text x={w - 172} y={pad.top - 12}>{t('输入')}</text>
          <rect x={w - 126} y={pad.top - 16} width="10" height="3" fill="#34d399" />
          <text x={w - 112} y={pad.top - 12}>{t('输出')}</text>
          <text x={w - 16} y={pad.top - 12} textAnchor="end">
            {t('合计')} {fmtInt(data.reduce((s, d) => s + (d.input_tokens ?? 0) + (d.output_tokens ?? 0), 0), lang)}
          </text>
        </g>
      </svg>
    </div>
  );
}
