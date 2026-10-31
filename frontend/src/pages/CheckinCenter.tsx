import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CalendarCheck, Play, Trash2, Plus, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Card, Loading, EmptyState, Badge, Switch } from '../components/ui';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import type { CheckinCenterData, CheckinRecord, CheckinSchedule } from '../types';
import './CheckinCenter.css';

type TabKey = 'center' | 'schedule' | 'records';

const OUTCOME_TONE: Record<string, 'success' | 'info' | 'warning' | 'danger'> = {
  success: 'success',
  already: 'info',
  pending: 'warning',
  failed: 'danger',
};

const OUTCOME_LABEL: Record<string, string> = {
  success: '已领取',
  already: '今日已领',
  pending: '待重试',
  failed: '失败',
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

export default function CheckinCenter(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [tab, setTab] = useState<TabKey>('center');
  const [data, setData] = useState<CheckinCenterData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [running, setRunning] = useState(false);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // 定时计划表单
  const [scheduleForm, setScheduleForm] = useState<ScheduleForm>(EMPTY_SCHEDULE);
  const [savingSchedule, setSavingSchedule] = useState(false);

  // 记录
  const [records, setRecords] = useState<CheckinRecord[]>([]);
  const [recordsLoading, setRecordsLoading] = useState(false);
  const [recordDays, setRecordDays] = useState(7);
  const [recordOutcome, setRecordOutcome] = useState('');

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

  const loadRecords = useCallback(async () => {
    setRecordsLoading(true);
    try {
      const res = await api.getCheckinRecords({
        days: recordDays,
        outcome: recordOutcome || undefined,
      });
      setRecords(res?.data?.records ?? []);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setRecordsLoading(false);
    }
  }, [recordDays, recordOutcome, addToast]);

  useEffect(() => {
    if (tab === 'records') void loadRecords();
  }, [tab, loadRecords]);

  const runAll = async (providers: string[] = []) => {
    setRunning(true);
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
      setRunning(false);
    }
  };

  const runOne = async (accountId: string) => {
    setRunning(true);
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
      setRunning(false);
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
    setRunning(true);
    try {
      const res = await api.runCheckinSchedule(schedule.id);
      addToast(`${schedule.label}：${res?.data?.summary ?? ''}`);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setRunning(false);
    }
  };

  const clearRecords = () => {
    setConfirmState({
      title: t('清空签到记录'),
      message: `${t('将删除最近')} ${recordDays} ${t('天的签到台账，不可恢复。')}`,
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

  const tabs = [
    { key: 'center' as TabKey, label: t('签到中心') },
    { key: 'schedule' as TabKey, label: t('定时签到') },
    { key: 'records' as TabKey, label: t('签到记录') },
  ];

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
        <div style={{ display: 'flex', gap: 8 }}>
          <button
            className="btn btn-outline btn-sm"
            onClick={() => void load()}
            disabled={loading}
            style={{ gap: 6 }}
          >
            <RefreshCw size={13} /> {t('刷新')}
          </button>
          <button
            className="btn btn-primary btn-sm"
            onClick={() => void runAll()}
            disabled={running || totals.accounts === 0}
            style={{ gap: 6 }}
          >
            <Play size={13} /> {running ? t('签到中...') : t('立即全部签到')}
          </button>
        </div>
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
      </div>

      {loading ? (
        <Loading text={t('加载签到中心')} />
      ) : tab === 'center' ? (
        <>
          <div className="ck-stats">
            <div className="ck-stat">
              <CalendarCheck size={16} className="ck-stat-icon" />
              <div>
                <div className="ck-stat-value">
                  {totals.done}/{totals.total}
                </div>
                <div className="ck-stat-label">{t('今日完成')}</div>
              </div>
            </div>
            <div className="ck-stat">
              <div>
                <div className="ck-stat-value">{totals.accounts}</div>
                <div className="ck-stat-label">{t('可签到账号')}</div>
              </div>
            </div>
            <div className="ck-stat">
              <div>
                <div className="ck-stat-value">{supported.length}</div>
                <div className="ck-stat-label">{t('支持的上游')}</div>
              </div>
            </div>
            <div className="ck-stat">
              <div>
                <div className="ck-stat-value">
                  {data?.settings.enabled ? t('已开启') : t('已关闭')}
                </div>
                <div className="ck-stat-label">{t('定时签到')}</div>
              </div>
            </div>
          </div>

          {providers.length === 0 ? (
            <Card>
              <EmptyState
                icon="📅"
                message={t(
                  '账号池里还没有可签到的账号。支持的上游：' + supported.join('、')
                )}
              />
            </Card>
          ) : (
            providers.map((group) => (
              <Card
                key={group.provider}
                title={`${group.label} · ${group.today_done}/${group.today_total}`}
                actions={
                  <button
                    className="btn btn-outline btn-sm"
                    onClick={() => void runAll([group.provider])}
                    disabled={running}
                    style={{ gap: 6 }}
                  >
                    <Play size={12} /> {t('签到本组')}
                  </button>
                }
              >
                <table className="ck-table">
                  <thead>
                    <tr>
                      <th>{t('账号')}</th>
                      <th style={{ width: 110 }}>{t('今日')}</th>
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
                            <Badge tone="neutral">{t('未签')}</Badge>
                          )}
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
                            className="btn btn-outline btn-sm"
                            onClick={() => void runOne(account.id)}
                            disabled={running}
                          >
                            {t('签到')}
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </Card>
            ))
          )}
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
                onClick={() => setScheduleForm(EMPTY_SCHEDULE)}
                style={{ gap: 6 }}
              >
                <Plus size={13} /> {t('新建计划')}
              </button>
            }
          >
            {(data?.schedules ?? []).length === 0 ? (
              <EmptyState icon="⏰" message={t('还没有签到计划')} />
            ) : (
              <table className="ck-table">
                <thead>
                  <tr>
                    <th>{t('名称')}</th>
                    <th style={{ width: 80 }}>{t('时刻')}</th>
                    <th>{t('范围')}</th>
                    <th>{t('上次')}</th>
                    <th style={{ width: 160 }}>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {(data?.schedules ?? []).map((schedule) => (
                    <tr key={schedule.id}>
                      <td>
                        {schedule.label}{' '}
                        {!schedule.enabled && <Badge tone="neutral">{t('停用')}</Badge>}
                      </td>
                      <td>{schedule.time}</td>
                      <td>{schedule.providers.length ? schedule.providers.join('、') : t('全部')}</td>
                      <td className="ck-message">
                        {schedule.last_fired_date || '—'} {schedule.last_result}
                      </td>
                      <td>
                        <div className="ck-actions">
                          <button
                            className="btn btn-outline btn-sm"
                            onClick={() =>
                              setScheduleForm({
                                id: schedule.id,
                                label: schedule.label,
                                enabled: schedule.enabled,
                                time: schedule.time,
                                providers: schedule.providers,
                              })
                            }
                          >
                            {t('编辑')}
                          </button>
                          <button
                            className="btn btn-outline btn-sm"
                            onClick={() => void runSchedule(schedule)}
                            disabled={running}
                          >
                            {t('立即')}
                          </button>
                          <button
                            className="btn btn-danger btn-sm"
                            onClick={() => removeSchedule(schedule)}
                          >
                            <Trash2 size={12} />
                          </button>
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Card>

          <Card title={scheduleForm.id ? t('编辑计划') : t('新建计划')}>
            <div className="ck-form">
              <div className="form-group">
                <label>{t('名称')}</label>
                <input
                  className="form-input"
                  value={scheduleForm.label}
                  onChange={(e) => setScheduleForm({ ...scheduleForm, label: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('每天时刻（HH:MM，北京时间）')}</label>
                <input
                  className="form-input"
                  value={scheduleForm.time}
                  onChange={(e) => setScheduleForm({ ...scheduleForm, time: e.target.value })}
                  placeholder="09:00"
                />
              </div>
              <div className="form-group">
                <label>{t('签到范围')}</label>
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
                <span className="form-hint">{t('不选 = 全部支持的上游')}</span>
              </div>
              <div className="ck-form-actions">
                <button
                  className="btn btn-primary"
                  onClick={() => void saveSchedule()}
                  disabled={savingSchedule}
                >
                  {savingSchedule ? t('保存中...') : t('保存计划')}
                </button>
              </div>
            </div>
          </Card>
        </>
      ) : (
        <Card
          title={t('签到记录')}
          actions={
            <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
              <select
                className="form-input"
                style={{ maxWidth: 110 }}
                value={recordDays}
                onChange={(e) => setRecordDays(Number(e.target.value))}
              >
                <option value={1}>{t('今天')}</option>
                <option value={7}>{t('最近 7 天')}</option>
                <option value={30}>{t('最近 30 天')}</option>
              </select>
              <select
                className="form-input"
                style={{ maxWidth: 110 }}
                value={recordOutcome}
                onChange={(e) => setRecordOutcome(e.target.value)}
              >
                <option value="">{t('全部结果')}</option>
                <option value="success">{t('已领取')}</option>
                <option value="already">{t('今日已领')}</option>
                <option value="pending">{t('待重试')}</option>
                <option value="failed">{t('失败')}</option>
              </select>
              <button className="btn btn-danger btn-sm" onClick={clearRecords}>
                <Trash2 size={12} /> {t('清空')}
              </button>
            </div>
          }
        >
          {recordsLoading ? (
            <Loading text={t('加载记录')} />
          ) : records.length === 0 ? (
            <EmptyState icon="📝" message={t('暂无签到记录')} />
          ) : (
            <table className="ck-table">
              <thead>
                <tr>
                  <th style={{ width: 150 }}>{t('时间')}</th>
                  <th>{t('账号')}</th>
                  <th style={{ width: 110 }}>{t('上游')}</th>
                  <th style={{ width: 100 }}>{t('来源')}</th>
                  <th style={{ width: 100 }}>{t('结果')}</th>
                  <th>{t('说明')}</th>
                </tr>
              </thead>
              <tbody>
                {records.map((record, index) => (
                  <tr key={`${record.ts}-${record.account_id}-${index}`}>
                    <td>{new Date(record.ts * 1000).toLocaleString()}</td>
                    <td>{record.account_name || record.account_id}</td>
                    <td>{record.provider}</td>
                    <td>{record.source === 'schedule' ? t('定时') : t('手动')}</td>
                    <td>
                      <Badge tone={OUTCOME_TONE[record.outcome] ?? 'neutral'}>
                        {t(OUTCOME_LABEL[record.outcome] ?? record.outcome)}
                      </Badge>
                    </td>
                    <td className="ck-message">{record.message || '—'}</td>
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
