import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Copy, FlaskConical, Loader2, X } from 'lucide-react';
import { api } from '../api';
import { useToast } from './Toast';
import { Switch } from './ui';
import type { ManagedModel, ModelTestResult } from '../types';

/** 与后端 DEFAULT_PROMPT 同句 */
const DEFAULT_PROMPT = '你好';

/** 思考等级候选（后端原样透传给 bridge；off = 显式关闭） */
const REASONING_OPTIONS = ['low', 'medium', 'high', 'off'] as const;

/** 一个测试槽：行来源（账号/渠道）+ 在途状态 + 结论 */
interface Slot {
  key: string;
  kind: string;
  name: string;
  account_id?: string;
  channel_id: string;
  running: boolean;
  result?: ModelTestResult;
  /** 传输层错误（请求根本没发出/网关 5xx） */
  transport?: string;
}

export interface ModelTestTarget {
  provider: string;
  providerLabel: string;
  model: ManagedModel;
}

interface Props {
  target: ModelTestTarget;
  onClose: () => void;
}

function formatMs(ms?: number | null): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

/** 按上游 HTTP 状态给下一步建议（对照 Agent2API STATUS_HINT） */
function statusHint(status: number | undefined, t: (s: string) => string): string {
  switch (status) {
    case 401:
    case 403:
      return t('凭证无效或权限不足：检查账号凭证 / 供应商密钥是否过期。');
    case 404:
      return t('上游不认识这个模型名：刷新模型清单，或检查供应商映射。');
    case 429:
      return t('上游限流或额度不足：稍后再试，或检查账号余额。');
    case 504:
      return t('上游超时：模型可能正在冷启动，或网络链路不稳定。');
    default:
      if (status && status >= 500) return t('上游服务错误：通常是供应商侧故障，稍后重试。');
      return '';
  }
}

/**
 * 模型测试弹窗（对照 Agent2API model-test-dialog）。
 *
 * 两层结构：参数层（提示词 / 思考等级 / 流式 / 来源多选）+ 结果层（按来源分块，
 * 每个勾选的来源并发一次 POST /api/models/test，结果回来就地替换骨架）。
 */
export default function ModelTestDialog({ target, onClose }: Props): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const { model } = target;

  const [systemPrompt, setSystemPrompt] = useState('');
  const [prompt, setPrompt] = useState(DEFAULT_PROMPT);
  const [reasoning, setReasoning] = useState('');
  const [stream, setStream] = useState(true);
  /** 勾选的来源 key 集合（默认勾第一个账号来源，没有账号则第一个来源） */
  const [picked, setPicked] = useState<Set<string>>(() => {
    const sources = model.sources ?? [];
    const first = sources.find((s) => s.kind === 'account') ?? sources[0];
    return first ? new Set([sourceKey(first)]) : new Set();
  });

  const [slots, setSlots] = useState<Slot[] | null>(null);
  const [running, setRunning] = useState(false);
  /** 读秒（有在途时 400ms 一跳） */
  const [elapsed, setElapsed] = useState(0);
  const runSeq = useRef(0);
  /** 本轮在途请求的取消令牌（对照 Agent2API terminateStatsRequest 的中止语义） */
  const abortRef = useRef<AbortController | null>(null);

  function sourceKey(s: { kind: string; channel_id: string; account_id?: string }): string {
    return s.kind === 'account' ? `a:${s.account_id}` : `c:${s.channel_id}`;
  }

  const usable = useMemo(() => model.sources ?? [], [model]);

  useEffect(() => {
    if (!running) return;
    const startedAt = Date.now();
    const timer = window.setInterval(() => setElapsed(Date.now() - startedAt), 400);
    return () => window.clearInterval(timer);
  }, [running]);

  const togglePick = (key: string) => {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const start = async () => {
    if (running) return;
    const chosen = usable.filter((s) => picked.has(sourceKey(s)));
    if (chosen.length === 0) return;
    const seq = ++runSeq.current;
    const ac = new AbortController();
    abortRef.current = ac;
    setElapsed(0);
    setSlots(
      chosen.map((s) => ({
        key: sourceKey(s),
        kind: s.kind,
        name: s.name,
        account_id: s.account_id,
        channel_id: s.channel_id,
        running: true,
      })),
    );
    setRunning(true);
    await Promise.all(
      chosen.map(async (s, idx) => {
        const patch = (p: Partial<Slot>) => {
          if (runSeq.current !== seq) return; // 已作废（再测一次/关闭）
          setSlots((prev) =>
            prev ? prev.map((it, i) => (i === idx ? { ...it, ...p } : it)) : prev,
          );
        };
        try {
          const res = await api.testModel(
            {
              provider: target.provider,
              model: model.id,
              account_id: s.kind === 'account' ? s.account_id : undefined,
              channel_id: s.kind === 'channel' ? s.channel_id : undefined,
              prompt: prompt.trim() || undefined,
              system_prompt: systemPrompt.trim() || undefined,
              reasoning_effort: reasoning || undefined,
              stream,
            },
            ac.signal,
          );
          patch({ running: false, result: res?.data });
        } catch (err) {
          if (err instanceof DOMException && err.name === 'AbortError') {
            patch({ running: false, transport: t('已中止') });
            return;
          }
          patch({ running: false, transport: err instanceof Error ? err.message : String(err) });
        }
      }),
    );
    if (runSeq.current === seq) {
      setRunning(false);
      abortRef.current = null;
    }
  };

  /** 中止本轮在途请求：回参数层，已回来的结论作废清空（对齐 Agent2API 测试中止） */
  const stopTests = () => {
    runSeq.current += 1;
    abortRef.current?.abort();
    abortRef.current = null;
    setRunning(false);
    setSlots(null);
  };

  const closeResults = () => {
    runSeq.current += 1; // 作废在途回调
    setRunning(false);
    setSlots(null);
  };

  const copyReply = (text: string) => {
    void navigator.clipboard?.writeText(text).then(
      () => addToast(t('已复制')),
      () => addToast(t('复制失败'), 'error'),
    );
  };

  const doneCount = slots?.filter((s) => !s.running).length ?? 0;
  const okCount = slots?.filter((s) => s.result?.ok).length ?? 0;
  const failCount = slots ? doneCount - okCount : 0;

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal mt-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h3>
            <FlaskConical size={15} style={{ verticalAlign: '-2px', marginRight: 6 }} />
            {t('测试模型')}
          </h3>
          <button className="modal-close" onClick={onClose}>
            &times;
          </button>
        </div>
        <div className="modal-body">
          {/* 目标行预览 */}
          <div className="mt-target">
            <code className="mt-mid">{model.id}</code>
            <span className="mm-src-tag provider">{target.providerLabel}</span>
            <span className="mt-target-tip">
              {t('以模型本名直发上游；别名映射不参与本次测试')}
            </span>
          </div>

          <div className="form-group">
            <label>{t('系统提示词（可选）')}</label>
            <textarea
              className="form-input"
              rows={2}
              maxLength={4000}
              value={systemPrompt}
              onChange={(e) => setSystemPrompt(e.target.value)}
              placeholder={t('留空则不携带')}
              disabled={running}
            />
          </div>
          <div className="form-group">
            <label>{t('用户提示词')}</label>
            <textarea
              className="form-input"
              rows={2}
              maxLength={4000}
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              disabled={running}
            />
          </div>

          <div className="mt-grid">
            <div className="form-group">
              <label>{t('思考等级')}</label>
              <select
                className="form-input"
                value={reasoning}
                onChange={(e) => setReasoning(e.target.value)}
                disabled={running}
              >
                <option value="">{t('不指定')}</option>
                {REASONING_OPTIONS.map((r) => (
                  <option key={r} value={r}>
                    {r}
                  </option>
                ))}
              </select>
            </div>
            <div className="form-group">
              <label>{t('流式请求')}</label>
              <div className="mt-stream-row">
                <Switch checked={stream} onChange={setStream} disabled={running} ariaLabel={t('流式请求')} />
                <span className="mt-stream-tip">{t('同时验证 SSE 链路与首字延迟')}</span>
              </div>
            </div>
          </div>

          <div className="form-group">
            <label>
              {t('测试来源')}
              <span className="mt-src-actions">
                <button type="button" onClick={() => setPicked(new Set(usable.map(sourceKey)))} disabled={running}>
                  {t('全选')}
                </button>
                <button type="button" onClick={() => setPicked(new Set())} disabled={running}>
                  {t('清空')}
                </button>
              </span>
            </label>
            <div className="mt-sources">
              {usable.map((s) => {
                const key = sourceKey(s);
                return (
                  <label key={key} className={`mt-source${picked.has(key) ? ' on' : ''}`}>
                    <input
                      type="checkbox"
                      checked={picked.has(key)}
                      onChange={() => togglePick(key)}
                      disabled={running}
                    />
                    <span className={`mm-src-tag${s.kind === 'account' ? '' : ' provider'}`}>
                      {s.kind === 'account' ? t('账号') : t('供应商')}
                    </span>
                    <span className="mt-source-name">{s.name}</span>
                  </label>
                );
              })}
            </div>
          </div>

          <p className="mt-note">
            {t('真实链路 · 消耗上游少量额度（管理员免站内计费） · 日志带 model_test 标记')}
          </p>
        </div>
        <div className="modal-footer">
          <button className="btn btn-outline" onClick={onClose} disabled={running}>
            {t('关闭')}
          </button>
          <button
            className="btn btn-primary"
            onClick={() => void start()}
            disabled={running || picked.size === 0 || usable.length === 0}
          >
            {running ? (
              <>
                <Loader2 size={13} className="spin" /> {t('测试中…')}
              </>
            ) : (
              t('开始测试')
            )}
          </button>
        </div>

        {/* 结果层（嵌在参数层内的覆盖层，对照 Agent2API 双层弹窗） */}
        {slots && (
          <div className="mt-results-overlay" onClick={closeResults}>
            <div className="mt-results" onClick={(e) => e.stopPropagation()}>
              <div className="mt-results-head">
                <h4>{t('测试结果')}</h4>
                <button className="modal-close" onClick={closeResults} title={t('关闭（回到参数）')}>
                  <X size={14} />
                </button>
              </div>
              <div className="mt-results-body">
                {slots.map((slot) => (
                  <div key={slot.key} className="mt-slot">
                    <div className="mt-slot-head">
                      <span className={`mm-src-tag${slot.kind === 'account' ? '' : ' provider'}`}>
                        {slot.kind === 'account' ? t('账号') : t('供应商')}
                      </span>
                      <strong>{slot.name}</strong>
                      {slot.running ? (
                        <span className="mt-slot-status">
                          <Loader2 size={12} className="spin" /> {t('已发出，等待上游…')}{' '}
                          {Math.floor(elapsed / 1000)}s
                        </span>
                      ) : slot.transport ? (
                        <span className="mm-badge-err">{t('传输失败')}</span>
                      ) : slot.result?.ok ? (
                        <span className="mm-badge-ok">
                          200 · {t('用时')} {formatMs(slot.result.duration_ms)}
                          {slot.result.ttfb_ms != null && ` · ${t('首字')} ${formatMs(slot.result.ttfb_ms)}`}
                        </span>
                      ) : (
                        <span className="mm-badge-err">
                          {slot.result?.status ? `${slot.result.status} · ` : ''}
                          {t('失败')}
                          {slot.result?.duration_ms != null && ` · ${formatMs(slot.result.duration_ms)}`}
                        </span>
                      )}
                    </div>

                    {!slot.running && slot.transport && (
                      <div className="mt-slot-err">{t('本机网关没有返回结论：')}{slot.transport}</div>
                    )}

                    {!slot.running && slot.result && !slot.result.ok && (
                      <div className="mt-slot-err">
                        {t('测试未通过：')}{slot.result.error}
                        {statusHint(slot.result.status, t) && (
                          <div className="mt-slot-hint">{statusHint(slot.result.status, t)}</div>
                        )}
                      </div>
                    )}

                    {!slot.running && slot.result?.ok && (
                      <>
                        <div className="mt-slot-meta">
                          {slot.result.upstream_model && slot.result.upstream_model !== model.id && (
                            <span>{t('上游模型')}：{slot.result.upstream_model}</span>
                          )}
                          <span>
                            tokens: {slot.result.prompt_tokens ?? 0} + {slot.result.completion_tokens ?? 0} ={' '}
                            {slot.result.total_tokens ?? 0}
                          </span>
                        </div>
                        {slot.result.reasoning && (
                          <details className="mt-reasoning">
                            <summary>{t('思考过程')}（{slot.result.reasoning.length} {t('字')}）</summary>
                            <pre>{slot.result.reasoning}</pre>
                          </details>
                        )}
                        <div className="mt-reply">
                          <button
                            type="button"
                            className="mt-copy"
                            title={t('复制回复')}
                            onClick={() => copyReply(slot.result?.reply ?? '')}
                          >
                            <Copy size={11} />
                          </button>
                          <pre>{slot.result.reply || t('（空回复）')}</pre>
                        </div>
                      </>
                    )}
                  </div>
                ))}
              </div>
              <div className="mt-results-foot">
                <span className="mt-summary">
                  {running
                    ? t('正在测 {{n}} 个来源，已完成 {{d}}。', { n: slots.length, d: doneCount })
                    : failCount > 0
                      ? `${okCount} ${t('成功')} · ${failCount} ${t('失败')}`
                      : t('全部成功')}
                </span>
                <div style={{ display: 'flex', gap: 8 }}>
                  {running && (
                    <button className="btn btn-danger btn-sm" onClick={stopTests} title={t('取消尚未返回的请求，已回来的结论一并作废')}>
                      {t('中止测试')}
                    </button>
                  )}
                  <button className="btn btn-outline btn-sm" onClick={closeResults}>
                    {running ? t('关闭（请求会在后台跑完并记日志）') : t('关闭')}
                  </button>
                  {!running && (
                    <button className="btn btn-primary btn-sm" onClick={() => void start()}>
                      {t('再测一次')}
                    </button>
                  )}
                </div>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
