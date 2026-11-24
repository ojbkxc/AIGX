/**
 * RoutingCanvasEdges —— 配置画布的 SVG 连线层（对齐 A2A `routing-canvas-edges.tsx`）。
 *
 * 算法逐条照搬 A2A（不给 CSS 里画死线，全部靠实测位置算）：
 *   1. `ResizeObserver` 盯住画布与所有目标节点 → 下一帧重算（rAF 合帧，避免抖动）
 *   2. `MutationObserver` 盯 `data-route-*` 属性变化 → 只重画
 *   3. 位置一律用 `getBoundingClientRect()` 相对画布容器换算（含 CSS 缩放 scaleX/scaleY）
 *   4. 起点 = 源卡右缘中点；终点 = 节点/成员行左缘中点；曲线走三次贝塞尔，
 *      中点取横向 52%（A2A 同值），终点先到 entryX 再拉一条短线到真实终点
 *   5. 起点画实心圆（`routing-canvas-origin`），终点画白心圆（r=3）
 *   6. **目标节点被滚出可视区就不画**（A2A 的 `visible()` 判据，含裁剪容器上下界）
 *
 * 与 A2A 的差异只有两处，都是为了本仓环境/结构，不改变画法：
 *   · 选择器换成 AIGX 的（`.rt-shell-rail` 里的 `[data-route-scope][data-active]`）；
 *   · `ResizeObserver` 在 jsdom 里不存在时跳过观察（单测不必为它打桩），
 *     位置测量仍走真实 DOM，不伪造坐标。
 */
import { useEffect, useRef, useState } from 'react';

type Point = { x: number; y: number };
type Edge = { id: string; d: string; from: Point; to: Point; running: boolean };
type Drawing = { width: number; height: number; edges: Edge[] };

/** 参与重算的容器（A2A 同款清单，换成本仓类名） */
const RESIZE_TARGETS = [
  '[data-route-source]',
  '[data-route-scroll]',
  '[data-route-node]',
  '[data-route-account]',
  '.routing-config-column',
  '.routing-groups-column',
  '.routing-column-head',
  '.routing-canvas-note',
  '.rt-shell-rail',
  '.rt-shell-main',
  '.rt-nav-item',
].join(', ');

/** 会改变连线的属性（A2A 的 attributeFilter 逐项对应） */
const MUTATION_ATTRS = [
  'data-active',
  'data-route-connected',
  'data-route-expanded',
  'data-route-running',
  'data-route-node',
  'data-route-account',
  'data-open',
  'data-closed',
  'hidden',
];

export default function RoutingCanvasEdges({
  root,
}: {
  root: React.RefObject<HTMLDivElement | null>;
}): JSX.Element {
  const svg = useRef<SVGSVGElement>(null);
  const [drawing, setDrawing] = useState<Drawing | null>(null);

  useEffect(() => {
    const canvas = root.current;
    if (!canvas) return;
    let frame = 0;

    const draw = (): void => {
      const card = canvas.querySelector<HTMLElement>('[data-route-source]');
      const groups = canvas.querySelector<HTMLElement>('[data-route-scroll]');
      if (!card || !groups || !canvas.clientWidth || !canvas.clientHeight) {
        setDrawing(null);
        return;
      }
      const bounds = canvas.getBoundingClientRect();
      const scaleX = bounds.width / canvas.offsetWidth || 1;
      const scaleY = bounds.height / canvas.offsetHeight || 1;
      const left = bounds.left + canvas.clientLeft * scaleX;
      const top = bounds.top + canvas.clientTop * scaleY;
      const point = (x: number, y: number): Point => ({ x: (x - left) / scaleX, y: (y - top) / scaleY });
      // 节点中心必须落在裁剪容器与画布的交集里 —— 被滚出去就不画（A2A 同判据）
      const visible = (rect: DOMRect, clip: DOMRect): boolean =>
        rect.width > 0 &&
        rect.height > 0 &&
        rect.top + rect.height / 2 >= Math.max(clip.top, bounds.top) &&
        rect.top + rect.height / 2 <= Math.min(clip.bottom, bounds.bottom);

      const source = card.getBoundingClientRect();
      const sourceY = source.top + source.height / 2;
      const list = groups.getBoundingClientRect();
      const edges: Edge[] = [];
      const connect = (
        id: string,
        from: Point,
        to: Point,
        running: boolean,
        entryX = to.x,
      ): void => {
        // 目标在起点左边（窄屏堆叠 / 未排版）时不画 —— 反向线没有语义
        if (entryX <= from.x) return;
        const mid = from.x + (entryX - from.x) * 0.52;
        const d = `M ${from.x} ${from.y} C ${mid} ${from.y}, ${mid} ${to.y}, ${entryX} ${to.y}`;
        edges.push({
          id,
          from,
          to,
          running,
          d: entryX === to.x ? d : `${d} L ${to.x} ${to.y}`,
        });
      };

      if (visible(source, bounds)) {
        // ① rail 上被选中的作用域 → 源卡（A2A 的 scope 边）
        const selected = canvas.querySelector<HTMLElement>('[data-route-scope][data-active]');
        const rail = canvas.querySelector<HTMLElement>('.rt-shell-rail');
        if (selected && rail) {
          const start = selected.getBoundingClientRect();
          if (visible(start, rail.getBoundingClientRect())) {
            connect(
              'scope',
              point(start.right, start.top + start.height / 2),
              point(source.left, sourceY),
              card.hasAttribute('data-route-running'),
            );
          }
        }

        // ② 源卡 → 右列每个组节点 / 成员行
        if (list.left > source.right) {
          const from = point(source.right, sourceY);
          const nodes = groups.querySelectorAll<HTMLElement>('[data-route-node]');
          for (const node of nodes) {
            const connected = node.hasAttribute('data-route-connected');
            const running = node.hasAttribute('data-route-running');
            if (!connected && !running) continue;
            const nodeBounds = node.getBoundingClientRect();
            if (!nodeBounds.width || !nodeBounds.height) continue;
            if (node.hasAttribute('data-route-expanded')) {
              for (const account of node.querySelectorAll<HTMLElement>('[data-route-account]')) {
                const active = account.hasAttribute('data-route-running');
                if (!active && !(connected && account.hasAttribute('data-route-connected'))) continue;
                const target = account.getBoundingClientRect();
                if (!visible(target, list) || !visible(target, nodeBounds)) continue;
                connect(
                  JSON.stringify([node.dataset.routeNode, account.dataset.routeAccount]),
                  from,
                  point(target.left, target.top + target.height / 2),
                  active,
                  point(nodeBounds.left - 4 * scaleX, 0).x,
                );
              }
            } else {
              const header = node.querySelector('header')?.getBoundingClientRect() || nodeBounds;
              if (!visible(header, list)) continue;
              connect(
                JSON.stringify([node.dataset.routeNode]),
                from,
                point(nodeBounds.left, header.top + header.height / 2),
                running,
              );
            }
          }
        }
      }
      setDrawing({ width: canvas.clientWidth, height: canvas.clientHeight, edges });
    };

    const schedule = (): void => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(draw);
    };

    const resize =
      typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(schedule);
    let observed = new Set<Element>();
    const observeNodes = (): void => {
      if (!resize) return;
      const nodes = new Set<Element>([canvas, ...canvas.querySelectorAll(RESIZE_TARGETS)]);
      for (const node of observed) if (!nodes.has(node)) resize.unobserve(node);
      for (const node of nodes) if (!observed.has(node)) resize.observe(node);
      observed = nodes;
    };
    observeNodes();

    const mutation = new MutationObserver((records) => {
      // 连线层自己的改动不触发重算（否则 setDrawing → 变更 → 再重算，自激）
      if (!records.some((record) => !svg.current?.contains(record.target))) return;
      if (records.some((record) => record.type === 'childList')) observeNodes();
      schedule();
    });
    mutation.observe(canvas, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: MUTATION_ATTRS,
    });

    canvas.addEventListener('scroll', schedule, true);
    window.addEventListener('resize', schedule);
    schedule();
    return () => {
      resize?.disconnect();
      mutation.disconnect();
      cancelAnimationFrame(frame);
      canvas.removeEventListener('scroll', schedule, true);
      window.removeEventListener('resize', schedule);
    };
  }, [root]);

  // 同一坐标的起点只画一颗实心圆（多条边共用同一个源点时堆叠会变粗）
  const origins = new Map<string, Point>();
  for (const edge of drawing?.edges || []) origins.set(`${edge.from.x}:${edge.from.y}`, edge.from);

  return (
    <svg
      ref={svg}
      className="routing-canvas-edges"
      viewBox={drawing ? `0 0 ${drawing.width} ${drawing.height}` : undefined}
      aria-hidden="true"
    >
      {drawing?.edges.map((edge) => (
        <g key={edge.id} className={edge.running ? 'is-running' : undefined}>
          <path d={edge.d} />
          <circle cx={edge.to.x} cy={edge.to.y} r={3} />
        </g>
      ))}
      {[...origins].map(([key, origin]) => (
        <circle key={key} className="routing-canvas-origin" cx={origin.x} cy={origin.y} r={3} />
      ))}
    </svg>
  );
}
