/**
 * 「这次请求被谁改写过」—— 特殊请求路由（压缩 / 图片兼容 / 标题生成）的改写快照判据。
 *
 * 移植自 A2A `ui-islands/routing-exec-rewrite.tsx` 的 `requestRewrite`，两处调用方共用：
 *   · 请求追踪的详情（`RoutingTrace`）—— 改写块排在承载之前；
 *   · 特殊路由右栏「最近命中」（`special/SpecialHits`）—— 按 kind 筛出本条路由的命中。
 *
 * ── 数据从哪来 ────────────────────────────────────────────────
 * 后端在选路之前就把改写记录写进 route 快照（`compactRoute` / `titleRoute`），
 * 图片兼容则由 `visionCompatibility` 表达（记录读图调用与接管方式）。
 * 三者都没有时返回 null —— 普通请求不该多出一个空壳。
 */
import type { RoutingTraceRow } from '../../types/routing-workspace';

type T = (key: string, vars?: Record<string, string | number>) => string;

export type RewriteKind = 'compact' | 'title' | 'vision';

export type Rewrite = {
  kind: RewriteKind;
  label: string;
  from: string;
  to: string;
  detail: string;
};

const CLIENT_LABEL: Record<string, string> = {
  zcode: 'ZCode',
  'claude-code': 'Claude Code',
  codex: 'Codex',
};

export const clientText = (client: string | undefined, t: T): string =>
  client ? CLIENT_LABEL[client] || client : t('客户端未记录');

/** 请求的客户端模型（改写块的「客户端模型」一格）。 */
export function rewriteSourceModel(row: RoutingTraceRow): string {
  return row.clientModel || row.model || row.upstreamModel || '';
}

export function requestRewrite(row: RoutingTraceRow, t: T): Rewrite | null {
  const route = row.route as (NonNullable<RoutingTraceRow['route']> & {
    compactRoute?: { source?: string; target?: string; client?: string } | null;
    titleRoute?: { source?: string; target?: string; client?: string } | null;
    visionCompatibility?: {
      mode?: string;
      calls?: unknown[];
      target?: { model?: string } | null;
    } | null;
  }) | null | undefined;
  if (!route) return null;

  const compact = route.compactRoute;
  if (compact?.source && compact.target) {
    return {
      kind: 'compact',
      label: t('压缩上下文'),
      from: compact.source,
      to: compact.target,
      detail: t('{{client}} 的上下文压缩请求', { client: clientText(compact.client, t) }),
    };
  }
  const title = route.titleRoute;
  if (title?.source && title.target) {
    return {
      kind: 'title',
      label: t('标题生成'),
      from: title.source,
      to: title.target,
      detail: t('{{client}} 的标题请求', { client: clientText(title.client, t) }),
    };
  }
  const vision = route.visionCompatibility;
  if (vision?.mode) {
    // 图片兼容不改「请求的模型」：辅助识图是另起一次读图调用，整体接管才是换模型。
    const calls = Array.isArray(vision.calls) ? vision.calls.length : 0;
    const assist = vision.mode === 'assist_call' || vision.mode === 'assist';
    return {
      kind: 'vision',
      label: t('图片兼容'),
      from: rewriteSourceModel(row),
      to: assist ? t('辅助识图 · {{n}} 次读图', { n: calls }) : (vision.target?.model || t('视觉模型接管')),
      detail: assist ? t('读图结果交回原模型继续回答') : t('整次请求交给视觉模型'),
    };
  }
  return null;
}
