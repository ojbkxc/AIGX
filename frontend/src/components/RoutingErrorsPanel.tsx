// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 错误处理面板（系统设置 → 重试）—— 对照 A2A ui-shared/routing-errors-panel.tsx +
// routing-errors-types.ts：上游失败后的重试上限、换号上限、冷却与失败保护，
// 以及按错误类型（kind）配置的处理动作（切换账号 / 跳过提供商 / 返回错误…）。
// 参考树通过 window.workbuddyDesktop 桥接；此处改用 AIGX 的 api.* 直连后端：
//   GET /api/routing/errors、PUT /api/routing/errors `{settings, revision}`。
// 端点未就绪（404）时整块降级。

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, Input, Select } from '../components/ui';

type ErrorKind = 'rate_limit' | 'quota' | 'auth' | 'balance' | 'transient' | 'unsupported' | 'bad_request' | 'content' | 'unknown';
type ErrorAction = 'switch_account' | 'retry_then_switch' | 'refresh_then_switch' | 'switch_provider' | 'return_error';
type CooldownMode = 'upstream' | 'fixed' | 'protection' | 'none';
type CooldownScope = 'auto' | 'model' | 'account' | 'provider';

interface ErrorRule {
  id: string;
  name: string;
  kind: ErrorKind;
  statuses: number[];
  code: string;
  provider: string;
  model: string;
  enabled: boolean;
  action: ErrorAction;
  cooldown: CooldownMode;
  fallbackSeconds: number;
  exponential: boolean;
  fallbackCapSeconds: number;
  scope: CooldownScope;
}

interface ErrorSettings {
  resends: number;
  switches: number;
  intervalSeconds: number;
  waitBudgetSeconds: number;
  cooldownFloorSeconds: number;
  backoff: boolean;
  backoffSeconds: number;
  backoffCapSeconds: number;
  sink: boolean;
  sinkSeconds: number;
  breaker: boolean;
  breakerFailures: number;
  breakerSeconds: number;
  breakerSuccesses: number;
  rules: ErrorRule[];
  overrides: ErrorRule[];
}

interface ErrorPolicyView {
  settings: ErrorSettings;
  defaults: ErrorSettings;
  revision: string;
  configured: boolean;
  legacy: boolean;
  providers: Array<{ id: string; label: string }>;
}

const KIND_LABEL: Record<ErrorKind, string> = {
  rate_limit: '请求限流',
  quota: '额度耗尽',
  auth: '登录失效',
  balance: '余额不足',
  transient: '连接或服务异常',
  unsupported: '能力不支持',
  bad_request: '请求有误',
  content: '内容被拒绝',
  unknown: '其他错误',
};

const ACTION_LABEL: Record<ErrorAction, string> = {
  switch_account: '直接换账号',
  retry_then_switch: '先重发，再换账号',
  refresh_then_switch: '续期一次，失败再换号',
  switch_provider: '跳过提供商',
  return_error: '直接返回错误',
};

const LIMIT_FIELDS: Array<{ key: keyof ErrorSettings; label: string; min: number; max: number }> = [
  { key: 'resends', label: '原账号重发（次）', min: 0, max: 10 },
  { key: 'switches', label: '最多再换账号（个）', min: 0, max: 10 },
  { key: 'intervalSeconds', label: '重发间隔（秒）', min: 0, max: 300 },
  { key: 'waitBudgetSeconds', label: '累计等待上限（秒）', min: 0, max: 300 },
  { key: 'cooldownFloorSeconds', label: '最短限流冷却（秒）', min: 1, max: 60 },
  { key: 'backoffSeconds', label: '首次休息时间（秒）', min: 1, max: 3600 },
  { key: 'backoffCapSeconds', label: '连续失败最长休息（秒）', min: 1, max: 3600 },
];

function validate(settings: ErrorSettings, t: (k: string, v?: Record<string, string | number>) => string): string | null {
  for (const { key, label, min, max } of LIMIT_FIELDS) {
    const v = settings[key] as number;
    if (!Number.isFinite(v) || v < min || v > max) {
      return t('{{name}}必须是 {{min}}–{{max}} 的整数', { name: t(label), min, max });
    }
  }
  for (const rule of [...settings.rules, ...settings.overrides]) {
    if (!rule.name.trim()) return t('请填写规则名称。');
    if (rule.fallbackCapSeconds < rule.fallbackSeconds) return t('冷却上限不能短于首次冷却时间。');
  }
  return null;
}

export default function RoutingErrorsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [view, setView] = useState<ErrorPolicyView | null>(null);
  const [draft, setDraft] = useState<ErrorSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const edited = useRef(false);

  const adopt = (next: ErrorPolicyView): void => {
    setView(next);
    setDraft(structuredClone(next.settings));
    edited.current = false;
  };

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getRoutingErrors();
      const data = (res?.data ?? null) as unknown as ErrorPolicyView | null;
      if (!data) return;
      adopt(data);
      setNotReady(false);
      setError('');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const patch = (next: Partial<ErrorSettings>): void => {
    if (!draft) return;
    setDraft({ ...draft, ...next });
    edited.current = true;
  };

  const patchRule = (index: number, next: Partial<ErrorRule>): void => {
    if (!draft) return;
    patch({ rules: draft.rules.map((r, i) => (i === index ? { ...r, ...next } : r)) });
  };

  const save = async (): Promise<void> => {
    if (!draft || !view || busy) return;
    const invalid = validate(draft, t);
    if (invalid) {
      setError(invalid);
      return;
    }
    setBusy(true);
    setError('');
    try {
      const res = await api.saveRoutingErrors({ settings: draft, revision: view.revision });
      const data = (res?.data ?? null) as unknown as ErrorPolicyView | null;
      if (data) adopt(data);
      addToast(t('错误处理设置已保存，下一个请求生效'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  if (loading) {
    return (
      <Card title={t('错误处理')}>
        <div className="settings-form">
          <Loader2 size={14} className="confirm-dialog-spin" /> {t('读取中')}
        </div>
      </Card>
    );
  }
  if (notReady) {
    return (
      <Card title={t('错误处理')}>
        <div className="empty-state">
          <p>{t('错误处理接口尚未就绪')}</p>
        </div>
      </Card>
    );
  }
  if (!draft || !view) {
    return (
      <Card title={t('错误处理')}>
        <div className="empty-state">
          <p>{t('错误处理设置读取失败')}</p>
        </div>
      </Card>
    );
  }

  const dirty = JSON.stringify(draft) !== JSON.stringify(view.settings);

  return (
    <Card
      title={t('错误处理')}
      actions={
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={busy || dirty}>
          <RefreshCw size={13} /> {t('重新读取')}
        </Button>
      }
    >
      <div className="settings-form">
        <span className="form-hint">
          {t('上游失败后按规则换账号或跳过提供商；每条请求的重试与换号受下列上限约束。保存后下一个请求生效。')}
        </span>
        {view.legacy ? (
          <span className="form-hint">{t('已保留原重试与失败休息参数，错误处理已统一；429 也计入换号上限。')}</span>
        ) : null}

        {/* 重试 / 换号 / 冷却上限 */}
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))', gap: 16 }}>
          {LIMIT_FIELDS.map((f) => (
            <div key={f.key} className="form-group">
              <label>{t(f.label)}</label>
              <Input
                type="number"
                min={f.min}
                max={f.max}
                value={draft[f.key] as number}
                disabled={busy}
                onChange={(e) => patch({ [f.key]: Number(e.target.value) } as Partial<ErrorSettings>)}
              />
            </div>
          ))}
        </div>

        <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap' }}>
          <label style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <input type="checkbox" checked={draft.backoff} disabled={busy} onChange={(e) => patch({ backoff: e.target.checked })} />
            {t('失败后退避休息')}
          </label>
          <label style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <input type="checkbox" checked={draft.breaker} disabled={busy} onChange={(e) => patch({ breaker: e.target.checked })} />
            {t('熔断保护')}
          </label>
        </div>

        {/* 规则列表：按错误类型配置处理动作 */}
        <div className="form-group">
          <label>{t('按错误类型的处理规则')}</label>
          <span className="form-hint" style={{ marginBottom: 8 }}>
            {t('每种错误类型的默认动作与冷却方式；规则由后端编译执行，启用/停用与动作调整即时生效。')}
          </span>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {draft.rules.map((rule, index) => (
              <div
                key={rule.id}
                style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap', border: '1px solid var(--border-color)', borderRadius: 8, padding: '8px 10px' }}
              >
                <Badge tone={rule.enabled ? 'info' : 'neutral'}>{t(KIND_LABEL[rule.kind] || rule.name)}</Badge>
                <span className="form-hint">{rule.name}</span>
                <label style={{ display: 'flex', alignItems: 'center', gap: 6, marginLeft: 'auto' }}>
                  <input
                    type="checkbox"
                    checked={rule.enabled}
                    disabled={busy}
                    onChange={(e) => patchRule(index, { enabled: e.target.checked })}
                  />
                  {t('启用')}
                </label>
                <Select
                  style={{ maxWidth: 180 }}
                  value={rule.action}
                  disabled={busy}
                  aria-label={t('处理动作')}
                  onChange={(e) => patchRule(index, { action: e.target.value as ErrorAction })}
                >
                  {(Object.keys(ACTION_LABEL) as ErrorAction[]).map((a) => (
                    <option key={a} value={a}>{t(ACTION_LABEL[a])}</option>
                  ))}
                </Select>
                {rule.cooldown !== 'none' ? (
                  <Input
                    type="number"
                    style={{ width: 100 }}
                    min={1}
                    max={3600}
                    value={rule.fallbackSeconds}
                    disabled={busy}
                    aria-label={t('冷却秒数')}
                    onChange={(e) => patchRule(index, { fallbackSeconds: Number(e.target.value) })}
                  />
                ) : null}
              </div>
            ))}
          </div>
        </div>

        {error ? <div className="error-message">{error}</div> : null}

        <div className="settings-actions">
          <Button variant="primary" disabled={!dirty || busy} onClick={() => void save()}>
            {busy ? t('保存中…') : t('保存修改')}
          </Button>
        </div>
      </div>
    </Card>
  );
}
