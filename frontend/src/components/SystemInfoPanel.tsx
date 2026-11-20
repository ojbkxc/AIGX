/**
 * 设置页「系统信息」面板（对照 Agent2API v3.5.0-beta.3 `system-info-pane.tsx`）。
 *
 * 把「装的是哪一版、跑在什么环境、数据与日志在哪、哪些开关是开的」一次摆全，
 * 且能一键复制成纯文本 —— 截图只能看，文本能直接贴进 issue。数据来自
 * `GET /api/system-info`（管理员；服务端一次性合成，分项失败各自降级）。
 *
 * 「隐藏用户名」走请求参数（`?hideUsername=1`）：脱敏在后端做，因此
 * 「复制全部信息」拿到的也是脱敏后的文本（前端替换容易在剪贴板路径漏掉）。
 */
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import type { SystemInfo } from '../types/system_info';

/** 运行形态 → 展示文案 */
function deploymentLabel(t: (key: string) => string, value?: string): string {
  switch (value) {
    case 'docker':
      return t('Docker 容器');
    case 'systemd':
      return t('systemd 服务');
    case 'windows-service':
      return t('Windows 服务');
    case 'foreground':
      return t('前台运行');
    case 'desktop':
      return t('桌面端');
    default:
      return value || '—';
  }
}

/** 已运行秒数 → 「3d 4h」「2h 5m」「45m」 */
function fmtUptime(seconds?: number): string {
  const total = Number(seconds);
  if (!Number.isFinite(total) || total <= 0) return '—';
  const days = Math.floor(total / 86400);
  const hours = Math.floor((total % 86400) / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const parts: string[] = [];
  if (days > 0) parts.push(`${days}d`);
  if (hours > 0 || days > 0) parts.push(`${hours}h`);
  if (minutes > 0 || parts.length > 0) parts.push(`${minutes}m`);
  if (parts.length === 0) parts.push(`${Math.floor(total)}s`);
  return parts.join(' ');
}

/** 毫秒时间戳 → 本地时间串 */
function fmtTime(ms?: number): string {
  const value = Number(ms);
  if (!Number.isFinite(value) || value <= 0) return '—';
  return new Date(value).toLocaleString();
}

function fmtBytes(bytes?: number | null): string {
  const n = Number(bytes);
  if (!Number.isFinite(n) || n <= 0) return '0 B';
  if (n >= 1073741824) return `${(n / 1073741824).toFixed(2)} GB`;
  if (n >= 1048576) return `${(n / 1048576).toFixed(2)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${n} B`;
}

function onOff(t: (key: string) => string, value?: boolean | null): string {
  if (value === true) return t('开');
  if (value === false) return t('关');
  return '—';
}

/** 系统名与版本拼一句（版本描述可能已自带系统名，避免「Windows Windows 11」） */
function formatOs(os?: string, version?: string): string {
  const name = os ? os.charAt(0).toUpperCase() + os.slice(1) : '—';
  const detail = (version || '').trim();
  if (!detail) return name;
  if (detail.toLowerCase().startsWith(name.toLowerCase())) return detail;
  return `${name} ${detail}`;
}

/** 文本行：键定宽、值等宽可折行 */
function Row({ label, value }: { label: string; value: React.ReactNode }): JSX.Element {
  return (
    <div className="log-detail-row" style={{ display: 'flex', gap: 12, padding: '2px 0' }}>
      <span className="log-detail-label" style={{ minWidth: 132, opacity: 0.7 }}>{label}</span>
      <span className="log-detail-mono" style={{ wordBreak: 'break-all' }}>{value}</span>
    </div>
  );
}

function Group({ title, rows }: { title: string; rows: Array<[string, React.ReactNode]> }): JSX.Element | null {
  if (rows.length === 0) return null;
  return (
    <div className="card">
      <div className="card-header">
        <h2>{title}</h2>
      </div>
      <div className="card-body">
        {rows.map(([label, value], index) => (
          <Row key={`${label}-${index}`} label={label} value={value} />
        ))}
      </div>
    </div>
  );
}

export default function SystemInfoPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [info, setInfo] = useState<SystemInfo | null>(null);
  const [hideUsername, setHideUsername] = useState(false);

  const load = useCallback(async (hide: boolean): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getSystemInfo(hide);
      setInfo(res?.data ?? null);
    } catch (err) {
      // 失败不清空已有读数：环境信息不会因一次网络抖动就变成未知
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(false);
  }, [load]);

  const toggleHide = (next: boolean): void => {
    setHideUsername(next);
    // 脱敏在后端做：改开关立即重拉一次（让路径立刻变化）
    void load(next);
  };

  const copyAll = async (): Promise<void> => {
    if (!info) return;
    const app = info.app ?? {};
    const runtime = info.runtime ?? {};
    const deployment = info.deployment ?? {};
    const security = info.security ?? {};
    const features = info.features ?? {};
    const stats = info.stats ?? {};
    const lines: string[] = [];
    const section = (title: string, rows: Array<[string, string]>): void => {
      lines.push(`[${title}]`);
      rows.forEach(([key, value]) => lines.push(`${key}: ${value}`));
      lines.push('');
    };
    section(t('版本'), [
      [t('软件版本'), app.version || '—'],
      [t('开源许可'), app.license || '—'],
      [t('更新含预发布'), onOff(t, app.includePrerelease)],
      [t('源代码'), app.sourceUrl || '—'],
    ]);
    section(t('环境'), [
      [t('运行形态'), deploymentLabel(t, runtime.deployment)],
      [t('Docker 容器'), onOff(t, runtime.container)],
      [t('操作系统'), formatOs(runtime.os, runtime.osVersion)],
      [t('架构'), runtime.arch || '—'],
      [t('进程 PID'), String(runtime.pid ?? '—')],
      [t('启动时间'), fmtTime(runtime.startedAt)],
      [t('已运行'), fmtUptime(runtime.uptimeSeconds)],
      [t('开发构建'), onOff(t, runtime.debugBuild)],
    ]);
    section(t('位置与端口'), [
      [t('监听地址'), `${deployment.host ?? '—'}:${deployment.port ?? '—'}`],
      [t('管理面独立监听'), deployment.adminBind || t('与主端口共用')],
      [t('配置文件'), deployment.configFile || '—'],
      [t('数据目录'), deployment.dataDir || '—'],
      [t('日志目录'), deployment.logDir || '—'],
      [t('数据库后端'), deployment.dbBackend || '—'],
      [t('数据库文件'), `${deployment.dbFile || '—'}（${fmtBytes(deployment.dbBytes)}）`],
      [t('程序路径'), deployment.exePath || '—'],
      [t('托管网页界面'), onOff(t, deployment.uiHosted)],
      [t('原地更新'), onOff(t, deployment.selfUpdate)],
    ]);
    section(t('安全'), [
      [t('管理员已注册'), onOff(t, security.adminRegistered)],
      [t('可用 Key 数量'), String(security.apiKeyCount ?? '—')],
      [t('数据面强制 Key'), onOff(t, security.dataPlaneKeyRequired)],
      [t('信任反代头'), onOff(t, security.trustProxyHeaders)],
      [t('允许内网上游'), onOff(t, security.allowInternalUpstream)],
    ]);
    section(t('开关'), [
      [t('调试模式'), onOff(t, features.debugMode)],
      [t('请求日志快照'), onOff(t, features.logBody)],
      [t('指纹脱敏'), onOff(t, features.sanitizeFingerprints)],
      [t('跨域访问'), onOff(t, features.cors)],
      [t('自动续写'), features.autoContinue
        ? t('开（最多 {{n}} 轮）', { n: features.autoContinueMaxRounds ?? '—' })
        : t('关')],
      [t('登录人机验证'), onOff(t, features.captchaEnabled)],
      [t('更新含预发布'), onOff(t, features.updateBeta)],
    ]);
    section(t('计数'), [
      [t('账号（启用 / 总数）'), `${stats.activeAccounts ?? '—'} / ${stats.accounts ?? '—'}`],
      [t('供应商（启用 / 总数）'), `${stats.enabledChannels ?? '—'} / ${stats.channels ?? '—'}`],
      [t('模型别名'), String(stats.modelAliases ?? '—')],
      [t('可用 Key'), String(stats.apiKeys ?? '—')],
      [t('累计请求'), String(stats.requests ?? '—')],
      [t('在途请求'), String(stats.inFlight ?? '—')],
    ]);
    const payload = lines.join('\n').trimEnd();
    try {
      await navigator.clipboard.writeText(payload);
      addToast(t('已复制全部系统信息'), 'success');
      return;
    } catch {
      /* 落到下面的兜底 */
    }
    try {
      const area = document.createElement('textarea');
      area.value = payload;
      area.style.position = 'fixed';
      area.style.opacity = '0';
      document.body.appendChild(area);
      area.select();
      const ok = document.execCommand('copy');
      area.remove();
      addToast(ok ? t('已复制全部系统信息') : t('复制失败，请手动选择复制'), ok ? 'success' : 'error');
    } catch {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  const app = info?.app ?? {};
  const runtime = info?.runtime ?? {};
  const deployment = info?.deployment ?? {};
  const security = info?.security ?? {};
  const features = info?.features ?? {};
  const stats = info?.stats ?? {};

  const versionRows: Array<[string, React.ReactNode]> = [
    [t('软件版本'), <b>{app.version ? `v${app.version}` : '—'}</b>],
    [t('开源许可'), app.license || '—'],
    [t('更新含预发布'), onOff(t, app.includePrerelease)],
    [t('源代码'), app.sourceUrl
      ? <a href={app.sourceUrl} target="_blank" rel="noreferrer">{app.sourceUrl}</a>
      : '—'],
    [t('版权声明'), app.copyright || '—'],
  ];
  const environmentRows: Array<[string, React.ReactNode]> = [
    [t('运行形态'), deploymentLabel(t, runtime.deployment)],
    [t('Docker 容器'), onOff(t, runtime.container)],
    [t('操作系统'), formatOs(runtime.os, runtime.osVersion)],
    [t('架构'), runtime.arch || '—'],
    [t('进程 PID'), String(runtime.pid ?? '—')],
    [t('启动时间'), fmtTime(runtime.startedAt)],
    [t('已运行'), fmtUptime(runtime.uptimeSeconds)],
    [t('开发构建'), onOff(t, runtime.debugBuild)],
  ];
  const locationRows: Array<[string, React.ReactNode]> = [
    [t('监听地址'), `${deployment.host ?? '—'}:${deployment.port ?? '—'}`],
    [t('管理面独立监听'), deployment.adminBind || t('与主端口共用')],
    [t('配置文件'), deployment.configFile || '—'],
    [t('数据目录'), deployment.dataDir || '—'],
    [t('日志目录'), deployment.logDir || '—'],
    [t('数据库后端'), deployment.dbBackend || '—'],
    [
      t('数据库文件'),
      <>
        {deployment.dbFile || '—'}
        {deployment.dbBytes != null ? `（${fmtBytes(deployment.dbBytes)}）` : ''}
        {deployment.dbAvailable === false ? ` · ${t('不可用')}` : ''}
      </>,
    ],
    [t('程序路径'), deployment.exePath || '—'],
    [t('托管网页界面'), onOff(t, deployment.uiHosted)],
    [t('原地更新'), onOff(t, deployment.selfUpdate)],
  ];
  const securityRows: Array<[string, React.ReactNode]> = [
    [t('管理员已注册'), onOff(t, security.adminRegistered)],
    [t('可用 Key 数量'), String(security.apiKeyCount ?? '—')],
    [t('数据面强制 Key'), onOff(t, security.dataPlaneKeyRequired)],
    [t('信任反代头'), onOff(t, security.trustProxyHeaders)],
    [t('允许内网上游'), onOff(t, security.allowInternalUpstream)],
  ];
  const featureRows: Array<[string, React.ReactNode]> = [
    [t('调试模式'), onOff(t, features.debugMode)],
    [t('请求日志快照'), onOff(t, features.logBody)],
    [t('指纹脱敏'), onOff(t, features.sanitizeFingerprints)],
    [t('跨域访问'), onOff(t, features.cors)],
    [t('自动续写'), features.autoContinue
      ? t('开（最多 {{n}} 轮）', { n: features.autoContinueMaxRounds ?? '—' })
      : t('关')],
    [t('登录人机验证'), onOff(t, features.captchaEnabled)],
    [t('更新含预发布'), onOff(t, features.updateBeta)],
  ];
  const statRows: Array<[string, React.ReactNode]> = [
    [t('账号（启用 / 总数）'), `${stats.activeAccounts ?? '—'} / ${stats.accounts ?? '—'}`],
    [t('供应商（启用 / 总数）'), `${stats.enabledChannels ?? '—'} / ${stats.channels ?? '—'}`],
    [t('模型别名'), String(stats.modelAliases ?? '—')],
    [t('可用 Key'), String(stats.apiKeys ?? '—')],
    [t('累计请求'), String(stats.requests ?? '—')],
    [t('在途请求'), String(stats.inFlight ?? '—')],
  ];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
      <div className="card">
        <div className="card-header">
          <h2>{t('系统信息')}</h2>
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <button className="btn" onClick={() => void load(hideUsername)} disabled={loading}>
              {loading ? t('读取中...') : t('刷新')}
            </button>
            <button className="btn btn-primary" onClick={() => void copyAll()} disabled={!info}>
              {t('复制全部信息')}
            </button>
          </div>
        </div>
        <div className="card-body">
          <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', alignItems: 'baseline' }}>
            <b style={{ fontSize: 18 }}>{app.version ? `v${app.version}` : '—'}</b>
            <span className="badge">{deploymentLabel(t, runtime.deployment)}</span>
            <span style={{ opacity: 0.75 }}>
              {formatOs(runtime.os, runtime.osVersion)} · {runtime.arch || '—'}
            </span>
          </div>
          <p className="card-subtitle" style={{ marginTop: 8 }}>
            {t('出问题时把这一页截图，或点「复制全部信息」把文本贴进反馈，一般就能定位到大致原因。')}
          </p>
          <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <input
              type="checkbox"
              checked={hideUsername}
              onChange={(e) => toggleHide(e.target.checked)}
            />
            <span>{t('隐藏用户名')}</span>
          </label>
          <div className="form-hint">
            {t('把路径里的主目录显示成 ~，截图分享时不暴露用户名。只影响本页显示与复制，不改任何文件。')}
          </div>
          {error && <div className="error-message">{error}</div>}
        </div>
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(340px, 1fr))', gap: 16 }}>
        <Group title={t('版本')} rows={versionRows} />
        <Group title={t('环境')} rows={environmentRows} />
        <Group title={t('位置与端口')} rows={locationRows} />
        <Group title={t('安全')} rows={securityRows} />
        <Group title={t('开关')} rows={featureRows} />
        <Group title={t('计数')} rows={statRows} />
      </div>
    </div>
  );
}
