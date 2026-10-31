import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  AlertTriangle, CheckCircle2, Download, ExternalLink, Loader2, Power, RefreshCw, RotateCcw,
} from 'lucide-react';
import { api } from '../api';
import { useToast } from './Toast';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';
import type { UpdateSettingsItem, UpdateStatusItem } from '../types';
import './UpdatePanel.css';

interface SettingsForm {
  enabled: boolean;
  auto_check: boolean;
  auto_apply: boolean;
  interval_hours: string;
  repo: string;
  api_base: string;
  include_prerelease: boolean;
  proxy: string;
  restart_after_apply: boolean;
  token: string;
  tokenConfigured: boolean;
}

function fmtBytes(bytes: number | null | undefined): string {
  const n = Number(bytes || 0);
  if (n >= 1048576) return `${(n / 1048576).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${n} B`;
}

function fmtTime(ts: number | null | undefined): string {
  if (!ts) return '';
  return new Date(ts * 1000).toLocaleString();
}

/**
 * 服务器更新面板 —— 检查 GitHub Release、下载替换、重启、回滚与自动更新设置。
 *
 * 应用更新在后台执行（下载进度轮询 status），替换成功后按配置 exec 自重启；
 * 面板在重启期间继续轮询，服务回来后自动换成新版本号。
 */
export default function UpdatePanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [status, setStatus] = useState<UpdateStatusItem | null>(null);
  const [settings, setSettings] = useState<SettingsForm | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [checking, setChecking] = useState(false);
  const [saving, setSaving] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [clearToken, setClearToken] = useState(false);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const mountedRef = useRef(true);
  const restartingRef = useRef(false);
  /** 重启请求时刻的 startedAt：新进程的 startedAt 变了才算重启完成（exec 不换 PID） */
  const restartBaselineRef = useRef<number | null>(null);
  /** 重启请求发起时间（超时判定用） */
  const restartStartRef = useRef(0);

  const loadStatus = useCallback(async (showError: boolean): Promise<void> => {
    try {
      const res = await api.getUpdateStatus();
      if (!mountedRef.current) return;
      if (res.data) {
        setStatus(res.data);
        if (restartingRef.current) {
          const baseline = restartBaselineRef.current;
          const startedAt = res.data.startedAt ?? 0;
          const restarted = baseline != null && startedAt !== baseline;
          if (restarted) {
            // 新进程起来了：换前端再回来（无缝更新的关键一步）
            restartingRef.current = false;
            setRestarting(false);
            addToast(t('服务已重启，正在刷新界面'));
            window.location.reload();
            return;
          }
          if (res.data.restartError) {
            restartingRef.current = false;
            setRestarting(false);
            setError(res.data.restartError);
            return;
          }
          if (Date.now() - restartStartRef.current > 40000) {
            // 40s 还是同一个进程：不假报成功
            restartingRef.current = false;
            setRestarting(false);
            setError(
              res.data.restartError
                || t('重启超时未检测到新进程（可能 exec 失败），请在服务器上查看日志'),
            );
          }
        }
      }
      if (showError) setError('');
    } catch (e) {
      if (!mountedRef.current) return;
      // 重启期间请求失败是预期行为：静默等待下一轮轮询
      if (showError && !restartingRef.current) {
        setError(e instanceof Error ? e.message : String(e));
      }
    }
  }, [addToast, t]);

  const loadSettings = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getUpdateSettings();
      const data: UpdateSettingsItem | undefined = res.data;
      if (!data || !mountedRef.current) return;
      setSettings({
        enabled: data.enabled,
        auto_check: data.auto_check,
        auto_apply: data.auto_apply,
        interval_hours: String(data.interval_hours),
        repo: data.repo,
        api_base: data.api_base,
        include_prerelease: data.include_prerelease,
        proxy: data.proxy,
        restart_after_apply: data.restart_after_apply,
        token: '',
        tokenConfigured: data.tokenConfigured,
      });
    } catch (e) {
      if (mountedRef.current) {
        setError(`${t('读取更新设置失败')}：${e instanceof Error ? e.message : String(e)}`);
      }
    }
  }, [t]);

  useEffect(() => {
    mountedRef.current = true;
    void (async () => {
      try {
        await Promise.all([loadStatus(true), loadSettings()]);
      } catch (e) {
        if (mountedRef.current) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (mountedRef.current) setLoading(false);
      }
    })();
    return () => { mountedRef.current = false; };
  }, [loadSettings, loadStatus]);

  // 活跃阶段 2s、空闲 15s 轮询：重启后版本号能自动刷新
  const active = Boolean(
    status?.checking || status?.applying || status?.download?.active || restarting,
  );
  useEffect(() => {
    const timer = window.setInterval(() => { void loadStatus(false); }, active ? 2000 : 15000);
    return () => window.clearInterval(timer);
  }, [active, loadStatus]);

  const runCheck = async (): Promise<void> => {
    setChecking(true);
    setError('');
    try {
      const res = await api.checkUpdate();
      if (res.data) setStatus(res.data);
      addToast(t('检查完成'));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setChecking(false);
    }
  };

  const confirmApply = (): void => {
    const version = status?.latestVersion ?? '';
    setConfirmState({
      title: t('下载并更新'),
      message: t('确定下载并应用 v{{version}}？替换程序与界面文件后会自动重启，期间请求会短暂失败。', { version }),
      confirmText: t('开始更新'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          const res = await api.applyUpdate();
          if (res.data) setStatus(res.data);
          addToast(t('更新包下载中…'));
        } catch (e) {
          setError(e instanceof Error ? e.message : String(e));
        }
      },
    });
  };

  const confirmRestart = (): void => {
    setConfirmState({
      title: t('重启服务'),
      message: t('确定重启 AIGX 服务？重启期间请求会短暂失败。'),
      confirmText: t('立即重启'),
      danger: true,
      onConfirm: async () => {
        try {
          await api.restartService();
          restartBaselineRef.current = status?.startedAt ?? null;
          restartStartRef.current = Date.now();
          restartingRef.current = true;
          setRestarting(true);
          addToast(t('服务正在重启（约 2 秒），完成后界面自动刷新'));
          window.setTimeout(() => { void loadStatus(false); }, 3000);
        } catch (e) {
          setError(e instanceof Error ? e.message : String(e));
        }
      },
    });
  };

  /** 修复前端（修复模式重应用：跳过「已是最新」拦截，重装最新 .upd） */
  const confirmRepair = (): void => {
    setConfirmState({
      title: t('修复前端（重新应用更新）'),
      message: t('将重新下载并安装最新更新包（含 static/ 前端产物），修复前后端版本不一致。安装后需重启服务。'),
      confirmText: t('重新应用'),
      danger: false,
      onConfirm: async () => {
        try {
          await api.applyUpdate(true);
          addToast(t('已开始重新应用，完成后请重启服务'));
          void loadStatus(false);
        } catch (e) {
          setError(e instanceof Error ? e.message : String(e));
        }
      },
    });
  };

  const confirmRollback = (): void => {
    setConfirmState({
      title: t('回滚上一版本'),
      message: t('确定回滚到上一版本？回滚后会重启服务，当前版本的程序会被替换。'),
      confirmText: t('回滚'),
      danger: true,
      onConfirm: async () => {
        try {
          await api.rollbackUpdate();
          if (settings?.restart_after_apply !== false) {
            restartBaselineRef.current = status?.startedAt ?? null;
            restartStartRef.current = Date.now();
            restartingRef.current = true;
            setRestarting(true);
            addToast(t('已回滚，服务正在重启'));
            window.setTimeout(() => { void loadStatus(false); }, 3000);
          } else {
            addToast(t('已回滚，请手动重启服务'));
            void loadStatus(false);
          }
        } catch (e) {
          setError(e instanceof Error ? e.message : String(e));
        }
      },
    });
  };

  const saveSettings = async (): Promise<void> => {
    if (!settings) return;
    setSaving(true);
    setError('');
    try {
      const payload: Record<string, unknown> = {
        enabled: settings.enabled,
        auto_check: settings.auto_check,
        auto_apply: settings.auto_apply,
        interval_hours: Number(settings.interval_hours) || 24,
        repo: settings.repo,
        api_base: settings.api_base,
        include_prerelease: settings.include_prerelease,
        proxy: settings.proxy,
        restart_after_apply: settings.restart_after_apply,
      };
      if (settings.token.trim()) {
        payload.token = settings.token.trim();
      } else if (clearToken) {
        payload.token = '';
      }
      const res = await api.saveUpdateSettings(payload);
      if (res.data) {
        setSettings((prev) => (prev ? {
          ...prev,
          token: '',
          tokenConfigured: res.data?.tokenConfigured ?? prev.tokenConfigured,
        } : prev));
        setClearToken(false);
      }
      addToast(t('设置已保存'));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div className="update-loading"><Loader2 size={16} /> {t('加载中…')}</div>;
  }

  const download = status?.download;
  const canApply = Boolean(
    status?.hasUpdate && status.bundle && status.checksumAvailable !== false
    && status.supported && status.writable
    && !status.checking && !status.applying && !status.pendingRestart,
  );

  return (
    <div className="update-panel">
      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header">
          <h2>{t('服务器更新')}</h2>
          <p className="card-subtitle">
            {t('自动检查 GitHub Release，一键下载替换并重启；仅 Linux 服务器/容器支持原地更新。')}
          </p>
        </div>
        <div className="card-body update-body">
          <div className="update-hero">
            <div className="update-version">
              <span>{t('当前版本')}</span>
              <strong>v{status?.currentVersion ?? '—'}</strong>
            </div>
            <div className="update-version">
              <span>{t('最新版本')}</span>
              <strong>{status?.latestVersion ? `v${status.latestVersion}` : t('未检查')}</strong>
            </div>
            <button
              type="button"
              className="btn btn-outline"
              disabled={checking || Boolean(status?.checking)}
              onClick={() => void runCheck()}
            >
              {checking || status?.checking
                ? <Loader2 size={14} className="update-spin" />
                : <RefreshCw size={14} />}
              {checking || status?.checking ? t('检查中…') : t('检查更新')}
            </button>
            {status?.recentlyChecked && (status.retryAfterSecs ?? 0) > 0 && (
              <span className="update-cooldown">
                {t('刚检查过，{{n}} 秒后可再查', { n: status.retryAfterSecs })}
              </span>
            )}
          </div>

          {!status?.supported && (
            <div className="update-banner warn">
              <AlertTriangle size={15} />
              <span>{t('当前平台不支持原地更新（仅 Linux 服务器/容器），可在发布页手动下载替换。')}</span>
            </div>
          )}
          {status?.supported && !status.writable && (
            <div className="update-banner warn">
              <AlertTriangle size={15} />
              <span>{t('程序目录不可写，无法原地更新；请给运行用户写入权限或改用容器更新。')}</span>
            </div>
          )}

          {status?.containerized && (
            <div className="update-banner warn">
              <AlertTriangle size={15} />
              <span>
                {t('检测到容器部署：原地更新/重启在容器内不可靠（发布二进制为 glibc，Alpine 镜像为 musl），建议拉取新镜像重建容器。')}
              </span>
            </div>
          )}
          {status?.versionMismatch && (
            <div className="update-banner warn">
              <AlertTriangle size={15} />
              <span>
                {t('前端与后端版本不一致')}
                {status.staticVersion ? `（前端 v${status.staticVersion} / 后端 v${status.currentVersion}）` : ''}
                {t('，界面可能异常，可一键修复前端。')}
              </span>
              <button
                type="button"
                className="btn btn-outline btn-sm"
                onClick={confirmRepair}
                disabled={Boolean(status?.applying)}
                style={{ marginLeft: 8 }}
              >
                {t('修复前端')}
              </button>
            </div>
          )}
          {(status?.restartError || restarting) && (
            <div className={`update-banner ${status?.restartError ? 'error' : 'info'}`}>
              {status?.restartError ? <AlertTriangle size={15} /> : <Loader2 size={15} className="update-spin" />}
              <span>
                {status?.restartError
                  ?? t('正在等待新进程启动（检测到启动时间变化后自动刷新界面）…')}
              </span>
            </div>
          )}
          {status?.lastError && !status.applying && (
            <div className="update-banner error">
              <AlertTriangle size={15} />
              <span>{status.lastError}</span>
            </div>
          )}

          {status?.hasUpdate && (
            <div className="update-banner ok">
              <CheckCircle2 size={15} />
              <span>
                {t('发现新版本')} v{status.latestVersion}
                {status.releaseName ? ` · ${status.releaseName}` : ''}
                {status.publishedAt ? ` · ${status.publishedAt.slice(0, 10)}` : ''}
              </span>
            </div>
          )}
          {status && status.hasUpdate === false && (
            <div className="update-banner ok">
              <CheckCircle2 size={15} />
              <span>{t('已是最新版本')}</span>
            </div>
          )}

          {status?.notes && (
            <div className="update-notes">
              <div className="update-notes-title">{t('更新说明')}</div>
              <pre>{status.notes}</pre>
            </div>
          )}

          {download && (download.active || download.done || download.error) && (
            <div className="update-progress">
              <div className="update-progress-head">
                <span>
                  {download.error
                    ? t('更新失败')
                    : download.done
                      ? t('下载完成，正在替换文件…')
                      : t('更新包下载中…')}
                </span>
                <span>{download.percent}% · {fmtBytes(download.received)} / {fmtBytes(download.total)}</span>
              </div>
              <div className="update-progress-track">
                <div className="update-progress-fill" style={{ width: `${download.percent}%` }} />
              </div>
            </div>
          )}

          {status?.pendingRestart && (
            <div className="update-banner warn">
              <Power size={15} />
              <span>{t('更新已应用，重启后生效')}</span>
            </div>
          )}

          {status && (
            <div className="update-meta">
              {status.checkedAt > 0 && <span>{t('上次检查')}：{fmtTime(status.checkedAt)}</span>}
              {status.appliedVersion && <span>{t('已应用版本')}：v{status.appliedVersion}</span>}
              {status.pageUrl && (
                <a href={status.pageUrl} target="_blank" rel="noreferrer">
                  {t('发布页')} <ExternalLink size={11} />
                </a>
              )}
            </div>
          )}

          <div className="update-actions">
            {status?.pendingRestart ? (
              <button type="button" className="btn btn-primary" onClick={confirmRestart}>
                <Power size={14} /> {t('立即重启')}
              </button>
            ) : (
              <button type="button" className="btn btn-primary" disabled={!canApply} onClick={confirmApply}>
                <Download size={14} /> {t('下载并更新')}
              </button>
            )}
            <button
              type="button"
              className="btn btn-outline"
              disabled={!status?.supported || restarting}
              onClick={confirmRestart}
            >
              <Power size={14} /> {t('重启服务')}
            </button>
            {status?.backupAvailable && (
              <button type="button" className="btn btn-outline update-danger" onClick={confirmRollback}>
                <RotateCcw size={14} /> {t('回滚上一版本')}
              </button>
            )}
          </div>
          {status?.downloadDir && (
            <div className="update-hint">{t('下载目录')}：{status.downloadDir}</div>
          )}
        </div>
      </div>

      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('自动更新设置')}</h2>
          <p className="card-subtitle">{t('自动检查默认开启；自动应用会下载替换并重启，默认关闭。')}</p>
        </div>
        <div className="card-body">
          {settings && (
            <div className="settings-form">
              <label className="update-check">
                <input
                  type="checkbox"
                  checked={settings.enabled}
                  onChange={(e) => setSettings({ ...settings, enabled: e.target.checked })}
                />
                {t('启用更新模块')}
              </label>
              <label className="update-check">
                <input
                  type="checkbox"
                  checked={settings.auto_check}
                  onChange={(e) => setSettings({ ...settings, auto_check: e.target.checked })}
                />
                {t('启用自动检查')}
              </label>
              <label className="update-check">
                <input
                  type="checkbox"
                  checked={settings.auto_apply}
                  onChange={(e) => setSettings({ ...settings, auto_apply: e.target.checked })}
                />
                {t('发现新版本后自动下载并应用（会重启服务）')}
              </label>
              <label className="update-check">
                <input
                  type="checkbox"
                  checked={settings.restart_after_apply}
                  onChange={(e) => setSettings({ ...settings, restart_after_apply: e.target.checked })}
                />
                {t('应用更新后自动重启')}
              </label>
              <label className="update-check">
                <input
                  type="checkbox"
                  checked={settings.include_prerelease}
                  onChange={(e) => setSettings({ ...settings, include_prerelease: e.target.checked })}
                />
                {t('包含预发布版本')}
              </label>

              <div className="form-group">
                <label>{t('检查间隔（小时）')}</label>
                <input
                  className="form-input"
                  type="number"
                  min={1}
                  max={720}
                  value={settings.interval_hours}
                  onChange={(e) => setSettings({ ...settings, interval_hours: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('更新源仓库')}</label>
                <input
                  className="form-input"
                  placeholder="ojbkxc/AIGX"
                  value={settings.repo}
                  onChange={(e) => setSettings({ ...settings, repo: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('自建更新源（可选）')}</label>
                <input
                  className="form-input"
                  placeholder="https://mirror.example.com"
                  value={settings.api_base}
                  onChange={(e) => setSettings({ ...settings, api_base: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('使用代理（可选）')}</label>
                <input
                  className="form-input"
                  placeholder="http://127.0.0.1:7890"
                  value={settings.proxy}
                  onChange={(e) => setSettings({ ...settings, proxy: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('GitHub 令牌（可选）')}</label>
                <input
                  className="form-input"
                  type="password"
                  autoComplete="off"
                  placeholder={settings.tokenConfigured ? t('令牌已配置，留空保持不变') : t('提高 API 限额（匿名 60 次/小时）')}
                  value={settings.token}
                  onChange={(e) => setSettings({ ...settings, token: e.target.value })}
                />
                {settings.tokenConfigured && (
                  <label className="update-check">
                    <input
                      type="checkbox"
                      checked={clearToken}
                      onChange={(e) => setClearToken(e.target.checked)}
                    />
                    {t('清除已保存的令牌')}
                  </label>
                )}
              </div>
              <div className="settings-actions">
                <button type="button" className="btn btn-primary" disabled={saving} onClick={() => void saveSettings()}>
                  {saving ? t('保存中…') : t('保存设置')}
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
