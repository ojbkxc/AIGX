// 365 天消费热力图（A2A heatmapView 移植）：GitHub 贡献图布局（列=周、
// 行=星期、周一起算）、格边长按容器宽反推（7–26px 夹取）、5 档色阶阈值
// 随窗口 token 峰值现算。悬停/点选读数：日期 · 请求数 · 金额 · token ·
// 按模型展开（≤8 个，超出归并一句）。点选态给触屏用（没有 hover 时点一下也能读）。
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { fmtCost, fmtInt, fmtTokens, heatLevelText, heatLevelsFor, TIP_MODEL_LIMIT } from '../../lib/format';
import { useMeasuredWidth } from './TrendBars';
import type { HeatmapCell } from '../../hooks/useDashboard';

const TIP_DAY_LIMIT = TIP_MODEL_LIMIT;

export default function Heatmap({ data, lang }: { data: HeatmapCell[]; lang?: string }): JSX.Element | null {
  const [ref, measured] = useMeasuredWidth<HTMLDivElement>();
  const { t } = useTranslation();
  const [picked, setPicked] = useState<HeatmapCell | null>(null);
  const [hovered, setHovered] = useState<HeatmapCell | null>(null);
  if (!data.length) return null;
  const boxWidth = Math.max(measured || 720, 320);

  const isZh = (lang ?? 'zh').startsWith('zh');
  const view = useMemo(() => {
    const padToMonday = (key: string): number => {
      const [y, m, d] = key.split('-').map(Number);
      return (new Date(y, m - 1, d).getDay() + 6) % 7;
    };
    const sorted = [...data].filter((x) => x.date).sort((a, b) => (a.date! < b.date! ? -1 : 1));
    const first = sorted[0]?.date ?? '';
    const pad = first ? padToMonday(first) : 0;
    const cells: Array<HeatmapCell | null> = new Array(pad).fill(null).concat(sorted);
    const cols = Math.ceil(cells.length / 7);
    const gap = 3;
    const labelW = 20;
    const fit = Math.floor((boxWidth - labelW - gap * (cols - 1)) / cols);
    const size = Math.max(7, Math.min(26, fit));
    const step = size + gap;
    const topH = size + 3;
    const gridW = labelW + cols * step - gap;
    const offsetX = Math.max(0, Math.round((boxWidth - gridW) / 2));
    // 分档看 token，「有没有用过」看请求数（全失败的日子 token=0 但不该画成空白）
    const thresholds = heatLevelsFor(Math.max(...data.map((d) => d.tokens ?? 0), 0));
    const levelOf = (tokens: number): number =>
      thresholds.findIndex((limit) => tokens <= limit);
    const out: Array<HeatmapCell & { x: number; y: number; level: number }> = [];
    for (let c = 0; c < cols; c += 1) {
      for (let r = 0; r < 7; r += 1) {
        const day = cells[c * 7 + r];
        if (!day) continue;
        out.push({ ...day, x: offsetX + labelW + c * step, y: topH + r * step, level: Math.max(0, levelOf(day.tokens ?? 0)) });
      }
    }
    // 月份标签落在「该列最早一天所属月份」变化的列（<3 列间距时跳过）
    const months: Array<{ x: number; text: string }> = [];
    let lastMonth = -1;
    let lastCol = -9;
    for (let c = 0; c < cols; c += 1) {
      const head = cells.slice(c * 7, c * 7 + 7).find(Boolean);
      if (!head?.date) continue;
      const month = Number(head.date.slice(5, 7)) - 1;
      if (month === lastMonth) continue;
      lastMonth = month;
      if (c - lastCol < 3) continue;
      lastCol = c;
      months.push({ x: offsetX + labelW + c * step, text: isZh ? `${month + 1}月` : `${new Date(2000, month, 1).toLocaleString('en', { month: 'short' })}` });
    }
    return { out, months, thresholds, size, height: topH + 7 * step - gap + 2, width: Math.max(gridW, boxWidth) };
  }, [data, boxWidth, isZh]);

  const COLORS = ['var(--bg-color)', 'rgba(47,111,237,0.25)', 'rgba(47,111,237,0.45)', 'rgba(47,111,237,0.7)', 'rgb(47,111,237)'];
  const readCell = hovered ?? picked;
  const WEEKDAYS: Array<[number, string]> = isZh ? [[0, '一'], [2, '三'], [4, '五']] : [[0, 'M'], [2, 'W'], [4, 'F']];

  return (
    <div ref={ref} style={{ width: '100%' }}>
      <svg viewBox={`0 0 ${view.width} ${view.height}`} style={{ width: '100%', height: 'auto', display: 'block' }}>
        {view.out.map((c) => (
          <rect
            key={c.date}
            x={c.x}
            y={c.y}
            width={view.size}
            height={view.size}
            rx={3}
            fill={COLORS[c.level] ?? COLORS[0]}
            stroke={readCell?.date === c.date ? 'var(--accent-color)' : 'transparent'}
            strokeWidth={1.5}
            style={{ cursor: 'pointer' }}
            onMouseEnter={() => setHovered(c)}
            onMouseLeave={() => setHovered(null)}
            onClick={() => setPicked((p) => (p?.date === c.date ? null : c))}
          />
        ))}
        {view.months.map((m, i) => (
          <text key={i} x={m.x} y={view.size} fill="var(--text-muted)" fontSize="10">{m.text}</text>
        ))}
        {WEEKDAYS.map(([r, label]) => (
          <text key={label} x={20 - 6} y={view.size + 3 + r * (view.size + 3) + view.size / 2} fill="var(--text-muted)" fontSize="10" dominantBaseline="middle">
            {label}
          </text>
        ))}
      </svg>
      {/* 读数行：悬停/点选的明细（含按模型展开），否则图例 */}
      <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 8, whiteSpace: 'pre-wrap' }}>
        {readCell ? (
          `${readCell.date} · ${fmtInt(readCell.count ?? 0, lang)} ${isZh ? t('次') : 'req'} · ${fmtCost(readCell.cost, lang)} · ${fmtTokens(readCell.tokens, lang)} tokens` +
          (readCell.models?.length
            ? '\n' +
              readCell.models
                .slice(0, TIP_DAY_LIMIT)
                .map((m) => `${m.model}: ${fmtTokens(m.tokens, lang)}`)
                .concat(
                  readCell.models.length > TIP_DAY_LIMIT
                    ? [`…${readCell.models.length - TIP_DAY_LIMIT} ${isZh ? t('个模型') : 'more'}`]
                    : [],
                )
                .join('\n')
            : '')
        ) : (
          [0, 1, 2, 3, 4].map((i) => heatLevelText(i, view.thresholds, lang)).map((text, i) => (
            <span key={i} style={{ marginRight: 10 }}>
              <span
                style={{
                  display: 'inline-block',
                  width: 10,
                  height: 10,
                  borderRadius: 2,
                  background: COLORS[i],
                  verticalAlign: -1,
                  marginRight: 4,
                  border: '1px solid var(--border-color)',
                }}
              />
              {text}
            </span>
          ))
        )}
      </div>
    </div>
  );
}
