/**
 * 实时面板「此刻队列」（移植自 A2A `ui-islands/routing-live-panel.tsx`）。
 *
 * ── 这是什么 ──────────────────────────────────────────────────
 * 服务端只读预演：给定一只模型，后端算出现在这一秒的候选顺序与被跳过的账号，
 * 面板只把它画出来。它**不推进轮转**，所以可以随时刷新而不会影响真实请求。
 * 两种形态共用同一份数据：
 *   · 完整形态（`compact=false`）—— 右栏「此刻队列」：队首 + 可用候选 + 被跳过 + 脚注；
 *   · 紧凑形态（`compact=true`）—— 页内摘要：前三名 + 「n 可用 · m 跳过」。
 *
 * ── 与上游的字段差（AIGX 后端当前不下发）─────────────────────
 *   · `entry.providerLabel` / `entry.model` / `entry.resetAt` / `entry.candidateId`：
 *     AIGX 的 `RoutingQueueEntryItem` 只有 `id` / `name` / `provider` / `state` / `reason`，
 *     所以候选行右侧只显示提供商，恢复时间与成员模型不显示（不填假值）。
 *   · `queue.strategy` / `queue.sticky` 有，文案用 AIGX 自己的枚举词
 *     （weighted / round_robin / ranked；auto / session / turn / off）。
 *
 * ── 挂载现状 ─────────────────────────────────────────────────
 * 上游这份面板**没有任何调用方**（`RoutingLivePanel` 只被自己定义），AIGX 侧同样
 * 只提供组件、不改页面挂载点（`pages/Routing.tsx` 不在本波写入范围）。
 * 谁要挂，按 `RoutingLivePanel` 的签名接 `api.getRoutingWorkspace(model)` 即可。
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw } from 'lucide-react';
import { Badge, Button } from '../ui';
import type { RoutingQueueEntryItem } from '../../types';
import type { RoutingWorkspaceView } from '../../types/routing-workspace';
import { execStickyLabel, execStrategyLabel } from './trace-exec-model';
import './trace-exec.css';

type Queue = NonNullable<RoutingWorkspaceView['queue']>;

export default function RoutingLiveQueue({
  workspace,
  model,
  loading,
  error,
  dirty,
  globalScope,
  modelsAvailable,
  onOpenScope,
  onRefresh,
  preview = false,
  compact = false,
}: {
  workspace: Pick<RoutingWorkspaceView, 'customized' | 'queue'> | null;
  model: string;
  preview?: boolean;
  compact?: boolean;
  loading: boolean;
  error: string;
  dirty: boolean;
  globalScope: boolean;
  modelsAvailable: boolean;
  onOpenScope(model: string): void;
  onRefresh(): void;
}): JSX.Element {
  const { t } = useTranslation();
  // 恢复时间要跟着秒走，所以每秒重算一次「还剩多久」。
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const queue: Partial<Queue> | undefined = workspace?.queue;
  const customized = Boolean(workspace?.customized);
  const available: RoutingQueueEntryItem[] = Array.isArray(queue?.ordered) ? queue!.ordered! : [];
  const blocked: RoutingQueueEntryItem[] = Array.isArray(queue?.blocked) ? queue!.blocked! : [];
  const next = available[0];
  const badge = preview
    ? { tone: 'warning' as const, text: t('草稿预演') }
    : dirty
      ? { tone: 'warning' as const, text: t('按已保存配置') }
      : customized
        ? { tone: 'neutral' as const, text: t('独立配置') }
        : { tone: 'success' as const, text: t('继承全局') };

  const side = (entry: RoutingQueueEntryItem) => (
    <div className="rt-live-side">
      <span>{entry.provider || '—'}</span>
      {entry.state ? <span className="rt-live-reset">{entry.state}</span> : null}
    </div>
  );

  const scopeBanner = globalScope && customized && workspace ? (
    <div className="rt-live-banner">
      <b>{t('{{model}} 有独立配置', { model })}</b>
      <span>{t('左侧修改的是全局默认，不会影响它。')}</span>
      {compact ? (
        <Button variant="secondary" size="sm" onClick={() => onOpenScope(model)}>
          {t('打开该模型配置')}
        </Button>
      ) : (
        <Button variant="outline" size="sm" onClick={() => onOpenScope(model)}>
          {t('打开该模型配置')}
        </Button>
      )}
    </div>
  ) : null;

  if (compact) {
    return (
      <div className="rt-live-summary" aria-label={t('草稿预演')}>
        {scopeBanner}
        {loading ? (
          <div className="rt-live-loading">
            <Loader2 size={13} className="spin" />
            {t('计算中…')}
          </div>
        ) : error ? (
          <p className="rt-live-error" role="alert">
            {error}
            <Button variant="secondary" size="sm" onClick={onRefresh}>
              {t('重试')}
            </Button>
          </p>
        ) : workspace ? (
          <>
            {available.slice(0, 3).map((entry, index) => (
              <div className={`rt-live-preview-row${index === 0 ? ' is-first' : ''}`} key={entry.id}>
                <span className="rt-live-rank">{String(index + 1).padStart(2, '0')}</span>
                <div className="rt-live-preview-main">
                  <b title={entry.name}>{entry.name}</b>
                  <span title={entry.reason}>{entry.provider || '—'}</span>
                </div>
                <Badge tone={index === 0 ? 'info' : 'neutral'}>
                  {index === 0 ? t('预演首选') : t('候选')}
                </Badge>
              </div>
            ))}
            {!available.length ? <p className="rt-live-empty">{t('没有可用账号')}</p> : null}
            <p className="rt-live-counts">
              {t('{{available}} 可用 · {{blocked}} 跳过', {
                available: available.length,
                blocked: blocked.length,
              })}
            </p>
          </>
        ) : (
          <p className="rt-live-empty">
            {modelsAvailable ? t('选择预览模型后查看队列') : t('还没有可用模型，先在账号页添加账号')}
          </p>
        )}
      </div>
    );
  }

  return (
    <aside className="rt-live-panel" aria-label={t('此刻队列')}>
      <div className="rt-live-head">
        <b>{t('此刻队列')}</b>
        <span className="rt-live-badge">
          <Badge tone={badge.tone}>{badge.text}</Badge>
        </span>
        <span className="rt-live-model">{model}</span>
        <Button
          variant="secondary"
          size="sm"
          className="rt-live-refresh"
          onClick={onRefresh}
          title={t('刷新')}
          aria-label={t('刷新')}
        >
          <RefreshCw size={13} />
        </Button>
      </div>

      {scopeBanner}

      {loading && !workspace ? (
        <div className="rt-live-loading">
          <Loader2 size={13} className="spin" />
        </div>
      ) : null}
      {error ? (
        <p className="rt-live-error" role="alert">
          {error}
        </p>
      ) : null}
      {!loading && !error && !workspace ? (
        <div className="rt-live-empty">
          {modelsAvailable ? t('选择预览模型后查看队列') : t('还没有可用模型，先在账号页添加账号')}
        </div>
      ) : null}

      {workspace ? (
        <>
          <div className="rt-live-summary-block">
            {next ? (
              <>
                <div className="rt-live-eyebrow">{preview ? t('预演首选') : t('当前可用队首')}</div>
                <div className="rt-live-name">{next.name || next.id}</div>
                <p className="rt-live-reason">{next.provider || '—'}</p>
                <p className="rt-live-reason">{next.reason || ''}</p>
              </>
            ) : (
              <>
                <div className="rt-live-eyebrow">{t('下一条请求')}</div>
                <div className="rt-live-name is-empty">{t('没有可用账号')}</div>
                <p className="rt-live-reason">{t('先处理凭证、限额或参与范围。')}</p>
              </>
            )}
            <div className="rt-live-counts">
              <span>
                {t('{{available}} 可用 · {{blocked}} 跳过', {
                  available: available.length,
                  blocked: blocked.length,
                })}
              </span>
              <span>
                {execStrategyLabel(queue?.strategy, t)} ·{' '}
                {t('粘滞 {{mode}}', { mode: execStickyLabel(queue?.sticky, t) })}
              </span>
            </div>
          </div>

          {available.length ? (
            <>
              <div className="rt-live-subhead">
                <span>{t('可用候选')}</span>
                <span>{available.length}</span>
              </div>
              <div className="rt-live-list">
                {available.map((entry, index) => (
                  <div className="rt-live-row" key={entry.id}>
                    <span className="rt-live-rank">{index + 1}</span>
                    <div className="rt-live-main">
                      <div className="rt-live-title">
                        <b>{entry.name}</b>
                        {entry.state ? <span className="rt-live-state">{entry.state}</span> : null}
                      </div>
                      <p className="rt-live-row-reason">{entry.reason || ''}</p>
                    </div>
                    {side(entry)}
                  </div>
                ))}
              </div>
            </>
          ) : null}

          {blocked.length ? (
            <>
              <div className="rt-live-subhead">
                <span>{t('被跳过')}</span>
                <span>{blocked.length}</span>
              </div>
              <div className="rt-live-list">
                {blocked.map((entry) => (
                  <div className="rt-live-row is-blocked" key={entry.id}>
                    <span className="rt-live-rank">–</span>
                    <div className="rt-live-main">
                      <div className="rt-live-title">
                        <b>{entry.name}</b>
                        {entry.state ? <span className="rt-live-state">{entry.state}</span> : null}
                      </div>
                      <p className="rt-live-row-reason">{entry.reason || ''}</p>
                    </div>
                    {side(entry)}
                  </div>
                ))}
              </div>
            </>
          ) : null}

          <div className="rt-live-foot">
            {dirty ? <p>{t('有未保存修改，预演仍按已保存配置。')}</p> : null}
            <p>{t('只读预演，不会推进轮转；保存后才会影响真实请求。')}</p>
            <p>{t('真实请求还会考虑当时的会话绑定、并发与限额状态。')}</p>
          </div>
          {/* now 只用于「恢复时间」倒计时的刷新节奏；AIGX 的队列条目不带 resetAt，
              这里保留订阅是为了以后后端补上该字段时不必再改结构。 */}
          <span hidden data-tick={now} />
        </>
      ) : null}
    </aside>
  );
}
