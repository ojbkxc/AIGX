import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, CalendarClock, CheckCircle2, History } from 'lucide-react';
import { api } from '../api';
import type { ReportSettingsItem, TzMigrateResult } from '../types/report-tz';
import { useToast } from './Toast';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';

/** UTC 偏移展示（0 也带符号：`UTC+0`），与后端 notices 里的 `UTC{+off}` 一致 */
function fmtOffset(v: number | null | undefined): string {
  if (v === null || v === undefined) return '';
  return `UTC${v >= 0 ? '+' : ''}${v}`;
}

/**
 * 报表时区设置（系统设置 → 报表）：#20 批3 后端契约的前端面。
 *
 * 数据契约（GET/PUT `/api/report/settings`，六键 camelCase）：
 * - `timezoneOffset` 配置文件值（输入框初值）/ `effectiveOffset` 进程快照（读路径实际用的）/
 *   `storedOffset` 存量聚合行口径（null＝老库从没记过）/ `needsMigration` 迁移提示条判据 /
 *   `minOffset`·`maxOffset` 表单校验边界（**别自己抄常量**）。
 *
 * 保存即热生效（后端保存后写进程快照，无需重启），但只对**之后到达**的日志生效；
 * 存量聚合行需管理员显式点「重算历史」（`POST /api/logs/agg/tz-migrate`，幂等）。
 *
 * 三条坑（§6.4）分散落位，不是静态清单：
 * - 坑 2（分裂窗口）：底部常驻提示条 + `needsMigration` 时高亮「重算历史」按钮；
 * - 坑 3（切换当日偏低、次日自愈）：重算确认框正文；
 * - 坑 1（顺序坑：先保存再重算）：重算确认框第二行；
 * - 坑 4（先删后算绕开 shrink 守卫）：`skipped_shrink > 0` 时结果面板给警示。
 * 后端 `notices` 是中文串，**原样渲染、不过 i18n**（无法翻译后端文案）。
 */
export default function ReportSettingsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [migrating, setMigrating] = useState(false);
  const [error, setError] = useState('');
  const [tzInput, setTzInput] = useState('8');
  const [settings, setSettings] = useState<ReportSettingsItem | null>(null);
  /** 迁移请求的旧口径（升级前实际生效的偏移；缺省 0 = 生产 UTC+0 现状） */
  const [offsetFrom, setOffsetFrom] = useState('0');
  const [migrateResult, setMigrateResult] = useState<TzMigrateResult | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const applySettings = (d: ReportSettingsItem): void => {
    setSettings(d);
    setTzInput(String(d.timezoneOffset));
  };

  const load = async (): Promise<void> => {
    try {
      const res = await api.getReportSettings();
      const d = res?.data;
      // 形状容错：端点未就绪 / 返回缺字段时不清空已有值（可选链短路≠首渲染崩）
      if (d && typeof d.timezoneOffset === 'number') applySettings(d);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getReportSettings();
        const d = res?.data;
        if (d && typeof d.timezoneOffset === 'number') applySettings(d);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const save = async (): Promise<void> => {
    setError('');
    const n = Number(tzInput);
    const min = settings?.minOffset ?? -12;
    const max = settings?.maxOffset ?? 14;
    if (!Number.isInteger(n) || n < min || n > max) {
      setError(t('报表时区偏移必须是 {{min}}~{{max}} 之间的整数（小时）。', { min, max }));
      return;
    }
    setSaving(true);
    try {
      // PUT body 走 snake_case（对齐后端 ReportConfig serde；缺省字段保持原值由后端处理）
      const res = await api.saveReportSettings({ timezone_offset: n });
      const d = res?.data;
      if (d && typeof d.timezoneOffset === 'number') applySettings(d);
      addToast(t('报表设置已保存'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const runMigrate = async (): Promise<void> => {
    setError('');
    const from = Number(offsetFrom);
    if (!Number.isInteger(from)) {
      throw new Error(t('旧口径必须是整数小时偏移。'));
    }
    setMigrating(true);
    try {
      const res = await api.migrateAggTimezone({ offset_from: from });
      setMigrateResult(res);
      // 迁移落标记 ⇒ needsMigration / storedOffset 会变；重读一次刷新提示条
      await load();
      addToast(t('历史聚合行重算完成'));
    } finally {
      setMigrating(false);
    }
  };

  const openMigrateConfirm = (): void => {
    setConfirmState({
      title: t('重算历史聚合行'),
      message: (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
          <span>
            {t('将先删除区间内的旧口径聚合行再回填，重算区间为「最早聚合日 ～ 昨天」，不含今天；切换当日报表会偏低、次日自愈。')}
          </span>
          <span style={{ color: 'var(--text-muted)' }}>
            {t('请先保存偏移再重算，否则端点会报错且不写入任何数据（顺序坑）。')}
          </span>
        </div>
      ),
      confirmText: t('开始重算'),
      onConfirm: () => runMigrate(),
    });
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  const effective = settings?.effectiveOffset ?? null;
  const stored = settings?.storedOffset ?? null;
  const needsMigration = Boolean(settings?.needsMigration);
  // 保存值 ≠ 进程生效值：有写入路径绕过本端点（如 env / 通用 config 导入）时才会出现
  const notLive = settings != null && settings.timezoneOffset !== settings.effectiveOffset;

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('报表时区')}</h2>
        <p className="card-subtitle">
          {t('报表日 / 小时桶按配置的日界归日；保存后立即生效，存量聚合行需手动重算。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}

        <div className="settings-form">
          {/* 坑 2：常驻提示条（改偏移只影响之后到达的日志） */}
          <div
            className="form-hint"
            style={{ display: 'flex', gap: 6, alignItems: 'flex-start', marginBottom: 12 }}
          >
            <CalendarClock size={15} style={{ flexShrink: 0, marginTop: 1 }} />
            <span>
              {t('改偏移只影响之后到达的日志；历史聚合行需点「重算历史」重算。')}
            </span>
          </div>

          {/* needsMigration 提示条（迁移提示的唯一判据） */}
          {needsMigration && (
            <div
              className="badge badge-warning"
              style={{ display: 'flex', gap: 6, alignItems: 'center', padding: '8px 10px', marginBottom: 12, whiteSpace: 'normal', textAlign: 'left' }}
            >
              <AlertTriangle size={14} style={{ flexShrink: 0 }} />
              <span>{t('存量聚合行口径与当前生效口径不一致，建议重算历史。')}</span>
            </div>
          )}

          <div className="form-group">
            <label>{t('报表时区偏移（整数小时 UTC 偏移）')}</label>
            <input
              className="form-input"
              type="number"
              step={1}
              min={settings?.minOffset ?? -12}
              max={settings?.maxOffset ?? 14}
              value={tzInput}
              onChange={(e) => setTzInput(e.target.value)}
            />
            <div className="form-hint">
              {t('值域 {{min}} ~ {{max}}；0 = 跟随系统本地时区（生产建议显式填写，如中国业务填 8）。', {
                min: settings?.minOffset ?? -12,
                max: settings?.maxOffset ?? 14,
              })}
            </div>
          </div>

          <div className="form-group">
            <label>{t('当前生效口径')}</label>
            <div style={{ fontSize: 13.5, color: 'var(--text-main)' }}>
              {effective === null ? t('未知') : `${fmtOffset(effective)}（${t('进程快照')}）`}
            </div>
            <div className="form-hint">
              {notLive
                ? t('与上方保存值不一致：说明保存尚未生效，通常需重启服务。')
                : t('进程当前真正用于归日的口径；保存后此处会立即更新（热生效）。')}
            </div>
          </div>

          <div className="form-group">
            <label>{t('存量聚合行口径')}</label>
            <div style={{ fontSize: 13.5, color: 'var(--text-main)' }}>
              {stored === null ? t('未知（老库从未记录口径）') : fmtOffset(stored)}
            </div>
            <div className="form-hint">
              {t('存量聚合行落盘时使用的口径；重算后才会写入该标记。')}
            </div>
          </div>

          <div className="form-group">
            <label>{t('重算旧口径（升级前实际生效的偏移）')}</label>
            <input
              className="form-input"
              type="number"
              step={1}
              min={settings?.minOffset ?? -12}
              max={settings?.maxOffset ?? 14}
              value={offsetFrom}
              onChange={(e) => setOffsetFrom(e.target.value)}
            />
            <div className="form-hint">
              {t('缺省 0（生产 UTC+0 机器即 0）；仅当存量聚合行没有口径标记时才会用到，有标记时以标记为准。')}
            </div>
          </div>

          <div className="settings-actions" style={{ display: 'flex', gap: 10, flexWrap: 'wrap' }}>
            <button className="btn btn-primary" onClick={() => void save()} disabled={saving || migrating}>
              {saving ? t('保存中...') : t('保存更改')}
            </button>
            <button
              className={needsMigration ? 'btn btn-primary' : 'btn btn-outline'}
              onClick={openMigrateConfirm}
              disabled={saving || migrating}
              style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}
            >
              <History size={14} />
              {migrating ? t('重算中...') : t('重算历史')}
            </button>
          </div>

          {/* 结果面板：成功详情 + skipped_shrink 警示（坑 4）+ 后端四条坑提示原样渲染 */}
          {migrateResult && (
            <div
              style={{
                marginTop: 20,
                padding: 14,
                border: '1px solid var(--border-color)',
                borderRadius: 10,
                background: 'var(--card-bg)',
              }}
            >
              <div style={{ display: 'flex', gap: 6, alignItems: 'center', fontWeight: 600, marginBottom: 8 }}>
                <CheckCircle2 size={15} color="var(--accent-color)" />
                {t('重算完成')}
              </div>
              <div style={{ fontSize: 13, lineHeight: 1.7 }}>
                <div>
                  {t('生效起始报表日')}：<code>{migrateResult.tzEffectiveFrom}</code>
                </div>
                <div>
                  {t('删除旧口径聚合行：{{count}} 行。', { count: migrateResult.data.rows_deleted })}
                </div>
                <div>
                  {migrateResult.data.window_applied
                    ? t('窗口已应用（首次重算）')
                    : t('未改动（口径已一致，幂等短路）')}
                </div>
                {migrateResult.data.backfill.skipped_shrink > 0 && (
                  <div className="badge badge-warning" style={{ display: 'flex', gap: 6, alignItems: 'center', marginTop: 8, padding: '8px 10px', whiteSpace: 'normal', textAlign: 'left' }}>
                    <AlertTriangle size={14} style={{ flexShrink: 0 }} />
                    <span>
                      {t('窗口外另有 {{count}} 行被 shrink 守卫拦住，需人工核对明细是否已被 retention 裁剪。', {
                        count: migrateResult.data.backfill.skipped_shrink,
                      })}
                    </span>
                  </div>
                )}
              </div>
              {migrateResult.notices.length > 0 && (
                <div style={{ marginTop: 12 }}>
                  <div style={{ fontSize: 12.5, color: 'var(--text-muted)', marginBottom: 6 }}>
                    {t('本次操作的边界提示（后端文案原样展示）')}
                  </div>
                  <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12.5, color: 'var(--text-muted)', lineHeight: 1.7 }}>
                    {migrateResult.notices.map((n, i) => (
                      <li key={i}>{n}</li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
