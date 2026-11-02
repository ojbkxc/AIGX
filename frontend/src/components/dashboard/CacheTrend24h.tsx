// 近 24 小时缓存命中双轴折线（A2A cacheTrendView 移植）：左轴命中率、
// 右轴总 token（量纲差几个数量级必须分轴，否则一条线压平贴边）。
// 读数直接标在点上并互相避让（放不下就跳过，悬停热区仍可补读）；
// 整点刻度每 3 小时一档；hover 多行（时刻/命中率/总 token/按模型分解）。
import { useState } from 'react';
import { axisMax, fmtPct, fmtTokens, textWidth, TIP_MODEL_LIMIT } from '../../lib/format';
import { useMeasuredWidth } from './TrendBars';
import type { CacheTrendHour } from '../../hooks/useDashboard';

export default function CacheTrend24h({ data, lang }: { data: CacheTrendHour[]; lang?: string }): JSX.Element | null {
  const [ref, measured] = useMeasuredWidth<HTMLDivElement>();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  const list = data.filter((h) => h.hour);
  if (!list.length) return null;
  const width = Math.max(measured || 640, 320);

  const height = 230;
  const pad = { left: 44, right: 56, top: 26, bottom: 24 };
  const plotW = Math.max(40, width - pad.left - pad.right);
  const plotH = height - pad.top - pad.bottom;

  const rates = list.map((h) => (Number(h.rate) || 0) / 100);
  const tokens = list.map((h) => Number(h.total_tokens) || 0);
  // 命中率上限按真实峰值抬（分母口径差时可能合法超 100%，夹顶等于谎报）
  const rateMax = Math.max(1, axisMax(Math.max(...rates)));
  const tokenPeak = Math.max(...tokens);
  const tokenMax = axisMax(tokenPeak) || 1;
  const hasTokens = tokenPeak > 0;

  const xAt = (i: number) => pad.left + (list.length > 1 ? (plotW * i) / (list.length - 1) : plotW / 2);
  const yRate = (r: number) => pad.top + plotH - plotH * (r / rateMax);
  const yTokens = (v: number) => pad.top + plotH - plotH * (v / tokenMax);

  const grid = [0, 1, 2, 3, 4].map((i) => {
    const y = Math.round(yRate((rateMax * i) / 4) * 10) / 10;
    const pct = ((rateMax * i) / 4) * 100;
    return {
      y,
      left: Number.isInteger(pct) ? `${pct}%` : `${pct.toFixed(1)}%`,
      right: hasTokens ? fmtTokens((tokenMax * i) / 4, lang) : '',
    };
  });

  const lineOf = (vals: number[], yOf: (v: number) => number) =>
    vals.map((v, i) => `${Math.round(xAt(i) * 10) / 10},${Math.round(yOf(v) * 10) / 10}`).join(' ');

  // 点标读数 + 避让（先标长串 token，再标短命中率）
  const boxes: Array<{ l: number; r: number; t: number; b: number }> = [];
  const place = (x: number, y: number, text: string): { x: number; y: number; text: string; anchor: string } | null => {
    const w = textWidth(text);
    let anchor = 'middle';
    if (x - w / 2 < pad.left - 6) anchor = 'start';
    else if (x + w / 2 > pad.left + plotW + 6) anchor = 'end';
    const l = anchor === 'middle' ? x - w / 2 : anchor === 'end' ? x - w : x;
    const box = { l, r: l + w, t: y - 8.5, b: y + 2.5 };
    if (boxes.some((o) => box.l < o.r + 2 && box.r > o.l - 2 && box.t < o.b + 1 && box.b > o.t - 1)) return null;
    boxes.push(box);
    return { x, y, text, anchor };
  };
  const tokenLabels: Array<{ x: number; y: number; text: string; anchor: string }> = [];
  if (hasTokens) {
    list.forEach((h, i) => {
      if (!(tokens[i] > 0)) return;
      const y = yTokens(tokens[i]);
      const above = y - 12 >= pad.top - 10;
      const p = place(xAt(i), above ? y - 12 : y + 13, fmtTokens(tokens[i], lang));
      if (p) tokenLabels.push(p);
    });
  }
  const rateLabels: Array<{ x: number; y: number; text: string; anchor: string }> = [];
  list.forEach((h, i) => {
    const used = (h.input_tokens ?? 0) + (h.hit_input_tokens ?? 0) > 0 || (h.requests ?? 0) > 0;
    if (!used) return; // 补零的 0% 不标，否则连成一排毫无信息量
    const y = yRate(rates[i]);
    const below = y + 15 <= pad.top + plotH + 8;
    const p = place(xAt(i), below ? y + 13 : y - 7, fmtPct(rates[i] * 100));
    if (p) rateLabels.push(p);
  });

  const tickStep = Math.max(1, Math.ceil(list.length / 8));
  const dotR = list.length === 1 ? 4.5 : 2.5;

  const hover = hoverIdx != null ? list[hoverIdx] : null;
  const tipLines = hover
    ? [
        `${(hover.hour || '').slice(5, 10)} ${(hover.hour || '').slice(11, 13)}:00`,
        `${fmtPct(hover.rate)} · ${fmtTokens(hover.total_tokens, lang)} tokens`,
        ...(hover.models ?? []).slice(0, TIP_MODEL_LIMIT).map((m) => `${m.model}: ${fmtTokens(m.tokens, lang)}`),
      ]
    : [];
  const tipW = Math.min(width - pad.left - 10, 220);
  const tipX = hoverIdx != null ? Math.max(pad.left, Math.min(xAt(hoverIdx) + 10, pad.left + plotW - tipW)) : 0;

  return (
    <div ref={ref} style={{ width: '100%' }}>
      <svg viewBox={`0 0 ${width} ${height}`} style={{ width: '100%', height: 'auto', display: 'block' }} onMouseLeave={() => setHoverIdx(null)}>
        {grid.map((g, i) => (
          <g key={i}>
            <line x1={pad.left} y1={g.y} x2={pad.left + plotW} y2={g.y} stroke="var(--border-color)" strokeWidth="1" />
            <text x={pad.left - 6} y={g.y + 3} textAnchor="end" fill="#7ca4f5" fontSize="9">{g.left}</text>
            {hasTokens ? (
              <text x={pad.left + plotW + 6} y={g.y + 3} textAnchor="start" fill="#f59e0b" fontSize="9">{g.right}</text>
            ) : null}
          </g>
        ))}
        {hasTokens ? (
          <polyline points={lineOf(tokens, yTokens)} fill="none" stroke="#f59e0b" strokeWidth="1.8" opacity={0.85} />
        ) : null}
        <polyline points={lineOf(rates, yRate)} fill="none" stroke="#7ca4f5" strokeWidth="2" />
        {list.map((h, i) => (
          <g key={h.hour}>
            <circle cx={xAt(i)} cy={yRate(rates[i])} r={hoverIdx === i ? dotR + 1 : dotR} fill="#7ca4f5" />
            {hasTokens ? <circle cx={xAt(i)} cy={yTokens(tokens[i])} r={dotR} fill="#f59e0b" /> : null}
          </g>
        ))}
        {tokenLabels.map((p, i) => (
          <text key={`t${i}`} x={p.x} y={p.y} textAnchor={p.anchor} fill="#f59e0b" fontSize="8" pointerEvents="none">{p.text}</text>
        ))}
        {rateLabels.map((p, i) => (
          <text key={`r${i}`} x={p.x} y={p.y} textAnchor={p.anchor} fill="#7ca4f5" fontSize="8" pointerEvents="none">{p.text}</text>
        ))}
        {list.map((h, i) =>
          i % tickStep === 0 || i === list.length - 1 ? (
            <text
              key={`x${i}`}
              x={xAt(i)}
              y={height - 8}
              textAnchor={i === 0 ? 'start' : i === list.length - 1 ? 'end' : 'middle'}
              fill="var(--text-muted)"
              fontSize="9"
            >
              {(h.hour || '').slice(11, 13)}:00
            </text>
          ) : null,
        )}
        {/* hover 热区 */}
        {list.map((_h, i) => {
          const band = list.length > 1 ? plotW / (list.length - 1) : plotW;
          return (
            <rect
              key={`h${i}`}
              x={Math.max(pad.left, xAt(i) - band / 2)}
              y={pad.top}
              width={Math.min(band, plotW)}
              height={plotH}
              fill="transparent"
              onMouseEnter={() => setHoverIdx(i)}
            />
          );
        })}
        {hoverIdx != null && (
          <g pointerEvents="none">
            <line x1={xAt(hoverIdx)} y1={pad.top} x2={xAt(hoverIdx)} y2={pad.top + plotH} stroke="var(--text-muted)" strokeDasharray="3,3" />
            <rect x={tipX} y={pad.top} width={tipW} height={16 + tipLines.length * 13} rx={7} fill="var(--card-bg)" stroke="var(--border-color)" opacity={0.97} />
            {tipLines.map((line, li) => (
              <text key={li} x={tipX + 8} y={pad.top + 14 + li * 13} fill={li === 0 ? 'var(--text-main)' : 'var(--text-muted)'} fontSize={li === 0 ? 11 : 10} fontWeight={li === 0 ? 600 : 400}>
                {line}
              </text>
            ))}
          </g>
        )}
        {/* 图例 */}
        <g fontSize="10" fill="var(--text-muted)">
          <line x1={width - 190} y1={10} x2={width - 176} y2={10} stroke="#7ca4f5" strokeWidth="2" />
          <text x={width - 172} y={13}>%</text>
          {hasTokens ? (
            <>
              <line x1={width - 150} y1={10} x2={width - 136} y2={10} stroke="#f59e0b" strokeWidth="2" />
              <text x={width - 132} y={13}>tokens</text>
            </>
          ) : null}
        </g>
      </svg>
    </div>
  );
}
