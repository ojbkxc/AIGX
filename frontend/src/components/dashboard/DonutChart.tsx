// 用量环形图（A2A donutView 移植）：内 62%/外 88% 环、中心总量读数、
// 右侧图例「tokens + 占比」、扇区 ≥3% 才画百分比标注、色映射由调用方
// 传入（与 tooltip 色点共用一份，杜绝两处各自按下标取色的串位问题）。
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  DONUT,
  donutSlicePath,
  fmtPct,
  round1,
} from '../../lib/format';

export interface DonutDatum {
  name: string;
  value: number;
  /** tooltip/图例的绝对量读数（缺省用 value 本身） */
  labelValue?: string;
}

interface Props {
  data: DonutDatum[];
  colors: Map<string, string>;
  /** 中心读数的单位说明行（如 tokens / 次请求） */
  totalText: string;
  totalSub: string;
  /** 超过这个条数折叠为「其他」组（图例仍列全量） */
  maxSlices?: number;
  otherName?: string;
}

export default function DonutChart({
  data,
  colors,
  totalText,
  totalSub,
  maxSlices = 9,
  otherName = '其他',
}: Props): JSX.Element | null {
  const { t, i18n } = useTranslation();
  const [hover, setHover] = useState<number | null>(null);
  const rows = data.filter((d) => d.value > 0);
  if (!rows.length) return null;

  // 扇区列表：>maxSlices 时尾部并进「其他」（图例列全量，中心总量不变）
  const head = rows.slice(0, maxSlices);
  const rest = rows.slice(maxSlices);
  const slicesData: DonutDatum[] = rest.length
    ? [...head, { name: otherName, value: rest.reduce((s, d) => s + d.value, 0) }]
    : head;
  const total = rows.reduce((s, d) => s + d.value, 0);

  const size = DONUT.size;
  const cx = size / 2;
  const cy = size / 2;
  const outerR = (size / 2) * DONUT.outer;
  const innerR = (size / 2) * DONUT.inner;

  let angle = 0;
  const slices = slicesData.map((d) => {
    const percent = d.value / total;
    const sweep = percent * 360;
    // 间隙两端各让半，且不超过本段 1/4（否则小扇区被间隙吃没了、图例还在）
    const gap = slicesData.length > 1 ? Math.min(DONUT.padAngle / 2, sweep / 4) : 0;
    const start = angle + gap;
    const end = angle + sweep - gap;
    angle += sweep;
    let label: { x: number; y: number; text: string } | null = null;
    if (percent >= 0.03) {
      const mid = (start + end) / 2;
      const r = innerR + (outerR - innerR) * 0.6;
      const rad = ((mid - 90) * Math.PI) / 180;
      label = {
        x: round1(cx + r * Math.cos(rad)),
        y: round1(cy + r * Math.sin(rad)),
        text: `${Math.round(percent * 100)}%`,
      };
    }
    return { d, percent, start, end, label, visible: end > start };
  });

  const lang = i18n.language;
  const isZh = (lang ?? 'zh').startsWith('zh');
  return (
    <div style={{ display: 'flex', gap: 24, flexWrap: 'wrap', alignItems: 'center' }}>
      <svg viewBox={`0 0 ${size} ${size}`} style={{ width: size, height: size, flexShrink: 0 }}>
        {slices.map((s, i) =>
          s.visible ? (
            <path
              key={i}
              d={donutSlicePath(cx, cy, outerR, innerR, s.start, s.end)}
              fill={colors.get(s.d.name) ?? '#9aa0a6'}
              fillRule={s.end - s.start >= 359.999 ? 'evenodd' : undefined}
              stroke="var(--card-bg)"
              strokeWidth="1"
              opacity={hover === null || hover === i ? 1 : 0.35}
              style={{ transition: 'opacity 0.2s ease', cursor: 'pointer' }}
              onMouseEnter={() => setHover(i)}
              onMouseLeave={() => setHover(null)}
            >
              <title>{`${s.d.name} · ${s.d.labelValue ?? s.d.value} · ${fmtPct(s.percent * 100)}`}</title>
            </path>
          ) : null,
        )}
        {slices.map((s, i) => (s.label && s.visible ? (
          <text
            key={`l${i}`}
            x={s.label.x}
            y={s.label.y}
            textAnchor="middle"
            dominantBaseline="middle"
            fill="#fff"
            fontSize="9"
            fontWeight="700"
            stroke="rgba(0,0,0,0.35)"
            strokeWidth="0.5"
            pointerEvents="none"
          >
            {s.label.text}
          </text>
        ) : null))}
        {/* 中心：默认总量，悬停换成该扇区 */}
        <text x={cx} y={cy - 4} textAnchor="middle" fill="var(--text-main)" fontSize="16" fontWeight="700">
          {hover != null ? fmtPct(slices[hover].percent * 100) : totalText}
        </text>
        <text x={cx} y={cy + 14} textAnchor="middle" fill="var(--text-muted)" fontSize="10">
          {hover != null
            ? (slices[hover].d.name.length > 12 ? slices[hover].d.name.slice(0, 12) + '…' : slices[hover].d.name)
            : (isZh ? totalSub : t('总量'))}
        </text>
      </svg>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 8, flex: 1, minWidth: 200 }}>
        {rows.map((d, i) => {
          const percent = (d.value / total) * 100;
          return (
            <div
              key={`${i}-${d.name}`}
              style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 13 }}
              title={`${d.name} · ${d.labelValue ?? d.value} · ${percent.toFixed(1)}%`}
            >
              <span style={{ width: 12, height: 12, borderRadius: 3, background: colors.get(d.name) ?? '#9aa0a6', flexShrink: 0 }} />
              <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{d.name}</span>
              <span style={{ color: 'var(--text-muted)' }}>{d.labelValue}</span>
              <span style={{ color: 'var(--text-muted)', fontWeight: 600, width: 52, textAlign: 'right' }}>{percent.toFixed(1)}%</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
