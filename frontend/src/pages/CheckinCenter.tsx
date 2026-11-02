import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CalendarCheck, ChevronRight, Play, Plus, RefreshCw, Trash2, X } from 'lucide-react';
import { api } from '../api';
import { providerIcon } from '../lib/providerIcons';
import { useToast } from '../components/Toast';
import { Badge, Card, EmptyState, Loading, Switch } from '../components/ui';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import BenefitsPanel from '../components/BenefitsPanel';
import type { CheckinCenterData, CheckinRecord, CheckinSchedule } from '../types';
import './CheckinCenter.css';

type TabKey = 'center' | 'schedule' | 'records' | 'benefits';

const TAB_STORAGE_KEY = 'aigx-checkin-tab';

const OUTCOME_TONE: Record<string, 'success' | 'info' | 'warning' | 'danger'> = {
  success: 'success',
  already: 'info',
  pending: 'warning',
  failed: 'danger',
};

const OUTCOME_LABEL: Record<string, string> = {
  success: '成功领取',
  already: '今日已领',
  pending: '待重试',
  failed: '失败',
};

const SOURCE_LABEL: Record<string, string> = {
  schedule: '定时',
  manual: '手动',
  startup: '补签',
};

interface ScheduleForm {
  id: string;
  label: string;
  enabled: boolean;
  time: string;
  providers: string[];
}

const EMPTY_SCHEDULE: ScheduleForm = {
  id: '',
  label: '每日签到',
  enabled: true,
  time: '09:00',
  providers: [],
};

/** 北京时间 MM-DD HH:mm —— 「下次执行」用（对齐 Agent2API 的下次时刻展示） */
function beijingNextRun(time: string): string {
  const fmt = new Intl.DateTimeFormat('en-CA', {
    timeZone: 'Asia/Shanghai',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });
  const parts = fmt.formatToParts(new Date());
  const get = (type: Intl.DateTimeFormatPartTypes) =>
    parts.find((p) => p.type === type)?.value ?? '00';
  const nowHM = `${get('hour')}:${get('minute')}`;
  const today = `${get('month')}-${get('day')}`;
  if (time >= nowHM) return `${today} ${time}`;
  const tomorrow = fmt.formatToParts(new Date(Date.now() + 86_400_000));
  const t2 = (type: Intl.DateTimeFormatPartTypes) =>
    tomorrow.find((p) => p.type === type)?.value ?? '00';
  return `${t2('month')}-${t2('day')} ${time}`;
}

/** 北京时间的 YYYY-MM-DD（与后端 last_fired_date 同口径） */
function beijingDateStr(): string {
  return new Intl.DateTimeFormat('en-CA', {
    timeZone: 'Asia/Shanghai',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  }).format(new Date());
}

export default function CheckinCenter(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [tab, setTab] = useState<TabKey>(() => {
    const saved = localStorage.getItem(TAB_STORAGE_KEY);
    return saved === 'schedule' || saved === 'records' || saved === 'benefits' ? saved : 'center';
  });
  const [data, setData] = useState<CheckinCenterData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  // 单飞：一次只允许一个签到动作在途（上游有全局 running 防重），
  // 但按钮级 busy 只落在对应行上，不让一颗在跑拖灰整页
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  // 定时计划：行内编辑（editingId='new' 显示首行新建表单）
  const [editingId, setEditingId] = useState<string | null>(null);
  const [scheduleForm, setScheduleForm] = useState<ScheduleForm>(EMPTY_SCHEDULE);
  const [savingSchedule, setSavingSchedule] = useState(false);

  // 记录
  const [records, setRecords] = useState<CheckinRecord[]>([]);
  const [recordsLoading, setRecordsLoading] = useState(false);
  const [recordDays, setRecordDays] = useState(7);
  const [recordOutcome, setRecordOutcome] = useState('');
  const [recordProvider, setRecordProvider] = useState('');
  const [keyword, setKeyword] = useState('');
  const [kwApplied, setKwApplied] = useState('');

  const load = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getCheckinCenter();
      setData(res?.data ?? null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    localStorage.setItem(TAB_STORAGE_KEY, tab);
  }, [tab]);

  const loadRecords = useCallback(async () => {
    setRecordsLoading(true);
    try {
      const res = await api.getCheckinRecords({
        days: recordDays,
        provider: recordProvider || undefined,
        outcome: recordOutcome || undefined,
      });
      setRecords(res?.data?.records ?? []);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setRecordsLoading(false);
    }
  }, [recordDays, recordProvider, recordOutcome, addToast]);

  useEffect(() => {
    if (tab === 'records') void loadRecords();
  }, [tab, loadRecords]);

  const runAll = async (providers: string[] = [], key = 'all') => {
    setBusyKey(key);
    try {
      const res = await api.runCheckin({ providers });
      const d = res?.data;
      if (d) {
        addToast(`${t('签到完成')}：${d.done}/${d.total}`);
      }
      await load();
      if (tab === 'records') await loadRecords();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyKey(null);
    }
  };

  const runOne = async (accountId: string) => {
    setBusyKey(accountId);
    try {
      const res = await api.runCheckin({ account_ids: [accountId] });
      const r = res?.data?.results?.[0];
      if (r) {
        addToast(`${r.account_name}：${r.message}`, r.success ? 'success' : 'error');
      }
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyKey(null);
    }
  };

  const toggleSetting = async (enabled: boolean) => {
    if (!data) return;
    try {
      await api.updateCheckinSettings({ ...data.settings, enabled });
      await load();
      addToast(enabled ? t('已开启定时签到') : t('已关闭定时签到'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  /** 定时条目行内开关：直接 PATCH 同 id 其余字段不动 */
  const toggleSchedule = async (schedule: CheckinSchedule, enabled: boolean) => {
    try {
      await api.upsertCheckinSchedule({
        id: schedule.id,
        label: schedule.label,
        enabled,
        time: schedule.time,
        providers: schedule.providers,
      });
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  const openNewSchedule = () => {
    setScheduleForm(EMPTY_SCHEDULE);
    setEditingId('new');
  };

  const openEditSchedule = (schedule: CheckinSchedule) => {
    setScheduleForm({
      id: schedule.id,
      label: schedule.label,
      enabled: schedule.enabled,
      time: schedule.time,
      providers: schedule.providers,
    });
    setEditingId(schedule.id);
  };

  const saveSchedule = async () => {
    setSavingSchedule(true);
    try {
      await api.upsertCheckinSchedule({
        id: scheduleForm.id,
        label: scheduleForm.label,
        enabled: scheduleForm.enabled,
        time: scheduleForm.time,
        providers: scheduleForm.providers,
      });
      addToast(t('计划已保存'));
      setEditingId(null);
      setScheduleForm(EMPTY_SCHEDULE);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSavingSchedule(false);
    }
  };

  const removeSchedule = (schedule: CheckinSchedule) => {
    setConfirmState({
      title: t('删除签到计划'),
      message: `${schedule.label}（${schedule.time}）`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        await api.removeCheckinSchedule(schedule.id);
        addToast(t('已删除'));
        await load();
      },
    });
  };

  const runSchedule = async (schedule: CheckinSchedule) => {
    setBusyKey(`sched:${schedule.id}`);
    try {
      const res = await api.runCheckinSchedule(schedule.id);
      addToast(`${schedule.label}：${res?.data?.summary ?? ''}`);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setBusyKey(null);
    }
  };

  const clearRecords = () => {
    setConfirmState({
      title: t('清空签到记录'),
      message: t('将删除最近 {{days}} 天的全部签到台账（不受当前筛选限制），不可恢复。', {
        days: recordDays,
      }),
      confirmText: t('清空'),
      danger: true,
      onConfirm: async () => {
        const res = await api.clearCheckinRecords(recordDays);
        addToast(`${t('已删除')} ${res?.data?.removed ?? 0} ${t('条记录')}`);
        await loadRecords();
      },
    });
  };

  const providers = useMemo(() => data?.providers ?? [], [data?.providers]);
  const supported = useMemo(() => data?.supported_providers ?? [], [data?.supported_providers]);

  const totals = useMemo(() => {
    let done = 0;
    let total = 0;
    for (const p of providers) {
      done += p.today_done;
      total += p.today_total;
    }
    return { done, total, accounts: providers.reduce((n, p) => n + p.accounts.length, 0) };
  }, [providers]);

  const filteredRecords = useMemo(() => {
    if (!kwApplied) return records;
    const needle = kwApplied.toLowerCase();
    return records.filter(
      (r) =>
        (r.account_name || '').toLowerCase().includes(needle) ||
        r.account_id.toLowerCase().includes(needle) ||
        (r.message || '').toLowerCase().includes(needle) ||
        r.provider.toLowerCase().includes(needle),
    );
  }, [records, kwApplied]);

  const hasRecordFilter = !!kwApplied || !!recordOutcome || !!recordProvider;

  /** 顶栏徽标（对齐 Agent2API #checkin-badge） */
  const autoBadge = useMemo(() => {
    if (!data) return null;
    const enabled = data.settings.enabled;
    const firedToday = (data.schedules ?? []).some(
      (s) => s.enabled && s.last_fired_date === beijingDateStr(),
    );
    if (!enabled) {
      return (
        <span className="ck-tab-badge is-off">{t('定时已关闭')}</span>
      );
    }
    return (
      <span className={`ck-tab-badge ${firedToday ? 'is-ok' : 'is-on'}`}>
        {firedToday ? t('已开启 · 今日已执行') : t('定时已开启')}
      </span>
    );
  }, [data, t]);

  const toggleGroup = (provider: string): void => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(provider)) next.delete(provider);
      else next.add(provider);
      return next;
    });
  };

  const tabs = [
    { key: 'center' as TabKey, label: t('签到中心') },
    { key: 'schedule' as TabKey, label: t('定时签到') },
    { key: 'records' as TabKey, label: t('签到记录') },
    { key: 'benefits' as TabKey, label: t('福利通道') },
  ];

  /** 提供商行徽章（对齐 Agent2API GroupBadge） */
  const groupBadge = (done: number, total: number): JSX.Element => {
    if (total === 0) return <Badge tone="neutral">{t('无账号')}</Badge>;
    if (done === total) return <Badge tone="success">{t('已签')}</Badge>;
    if (done > 0) return <Badge tone="warning">{t('部分')}</Badge>;
    return <Badge tone="info">{t('待签')}</Badge>;
  };

  const scheduleEditor = (
    <div className="ck-editor">
      <div className="ck-editor-grid">
        <label className="form-group">
          <span className="ck-editor-label">{t('名称')}</span>
          <input
            className="form-input"
            value={scheduleForm.label}
            onChange={(e) => setScheduleForm({ ...scheduleForm, label: e.target.value })}
          />
        </label>
        <label className="form-group">
          <span className="ck-editor-label">{t('每天时刻（北京时间·错过自动补签）')}</span>
          <input
            type="time"
            className="form-input ck-time-input"
            value={scheduleForm.time}
            onChange={(e) => setScheduleForm({ ...scheduleForm, time: e.target.value })}
          />
        </label>
        <label className="ck-editor-enabled">
          <Switch
            checked={scheduleForm.enabled}
            onChange={(next) => setScheduleForm({ ...scheduleForm, enabled: next })}
            ariaLabel={t('启用')}
          />
          <span>{t('启用')}</span>
        </label>
      </div>
      <div className="form-group">
        <div className="ck-scope-head">
          <span className="ck-editor-label">{t('签到范围')}</span>
          <span className="ck-scope-tools">
            <button type="button" onClick={() => setScheduleForm({ ...scheduleForm, providers: [...supported] })}>
              {t('全选')}
            </button>
            <button
              type="button"
              onClick={() => setScheduleForm({ ...scheduleForm, providers: [] })}
            >
              {t('清空')}
            </button>
          </span>
        </div>
        <div className="ck-provider-picks">
          {supported.map((provider) => {
            const checked = scheduleForm.providers.includes(provider);
            return (
              <label key={provider} className="ck-provider-pick">
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={(e) => {
                    const next = e.target.checked
                      ? [...scheduleForm.providers, provider]
                      : scheduleForm.providers.filter((p) => p !== provider);
                    setScheduleForm({ ...scheduleForm, providers: next });
                  }}
                />
                {provider}
              </label>
            );
          })}
        </div>
        {scheduleForm.providers.length === 0 && (
          <span className="form-hint">{t('不选 = 全部支持的上游')}</span>
        )}
      </div>
      <div className="ck-form-actions">
        <button type="button" className="btn btn-outline btn-sm" onClick={() => setEditingId(null)}>
          {t('取消')}
        </button>
        <button
          type="button"
          className="btn btn-primary btn-sm"
          onClick={() => void saveSchedule()}
          disabled={savingSchedule}
        >
          {savingSchedule ? t('保存中...') : t('保存')}
        </button>
      </div>
    </div>
  );

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('签到中心')}</h1>
          <p>
            {t(
              '替账号池里的上游账号每日领取它们自带的额度。与钱包里的「用户签到」是两套。'
            )}
          </p>
        </div>
        <button
          className="btn btn-outline btn-sm"
          onClick={() => void load()}
          disabled={loading}
          style={{ gap: 6 }}
        >
          <RefreshCw size={13} /> {t('刷新')}
        </button>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="sub-tabs">
        {tabs.map((item) => (
          <button
            key={item.key}
            className={`sub-tab${tab === item.key ? ' active' : ''}`}
            onClick={() => setTab(item.key)}
          >
            {item.label}
          </button>
        ))}
        <span className="ck-tab-badge-slot">{autoBadge}</span>
      </div>

      {loading ? (
        <Loading text={t('加载签到中心')} />
      ) : tab === 'center' ? (
        <>
          <div className="ck-stats">
            <div className="ck-stat">
              <CalendarCheck size={16} className="ck-stat-icon" />
              <div className="ck-stat-body">
                <div className="ck-stat-value">
                  {totals.done}/{totals.total}
                </div>
                <div className="ck-stat-label">{t('今日完成')}</div>
                <div className="ck-progress" role="progressbar" aria-valuenow={totals.done} aria-valuemax={totals.total}>
                  <span
                    className="ck-progress-fill"
                    style={{
                      width: totals.total > 0 ? `${Math.min(100, (totals.done / totals.total) * 100)}%` : '0%',
                    }}
                  />
                </div>
              </div>
            </div>
            <div className="ck-stat">
              <div className="ck-stat-body">
                <div className="ck-stat-value">{totals.accounts}</div>
                <div className="ck-stat-label">{t('可签到账号')}</div>
              </div>
            </div>
            <div className="ck-stat">
              <div className="ck-stat-body">
                <div className="ck-stat-value">{supported.length}</div>
                <div className="ck-stat-label">{t('支持的上游')}</div>
              </div>
            </div>
            <button type="button" className="ck-stat ck-stat-link" onClick={() => setTab('schedule')}>
              <div className="ck-stat-body">
                <div className="ck-stat-value">
                  {(data?.schedules ?? []).filter((s) => s.enabled).length}/
                  {(data?.schedules ?? []).length}
                </div>
                <div className="ck-stat-label">{t('定时计划启用')}</div>
              </div>
            </button>
          </div>

          <Card
            title={t('每日签到')}
            actions={
              <button
                className="btn btn-primary btn-sm"
                onClick={() => void runAll()}
                disabled={busyKey !== null || totals.accounts === 0}
                style={{ gap: 6 }}
              >
                <Play size={12} /> {busyKey === 'all' ? t('签到中...') : t('立即全部签到')}
              </button>
            }
          >
            {providers.length === 0 ? (
              <EmptyState
                icon="📅"
                message={t(
                  '账号池里还没有可签到的账号。支持的上游：' + supported.join('、')
                )}
              />
            ) : (
              <div className="ck-prov-list">
                {providers.map((group) => {
                  const open = expanded.has(group.provider);
                  const icon = providerIcon(group.provider);
                  return (
                    <div key={group.provider} className="ck-prov-block">
                      <div
                        className={`ck-prov-row${open ? ' is-open' : ''}`}
                        role="button"
                        tabIndex={0}
                        aria-expanded={open}
                        onClick={() => toggleGroup(group.provider)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            e.preventDefault();
                            toggleGroup(group.provider);
                          }
                        }}
                      >
                        {icon ? (
                          <img className="ck-prov-logo" src={icon as string} alt="" />
                        ) : (
                          <span className="ck-prov-logo ck-prov-letter">
                            {(group.label || group.provider).slice(0, 1).toUpperCase()}
                          </span>
                        )}
                        <div className="ck-prov-main">
                          <span className="ck-prov-name">{group.label}</span>
                          <span className="ck-prov-sub">
                            {t('{{n}} 个账号', { n: group.accounts.length })} · {t('已签')} {group.today_done}
                          </span>
                        </div>
                        {groupBadge(group.today_done, group.today_total)}
                        <ChevronRight size={14} className={`ck-prov-caret${open ? ' is-open' : ''}`} />
                      </div>
                      {open && (
                        <div className="ck-prov-accounts">
                          <div className="ck-prov-acts">
                            <button
                              type="button"
                              className="btn btn-outline btn-sm"
                              onClick={(e) => {
                                e.stopPropagation();
                                void runAll([group.provider], `g:${group.provider}`);
                              }}
                              disabled={busyKey !== null}
                            >
                              <Play size={12} />{' '}
                              {busyKey === `g:${group.provider}` ? t('签到中...') : t('签到本组')}
                            </button>
                          </div>
                          <table className="ck-table">
                            <thead>
                              <tr>
                                <th>{t('账号')}</th>
                                <th style={{ width: 90 }}>{t('今日')}</th>
                                <th style={{ width: 150 }}>{t('上次签到')}</th>
                                <th>{t('最近结果')}</th>
                                <th style={{ width: 90 }}>{t('操作')}</th>
                              </tr>
                            </thead>
                            <tbody>
                              {group.accounts.map((account) => (
                                <tr key={account.id}>
                                  <td>{account.name || account.id}</td>
                                  <td>
                                    {account.checked_in_today ? (
                                      <Badge tone="success">{t('已签')}</Badge>
                                    ) : (
                                      <Badge tone="info">{t('待签')}</Badge>
                                    )}
                                  </td>
                                  <td className="ck-time-subtle">
                                    {account.last_ts
                                      ? new Date(account.last_ts * 1000).toLocaleString()
                                      : '—'}
                                  </td>
                                  <td className="ck-message">
                                    {account.last_outcome && (
                                      <Badge tone={OUTCOME_TONE[account.last_outcome] ?? 'neutral'}>
                                        {t(OUTCOME_LABEL[account.last_outcome] ?? account.last_outcome)}
                                      </Badge>
                                    )}{' '}
                                    {account.last_message || '—'}
                                  </td>
                                  <td>
                                    <button
                                      type="button"
                                      className="btn btn-outline btn-sm"
                                      onClick={(e) => {
                                        e.stopPropagation();
                                        void runOne(account.id);
                                      }}
                                      disabled={busyKey !== null}
                                    >
                                      {busyKey === account.id
                                        ? t('签到中…')
                                        : account.checked_in_today
                                          ? t('重签')
                                          : t('签到')}
                                    </button>
                                  </td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </Card>
        </>
      ) : tab === 'schedule' ? (
        <>
          <Card title={t('定时签到')}>
            <div className="ck-setting-row">
              <div>
                <div className="ck-setting-label">{t('启用定时签到')}</div>
                <div className="ck-setting-hint">
                  {t('开启后按下面计划的时刻（北京时间）自动签到，当天只跑一次。')}
                </div>
              </div>
              <Switch
                checked={!!data?.settings.enabled}
                onChange={(next) => void toggleSetting(next)}
                ariaLabel={t('启用定时签到')}
              />
            </div>
            <div className="ck-setting-row">
              <div>
                <div className="ck-setting-label">{t('账号间隔（毫秒）')}</div>
                <div className="ck-setting-hint">
                  {t('逐个签到之间的等待，避免并发触发上游风控。')}
                </div>
              </div>
              <span className="ck-setting-value">{data?.settings.gap_ms ?? 800}</span>
            </div>
          </Card>

          <Card
            title={t('签到计划')}
            actions={
              <button
                className="btn btn-outline btn-sm"
                onClick={openNewSchedule}
                disabled={editingId !== null}
                style={{ gap: 6 }}
              >
                <Plus size={13} /> {t('添加条目')}
              </button>
            }
          >
            {(data?.schedules ?? []).length === 0 && editingId !== 'new' ? (
              <EmptyState icon="⏰" message={t('还没有签到计划')} />
            ) : (
              <div className="ck-sched-list">
                {editingId === 'new' && (
                  <div className="ck-sched-row is-editing">
                    <div className="ck-sched-body">{scheduleEditor}</div>
                  </div>
                )}
                {(data?.schedules ?? []).map((schedule) => {
                  const scope =
                    schedule.providers.length === 0 ||
                    schedule.providers.length >= supported.length
                      ? t('全部 {{n}} 家', { n: supported.length })
                      : schedule.providers.join('、');
                  if (editingId === schedule.id) {
                    return (
                      <div key={schedule.id} className="ck-sched-row is-editing">
                        <div className="ck-sched-body">{scheduleEditor}</div>
                      </div>
                    );
                  }
                  return (
                    <div key={schedule.id} className={`ck-sched-row${schedule.enabled ? '' : ' is-off'}`}>
                      <Switch
                        className="ck-sched-switch"
                        checked={schedule.enabled}
                        ariaLabel={t('启用/停用')}
                        onChange={(next) => void toggleSchedule(schedule, next)}
                      />
                      <div className="ck-sched-main">
                        <div className="ck-sched-name">
                          {schedule.label}
                          {!schedule.enabled && <Badge tone="neutral">{t('已停用')}</Badge>}
                        </div>
                        <div className="ck-sched-desc">
                          {t('每天 {{time}} · {{scope}}', { time: schedule.time, scope })}
                        </div>
                        <div className="ck-sched-desc ck-sched-last">
                          {schedule.last_fired_date
                            ? t('上次执行：{{d}} · {{r}}', {
                                d: schedule.last_fired_date,
                                r: schedule.last_result || '—',
                              })
                            : t('从未执行')}
                        </div>
                      </div>
                      <div className="ck-sched-next">
                        {schedule.enabled ? t('下次 {{when}}', { when: beijingNextRun(schedule.time) }) : '—'}
                      </div>
                      <div className="ck-sched-ops">
                        <button
                          type="button"
                          className="btn btn-outline btn-sm"
                          onClick={() => openEditSchedule(schedule)}
                        >
                          {t('编辑')}
                        </button>
                        <button
                          type="button"
                          className="btn btn-outline btn-sm"
                          onClick={() => void runSchedule(schedule)}
                          disabled={busyKey !== null}
                        >
                          {busyKey === `sched:${schedule.id}` ? t('执行中...') : t('立即')}
                        </button>
                        <button
                          type="button"
                          className="btn btn-danger btn-sm"
                          onClick={() => removeSchedule(schedule)}
                        >
                          <Trash2 size={12} />
                        </button>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </Card>
        </>
      ) : tab === 'benefits' ? (
        <BenefitsPanel />
      ) : (
        <Card
          title={t('签到记录')}
          actions={
            <div className="ck-records-tools">
              <div className="ck-seg" role="group" aria-label={t('时间筛选')}>
                {[
                  { v: 1, label: t('今天') },
                  { v: 7, label: t('7 天') },
                  { v: 30, label: t('30 天') },
                  { v: 3650, label: t('全部') },
                ].map((item) => (
                  <button
                    key={item.v}
                    type="button"
                    className={`ck-seg-item${recordDays === item.v ? ' is-active' : ''}`}
                    onClick={() => setRecordDays(item.v)}
                  >
                    {item.label}
                  </button>
                ))}
              </div>
              <select
                className="form-input"
                style={{ maxWidth: 120 }}
                value={recordProvider}
                onChange={(e) => setRecordProvider(e.target.value)}
              >
                <option value="">{t('全部上游')}</option>
                {supported.map((p) => (
                  <option key={p} value={p}>
                    {p}
                  </option>
                ))}
              </select>
              <select
                className="form-input"
                style={{ maxWidth: 110 }}
                value={recordOutcome}
                onChange={(e) => setRecordOutcome(e.target.value)}
              >
                <option value="">{t('全部结果')}</option>
                <option value="success">{t('成功领取')}</option>
                <option value="already">{t('今日已领')}</option>
                <option value="pending">{t('待重试')}</option>
                <option value="failed">{t('失败')}</option>
              </select>
              <input
                className="form-input"
                style={{ maxWidth: 150 }}
                placeholder={t('搜索账号或说明')}
                value={keyword}
                onChange={(e) => {
                  setKeyword(e.target.value);
                  if (e.target.value === '') setKwApplied('');
                }}
                onBlur={() => setKwApplied(keyword.trim())}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') setKwApplied(keyword.trim());
                  if (e.key === 'Escape') {
                    setKeyword('');
                    setKwApplied('');
                  }
                }}
              />
              <button className="btn btn-danger btn-sm" onClick={clearRecords}>
                <X size={12} /> {t('清空')}
              </button>
            </div>
          }
        >
          {recordsLoading ? (
            <Loading text={t('加载记录')} />
          ) : filteredRecords.length === 0 ? (
            <EmptyState
              icon="📝"
              message={hasRecordFilter ? t('没有匹配的签到记录') : t('暂无签到记录')}
            />
          ) : (
            <table className="ck-table">
              <thead>
                <tr>
                  <th style={{ width: 150 }}>{t('时间')}</th>
                  <th>{t('账号')}</th>
                  <th style={{ width: 110 }}>{t('上游')}</th>
                  <th style={{ width: 90 }}>{t('来源')}</th>
                  <th style={{ width: 100 }}>{t('结果')}</th>
                  <th>{t('说明')}</th>
                </tr>
              </thead>
              <tbody>
                {filteredRecords.map((record, index) => (
                  <tr key={`${record.ts}-${record.account_id}-${index}`}>
                    <td>
                      {new Date(record.ts * 1000).toLocaleString()}
                      {record.date && <div className="ck-time-subtle">{record.date}</div>}
                    </td>
                    <td>{record.account_name || record.account_id}</td>
                    <td>{record.provider}</td>
                    <td className="ck-record-source">
                      {t(SOURCE_LABEL[record.source] ?? record.source)}
                    </td>
                    <td>
                      <Badge tone={OUTCOME_TONE[record.outcome] ?? 'neutral'}>
                        {t(OUTCOME_LABEL[record.outcome] ?? record.outcome)}
                      </Badge>
                    </td>
                    <td className={record.outcome === 'failed' ? 'ck-message ck-record-detail-fail' : 'ck-message'}>
                      {record.message || '—'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Card>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
