import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { SkeletonList } from '../components/ui';
import PageHeader from '../components/PageHeader';
import RuntimeCapacityPanel from '../components/RuntimeCapacityPanel';

/**
 * 顶级「限流」页（从「系统设置 → 限流与缓存」整块迁出为独立入口）。
 *
 * 三个分区：
 *   1. 限流配置 —— 多维度 RPM/TPM（per-key/per-model/per-user/per-ip/global）；
 *   2. 缓存管理 —— 命中率/内存占用读数 + 一键清空；
 *   3. 运行容量 —— 复用 RuntimeCapacityPanel（自动/手动并发与内存预算）。
 *
 * 数据端点与设置页同源（getRateLimitConfig / getCacheStats / clearCache），
 * 只是入口从设置分类里独立出来，逻辑不重复实现。
 */

interface RateLimitConfig {
  enabled?: boolean;
  per_key_rpm?: number | null;
  per_key_tpm?: number | null;
  per_model_rpm?: number | null;
  per_user_rpm?: number | null;
  per_user_tpm?: number | null;
  per_ip_rpm?: number | null;
  global_rpm?: number | null;
  global_tpm?: number | null;
}

interface CacheStats {
  entries?: number;
  count?: number;
  hit_rate?: number;
  memory_bytes?: number;
  size_bytes?: number;
}

interface DataResponse<T> {
  data?: T;
}

const RL_FIELDS: Array<{ field: keyof RateLimitConfig; labelKey: string; hintKey: string }> = [
  { field: 'per_key_rpm', labelKey: '每 Key RPM', hintKey: '单个 API Key 每分钟最大请求数' },
  { field: 'per_key_tpm', labelKey: '每 Key TPM', hintKey: '单个 API Key 每分钟最大 Token 数' },
  { field: 'per_model_rpm', labelKey: '每模型 RPM', hintKey: '单个模型每分钟最大请求数' },
  { field: 'per_user_rpm', labelKey: '每用户 RPM', hintKey: '单个用户每分钟最大请求数' },
  { field: 'per_user_tpm', labelKey: '每用户 TPM', hintKey: '单个用户每分钟最大 Token 数' },
  { field: 'per_ip_rpm', labelKey: '每 IP RPM', hintKey: '单个 IP 每分钟最大请求数' },
  { field: 'global_rpm', labelKey: '全局 RPM', hintKey: '全系统每分钟最大请求数' },
  { field: 'global_tpm', labelKey: '全局 TPM', hintKey: '全系统每分钟最大 Token 数' },
];

function fmtBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

export default function LimitPage(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [rlConfig, setRlConfig] = useState<RateLimitConfig | null>(null);
  const [rlLoading, setRlLoading] = useState(true);
  const [rlSaving, setRlSaving] = useState(false);

  const [cacheStats, setCacheStats] = useState<CacheStats | null>(null);
  const [cacheLoading, setCacheLoading] = useState(true);
  const [cacheClearing, setCacheClearing] = useState(false);

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [error, setError] = useState('');

  const loadRateLimitConfig = useCallback(async (): Promise<void> => {
    setRlLoading(true);
    try {
      const res = (await api.getRateLimitConfig()) as DataResponse<RateLimitConfig> | RateLimitConfig;
      setRlConfig((res as DataResponse<RateLimitConfig>).data || (res as RateLimitConfig));
    } catch {
      // 限流配置可能未启用，静默处理
    } finally {
      setRlLoading(false);
    }
  }, []);

  const loadCacheStats = useCallback(async (): Promise<void> => {
    setCacheLoading(true);
    try {
      const res = (await api.getCacheStats()) as DataResponse<CacheStats> | CacheStats;
      setCacheStats((res as DataResponse<CacheStats>).data || (res as CacheStats));
    } catch {
      // 缓存统计可能未启用，静默处理
    } finally {
      setCacheLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadRateLimitConfig();
    void loadCacheStats();
  }, [loadRateLimitConfig, loadCacheStats]);

  const handleRlChange = (field: keyof RateLimitConfig, value: string): void => {
    // 页面提示写的是「留空或 0 = 不限制」，但后端 `Option<u64>` 里 0 与缺省**不等价**：
    // `check_and_increment` 判 `count + 1 > limit`，limit=0 时第一个请求就被拒
    // （Some(0) = 全量拦截，None = 不限制）。所以 0 必须按 null 发，否则用户照提示
    // 填 0 等于把该维度配成「一律拒绝」。
    const n = Number(value);
    setRlConfig({
      ...(rlConfig as RateLimitConfig),
      [field]: value.trim() === '' || n === 0 ? null : n,
    });
  };

  const handleRlSave = async (): Promise<void> => {
    setRlSaving(true);
    setError('');
    try {
      // 存量配置可能是历史版本存下的 Some(0)（或后端把 0 存进来）：那等于「该维度一律拦截」。
      // 提交前统一把 0 归一成 null（= 不限制），否则「打开页面点保存」就会把全站请求拦掉。
      const payload: Record<string, unknown> = { ...(rlConfig as RateLimitConfig) };
      for (const f of RL_FIELDS) {
        if (payload[f.field] === 0) payload[f.field] = null;
      }
      await api.updateRateLimitConfig(payload);
      addToast(t('限流配置更新成功'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRlSaving(false);
    }
  };

  const handleClearCache = (): void => {
    setConfirmState({
      title: t('清空缓存'),
      message: t('确定清空所有缓存？此操作不可撤销，可能短暂影响性能。'),
      confirmText: t('清空'),
      danger: true,
      onConfirm: async () => {
        setCacheClearing(true);
        setError('');
        try {
          await api.clearCache();
          addToast(t('缓存已清空'));
          void loadCacheStats();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        } finally {
          setCacheClearing(false);
        }
      },
    });
  };

  return (
    <div>
      <PageHeader
        forceLeftWrapper
        title={t('限流')}
        subtitle={t('配置多维度 RPM / TPM 限流与缓存，并查看运行容量预算')}
      />

      {error && <div className="error-message">{error}</div>}

      <div className="card">
        <div className="card-header">
          <h2>{t('限流配置')}</h2>
        </div>
        <div className="card-body">
          {rlLoading ? (
            <SkeletonList rows={4} />
          ) : rlConfig ? (
            <div className="settings-form">
              <p style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 16 }}>
                {t('配置多维度 RPM（每分钟请求数）/ TPM（每分钟 Token 数）限流。留空或 0 = 不限制。')}
              </p>
              <div className="form-group">
                <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <input
                    type="checkbox"
                    checked={rlConfig.enabled ?? false}
                    onChange={(e) => setRlConfig({ ...rlConfig, enabled: e.target.checked })}
                  />
                  {t('启用限流总开关')}
                </label>
                <span className="form-hint">{t('开启后限流规则才会生效')}</span>
              </div>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))', gap: 16 }}>
                {RL_FIELDS.map((f) => (
                  <div key={f.field} className="form-group">
                    <label>{t(f.labelKey)}</label>
                    <input
                      className="form-input"
                      type="number"
                      min="0"
                      placeholder="0"
                      value={(rlConfig[f.field] as number | null | undefined) ?? ''}
                      onChange={(e) => handleRlChange(f.field, e.target.value)}
                    />
                    <span className="form-hint">{t(f.hintKey)}</span>
                  </div>
                ))}
              </div>
              <div className="settings-actions" style={{ marginTop: 16 }}>
                <button className="btn btn-primary" onClick={() => void handleRlSave()} disabled={rlSaving}>
                  {rlSaving ? t('保存中...') : t('保存限流配置')}
                </button>
              </div>
            </div>
          ) : (
            <div className="empty-state">
              <p>{t('限流配置未启用或加载失败')}</p>
            </div>
          )}
        </div>
      </div>

      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('缓存管理')}</h2>
        </div>
        <div className="card-body">
          {cacheLoading ? (
            <SkeletonList rows={3} />
          ) : cacheStats ? (
            <div className="settings-form">
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))', gap: 16, marginBottom: 16 }}>
                <div className="form-group">
                  <label>{t('缓存条目数')}</label>
                  <div style={{ fontSize: 20, fontWeight: 600, color: 'var(--text-main)' }}>
                    {Number(cacheStats.entries || cacheStats.count || 0).toLocaleString()}
                  </div>
                </div>
                <div className="form-group">
                  <label>{t('命中率')}</label>
                  <div style={{ fontSize: 20, fontWeight: 600, color: 'var(--text-main)' }}>
                    {Number(cacheStats.hit_rate || 0).toFixed(1)}%
                  </div>
                </div>
                <div className="form-group">
                  <label>{t('内存占用')}</label>
                  <div style={{ fontSize: 20, fontWeight: 600, color: 'var(--text-main)' }}>
                    {/* 后端 /api/cache/stats 恒返回 memory_bytes=0 并在源码里注明
                        「由前端显示「—」，不编一个假数字」——这里必须照契约显示「—」，
                        显示 0 B 等于声称「缓存不占内存」。 */}
                    {cacheStats.memory_bytes || cacheStats.size_bytes
                      ? fmtBytes(cacheStats.memory_bytes || cacheStats.size_bytes || 0)
                      : '—'}
                  </div>
                </div>
              </div>
              <div className="settings-actions">
                <button className="btn btn-danger" onClick={handleClearCache} disabled={cacheClearing}>
                  {cacheClearing ? t('清空中...') : t('清空缓存')}
                </button>
              </div>
            </div>
          ) : (
            <div className="empty-state">
              <p>{t('缓存统计未启用或加载失败')}</p>
            </div>
          )}
        </div>
      </div>

      <div style={{ marginTop: 16 }}>
        {/* 运行容量（自动/手动并发与内存预算；后端未就绪时面板自带降级） */}
        <RuntimeCapacityPanel />
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
