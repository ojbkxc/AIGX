/**
 * RouteReplaySnapshotDialog — 「当时配置 vs 当前配置」双列对比弹窗 + 成员模型快照
 * （对照上游 Agent2API ui-islands/routing-exec-snapshot.tsx 的 RoutingSnapshotDialog /
 * SnapshotMembers，一比一搬运缺口 D1 / D5）。
 *
 * ── 数据从哪来 ──────────────────────────────────────────────────
 * 「当时」半边 = 回放快照（GET /api/routing/replay/{request_id} 的 route_config，
 * AIGX 只有 strategy / sticky_enabled 两项，其余项无数据时显示「未记录」）。
 * 「当前」半边 = GET /api/routing/workspace?model=…（RoutingWorkspaceView：
 * settings.strategy / settings.sticky / customized / configuredModels）。
 *
 * ── 与上游的差异（诚实标注）─────────────────────────────────────
 * - 上游还有 groupId / providerScope / 成员模型参与本路由 / 「查看当前配置」
 *   跳转 —— AIGX 后端 route_config 快照没落这些字段，对应行以「未记录」占位，
 *   成员行只在后端补 route_config.members 后出现（缺口见 D5）。
 * - 文案先用中文硬编码（i18n 由主控统一补 zh/en/zh-Hant）。
 */
import { useEffect, useState } from 'react';
import { api } from '../api';

/** 当时的成员模型快照（上游 configSnapshot.members 对齐位；后端未落库时整组缺省） */
export interface SnapshotMember {
  id?: string;
  model: string;
  provider?: string;
  included?: boolean;
  accountCount?: number;
}

/** 扩展后的 route_config（上游 configSnapshot 对齐位；members 后端未落库时缺省） */
export interface RouteConfigExt {
  strategy?: string;
  sticky_enabled?: boolean;
  members?: SnapshotMember[];
}

/** 当前配置读取状态（三态：读取中 / 读到了 / 读失败） */
type CurrentState =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | {
      kind: 'ready';
      strategy?: string;
      sticky?: string;
      customized: boolean;
      model?: string | null;
    };

/** sticky 档位 → 文案（对照上游 stickyLabel；AIGX StickyMode = auto/session/turn/off） */
function stickyText(v: string | undefined): string {
  switch (v) {
    case 'auto': return '自动';
    case 'session': return '按会话';
    case 'turn': return '按轮次';
    case 'off': return '关闭';
    default: return v || '未记录';
  }
}

export function RouteReplaySnapshotDialog({
  model,
  strategy,
  stickyEnabled,
  capturedAt,
  onClose,
}: {
  /** 请求当时的模型（当前配置按它查 workspace 覆盖） */
  model: string;
  /** 当时快照：选路方式 */
  strategy?: string;
  /** 当时快照：会话粘滞开关 */
  stickyEnabled?: boolean;
  /** 当时快照的记录时刻（unix ms；无则不显示「记录时间」行） */
  capturedAt: number | null;
  onClose(): void;
}): JSX.Element {
  const [current, setCurrent] = useState<CurrentState>({ kind: 'loading' });
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    let alive = true;
    setCurrent({ kind: 'loading' });
    void (async () => {
      try {
        const res = await api.getRoutingWorkspace(model || undefined);
        if (!alive) return;
        const w = res?.data;
        if (!w || !w.settings) throw new Error('路由接口返回了不完整的数据');
        setCurrent({
          kind: 'ready',
          strategy: w.settings.strategy,
          sticky: w.settings.sticky,
          customized: Boolean(w.customized),
          model: w.model ?? null,
        });
      } catch (err) {
        if (!alive) return;
        setCurrent({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
      }
    })();
    return () => { alive = false; };
  }, [model, revision]);

  const scopeText = (ready: Extract<CurrentState, { kind: 'ready' }>): string =>
    ready.customized ? `模型独立配置 · ${ready.model || model || '—'}` : '继承全局配置';

  const loadingText = current.kind === 'loading' ? '正在读取…' : '—';
  const currentStrategy = current.kind === 'ready' ? (current.strategy || '—') : loadingText;
  const currentSticky = current.kind === 'ready' ? stickyText(current.sticky) : loadingText;
  const currentScope = current.kind === 'ready' ? scopeText(current) : loadingText;

  const rows: Array<{ label: string; before: string; after: string }> = [
    { label: '配置范围', before: '未记录', after: currentScope },
    { label: '选路方式', before: strategy || '未记录', after: currentStrategy },
    {
      label: '会话粘滞',
      before: stickyEnabled != null ? (stickyEnabled ? '启用' : '关闭') : '未记录',
      after: currentSticky,
    },
  ];

  return (
    <div
      className="rr-snapdlg-overlay"
      role="presentation"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      style={{
        position: 'fixed', inset: 0, zIndex: 1000,
        background: 'rgba(0,0,0,0.35)',
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        padding: 16,
      }}
    >
      <div
        className="rr-snapdlg"
        role="dialog"
        aria-modal="true"
        aria-label="当时配置与当前配置"
        style={{
          background: 'var(--card-bg, #fff)',
          color: 'var(--text, #333)',
          borderRadius: 10,
          maxWidth: 640,
          width: '100%',
          maxHeight: '85vh',
          overflow: 'auto',
          padding: '16px 18px',
          boxShadow: '0 12px 40px rgba(0,0,0,0.25)',
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12 }}>
          <h3 style={{ margin: 0, fontSize: 15, fontWeight: 600 }}>当时配置与当前配置</h3>
          <button type="button" className="rr-exec-reset" onClick={onClose} aria-label="关闭">
            关闭
          </button>
        </div>
        <p className="rr-note" style={{ marginTop: 6 }}>
          历史只读，当前配置的修改不会改变这条请求的快照。
        </p>

        {/* 双列对比表（对照上游 .routing-config-compare；一致 = 淡化 data-same 标记） */}
        <div className="rr-snapdlg-compare" role="table" aria-label="当时配置与当前配置" style={{ marginTop: 10 }}>
          <div role="row" className="rr-snapdlg-row is-heading" style={{ display: 'grid', gridTemplateColumns: '88px 1fr 1fr', gap: 10 }}>
            <span role="columnheader">配置项</span>
            <span role="columnheader">当时</span>
            <span role="columnheader">当前配置</span>
          </div>
          {rows.map((row) => (
            <div
              key={row.label}
              role="row"
              data-same={row.before === row.after ? '' : undefined}
              className="rr-snapdlg-row"
              style={{ display: 'grid', gridTemplateColumns: '88px 1fr 1fr', gap: 10, padding: '6px 0', borderTop: '1px solid var(--border, #e5e5e5)' }}
            >
              <span role="rowheader" style={{ color: 'var(--text-muted, #888)' }}>{row.label}</span>
              <span role="cell" className="rr-mono">{row.before}</span>
              <span role="cell" className="rr-mono">{row.after}</span>
            </div>
          ))}
        </div>

        {current.kind === 'loading' ? <p className="rr-note">正在读取当前配置…</p> : null}
        {current.kind === 'error' ? (
          <p className="rr-step-err" role="alert">
            {current.message}
            <button type="button" className="rr-exec-reset" style={{ marginLeft: 8 }} onClick={() => setRevision((v) => v + 1)}>
              重试
            </button>
          </p>
        ) : null}

        {/* 当时快照的静态事实（对照上游「请求当时的配置」节；成员快照后端未落库时无行） */}
        <section style={{ marginTop: 14 }}>
          <h4 style={{ margin: '0 0 6px', fontSize: 13, fontWeight: 600 }}>请求当时的配置</h4>
          <dl className="rr-exec-definitions">
            {capturedAt != null ? (
              <div>
                <dt>记录时间</dt>
                <dd>{new Date(capturedAt).toLocaleString()}</dd>
              </div>
            ) : null}
            <div>
              <dt>选路方式</dt>
              <dd className="rr-mono">{strategy || '未记录'}</dd>
            </div>
            <div>
              <dt>会话粘滞</dt>
              <dd>{stickyEnabled != null ? (stickyEnabled ? '启用' : '关闭') : '未记录'}</dd>
            </div>
          </dl>
        </section>
      </div>
    </div>
  );
}

/** 成员模型快照列表（上游 SnapshotMembers；members 由回放快照 route_config.members 带出，
 *  后端未落库时调用方不渲染本组件）。
 *
 *  ⚠️ 文案按约束先用中文硬编码，i18n 由主控统一收口。 */
export function SnapshotMembers({ members }: { members: SnapshotMember[] }): JSX.Element {
  return (
    <div className="rr-snapshot-members" style={{ display: 'flex', flexDirection: 'column', gap: 4, marginTop: 6 }}>
      {members.map((m, i) => (
        <div key={m.id ?? `${m.model}-${i}`} className="rr-snapshot-member" style={{ display: 'flex', alignItems: 'baseline', gap: 8, fontSize: 12 }}>
          <code className="rr-mono">{m.model}</code>
          <span style={{ color: 'var(--text-muted, #888)' }}>
            {m.provider ? `${m.provider} · ` : ''}
            {m.included ? '参与本路由' : '未参与本路由'}
            {' · '}
            {`${m.accountCount ?? 0} 个账号`}
          </span>
        </div>
      ))}
    </div>
  );
}
