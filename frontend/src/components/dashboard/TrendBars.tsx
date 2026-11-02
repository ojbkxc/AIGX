// 按天柱状图（A2A dailyTrendView 移植）：nice-step 纵轴、柱顶读数三级退让
// （横排 → 竖排 → 只标峰值）、hover 多行气泡（总量 + 按模型分解 ≤8）。
// 与 A2A 同法先量容器宽度再按像素作画（ResizeObserver 驱动重算），
// 文字与刻度因此不会被 viewBox 缩放拉变形。
import { useCallback, useEffect, useRef, useState } from 'react';
import { axisMax, fmtTokens, textWidth, TIP_MODEL_LIMIT } from '../../lib/format';

/** 容器宽度测量（ResizeObserver；SSR/不支持时回落 clientWidth） */
export function useMeasuredWidth<T extends HTMLElement>(): [(node: T | null) => void, number] {
  const [width, setWidth] = useState(0);
  const roRef = useRef<ResizeObserver | null>(null);
  const ref = useCallback((node: T | null) => {
    roRef.current?.disconnect();
    if (!node) return;
    setWidth(node.clientWidth || 0);
    if (typeof ResizeObserver !== 'undefined') {
      const ro = new ResizeObserver(() => setWidth(node.clientWidth || 0));
      ro.observe(node);
      roRef.current = ro;
    }
  }, []);
  useEffect(() => () => roRef.current?.disconnect(), []);
  return [ref, width];
}

export interface BarRow {
  date: string;
  value: number;
  /** 气泡里的模型分解行 */
  models?: Array<{ model: string; tokens: number }>;
  /** 气泡第二行读数（如「123 次请求」），调用方给文案 */
  sub?: string;
}

interface Props {
  rows: BarRow[];
  /** 读数格式化（token/请求/金额各传各的） */
  formatValue: (v: number) => string;
  /** 轴刻度格式化（与 formatValue 同一套量级开关） */
  formatAxis: (v: number, max: number) => string;
  colors?: Map<string, string>;
  lang?: string;
  height?: number;
}

const VERTICAL_LABEL_MIN_STEP = 13;

export default function TrendBars({
  rows,
  formatValue,
  formatAxis,
  lang,
  height = 240,
}: Props): JSX.Element | null {
  const [ref, measured] = useMeasuredWidth<HTMLDivElement>();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  if (!rows.length) return null;
  const width = Math.max(measured || 640, 320);

  const pad = { left: 50, right: 14, top: 26, bottom: 26 };
  const plotW = Math.max(40, width - pad.left - pad.right);
  const step = plotW / rows.length;

  // 柱顶读数的三级退让先定排法（竖排要把 pad.top 撑高，plotH 依赖它）
  const texts = rows.map((r) => (r.value > 0 ? formatValue(r.value) : ''));
  const widest = Math.max(0, ...texts.map(textWidth));
  const allFit = texts.every((x) => !x) || widest + 4 <= step;
  const vertical = !allFit && step >= VERTICAL_LABEL_MIN_STEP;
  if (vertical) pad.top = 10 + Math.ceil(widest * 1.25);
  const plotH = height - pad.top - pad.bottom;

  const values = rows.map((r) => r.value);
  const peak = Math.max(...values);
  const yMax = axisMax(peak) || 1;
  const blank = peak <= 0;
  const barW = Math.max(1, Math.min(28, step * 0.68));
  const baseY = pad.top + plotH;
  const yAt = (v: number) => pad.top + plotH - (plotH * v) / yMax;

  const grid = blank ? [] : [0, 1, 2, 3, 4].map((i) => {
    const v = (yMax * i) / 4;
    return { key: `g${i}`, y: Math.round(yAt(v) * 10) / 10, text: formatAxis(v, yMax) };
  });

  // 标注占位盒（第三级退让的避让）
  const boxes: Array<{ l: number; r: number; t: number; b: number }> = [];
  type Placed = { x: number; y: number; text: string; anchor: 'middle' | 'start' | 'end'; rotate?: boolean };
  const place = (index: number, avoid: boolean): Placed | null => {
    const text = texts[index];
    if (!text) return null;
    const top = yAt(rows[index].value);
    const x = pad.left + step * index + step / 2;
    if (vertical) return { x, y: top - 6, text, anchor: 'start', rotate: true };
    const y = top - 6;
    const inside = y - 9 < pad.top - 10;
    const ly = inside ? y + 13 : y;
    if (avoid) {
      const half = textWidth(text) / 2;
      const box = { l: x - half, r: x + half, t: ly - 8.5, b: ly + 2.5 };
      if (boxes.some((o) => box.l < o.r + 2 && box.r > o.l - 2 && box.t < o.b + 1 && box.b > o.t - 1)) return null;
      boxes.push(box);
    }
    return { x, y: ly, text, anchor: 'middle' };
  };
  const barLabels: Placed[] = [];
  if (allFit || vertical) {
    rows.forEach((_r, i) => {
      const p = place(i, false);
      if (p) barLabels.push(p);
    });
  } else {
    const ranked = rows
      .map((r, i) => ({ i, v: r.value }))
      .filter((x) => x.v > 0)
      .sort((a, b) => b.v - a.v);
    for (const { i } of ranked) {
      if (barLabels.length >= 8) break;
      const p = place(i, true);
      if (p) barLabels.push(p);
    }
  }

  // 横轴刻度：最多 6 个且必含最后一天（等距取样常漏「今天」，今天恰是用户最先看的）
  const labelStep = Math.max(1, Math.ceil(rows.length / 6));
  const marks: number[] = [];
  for (let i = 0; i < rows.length; i += labelStep) marks.push(i);
  if (marks[marks.length - 1] !== rows.length - 1) marks.push(rows.length - 1);

  const hover = hoverIdx != null ? rows[hoverIdx] : null;
  // hover 多行气泡：日期 / 总量+sub / 模型分解（≤8，超出归并一句）
  const tipLines = hover
    ? [
        hover.date,
        `${formatValue(hover.value)}${hover.sub ? ` · ${hover.sub}` : ''}`,
        ...(hover.models ?? [])
          .slice(0, TIP_MODEL_LIMIT)
          .map((m) => `${m.model}: ${fmtTokens(m.tokens, lang)}`),
        (hover.models?.length ?? 0) > TIP_MODEL_LIMIT
          ? `…${(hover.models?.length ?? 0) - TIP_MODEL_LIMIT}`
          : null,
      ].filter(Boolean)
    : [];
  const tipW = Math.min(width - pad.left - 10, 220);
  const tipX = hoverIdx != null
    ? Math.min(pad.left + step * hoverIdx + step / 2 + 10, pad.left + plotW - tipW)
    : 0;

  const useNativeTip = rows.length > 400;

  return (
    <div ref={ref} style={{ width: '100%' }}>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        style={{ width: '100%', height: 'auto', display: 'block' }}
        onMouseLeave={() => setHoverIdx(null)}
      >
        {grid.map((g) => (
          <g key={g.key}>
            <line x1={pad.left} y1={g.y} x2={pad.left + plotW} y2={g.y} stroke="var(--border-color)" strokeWidth="1" />
            <text x={pad.left - 8} y={g.y + 3} textAnchor="end" fill="var(--text-muted)" fontSize="10">
              {g.text}
            </text>
          </g>
        ))}
        {blank ? (
          <text x={width / 2} y={height / 2} textAnchor="middle" fill="var(--text-muted)" fontSize="12">
            —
          </text>
        ) : (
          rows.map((r, i) =>
            r.value <= 0 ? null : (
              <rect
                key={r.date + i}
                x={pad.left + step * i + (step - barW) / 2}
                y={yAt(r.value)}
                width={barW}
                height={Math.max(1, baseY - yAt(r.value))}
                rx={barW > 4 ? 2 : 1}
                fill="var(--accent-color)"
                opacity={hoverIdx == null || hoverIdx === i ? 0.85 : 0.4}
              >
                {useNativeTip ? <title>{`${r.date} ${formatValue(r.value)}`}</title> : null}
              </rect>
            ),
          )
        )}
        {barLabels.map((p, i) => (
          <text
            key={i}
            x={p.x}
            y={p.y}
            textAnchor={p.anchor}
            dominantBaseline={p.rotate ? 'text-after-edge' : undefined}
            fill="var(--text-muted)"
            fontSize="9"
            transform={p.rotate ? `rotate(-90 ${p.x} ${p.y})` : undefined}
            pointerEvents="none"
          >
            {p.text}
          </text>
        ))}
        {marks.map((mi) => {
          const only = rows.length === 1;
          const last = !only && mi === rows.length - 1;
          return (
            <text
              key={mi}
              x={only ? pad.left + plotW / 2 : last ? pad.left + plotW : pad.left + step * mi + step / 2}
              y={height - 8}
              textAnchor={only ? 'middle' : last ? 'end' : mi === 0 ? 'start' : 'middle'}
              fill="var(--text-muted)"
              fontSize="9"
            >
              {(rows[mi].date || '').slice(5).replace('-', '/')}
            </text>
          );
        })}
        {/* hover 热区：整列铺满，扫过任意一列都能读到 */}
        {!useNativeTip &&
          rows.map((_, i) => (
            <rect
              key={`h${i}`}
              x={pad.left + step * i}
              y={pad.top}
              width={step}
              height={plotH}
              fill="transparent"
              onMouseEnter={() => setHoverIdx(i)}
            />
          ))}
        {hoverIdx != null && !useNativeTip && (
          <g pointerEvents="none">
            <line
              x1={pad.left + step * hoverIdx + step / 2}
              y1={pad.top}
              x2={pad.left + step * hoverIdx + step / 2}
              y2={pad.top + plotH}
              stroke="var(--accent-color)"
              strokeDasharray="4 3"
              opacity={0.6}
            />
            <rect
              x={tipX}
              y={pad.top}
              width={tipW}
              height={18 + tipLines.length * 13}
              rx={7}
              fill="var(--card-bg)"
              stroke="var(--border-color)"
              opacity={0.97}
            />
            {tipLines.map((line, li) => (
              <text
                key={li}
                x={tipX + 8}
                y={pad.top + 15 + li * 13}
                fill={li === 0 ? 'var(--text-main)' : 'var(--text-muted)'}
                fontSize={li === 0 ? 11 : 10}
                fontWeight={li === 0 ? 600 : 400}
              >
                {line}
              </text>
            ))}
          </g>
        )}
      </svg>
    </div>
  );
}
