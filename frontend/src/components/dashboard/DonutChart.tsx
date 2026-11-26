// 用量环形图（A2A donutView 移植）：内 62%/外 88% 环、中心总量读数、
// 右侧图例「tokens + 占比」、扇区 ≥3% 才画百分比标注、色映射由调用方
// 传入（与 tooltip 色点共用一份，杜绝两处各自按下标取色的串位问题）。
// tooltip：跟随鼠标的自定义浮层（纯 React state + CSS 定位，无第三方依赖），
// 替代原生 <title>（1s 延迟且不可样式化）；图例 hover 联动高亮对应扇区。
// 尺寸：容器 max-width 100% 按比例缩放，窄屏不再溢出（原固定 220px）。
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
  /**
   * 占比低于该百分比（0-100）的扇区并进「其他」（A2A usage-donuts 的
   * `settings.threshold`，`donutView` 里 `percentage < threshold/100` 同一判据）。
   * 0 = 不按阈值合并（此时行为与引入该参数之前逐字一致，仍只受 maxSlices 约束）。
   */
  minPercent?: number;
}

export default function DonutChart({
  data,
  colors,
  totalText,
  totalSub,
  maxSlices = 9,
  otherName = '其他',
  minPercent = 0,
}: Props): JSX.Element | null {
  const { t, i18n } = useTranslation();
  const [hover, setHover] = useState<number | null>(null);
  /** 自定义 tooltip 的相对定位（跟随鼠标，仅 wrapper 内坐标） */
  const [tip, setTip] = useState<{ x: number; y: number } | null>(null);
  const rows = data.filter((d) => d.value > 0);
  if (!rows.length) return null;

  // 扇区装配：先按阈值并小项，再按 maxSlices 兜底收尾（两段都并进同一个「其他」）。
  // - minPercent>0：占比 < minPercent% 的项归「其他（N 项）」（A2A donutView 的
  //   `percentage < threshold/100` 同判据）；
  // - maxSlices：扇区条数上限（默认 9；调用方可显式传更小值），尾部同样并进「其他」。
  // 图例始终列全量（rows），折叠只作用于饼图本身 —— 与 A2A「右侧列表完整保留、
  // 总量与占比不变」一致。
  const total = rows.reduce((s, d) => s + d.value, 0);
  const otherDatum = (members: DonutDatum[]): DonutDatum => ({
    name: members.length > 1 ? t('{{name}}（{{n}} 项）', { name: otherName, n: members.length }) : otherName,
    value: members.reduce((s, d) => s + d.value, 0),
  });

  type Bucket = { datum: DonutDatum; members: DonutDatum[] };
  const minShare = minPercent > 0 ? minPercent / 100 : 0;
  const buckets: Bucket[] = [];
  const small: DonutDatum[] = [];
  for (const d of rows) {
    if (minShare > 0 && d.value / total < minShare) small.push(d);
    else buckets.push({ datum: d, members: [d] });
  }
  if (small.length) buckets.push({ datum: otherDatum(small), members: small });
  // 上限收尾：保留既有「其他」，把被挤出的项并进去（不新造第二个「其他」）
  if (buckets.length > maxSlices) {
    // **留一位给「其他」**：曾经无小项时 `keep = maxSlices`，`splice` 后剩
    // maxSlices 个，收尾再 push 一个「其他」就成了 maxSlices + 1 —— 声明上限 9
    // 却画出 10 片。有/无小项统一成 `maxSlices - 1`，两个分支都不越过上限。
    const keep = Math.max(1, maxSlices - 1);
    const tail = buckets.splice(keep);
    const members = tail.flatMap((b) => b.members);
    const last = buckets[buckets.length - 1];
    if (small.length && last) {
      last.members = [...last.members, ...members];
      last.datum = otherDatum(last.members);
    } else {
      buckets.push({ datum: otherDatum(members), members });
    }
  }
  const slicesData = buckets.map((b) => b.datum);
  // 图例行 → 扇区下标（合并后同一扇区可能对应多行）
  const sliceOf = new Map<DonutDatum, number>();
  buckets.forEach((b, si) => b.members.forEach((m) => sliceOf.set(m, si)));

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
  // 悬停下标可能因阈值/条数变化落到已消失的扇区上（改设置时鼠标还停着），
  // 取用时统一判界，避免越界读。
  const active = hover != null && hover < slices.length ? slices[hover] : null;
  /** 判界后的悬停下标（扇区可能因设置变化消失，透明度/高亮都按它取） */
  const hoverIdx = active ? hover : null;
  const sliceIdxOf = (rowIdx: number): number => {
    const si = sliceOf.get(rows[rowIdx]);
    return si === undefined ? slicesData.length - 1 : si;
  };
  return (
    <div
      style={{ display: 'flex', gap: 24, flexWrap: 'wrap', alignItems: 'center', position: 'relative' }}
      onMouseMove={(e) => {
        const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
        setTip({ x: e.clientX - rect.left, y: e.clientY - rect.top });
      }}
      onMouseLeave={() => { setHover(null); setTip(null); }}
    >
      <div style={{ width: size, maxWidth: '100%', flexShrink: 0 }}>
        <svg viewBox={`0 0 ${size} ${size}`} style={{ width: '100%', height: 'auto', display: 'block' }}>
        {slices.map((s, i) =>
          s.visible ? (
            <path
              key={i}
              d={donutSlicePath(cx, cy, outerR, innerR, s.start, s.end)}
              fill={colors.get(s.d.name) ?? '#9aa0a6'}
              fillRule={s.end - s.start >= 359.999 ? 'evenodd' : undefined}
              stroke="var(--card-bg)"
              strokeWidth="1"
              opacity={hoverIdx === null || hoverIdx === i ? 1 : 0.35}
              style={{ transition: 'opacity 0.2s ease', cursor: 'pointer' }}
              onMouseEnter={() => setHover(i)}
              onMouseLeave={() => setHover(null)}
            >
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
          {active ? fmtPct(active.percent * 100) : totalText}
        </text>
        <text x={cx} y={cy + 14} textAnchor="middle" fill="var(--text-muted)" fontSize="10">
          {active
            ? (active.d.name.length > 12 ? active.d.name.slice(0, 12) + '…' : active.d.name)
            : (isZh ? totalSub : t('总量'))}
        </text>
        </svg>
      </div>
      {/* 跟随式 tooltip：扇区与图例 hover 共用（原生 <title> 有 ~1s 延迟且不可样式化） */}
      {active && tip && (
        <div
          className="donut-tip"
          style={{
            position: 'absolute',
            left: Math.min(tip.x + 12, 320),
            top: tip.y + 12,
            background: 'var(--card-bg)',
            border: '1px solid var(--border-color)',
            borderRadius: 8,
            padding: '6px 10px',
            fontSize: 12,
            color: 'var(--text-main)',
            boxShadow: 'var(--shadow)',
            pointerEvents: 'none',
            whiteSpace: 'nowrap',
            zIndex: 20,
          }}
        >
          <span style={{ display: 'inline-block', width: 10, height: 10, borderRadius: 2, background: colors.get(active.d.name), marginRight: 6, verticalAlign: -1 }} />
          {active.d.name} · {active.d.labelValue ?? active.d.value} · {fmtPct(active.percent * 100)}
        </div>
      )}
      <div style={{ display: 'flex', flexDirection: 'column', gap: 8, flex: 1, minWidth: 200 }}>
        {rows.map((d, i) => {
          const percent = (d.value / total) * 100;
          return (
            <div
              key={`${i}-${d.name}`}
              className="donut-legend-row"
              style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 13, cursor: 'pointer', borderRadius: 4, padding: '2px 4px' }}
              onMouseEnter={() => { setHover(sliceIdxOf(i)); setTip(null); }}
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
