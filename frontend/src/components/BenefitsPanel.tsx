import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { GripVertical, Plus, Play, RefreshCw, RotateCcw, Trash2 } from 'lucide-react';
import { api } from '../api';
import { providerIcon } from '../lib/providerIcons';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, EmptyState, Input, Loading } from '../components/ui';
import type { BenefitResult, CheckinBenefitsData } from '../types';
import './BenefitsPanel.css';

/**
 * 福利通道（签到中心第 4 个 tab）
 *
 * 对照 Agent2API 的「新手福利 / 保活 / 成长任务 / 限时套餐」：这些动作
 * **一次性或限时**，领完即结清，所以不进每日调度，只能在这里手动触发。
 * 每个账号只渲染它自己 provider 支持的通道（后端 `actions_for` 决定）。
 */

/** 动作显示名与一句说明。 */
const ACTION_META: Record<string, { label: string; hint: string }> = {
  newbie: { label: '领新人礼', hint: 'CodeArts 新人注册礼（一次性）' },
  onboarding: { label: '盘点新手任务', hint: '只读：还剩哪些没领（结清后零上游请求）' },
  onboarding_claim: { label: '领取新手任务', hint: '领取全部未完成项' },
  keepalive: { label: '保活', hint: '只打免费模型最小对话，维持活跃（不领奖励）' },
  claim: { label: '领取日活', hint: '只探测并领取每日活跃奖励' },
  keepalive_claim: { label: '保活+领取', hint: '探测 → 领取 → 免费模型最小对话（桌面 + 网页）' },
  growth: { label: '盘点成长任务', hint: '只读：WorkBuddy 国内版成长任务树' },
  plan: { label: '套餐盘点', hint: 'ZCode 限时套餐预览（领取需先在前端求解验证码）' },
  umid: { label: 'UMID 组件状态', hint: 'Qoder 国际版风控组件探测（不代为下载）' },
};

/** WorkBuddy 国际版三颗按钮（对齐 Agent2API 手动粒度入口）。 */
const WB_ACTIONS = ['keepalive', 'claim', 'keepalive_claim'] as const;

/** 结果徽标色调。 */
const OUTCOME_TONE: Record<string, 'success' | 'info' | 'warning' | 'danger' | 'neutral'> = {
  done: 'success',
  already: 'info',
  pending: 'warning',
  failed: 'danger',
  skipped: 'neutral',
};

const OUTCOME_LABEL: Record<string, string> = {
  done: '已领取',
  already: '已结清',
  pending: '待重试',
  failed: '失败',
  skipped: '不适用',
};

/** UMID 常驻提示条的轮询间隔（未就绪时等管理员放好文件）。 */
const UMID_POLL_MS = 3000;

interface UmidState {
  available: boolean;
  path?: string;
  note?: string;
}

export default function BenefitsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [data, setData] = useState<CheckinBenefitsData | null>(null);
  const [error, setError] = useState('');
  const [running, setRunning] = useState<string>('');
  const [results, setResults] = useState<Record<string, BenefitResult>>({});

  // 保活模型链清单式编辑（对齐 Agent2API 保活链编辑器）
  const [keepaliveModels, setKeepaliveModels] = useState<string[]>([]);
  const [keepaliveDraft, setKeepaliveDraft] = useState('');
  const [savingKeepalive, setSavingKeepalive] = useState(false);

  // UMID 常驻提示条状态（机器级，账号无关，挂在 Qoder 国际版行下）
  const [umid, setUmid] = useState<UmidState | null>(null);
  const [umidChecking, setUmidChecking] = useState(false);

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getCheckinBenefits();
      setData(res?.data ?? null);
      setKeepaliveModels(res?.data?.keepalive?.models ?? []);
      setKeepaliveDraft((res?.data?.keepalive?.models ?? []).join(', '));
      setError('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const run = async (accountId: string, action: string): Promise<void> => {
    const key = `${accountId}:${action}`;
    setRunning(key);
    try {
      const res = await api.runCheckinBenefit(accountId, action);
      const result = res?.data;
      if (result) {
        setResults((prev) => ({ ...prev, [key]: result }));
        addToast(result.message, result.ok ? 'success' : 'error');
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setRunning('');
    }
  };

  /** UMID 探测（只读，零副作用）。 */
  const checkUmid = useCallback(async (): Promise<void> => {
    setUmidChecking(true);
    try {
      // UMID 是机器级通道：任意一个 Qoder 国际版账号触发即可（用第一个）
      const target = data?.accounts.find((a) => a.provider === 'qoder-intl');
      if (!target) {
        setUmid({ available: false, note: t('没有 Qoder 国际版账号') });
        return;
      }
      const res = await api.runCheckinBenefit(target.id, 'umid');
      const d = res?.data?.data as { available?: boolean; path?: string; note?: string } | undefined;
      setUmid({ available: d?.available === true, path: d?.path, note: d?.note });
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setUmidChecking(false);
    }
  }, [data, t, addToast]);

  // 未就绪时每 3 秒轮询回读：管理员放好 runtime-info 后提示条自动消失
  useEffect(() => {
    if (umid?.available || !data?.accounts.some((a) => a.provider === 'qoder-intl')) return;
    const timer = window.setInterval(() => {
      void checkUmid();
    }, UMID_POLL_MS);
    return () => window.clearInterval(timer);
  }, [umid?.available, data, checkUmid]);

  const saveKeepalive = async (): Promise<void> => {
    setSavingKeepalive(true);
    try {
      const models = keepaliveModels.map((m) => m.trim()).filter((m) => m.length > 0);
      const res = await api.saveKeepaliveConfig(models);
      setKeepaliveModels(res?.data?.models ?? models);
      setKeepaliveDraft((res?.data?.models ?? models).join(', '));
      addToast(t('保活模型链已保存'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSavingKeepalive(false);
    }
  };

  if (loading) return <Loading />;
  if (error) return <div className="error-message">{error}</div>;
  if (!data || data.accounts.length === 0) {
    return (
      <EmptyState
        message={t(
          '没有可用福利通道的账号。福利通道覆盖 CodeArts / Loomy / 小浣熊 / OfficeAce / WorkBuddy / ZCode / Qoder 国际版。',
        )}
      />
    );
  }

  // ── 保活模型链清单编辑（拖拽排序）──
  const dirty = keepaliveModels.join('') !== (data.keepalive.models ?? []).join('');
  const moveModel = (from: number, to: number): void => {
    if (to < 0 || to >= keepaliveModels.length || from === to) return;
    setKeepaliveModels((prev) => {
      const next = [...prev];
      const [item] = next.splice(from, 1);
      next.splice(to, 0, item);
      return next;
    });
  };
  const removeModel = (index: number): void => {
    setKeepaliveModels((prev) => prev.filter((_, i) => i !== index));
  };
  const addModel = (): void => {
    const items = keepaliveDraft
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
    if (items.length === 0) return;
    const next = [...keepaliveModels, ...items.filter((s) => !keepaliveModels.includes(s))];
    setKeepaliveModels(next);
    setKeepaliveDraft('');
  };
  const fillDefaults = (): void => {
    setKeepaliveModels(data.keepalive.default_models);
  };

  return (
    <>
      <Card
        title={t('福利与一次性领取')}
        actions={
          <Button variant="outline" onClick={() => void load()}>
            <RefreshCw size={14} /> {t('刷新')}
          </Button>
        }
      >
        <p className="bp-note">
          {t('一次性或限时的额度领取（新手礼 / 保活 / 成长任务 / 限时套餐）。这些动作不进每日调度，只能手动触发；已结清的通道再点也是零上游请求。')}
        </p>
        <div className="bp-list">
          {data.accounts.map((account) => {
            const icon = providerIcon(account.provider);
            const isQoderIntl = account.provider === 'qoder-intl';
            const wbActions = account.actions.filter((a) => WB_ACTIONS.includes(a as (typeof WB_ACTIONS)[number]));
            const otherActions = account.actions.filter((a) => !WB_ACTIONS.includes(a as (typeof WB_ACTIONS)[number]));
            return (
              <div key={account.id} className="bp-row">
                <div className="bp-head">
                  {icon ? (
                    <img className="bp-logo" src={icon} alt="" />
                  ) : (
                    <Play size={16} />
                  )}
                  <span className="bp-name">{account.name || account.id}</span>
                  <span className="bp-provider">{account.provider}</span>
                  {account.pending_onboarding != null && account.pending_onboarding > 0 && (
                    <Badge tone="warning">{t('待领新手福利 {{n}}', { n: account.pending_onboarding })}</Badge>
                  )}
                  {account.pending_onboarding === 0 && (
                    <Badge tone="neutral">{t('新手福利已结清')}</Badge>
                  )}
                  {account.keepalive?.date && (
                    <span className="bp-keepalive-note">
                      {t('保活')} {account.keepalive.date} · {account.keepalive.keepalive_ok ?? 0}/2
                    </span>
                  )}
                </div>
                <div className="bp-actions">
                  {wbActions.map((action) => {
                    const meta = ACTION_META[action] ?? { label: action, hint: '' };
                    const key = `${account.id}:${action}`;
                    const result = results[key];
                    return (
                      <div key={action} className="bp-action">
                        <Button
                          variant="outline"
                          title={t(meta.hint)}
                          disabled={running === key}
                          onClick={() => void run(account.id, action)}
                        >
                          <Play size={13} /> {t(meta.label)}
                        </Button>
                        {result && (
                          <Badge tone={OUTCOME_TONE[result.outcome] ?? 'neutral'}>
                            {t(OUTCOME_LABEL[result.outcome] ?? result.outcome)} · {result.message}
                          </Badge>
                        )}
                      </div>
                    );
                  })}
                  {otherActions.map((action) => {
                    const meta = ACTION_META[action] ?? { label: action, hint: '' };
                    const key = `${account.id}:${action}`;
                    const result = results[key];
                    return (
                      <div key={action} className="bp-action">
                        <Button
                          variant="outline"
                          title={t(meta.hint)}
                          disabled={running === key}
                          onClick={() => void run(account.id, action)}
                        >
                          <Play size={13} /> {t(meta.label)}
                        </Button>
                        {result && (
                          <Badge tone={OUTCOME_TONE[result.outcome] ?? 'neutral'}>
                            {t(OUTCOME_LABEL[result.outcome] ?? result.outcome)} · {result.message}
                          </Badge>
                        )}
                      </div>
                    );
                  })}
                </div>
                {isQoderIntl && (
                  <div className={`bp-umid-bar${umid?.available ? ' is-ok' : ' is-warn'}`}>
                    <span className="bp-umid-icon">
                      {umid?.available ? '✅' : '⚠️'}
                    </span>
                    <div className="bp-umid-body">
                      <div className="bp-umid-title">
                        {umid?.available
                          ? t('UMID 组件已就绪（{{path}}）', { path: umid.path ?? '' })
                          : t('UMID 组件未安装')}
                      </div>
                      <div className="bp-umid-note">{umid?.note ?? t('正在检测…')}</div>
                    </div>
                    <button
                      type="button"
                      className="btn btn-outline btn-sm"
                      onClick={() => void checkUmid()}
                      disabled={umidChecking}
                    >
                      <RefreshCw size={12} /> {umidChecking ? t('检测中…') : t('重新检测')}
                    </button>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </Card>

      <Card
        title={t('日活保活免费模型链')}
        actions={dirty ? <Badge tone="warning">{t('未保存')}</Badge> : undefined}
      >
        <p className="bp-note">
          {t('WorkBuddy 国际版保活用这些模型各打一发最小对话（不消耗积分）。拖拽排序 / 添加 / 移除；留空保存 = 恢复内置默认。')}
        </p>
        <div className="bp-model-list">
          {keepaliveModels.length === 0 && (
            <div className="bp-model-empty">{t('当前为空清单（保存后恢复内置默认链）')}</div>
          )}
          {keepaliveModels.map((model, index) => (
            <div
              key={`${model}-${index}`}
              className="bp-model-row"
              draggable
              onDragStart={(e) => {
                e.dataTransfer.setData('text/plain', String(index));
                e.dataTransfer.effectAllowed = 'move';
              }}
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = 'move';
              }}
              onDrop={(e) => {
                e.preventDefault();
                const from = Number(e.dataTransfer.getData('text/plain'));
                moveModel(from, index);
              }}
            >
              <GripVertical size={14} className="bp-model-handle" />
              <span className="bp-model-index">{index + 1}</span>
              <span className="bp-model-name">{model}</span>
              <button
                type="button"
                className="bp-model-remove"
                title={t('移除')}
                onClick={() => removeModel(index)}
              >
                <Trash2 size={12} />
              </button>
            </div>
          ))}
        </div>
        <div className="bp-model-add">
          <Input
            label={t('添加模型（回车加入清单）')}
            value={keepaliveDraft}
            placeholder="deepseek-v4.1-flash, hy3, …"
            onChange={(e) => setKeepaliveDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                addModel();
              }
            }}
          />
          <button type="button" className="btn btn-outline btn-sm" onClick={addModel}>
            <Plus size={13} /> {t('加入清单')}
          </button>
        </div>
        <div className="bp-actions" style={{ marginTop: 12 }}>
          <Button onClick={() => void saveKeepalive()} disabled={savingKeepalive}>
            {savingKeepalive ? t('保存中…') : t('保存')}
          </Button>
          <Button variant="outline" onClick={fillDefaults}>
            <RotateCcw size={13} /> {t('填入缺省链')}
          </Button>
          <Button
            variant="outline"
            onClick={() => {
              setKeepaliveModels([]);
              setKeepaliveDraft('');
            }}
          >
            {t('清空（=恢复默认）')}
          </Button>
        </div>
      </Card>
    </>
  );
}
