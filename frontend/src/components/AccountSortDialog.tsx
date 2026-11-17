// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 账号服务端排序规则弹窗（账号页「排序规则」入口）。
// 契约：GET/PUT /api/accounts/sorting（config/account_sort.rs）。
//   - provider → 规则数组；最多三条（credit / balance / added），键不可重复
//   - 传 rules=null 恢复默认（清掉该 provider 的规则）
// 这里维护一家 provider 的三条规则（各带开关 + 方向），保存后回读整表。

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RotateCcw } from 'lucide-react';
import { api } from '../api';
import { useToast } from './Toast';
import { Badge, Button, Select, Switch } from './ui';
import type { AccountProviderItem, AccountSortRule, AccountSortSettings } from '../types';
import './AccountSortDialog.css';

/** 一家 provider 的规则草稿：键 → {on, dir}（缺项 = 未启用该规则） */
interface RuleDraft {
  credit: { on: boolean; dir: 'soon' | 'late' };
  balance: { on: boolean; dir: 'high' | 'low' };
  added: { on: boolean; dir: 'new' | 'old' };
}

const EMPTY_DRAFT: RuleDraft = {
  credit: { on: false, dir: 'soon' },
  balance: { on: false, dir: 'high' },
  added: { on: false, dir: 'new' },
};

/** 后端规则数组 → 草稿（顺序无关；同键重复按最后一条） */
function draftOf(rules: AccountSortRule[] | undefined): RuleDraft {
  const draft: RuleDraft = {
    credit: { ...EMPTY_DRAFT.credit },
    balance: { ...EMPTY_DRAFT.balance },
    added: { ...EMPTY_DRAFT.added },
  };
  for (const rule of rules ?? []) {
    if (rule.key === 'credit') draft.credit = { on: rule.on, dir: rule.dir };
    else if (rule.key === 'balance') draft.balance = { on: rule.on, dir: rule.dir };
    else if (rule.key === 'added') draft.added = { on: rule.on, dir: rule.dir };
  }
  return draft;
}

/** 草稿 → 后端规则数组（只提交 on=true 的；顺序固定 credit→balance→added） */
function rulesOf(draft: RuleDraft): AccountSortRule[] {
  const out: AccountSortRule[] = [];
  if (draft.credit.on) out.push({ key: 'credit', on: true, dir: draft.credit.dir });
  if (draft.balance.on) out.push({ key: 'balance', on: true, dir: draft.balance.dir });
  if (draft.added.on) out.push({ key: 'added', on: true, dir: draft.added.dir });
  return out;
}

interface AccountSortDialogProps {
  open: boolean;
  onClose: () => void;
  /** provider 目录（名称映射；后端 sorting 的键是 provider id） */
  providers: AccountProviderItem[];
}

/**
 * AccountSortDialog — 账号服务端排序规则。
 *
 * 服务端排序决定**调度选路**（与账号页的「展示序」是两回事），所以放在
 * 独立弹窗里按 provider 逐家配置，改动显式保存，不会误触。
 */
export default function AccountSortDialog({ open, onClose, providers }: AccountSortDialogProps): JSX.Element | null {
  const { t } = useTranslation();
  const addToast = useToast();

  const [settings, setSettings] = useState<AccountSortSettings>({});
  const [drafts, setDrafts] = useState<Record<string, RuleDraft>>({});
  const [current, setCurrent] = useState('');
  const [loading, setLoading] = useState(false);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);

  const labelOf = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of providers) map.set(p.kind, p.label || p.kind);
    return (id: string): string => map.get(id) || id;
  }, [providers]);

  const load = async (): Promise<void> => {
    setLoading(true);
    try {
      const res = await api.getAccountSorting();
      const data = res?.data ?? {};
      setSettings(data);
      const next: Record<string, RuleDraft> = {};
      for (const [provider, rules] of Object.entries(data)) next[provider] = draftOf(rules);
      setDrafts(next);
      setCurrent((prev) => (prev && Object.prototype.hasOwnProperty.call(data, prev) ? prev : Object.keys(data)[0] ?? ''));
      setNotReady(false);
      setError('');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      setSettings({});
      setDrafts({});
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (!open) return;
    void load();
  }, [open]);

  const draft = drafts[current] ?? EMPTY_DRAFT;

  const patchDraft = (next: Partial<RuleDraft>): void => {
    setDrafts((prev) => ({ ...prev, [current]: { ...(prev[current] ?? EMPTY_DRAFT), ...next } }));
  };

  const save = async (): Promise<void> => {
    if (!current) return;
    setSaving(true);
    try {
      const rules = rulesOf(draft);
      const res = await api.saveAccountSorting(current, rules.length ? rules : null);
      const data = res?.data ?? {};
      setSettings(data);
      const next: Record<string, RuleDraft> = {};
      for (const [provider, list] of Object.entries(data)) next[provider] = draftOf(list);
      setDrafts(next);
      addToast(t('排序规则已保存'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSaving(false);
    }
  };

  const resetProvider = async (): Promise<void> => {
    if (!current) return;
    setSaving(true);
    try {
      const res = await api.saveAccountSorting(current, null);
      const data = res?.data ?? {};
      setSettings(data);
      const next: Record<string, RuleDraft> = {};
      for (const [provider, list] of Object.entries(data)) next[provider] = draftOf(list);
      setDrafts(next);
      addToast(t('已恢复默认排序'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSaving(false);
    }
  };

  if (!open) return null;

  const configured = Object.entries(settings).filter(([, rules]) => (rules ?? []).length > 0);

  return (
    <div className="asort-overlay" role="dialog" aria-modal="true" aria-label={t('账号排序规则')}>
      <div className="asort-panel">
        <div className="asort-head">
          <div>
            <h2>{t('账号排序规则')}</h2>
            <p>{t('按提供商配置服务端排序：决定调度时优先选谁。展示序不影响这里。')}</p>
          </div>
          <div className="asort-head-actions">
            <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
              {loading ? <Loader2 size={13} className="asort-spin" /> : <RotateCcw size={13} />}
              {t('刷新')}
            </Button>
            <Button variant="outline" size="sm" onClick={onClose}>{t('关闭')}</Button>
          </div>
        </div>

        <div className="asort-body">
          {error && <div className="error-message">{error}</div>}
          {notReady ? (
            <p className="asort-empty">{t('账号排序接口尚未就绪（后端 accounts/sorting 待补齐）。')}</p>
          ) : (
            <>
              <div className="asort-pick">
                <span>{t('提供商')}</span>
                <Select
                  value={current}
                  onChange={(e) => setCurrent(e.target.value)}
                  aria-label={t('选择提供商')}
                >
                  <option value="">—</option>
                  {Array.from(new Set([...Object.keys(settings), ...providers.filter((p) => p.source === 'plugin' || !p.source).map((p) => p.kind)]))
                    .sort()
                    .map((id) => (
                      <option key={id} value={id}>
                        {labelOf(id)}
                        {(settings[id] ?? []).length ? ` · ${(settings[id] ?? []).length}` : ''}
                      </option>
                    ))}
                </Select>
                {current && (
                  <Button variant="outline" size="sm" disabled={saving} onClick={() => void resetProvider()}>
                    <RotateCcw size={12} /> {t('恢复默认')}
                  </Button>
                )}
              </div>

              {current ? (
                <div className="asort-rules">
                  <RuleRow
                    title={t('到期时间（credit）')}
                    hint={t('凭证 / 套餐到期近的优先')}
                    on={draft.credit.on}
                    dir={draft.credit.dir}
                    dirOptions={[
                      { value: 'soon', label: t('最早到期优先') },
                      { value: 'late', label: t('最晚到期优先') },
                    ]}
                    onToggle={(on) => patchDraft({ credit: { ...draft.credit, on } })}
                    onDir={(dir) => patchDraft({ credit: { ...draft.credit, dir: dir as 'soon' | 'late' } })}
                  />
                  <RuleRow
                    title={t('余额（balance）')}
                    hint={t('余额高的优先')}
                    on={draft.balance.on}
                    dir={draft.balance.dir}
                    dirOptions={[
                      { value: 'high', label: t('余额高优先') },
                      { value: 'low', label: t('余额低优先') },
                    ]}
                    onToggle={(on) => patchDraft({ balance: { ...draft.balance, on } })}
                    onDir={(dir) => patchDraft({ balance: { ...draft.balance, dir: dir as 'high' | 'low' } })}
                  />
                  <RuleRow
                    title={t('添加时间（added）')}
                    hint={t('新加的账号优先')}
                    on={draft.added.on}
                    dir={draft.added.dir}
                    dirOptions={[
                      { value: 'new', label: t('新→旧') },
                      { value: 'old', label: t('旧→新') },
                    ]}
                    onToggle={(on) => patchDraft({ added: { ...draft.added, on } })}
                    onDir={(dir) => patchDraft({ added: { ...draft.added, dir: dir as 'new' | 'old' } })}
                  />
                  <div className="asort-save">
                    <span className="asort-save-hint">
                      {rulesOf(draft).length
                        ? t('将保存 {{n}} 条规则', { n: rulesOf(draft).length })
                        : t('未启用任何规则 = 恢复该提供商的默认排序')}
                    </span>
                    <Button variant="primary" size="sm" disabled={saving} onClick={() => void save()}>
                      {saving ? <Loader2 size={13} className="asort-spin" /> : null}
                      {t('保存')}
                    </Button>
                  </div>
                </div>
              ) : (
                <p className="asort-empty">{t('该提供商还没有配置排序规则。')}</p>
              )}

              {configured.length > 0 && (
                <div className="asort-summary">
                  <span className="asort-summary-title">{t('已配置的提供商')}</span>
                  {configured.map(([id, rules]) => (
                    <Badge key={id} tone="info">
                      {labelOf(id)} · {(rules ?? []).map((r) => r.key).join('/')}
                    </Badge>
                  ))}
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function RuleRow({
  title,
  hint,
  on,
  dir,
  dirOptions,
  onToggle,
  onDir,
}: {
  title: string;
  hint: string;
  on: boolean;
  dir: string;
  dirOptions: Array<{ value: string; label: string }>;
  onToggle: (on: boolean) => void;
  onDir: (dir: string) => void;
}): JSX.Element {
  return (
    <div className={`asort-rule${on ? ' is-on' : ''}`}>
      <div className="asort-rule-main">
        <span className="asort-rule-title">{title}</span>
        <span className="asort-rule-hint">{hint}</span>
      </div>
      <Select value={dir} onChange={(e) => onDir(e.target.value)} disabled={!on} aria-label={title}>
        {dirOptions.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </Select>
      <Switch checked={on} onChange={onToggle} ariaLabel={title} />
    </div>
  );
}