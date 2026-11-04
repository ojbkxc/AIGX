import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { isRunning as isPoolRunning } from '../lib/zcodeCaptchaPool';
import type { ZcodeCaptchaPoolSettings, ZcodeCaptchaPoolStats } from '../types';
import { useToast } from './Toast';

/**
 * ZCode 验证码令牌池（系统设置 → 网关 → 验证码令牌池）
 *
 * 对照 Agent2API「设置 → 网关 → 验证码令牌池」。ZCode 的「活动套餐」端点每条
 * 请求都要一个当次铸的阿里云验证码令牌，而 Rust 侧铸不出来（要跑阿里云 SDK），
 * 因此由管理页在后台铸（`lib/zcodeCaptchaPool.ts`，登录后自启）：
 *
 * - 令牌寿命两分钟、一次一用，所以面板显示的是**实时库存**，不是可累积的资源；
 * - 「该不该铸」由网关判定（有 ZCode 账号 + 库存低于目标），界面只展示与调节奏；
 * - 三项节奏热生效：前端铸造器下一轮（≤ 轮询间隔）就会读到新值。
 *
 * 手动粘贴入口是排障用的（后端允许单枚 `param` 入池），正常不需要碰。
 */

interface RowProps {
  label: string;
  hint: string;
  value: number;
  min: number;
  max: number;
  onChange: (value: number) => void;
}

function NumberRow({ label, hint, value, min, max, onChange }: RowProps): JSX.Element {
  return (
    <div className="form-group">
      <label>{label}</label>
      <input
        className="form-input"
        type="number"
        min={min}
        max={max}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <div className="form-hint">{`${hint}（${min} ~ ${max}）`}</div>
    </div>
  );
}

export default function ZcodeCaptchaPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [stats, setStats] = useState<ZcodeCaptchaPoolStats | null>(null);
  const [settings, setSettings] = useState<ZcodeCaptchaPoolSettings | null>(null);
  const [manualParam, setManualParam] = useState('');
  const pollTimer = useRef(0);

  const loadSettings = useCallback(async (): Promise<void> => {
    const res = await api.getZcodeCaptchaPool();
    if (res?.data) setSettings(res.data);
  }, []);

  /** 只刷概况（铸造器在后台跑，库存会自己变；这里跟着看）。 */
  const refreshStats = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getZcodeCaptcha();
      if (res?.data) setStats(res.data);
    } catch {
      /* 轮询失败不打扰用户：面板上保留上一次读数 */
    }
  }, []);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      await Promise.all([loadSettings(), refreshStats()]);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [loadSettings, refreshStats]);

  useEffect(() => {
    void load();
  }, [load]);

  // 概况自动刷新：令牌两分钟就过期，静态读数会骗人
  useEffect(() => {
    const tick = (): void => {
      void refreshStats();
      // 每 5 秒一轮；间隔跟设置的轮询间隔无关（那管的是铸造节奏，不是面板）
      pollTimer.current = window.setTimeout(tick, 5_000);
    };
    pollTimer.current = window.setTimeout(tick, 5_000);
    return () => window.clearTimeout(pollTimer.current);
  }, [refreshStats]);

  const patch = (
    key: 'target' | 'pollMs' | 'maxPerRound',
    value: number,
  ): void => {
    // 分开写三支：联合键 + 展开在 TS 里容易推出不可赋值的对象类型
    setSettings((prev) => {
      if (!prev) return prev;
      if (key === 'target') return { ...prev, target: value };
      if (key === 'pollMs') return { ...prev, pollMs: value };
      return { ...prev, maxPerRound: value };
    });
  };

  const save = async (): Promise<void> => {
    if (!settings) return;
    setSaving(true);
    setError('');
    try {
      const res = await api.saveZcodeCaptchaPool({
        target: settings.target,
        pollMs: settings.pollMs,
        maxPerRound: settings.maxPerRound,
      });
      if (res?.data) setSettings(res.data);
      await refreshStats();
      addToast(t('铸造节奏已保存（前端铸造器下一轮即生效）'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const pushManual = async (): Promise<void> => {
    const param = manualParam.trim();
    if (!param) return;
    setError('');
    try {
      // region 留空：上游对不带的头按默认站点处理；手工粘贴时不该猜
      const res = await api.pushZcodeCaptchaTokens([{ param, region: '' }]);
      setManualParam('');
      await refreshStats();
      addToast(t('已入池 {{n}} 枚（当前库存 {{ready}}）', {
        n: res?.data?.pushed ?? 0,
        ready: res?.data?.ready ?? 0,
      }));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  const ranges = settings?.ranges ?? {
    target: [1, 10] as [number, number],
    pollMs: [1000, 30000] as [number, number],
    maxPerRound: [1, 5] as [number, number],
  };

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('ZCode 验证码令牌池')}</h2>
        <p className="card-subtitle">
          {t('ZCode 活动套餐端点每条请求都要一个当次铸的阿里云验证码令牌。令牌由本页在后台静默铸造（无需操作），推给网关后由转发层按请求取用。令牌两分钟过期、一次一用，因此库存只需「够下一条请求立刻有得用」。没有 ZCode 账号时不会铸造（阿里云对铸造频率有风控）。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}

        {stats && (
          <div className="settings-form" style={{ marginBottom: 24 }}>
            <div className="form-group">
              <label>{t('实时库存')}</label>
              <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap', alignItems: 'baseline' }}>
                <strong style={{ fontSize: 24 }}>
                  {`${stats.ready} / ${stats.target}`}
                </strong>
                <span className="form-hint">
                  {stats.hasAccounts
                    ? (stats.needsTokens ? t('低于目标，正在补货') : t('库存充足'))
                    : t('没有 ZCode 账号，不铸造')}
                </span>
              </div>
              <div className="form-hint">
                {isPoolRunning()
                  ? t('后台铸造器：运行中（管理页登录后自动跑，关掉页面就停）')
                  : t('后台铸造器：未运行（它不是从本页启动的，而是登录后全局自启）')}
              </div>
              <div className="form-hint">
                {t('累计铸造 {{minted}} · 已消耗 {{consumed}} · 上游拒收 {{rejected}} · 过期丢弃 {{stale}}', {
                  minted: stats.minted,
                  consumed: stats.consumed,
                  rejected: stats.rejected,
                  stale: stats.stale,
                })}
              </div>
              <div className="form-hint">
                {`${t('令牌寿命')}: ${Math.round((stats.ttlMs ?? 0) / 1000)}s`}
                {stats.oldestAgeMs != null
                  ? ` · ${t('最旧一枚已放')} ${Math.round(stats.oldestAgeMs / 1000)}s`
                  : ''}
              </div>
            </div>
          </div>
        )}

        {!stats?.hasAccounts && (
          <div className="form-hint" style={{ marginBottom: 16 }}>
            {t('提示：headless / Docker 部署没有浏览器可跑阿里云 SDK，令牌池恒空 —— 请在网关里给 ZCode 账号改用编码套餐通道，或保持管理页打开由它后台铸造。')}
          </div>
        )}

        {settings && (
          <div className="settings-form">
            <h3 style={{ margin: 0 }}>{t('铸造节奏')}</h3>
            <NumberRow
              label={t('库存目标')}
              hint={t('够下一条请求立刻有得用即可；铸多了纯属烧阿里云风控配额')}
              value={settings.target}
              min={ranges.target[0]}
              max={ranges.target[1]}
              onChange={(v) => patch('target', v)}
            />
            <NumberRow
              label={t('轮询间隔（毫秒）')}
              hint={t('前端每隔这么久问一次网关要不要补货')}
              value={settings.pollMs}
              min={ranges.pollMs[0]}
              max={ranges.pollMs[1]}
              onChange={(v) => patch('pollMs', v)}
            />
            <NumberRow
              label={t('单轮铸造上限')}
              hint={t('1 = 单实例单次铸造，避免并发冲突与容器重建竞态')}
              value={settings.maxPerRound}
              min={ranges.maxPerRound[0]}
              max={ranges.maxPerRound[1]}
              onChange={(v) => patch('maxPerRound', v)}
            />
            <div className="settings-actions" style={{ display: 'flex', gap: 8 }}>
              <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
                {saving ? t('保存中...') : t('保存更改')}
              </button>
              <button className="btn btn-secondary" onClick={() => void load()} disabled={saving}>
                {t('放弃修改')}
              </button>
            </div>
          </div>
        )}

        <div className="settings-form" style={{ marginTop: 24 }}>
          <h3 style={{ margin: 0 }}>{t('手动入池（排障用）')}</h3>
          <div className="form-group">
            <label>{t('验证串')}</label>
            <input
              className="form-input"
              value={manualParam}
              placeholder={t('粘一枚 x-aliyun-captcha-verify-param 的值（正常不需要手动做）')}
              onChange={(e) => setManualParam(e.target.value)}
            />
            <div className="form-hint">
              {t('令牌两分钟就过期，手工粘贴的串多半来不及用；只在排查「铸造器为什么不工作时」有意义。')}
            </div>
          </div>
          <button
            className="btn btn-secondary"
            onClick={() => void pushManual()}
            disabled={!manualParam.trim()}
          >
            {t('入池')}
          </button>
        </div>
      </div>
    </div>
  );
}