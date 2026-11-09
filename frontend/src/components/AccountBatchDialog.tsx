/**
 * 批量操作对话框（六类 + 限制器五模式 + 逐条回执，对齐上游 accounts-batch-dialog）。
 *
 * - 六类：启用 / 停用 / 修改代理 / 设置并发上限 / 查询设置 / 限制器 / 删除
 *   （上游 BATCH_ACTIONS 七个枚举值；「启停」算一类即六类口径）
 * - 限制器五模式：append（追加，逐账号去重）/ replace（整组替换）/
 *   enable / disable（只翻开关）/ clear（清空）——append/replace 用下方
 *   单条规则草稿（AIGX 限制器为扁平规则模型，与 per-account 编辑器同构）
 * - 逐条回执：优先走 POST /api/account-sessions/batch（#12-A1，单请求多账号）；
 *   端点未上线（404/501）时降级为循环 PATCH/DELETE（前端逐账号合成 payload，
   * 功能等价），回执口径不变
 * - 危险操作（删除 / 替换全部 / 清空全部）需勾选确认框才能执行（删除保护）
 */
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle } from 'lucide-react';
import { api } from '../api';
import { Button, Input, Select, Switch } from './ui';
import type { AccountBatchOp, AccountBatchReceipt, AccountBatchRequest, AccountLimiterRule, AccountSessionItem } from '../types';

/** 与 per-account 限制器编辑器同构的规则草稿（数字输入以字符串中转） */
interface BatchLimiterDraft {
  type: 'balance' | 'token';
  action: 'skip' | 'disable';
  thresholdInput: string;
  reset: 'fixed' | 'daily';
  periodMins: string;
}

type LimiterMode = NonNullable<AccountBatchRequest['limiter_mode']>;

const BATCH_OPS: Array<{ value: AccountBatchOp; label: string }> = [
  { value: 'enable', label: '启用' },
  { value: 'disable', label: '停用' },
  { value: 'set_proxy', label: '修改代理' },
  { value: 'set_max_concurrency', label: '设置并发上限' },
  { value: 'query_settings', label: '查询设置' },
  { value: 'limiter', label: '限制器' },
  { value: 'delete', label: '删除' },
];

const LIMITER_MODES: Array<{ value: NonNullable<AccountBatchRequest['limiter_mode']>; label: string; hint: string }> = [
  { value: 'append', label: '追加规则（保留原有）', hint: '保留各账号原有规则，只追加下面的规则；完全相同的规则不会重复添加。' },
  { value: 'replace', label: '替换全部规则', hint: '用下面的规则统一替换各账号的全部规则，原有规则不会保留。' },
  { value: 'enable', label: '启用全部规则', hint: '仅把各账号现有规则统一改为启用，不改动阈值与动作。' },
  { value: 'disable', label: '停用全部规则', hint: '仅把各账号现有规则统一改为停用；已停用的账号不会自动启用。' },
  { value: 'clear', label: '清空全部规则', hint: '删除各账号的全部限制规则；余额与 Token 将不再设限。' },
];

/** 草稿 → 规则（校验不过抛错；与页面 per-account 编辑器同一套约束：token 固定窗 30 分钟~24 小时） */
function limiterRuleOfDraft(d: BatchLimiterDraft): AccountLimiterRule {
  const threshold = Number(d.thresholdInput);
  if (!Number.isFinite(threshold) || threshold <= 0) {
    throw new Error('限制阈值必须是大于 0 的数字');
  }
  if (d.type === 'balance') {
    return { type: 'balance', action: d.action, threshold, enabled: true };
  }
  const periodMins = Number(d.periodMins);
  if (!Number.isFinite(periodMins) || periodMins < 30 || periodMins > 24 * 60) {
    throw new Error('Token 固定窗口须在 30 分钟 ~ 24 小时之间');
  }
  return {
    type: 'token',
    action: d.action,
    threshold: Math.round(threshold),
    reset: d.reset,
    ...(d.reset === 'fixed' ? { period: Math.round(periodMins * 60) } : {}),
    enabled: true,
  };
}

/** 规则一句话描述（批量预览用） */
function describeBatchRule(r: AccountLimiterRule): string {
  const action = r.action === 'disable' ? '停用该账号' : '跳过该账号';
  if (r.type === 'balance') {
    return `余额 < ${r.threshold} 时 → ${action}`;
  }
  const period = r.period ?? 0;
  const periodText = r.reset === 'daily' ? '每日 0 点重置' : `每 ${Math.round(period / 60)} 分钟重置`;
  return `${periodText}消耗 ≥ ${r.threshold} Token 时 → ${action}`;
}

/** append 模式逐账号去重合并（JSON 全等判重，与上游「完全相同的规则不重复」口径一致） */
function mergeAppend(current: AccountLimiterRule[], add: AccountLimiterRule[]): AccountLimiterRule[] {
  const seen = new Set(current.map((r) => JSON.stringify(r)));
  const merged = [...current];
  for (const r of add) {
    const key = JSON.stringify(r);
    if (!seen.has(key)) {
      seen.add(key);
      merged.push(r);
    }
  }
  return merged;
}

export default function AccountBatchDialog({
  ids,
  accounts,
  busy,
  onClose,
  onApplied,
}: {
  /** 勾选的账号 id */
  ids: string[];
  /** 全量账号（取名字展示 + 降级路径读各账号现有 limiters） */
  accounts: AccountSessionItem[];
  /** 外层批量进行中（复用页面 batchRunning，禁执行按钮） */
  busy: boolean;
  onClose: () => void;
  /** 全部成功后回调（刷新列表 + 清勾选由外层做） */
  onApplied: () => Promise<void> | void;
}): JSX.Element {
  const { t } = useTranslation();
  const [op, setOp] = useState<AccountBatchOp>('enable');
  const [proxy, setProxy] = useState('');
  const [conc, setConc] = useState('0');
  const [usageEnabled, setUsageEnabled] = useState(false);
  const [usageMins, setUsageMins] = useState('30');
  const [limMode, setLimMode] = useState<LimiterMode>('append');
  const [limDraft, setLimDraft] = useState<BatchLimiterDraft>({ type: 'balance', action: 'skip', thresholdInput: '', reset: 'fixed', periodMins: '60' });
  const [armed, setArmed] = useState(false);
  const [running, setRunning] = useState(false);
  const [formError, setFormError] = useState('');
  const [receipts, setReceipts] = useState<AccountBatchReceipt[] | null>(null);

  const byId = new Map(accounts.map((s) => [s.id, s]));
  const targets = ids.map((id) => byId.get(id)).filter((s): s is AccountSessionItem => Boolean(s));
  const nameOf = (id: string): string => byId.get(id)?.name || id;
  /** 危险操作：删除 / 替换全部 / 清空全部（对齐上游二次确认口径，落地为勾选确认） */
  const danger = op === 'delete' || (op === 'limiter' && (limMode === 'replace' || limMode === 'clear'));
  const editingRules = op === 'limiter' && (limMode === 'append' || limMode === 'replace');
  const limModeHint = LIMITER_MODES.find((m) => m.value === limMode)?.hint ?? '';
  const verb = op === 'limiter' ? (LIMITER_MODES.find((m) => m.value === limMode)?.label ?? t('限制器')) : (BATCH_OPS.find((o) => o.value === op)?.label ?? '');

  /** 组装批量请求（输入校验失败抛错，不发请求） */
  function buildRequest(): AccountBatchRequest {
    const req: AccountBatchRequest = { op, ids };
    if (op === 'set_proxy') {
      if (!proxy.trim()) throw new Error(t('请填写代理串；留空请用「修改代理」以外的方式恢复默认'));
      req.proxy = proxy.trim();
    }
    if (op === 'set_max_concurrency') {
      const raw = Number(conc);
      if (!Number.isFinite(raw) || raw < 0) throw new Error(t('并发上限必须是 ≥ 0 的整数（0 = 不限）'));
      req.max_concurrency = Math.round(raw);
    }
    if (op === 'query_settings') {
      req.usage_query_enabled = usageEnabled;
      const mins = Number(usageMins);
      if (usageEnabled && (!Number.isFinite(mins) || mins < 1)) throw new Error(t('自动查询间隔至少 1 分钟'));
      req.usage_query_interval_secs = usageEnabled ? Math.round(mins * 60) : 0;
    }
    if (editingRules) {
      req.limiters = [limiterRuleOfDraft(limDraft)];
    }
    if (op === 'limiter') {
      req.limiter_mode = limMode;
    }
    return req;
  }

  /** 降级路径：循环既有 PATCH/DELETE（批量端点未上线时功能等价的兜底） */
  async function runFallback(req: AccountBatchRequest): Promise<AccountBatchReceipt[]> {
    const results = await Promise.allSettled(req.ids.map(async (id): Promise<AccountBatchReceipt> => {
      const s = byId.get(id);
      if (!s) throw new Error(t('账号不存在'));
      switch (req.op) {
        case 'enable':
        case 'disable':
          await api.patchAccountSession(id, { status: req.op });
          break;
        case 'delete':
          await api.deleteAccountSession(id);
          break;
        case 'set_proxy':
          await api.patchAccountSession(id, { proxy: req.proxy, clear_proxy: false });
          break;
        case 'set_max_concurrency':
          await api.patchAccountSession(id, { max_concurrency: req.max_concurrency });
          break;
        case 'query_settings':
          await api.patchAccountSession(id, { usage_query_interval_secs: req.usage_query_interval_secs ?? 0 });
          break;
        case 'limiter': {
          const cur = s.limiters ?? [];
          let next: AccountLimiterRule[];
          if (req.limiter_mode === 'replace') next = req.limiters ?? [];
          else if (req.limiter_mode === 'enable') next = cur.map((r) => ({ ...r, enabled: true }));
          else if (req.limiter_mode === 'disable') next = cur.map((r) => ({ ...r, enabled: false }));
          else if (req.limiter_mode === 'clear') next = [];
          else next = mergeAppend(cur, req.limiters ?? []);
          await api.patchAccountSession(id, { limiters: next });
          break;
        }
      }
      return { id, ok: true };
    }));
    return results.map((r, i) =>
      r.status === 'fulfilled'
        ? r.value
        : { id: req.ids[i], ok: false, error: r.reason instanceof Error ? r.reason.message : String(r.reason) },
    );
  }

  async function run(): Promise<void> {
    if (running || busy || !ids.length) return;
    let req: AccountBatchRequest;
    try {
      req = buildRequest();
    } catch (err) {
      setFormError(err instanceof Error ? err.message : String(err));
      return;
    }
    setRunning(true);
    setFormError('');
    setReceipts(null);
    try {
      let list: AccountBatchReceipt[];
      try {
        const res = await api.batchAccountSessions(req);
        list = Array.isArray(res.data) ? res.data : [];
        if (!list.length) throw new Error(t('批量端点返回空回执'));
      } catch {
        // 批量端点未上线（404/501）或异常 → 循环降级，回执口径不变
        list = await runFallback(req);
      }
      const failed = list.filter((r) => !r.ok);
      if (failed.length === 0) {
        await onApplied();
        onClose();
      } else {
        setReceipts(list);
      }
    } catch (err) {
      setFormError(err instanceof Error ? err.message : String(err));
    } finally {
      setRunning(false);
    }
  }

  const okCount = receipts?.filter((r) => r.ok).length ?? 0;
  const failedList = receipts?.filter((r) => !r.ok) ?? [];

  /** 后端保护类错误码 → 用户文案（be-fix 契约：referenced_by_channel / not_found） */
  function errorText(r: AccountBatchReceipt): string {
    if (r.error === 'referenced_by_channel') {
      return t('账号被启用中的供应商引用，先解除引用或停用供应商再删除');
    }
    if (r.error === 'not_found') {
      return t('账号不存在');
    }
    return r.error || t('未知错误');
  }

  return (
    <div className="modal-overlay" role="dialog" aria-modal="true">
      <div className="modal as-batch-dlg">
        <div className="modal-header">
          <h3>{t('批量操作 · 已选 {{count}} 个账号', { count: ids.length })}</h3>
          <button type="button" className="modal-close" onClick={onClose} disabled={running} aria-label={t('关闭')}>×</button>
        </div>
        <div className="modal-body">
          <div className="as-batch-targets">
            <span className="form-hint">{t('将作用于：')}</span>
            <span className="as-batch-names" title={targets.map((s) => s.name || s.id).join(t('、'))}>
              {targets.map((s) => s.name || s.id).join(t('、')) || t('（勾选的账号已不在列表中）')}
            </span>
          </div>

          <div className="form-group">
            <label>{t('操作')}</label>
            <div className="as-batch-ops" role="radiogroup" aria-label={t('批量动作')}>
              {BATCH_OPS.map((o) => (
                <label key={o.value} className={`as-batch-op${op === o.value ? ' on' : ''}`}>
                  <input
                    type="radio"
                    name="as-batch-op"
                    value={o.value}
                    checked={op === o.value}
                    disabled={running}
                    onChange={() => { setOp(o.value); setArmed(false); setReceipts(null); setFormError(''); }}
                  />
                  {t(o.label)}
                </label>
              ))}
            </div>
          </div>

          {op === 'set_proxy' && (
            <div className="form-group">
              <label>{t('代理串')}</label>
              <Input
                value={proxy}
                onChange={(e) => setProxy(e.target.value)}
                placeholder="socks5h://127.0.0.1:1080"
                disabled={running}
              />
              <span className="form-hint">{t('支持 http(s)://、socks5://、socks5h://，可带 userinfo；socks5 会按 socks5h（远端 DNS）处理；也可填 pool:<id> / rotate 引用代理池')}</span>
            </div>
          )}

          {op === 'set_max_concurrency' && (
            <div className="form-group">
              <label>{t('并发上限')}</label>
              <Input type="number" min={0} step={1} value={conc} onChange={(e) => setConc(e.target.value)} disabled={running} />
              <span className="form-hint">{t('0 = 不限；达到上限的账号会被跳过、请求转给其他账号')}</span>
            </div>
          )}

          {op === 'query_settings' && (
            <>
              <div className="form-group as-batch-row">
                <label>{t('自动查询余额')}</label>
                <Switch checked={usageEnabled} onChange={setUsageEnabled} disabled={running} />
              </div>
              {usageEnabled && (
                <div className="form-group">
                  <label>{t('查询间隔（分钟）')}</label>
                  <Input type="number" min={1} step={1} value={usageMins} onChange={(e) => setUsageMins(e.target.value)} disabled={running} />
                  <span className="form-hint">{t('间隔越短读数越新鲜，但上游请求也越频繁；关闭后余额限制器只能用上次读数')}</span>
                </div>
              )}
            </>
          )}

          {op === 'limiter' && (
            <>
              <div className="form-group">
                <label>{t('规则操作')}</label>
                <Select value={limMode} onChange={(e) => { setLimMode(e.target.value as LimiterMode); setArmed(false); setReceipts(null); }} disabled={running}>
                  {LIMITER_MODES.map((m) => (
                    <option key={m.value} value={m.value}>{t(m.label)}</option>
                  ))}
                </Select>
                <span className="form-hint">{t(limModeHint)}</span>
              </div>
              {editingRules && (
                <div className="as-batch-lim">
                  <div className="as-batch-row">
                    <Select value={limDraft.type} onChange={(e) => setLimDraft({ ...limDraft, type: e.target.value as BatchLimiterDraft['type'] })} disabled={running}>
                      <option value="balance">{t('余额阈值')}</option>
                      <option value="token">{t('Token 周期用量')}</option>
                    </Select>
                    <Select value={limDraft.action} onChange={(e) => setLimDraft({ ...limDraft, action: e.target.value as BatchLimiterDraft['action'] })} disabled={running}>
                      <option value="skip">{t('跳过该账号')}</option>
                      <option value="disable">{t('停用该账号')}</option>
                    </Select>
                  </div>
                  <div className="as-batch-row">
                    <Input
                      type="number"
                      min={0}
                      placeholder={limDraft.type === 'balance' ? t('余额下限') : t('Token 上限')}
                      value={limDraft.thresholdInput}
                      onChange={(e) => setLimDraft({ ...limDraft, thresholdInput: e.target.value })}
                      disabled={running}
                    />
                    {limDraft.type === 'token' && (
                      <>
                        <Select value={limDraft.reset} onChange={(e) => setLimDraft({ ...limDraft, reset: e.target.value as BatchLimiterDraft['reset'] })} disabled={running}>
                          <option value="fixed">{t('固定窗口')}</option>
                          <option value="daily">{t('每日 0 点重置')}</option>
                        </Select>
                        {limDraft.reset === 'fixed' && (
                          <Input type="number" min={30} max={1440} placeholder={t('窗口（分钟，30~1440）')} value={limDraft.periodMins} onChange={(e) => setLimDraft({ ...limDraft, periodMins: e.target.value })} disabled={running} />
                        )}
                      </>
                    )}
                  </div>
                  <span className="form-hint">
                    {(() => {
                      try {
                        return describeBatchRule(limiterRuleOfDraft(limDraft));
                      } catch {
                        return t('填好阈值后执行');
                      }
                    })()}
                  </span>
                </div>
              )}
            </>
          )}

          {op === 'delete' && (
            <div className="as-batch-danger-note">
              <AlertTriangle size={14} />
              <span>{t('删除会一并移除凭证与冷却记录，不可恢复。')}</span>
            </div>
          )}

          {danger && (
            <div className="form-group as-batch-arm">
              <label className="as-batch-arm-label">
                <input type="checkbox" checked={armed} disabled={running} onChange={(e) => setArmed(e.target.checked)} />
                {t('我确认对这 {{count}} 个账号执行「{{verb}}」', { count: ids.length, verb })}
              </label>
            </div>
          )}

          {receipts && (
            <div className="as-batch-receipts" role="status">
              <p className={failedList.length ? 'warn' : 'ok'}>
                {t('{{verb}}完成：成功 {{ok}} 个，失败 {{fail}} 个', { verb, ok: okCount, fail: failedList.length })}
              </p>
              {failedList.length > 0 && (
                <ul>
                  {failedList.map((r) => (
                    <li key={r.id}>
                      <span>{nameOf(r.id)}</span>
                      <span className="form-hint">{errorText(r)}</span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}

          {formError && <p className="as-batch-error" role="alert">{formError}</p>}
        </div>
        <div className="modal-footer">
          <Button variant="outline" onClick={onClose} disabled={running}>{t('关闭')}</Button>
          <Button
            variant={danger ? 'danger' : 'primary'}
            disabled={running || busy || (danger && !armed)}
            onClick={() => void run()}
          >
            {running ? t('执行中…') : t('执行（{{verb}}）', { verb })}
          </Button>
        </div>
      </div>
    </div>
  );
}
