import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import type { RouteStrategy, RoutingPolicyItem, StickyMode } from '../types';

/**
 * 路由策略（系统设置 → 运维 → 路由）
 *
 * 对照 Agent2API「设置 → 路由」：把 AIGX 选路组件（priority+weight 分组、
 * RoundRobin、多因子调度、亲和粘性、断路器）的硬编码旋钮收敛为可配置项。
 *
 * - 路由策略：加权随机（默认）/ 组内轮询 / 多因子调度（健康×成本×RPM）
 * - 会话粘性：自动（5min）/ 长会话（6h）/ 关闭 —— 同会话尽量跟住同一渠道，
 *   保留上游上下文缓存
 * - 断路器：连续失败 N 次熔断该渠道，冷却 X 秒后 HalfOpen 试探
 * - 调度权重：多因子策略下健康/成本/RPM 三维权重
 *
 * 全部热生效：保存后下一请求即按新策略选路，无需重启。
 */

const STRATEGIES: Array<{ value: RouteStrategy; label: string; tip: string }> = [
  {
    value: 'weighted',
    label: '加权随机',
    tip: '渠道按优先级分组，组内按权重随机（默认现状）。同优先级渠道按权重比例分摊流量。',
  },
  {
    value: 'round_robin',
    label: '组内轮询',
    tip: '同优先级组内确定性轮询：请求 1→2→3→1… 均匀分布。想把手里的渠道摊开用、不让某一个一直扛时选它。',
  },
  {
    value: 'ranked',
    label: '多因子调度',
    tip: '同优先级组内按 健康×成本×RPM 几何加权评分排序（权重见下方「调度权重」）。需要省钱或避开工健康差的渠道时选它。',
  },
];

const STICKY_OPTIONS: Array<{ value: StickyMode; label: string; tip: string }> = [
  {
    value: 'auto',
    label: '自动',
    tip: '同一会话 5 分钟内尽量跟住同一渠道，30 分钟强制过期。多数场景用它——多轮对话少换渠道，也不至于把渠道焊死。',
  },
  {
    value: 'session',
    label: '长会话',
    tip: '同一会话 6 小时内跟住同一渠道。适合一坐几个小时的长会话，上游的上下文缓存能吃到更多。',
  },
  {
    value: 'turn',
    label: '每轮',
    tip: '每条请求都重新按策略选路（不粘），仅保留会话识别记录。与「关闭」行为一致。',
  },
  {
    value: 'off',
    label: '关闭',
    tip: '完全不做会话粘性，每个请求独立选路。渠道池均匀性优先于上游缓存命中时选它。',
  },
];

interface FormState {
  strategy: RouteStrategy;
  sticky: StickyMode;
  breakerEnabled: boolean;
  breakerThreshold: string;
  breakerCooldown: string;
  breakerSuccessThreshold: string;
  healthWeight: string;
  costWeight: string;
  rpmWeight: string;
}

export default function RoutingPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [runtime, setRuntime] = useState<RoutingPolicyItem['runtime']>();
  const [form, setForm] = useState<FormState>({
    strategy: 'weighted',
    sticky: 'auto',
    breakerEnabled: false,
    breakerThreshold: '5',
    breakerCooldown: '30',
    breakerSuccessThreshold: '2',
    healthWeight: '0.4',
    costWeight: '0.4',
    rpmWeight: '0.2',
  });

  const applyItem = (d: RoutingPolicyItem): void => {
    setForm({
      strategy: d.strategy ?? 'weighted',
      sticky: d.sticky ?? 'auto',
      breakerEnabled: Boolean(d.breaker?.enabled),
      breakerThreshold: String(d.breaker?.threshold ?? 5),
      breakerCooldown: String(d.breaker?.cooldown_secs ?? 30),
      breakerSuccessThreshold: String(d.breaker?.success_threshold ?? 2),
      healthWeight: String(d.scheduler_weights?.health_weight ?? 0.4),
      costWeight: String(d.scheduler_weights?.cost_weight ?? 0.4),
      rpmWeight: String(d.scheduler_weights?.rpm_weight ?? 0.2),
    });
    setRuntime(d.runtime);
  };

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getRoutingPolicy();
        if (res?.data) applyItem(res.data as RoutingPolicyItem);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      const threshold = Number(form.breakerThreshold);
      const cooldown = Number(form.breakerCooldown);
      const successThreshold = Number(form.breakerSuccessThreshold);
      const hw = Number(form.healthWeight);
      const cw = Number(form.costWeight);
      const rw = Number(form.rpmWeight);
      if (!Number.isFinite(threshold) || threshold < 1 || threshold > 20) {
        setError(t('断路器阈值须在 1-20 之间'));
        return;
      }
      if (!Number.isFinite(cooldown) || cooldown < 10 || cooldown > 600) {
        setError(t('断路器冷却须在 10-600 秒之间'));
        return;
      }
      if (!Number.isFinite(successThreshold) || successThreshold < 1 || successThreshold > 10) {
        setError(t('半开成功阈值须在 1-10 之间'));
        return;
      }
      if (![hw, cw, rw].every((w) => Number.isFinite(w) && w >= 0)) {
        setError(t('调度权重须为非负数字'));
        return;
      }
      if (hw <= 0 && cw <= 0 && rw <= 0) {
        setError(t('调度权重至少一个为正'));
        return;
      }
      const res = await api.saveRoutingPolicy({
        strategy: form.strategy,
        sticky: form.sticky,
        breaker: {
          enabled: form.breakerEnabled,
          threshold,
          cooldown_secs: cooldown,
          success_threshold: successThreshold,
        },
        scheduler_weights: {
          health_weight: hw,
          cost_weight: cw,
          rpm_weight: rw,
        },
      });
      if (res?.data) applyItem(res.data as RoutingPolicyItem);
      addToast(t('路由策略已保存（下一请求即生效，无需重启）'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const resetDefaults = async (): Promise<void> => {
    try {
      const res = await api.getRoutingDefaults();
      if (res?.data) applyItem(res.data as RoutingPolicyItem);
      addToast(t('已回填默认值（尚未保存）'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  const strategyTip = STRATEGIES.find((s) => s.value === form.strategy)?.tip ?? '';
  const stickyTip = STICKY_OPTIONS.find((s) => s.value === form.sticky)?.tip ?? '';

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('路由策略')}</h2>
        <p className="card-subtitle">
          {t('渠道选路的策略、会话粘性、断路器与调度权重；全部热生效，保存后下一请求即按新策略选路。')}
        </p>
        {runtime && (
          <p className="card-subtitle">
            {t('运行时：亲和会话 {{affinity}} 条 · 断路器开 {{open}} / 半开 {{halfopen}}', {
              affinity: runtime.affinity_entries ?? 0,
              open: runtime.breaker_open ?? 0,
              halfopen: runtime.breaker_halfopen ?? 0,
            })}
          </p>
        )}
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label>{t('路由策略')}</label>
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
              {STRATEGIES.map((s) => (
                <button
                  key={s.value}
                  type="button"
                  className={`btn ${form.strategy === s.value ? 'btn-primary' : 'btn-secondary'}`}
                  onClick={() => setForm({ ...form, strategy: s.value })}
                >
                  {t(s.label)}
                </button>
              ))}
            </div>
            <div className="form-hint">{t(strategyTip)}</div>
          </div>

          <div className="form-group">
            <label>{t('会话粘性')}</label>
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
              {STICKY_OPTIONS.map((s) => (
                <button
                  key={s.value}
                  type="button"
                  className={`btn ${form.sticky === s.value ? 'btn-primary' : 'btn-secondary'}`}
                  onClick={() => setForm({ ...form, sticky: s.value })}
                >
                  {t(s.label)}
                </button>
              ))}
            </div>
            <div className="form-hint">{t(stickyTip)}</div>
          </div>

          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={form.breakerEnabled}
                onChange={(e) => setForm({ ...form, breakerEnabled: e.target.checked })}
              />
              <span>{t('启用断路器（连续失败自动熔断渠道；关闭后仍记录失败但不拦截）')}</span>
            </label>
            {form.breakerEnabled && (
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: 12, marginTop: 8 }}>
                <div className="form-group">
                  <label>{t('熔断阈值（次）')}</label>
                  <input
                    className="form-input"
                    type="number"
                    min={1}
                    max={20}
                    value={form.breakerThreshold}
                    onChange={(e) => setForm({ ...form, breakerThreshold: e.target.value })}
                  />
                  <div className="form-hint">{t('连续失败多少次后熔断该渠道（1-20）')}</div>
                </div>
                <div className="form-group">
                  <label>{t('冷却秒数')}</label>
                  <input
                    className="form-input"
                    type="number"
                    min={10}
                    max={600}
                    value={form.breakerCooldown}
                    onChange={(e) => setForm({ ...form, breakerCooldown: e.target.value })}
                  />
                  <div className="form-hint">
                    {t('熔断后冷却多少秒再放试探请求（10-600）')}
                  </div>
                </div>
                <div className="form-group">
                  <label>{t('半开成功阈值')}</label>
                  <input
                    className="form-input"
                    type="number"
                    min={1}
                    max={10}
                    value={form.breakerSuccessThreshold}
                    onChange={(e) => setForm({ ...form, breakerSuccessThreshold: e.target.value })}
                  />
                  <div className="form-hint">
                    {t('试探成功几次才真正恢复（1-10）；1=单次成功即恢复')}
                  </div>
                </div>
              </div>
            )}
          </div>

          <div className="form-group">
            <label>{t('调度权重（多因子策略生效）')}</label>
            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: 12 }}>
              <div className="form-group">
                <label>{t('健康')}</label>
                <input
                  className="form-input"
                  type="number"
                  step="0.1"
                  min={0}
                  value={form.healthWeight}
                  onChange={(e) => setForm({ ...form, healthWeight: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('成本')}</label>
                <input
                  className="form-input"
                  type="number"
                  step="0.1"
                  min={0}
                  value={form.costWeight}
                  onChange={(e) => setForm({ ...form, costWeight: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('RPM')}</label>
                <input
                  className="form-input"
                  type="number"
                  step="0.1"
                  min={0}
                  value={form.rpmWeight}
                  onChange={(e) => setForm({ ...form, rpmWeight: e.target.value })}
                />
              </div>
            </div>
            <div className="form-hint">
              {t('健康×成本×RPM 三维权重（默认 0.4/0.4/0.2）。只在「多因子调度」策略下参与排序，改小成本权重=优先健康渠道。')}
            </div>
          </div>

          <div className="settings-actions" style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
              {saving ? t('保存中...') : t('保存更改')}
            </button>
            <button className="btn btn-secondary" onClick={() => void resetDefaults()}>
              {t('恢复默认')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
