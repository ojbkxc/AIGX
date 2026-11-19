import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';

/**
 * 日志保留设置（系统设置 → 运维 tab 子标签）
 *
 * - 保留天数：设置后每日后台自动清理早于截止时间的请求/审计日志；
 *   不设置时长策略只受容量上限兜底。
 * - 容量上限：超过时自动清最旧一批（与后端 purge_overflow 对齐，此处可调）。
 * - 手动清理：立即按保留天数执行一次清理。
 */
export default function LogRetention(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [cleaning, setCleaning] = useState(false);
  /** 保留天数（空串 = 不限） */
  const [days, setDays] = useState('');
  const [capacity, setCapacity] = useState('');
  /** 审计日志独立保留天数（空串 = 跟随请求日志） */
  const [auditDays, setAuditDays] = useState('');
  /** 日聚合保留天数（空串 = 永久）。聚合是报表数据源，寿命独立于明细。 */
  const [dailyDays, setDailyDays] = useState('');
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  /** 读取失败：不能静默吞掉——否则表单显示空值，用户一保存就把原配置覆盖了 */
  const [loadError, setLoadError] = useState('');
  /** 已保存的快照（用于判定「有未保存更改」，清理必须按已保存策略执行） */
  const [saved, setSaved] = useState<{
    days: string; capacity: string; auditDays: string; dailyDays: string;
  } | null>(null);

  const load = useCallback(async (silent = false): Promise<void> => {
    if (!silent) setLoadError('');
    try {
      const res = await api.getLogRetention();
      const d = res?.data;
      const next = {
        days: d?.max_age_days != null ? String(d.max_age_days) : '',
        capacity: d?.max_capacity != null ? String(d.max_capacity) : '',
        auditDays: d?.audit_max_age_days != null ? String(d.audit_max_age_days) : '',
        dailyDays: d?.daily_max_age_days != null ? String(d.daily_max_age_days) : '',
      };
      setDays(next.days);
      setCapacity(next.capacity);
      setAuditDays(next.auditDays);
      setDailyDays(next.dailyDays);
      setSaved(next);
    } catch (err) {
      // 静默刷新（保存成功后回读）失败不覆盖页面；首次加载失败则进入错误态
      if (!silent) setLoadError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void (async () => {
      try {
        await load();
      } finally {
        setLoading(false);
      }
    })();
  }, [load]);

  /** 表单与已保存值是否存在差异（有差异时「立即清理」会按旧配置执行，需先保存） */
  const dirty = saved != null && (
    days !== saved.days
    || capacity !== saved.capacity
    || auditDays !== saved.auditDays
    || dailyDays !== saved.dailyDays
  );

  const save = async (): Promise<void> => {
    setSaving(true);
    try {
      const daysVal = days.trim() === '' ? undefined : Math.max(1, parseInt(days) || 0);
      const capVal = capacity.trim() === '' ? undefined : Math.max(1000, parseInt(capacity) || 0);
      const auditVal = auditDays.trim() === '' ? 0 : Math.max(1, parseInt(auditDays) || 0);
      const dailyVal = dailyDays.trim() === '' ? undefined : Math.max(1, parseInt(dailyDays) || 0);
      // 0 = 清除独立天数（回到跟随请求日志）；undefined 会被后端当缺省不修改
      const res = await api.updateLogRetention({
        max_age_days: daysVal,
        max_capacity: capVal,
        audit_max_age_days: auditVal,
        daily_max_age_days: dailyVal,
      });
      const pruned = res?.data?.daily_pruned;
      addToast(
        pruned
          ? `${t('日志保留配置已保存（每日自动清理）')} · ${t('已裁掉')} ${pruned} ${t('个更早的聚合日')}`
          : t('日志保留配置已保存（每日自动清理）'),
      );
      // 回读一次：把输入框规整成后端实际保存的值（如 "090" → "90"），
      // 并刷新 saved 快照，避免「保存后仍显示未保存」的假脏状态。
      await load(true);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSaving(false);
    }
  };

  const cleanupNow = (): void => {
    // 立即清理走的是后端「已保存」的保留策略（body 不带 days）。表单有未保存改动时
    // 若照常清理，会按旧配置删数据，与用户眼前看到的天数不符——先要求保存。
    if (dirty) {
      addToast(t('有未保存的更改，请先保存配置；立即清理按已保存的保留策略执行'), 'error');
      return;
    }
    if (!saved || (saved.days.trim() === '' && saved.auditDays.trim() === '' && saved.dailyDays.trim() === '')) {
      addToast(t('请先设置保留天数'), 'error');
      return;
    }
    setConfirmState({
      title: t('立即清理'),
      message: t('将立即删除所有早于各自保留天数的请求与审计日志，该操作不可撤销。清理明细后报表数字仍含已删请求（按天聚合独立保留，除非其保留天数也已到期）。确定继续？'),
      confirmText: t('清理'),
      danger: true,
      onConfirm: async () => {
        setCleaning(true);
        try {
          const res = await api.cleanupLogs();
          const removed = res?.data?.removed ?? 0;
          addToast(`${t('已清理')} ${removed} ${t('条日志')}`);
        } catch (err) {
          addToast(err instanceof Error ? err.message : String(err), 'error');
        } finally {
          setCleaning(false);
        }
      },
    });
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  // 读取失败：不能当作「空配置」让用户保存覆盖。给出错误与重试入口。
  if (loadError) {
    return (
      <div className="card">
        <div className="card-header">
          <h2>{t('日志保留')}</h2>
        </div>
        <div className="card-body">
          <div className="error-message">{t('日志保留配置加载失败：{{msg}}', { msg: loadError })}</div>
          <button
            type="button"
            className="btn btn-outline btn-sm"
            style={{ marginTop: 12 }}
            onClick={() => { setLoading(true); void load().finally(() => setLoading(false)); }}
          >
            {t('重试')}
          </button>
        </div>
      </div>
    );
  }

  return (
    <>
    <div className="card">
      <div className="card-header">
        <h2>{t('日志保留')}</h2>
      </div>
      <div className="card-body">
        <div className="settings-form">
          <p style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 16 }}>
            {t('请求日志与审计日志的保留策略。设置保留天数后每日自动清理过期日志；容量上限超限时自动清最旧一批。')}
          </p>
          <div className="form-group">
            <label>{t('保留天数（天）')}</label>
            <input
              className="form-input"
              type="number"
              min={1}
              max={3650}
              value={days}
              onChange={(e) => setDays(e.target.value)}
              placeholder={t('留空 = 不按天数清理')}
            />
            <div className="form-hint">{t('例：90 = 自动删除 90 天前的日志')}</div>
          </div>
          <div className="form-group">
            <label>{t('容量上限（条）')}</label>
            <input
              className="form-input"
              type="number"
              min={1000}
              max={1000000}
              value={capacity}
              onChange={(e) => setCapacity(e.target.value)}
              placeholder={t('留空 = 默认 100000')}
            />
            <div className="form-hint">{t('请求日志超过上限时自动清理最旧的一批（默认 10 万条）')}</div>
          </div>
          <div className="form-group">
            <label>{t('审计日志保留天数（天）')}</label>
            <input
              className="form-input"
              type="number"
              min={1}
              max={3650}
              value={auditDays}
              onChange={(e) => setAuditDays(e.target.value)}
              placeholder={t('留空 = 跟随请求日志天数')}
            />
            <div className="form-hint">{t('审计日志（登录/配置变更/高危操作）单独保留窗口，留空则与请求日志一致')}</div>
          </div>
          <div className="form-group">
            <label>{t('日聚合保留天数（天）')}</label>
            <input
              className="form-input"
              type="number"
              min={1}
              max={36500}
              value={dailyDays}
              onChange={(e) => setDailyDays(e.target.value)}
              placeholder={t('留空 = 永久保留')}
            />
            <div className="form-hint">
              {t('按天聚合行是报表数据源（一天一行，量极小），寿命独立于明细日志；N = 含今天在内的 N 个自然日。')}
            </div>
          </div>
          <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
            <button type="button" className="btn btn-primary btn-sm" onClick={() => void save()} disabled={saving}>
              {saving ? t('保存中…') : t('保存配置')}
            </button>
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={cleanupNow}
              disabled={cleaning || dirty}
              title={dirty ? t('有未保存的更改，请先保存配置') : undefined}
            >
              {cleaning ? t('清理中…') : t('立即清理')}
            </button>
          </div>
          {dirty && (
            <div className="form-hint" style={{ marginTop: 8, color: 'var(--warning-text, var(--text-muted))' }}>
              {t('有未保存的更改：请先保存配置，立即清理按已保存的保留策略执行。')}
            </div>
          )}
        </div>
      </div>
    </div>
    <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </>
  );
}
