// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 运行容量面板（系统设置 → 限流与缓存）—— 对照 A2A ui-shared/runtime-capacity-panel.tsx：
//   自动 / 手动两种模式；手动模式可调对话并发、后台并发、连接上限、内存预算、排队上限等；
//   顶部实时展示正在处理 / 排队 / 后台 / 运行内存与资源紧张提示。
// 契约：GET /api/plugins/capacity、PATCH /api/plugins/capacity `{settings, revision}`。
// 端点未就绪（404）时整块降级「后端未就绪」，后端上线后零改动生效。

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Button, Card, Input, Select } from '../components/ui';

const MIB = 1024 * 1024;

type CapacityMode = 'auto' | 'manual';

interface CapacitySettings {
  mode: CapacityMode;
  maxConversations: number;
  maxInteractive: number;
  maxBackground: number;
  backgroundPerProvider: number;
  maxImages: number;
  maxConnections: number;
  memoryMib: number;
  queueRequests: number;
  queueMemoryMib: number;
  queueWaitSeconds: number;
  providerLimits: Record<string, number>;
}

interface CapacitySnapshot {
  settings: CapacitySettings;
  revision: string;
  error?: string | null;
  effective: {
    conversations: number;
    interactive: number;
    background: number;
    backgroundPerProvider: number;
    images: number;
    connections: number;
    memoryBytes: number;
    queueRequests: number;
    queueBytes: number;
    queueWaitSeconds: number;
    cpuPressure: boolean;
    memoryPressure: boolean;
  };
  usage: {
    conversations: number;
    interactive: number;
    background: number;
    images: number;
    connections: number;
    memoryBytes: number;
    queued: number;
    queueBytes: number;
    requests: number;
    imageRequests: number;
  };
  rejected: number;
}

type NumericField = keyof Omit<CapacitySettings, 'mode' | 'providerLimits'>;

const FIELDS: Array<{ key: NumericField; label: string; min: number; max: number }> = [
  { key: 'maxConversations', label: '对话并发上限', min: 1, max: 4096 },
  { key: 'maxInteractive', label: '登录与续期并发', min: 4, max: 256 },
  { key: 'maxBackground', label: '后台任务并发', min: 1, max: 256 },
  { key: 'backgroundPerProvider', label: '单提供商后台并发', min: 1, max: 64 },
  { key: 'maxImages', label: '图片任务并发', min: 1, max: 128 },
  { key: 'maxConnections', label: '出站连接上限', min: 64, max: 32768 },
  { key: 'memoryMib', label: '运行内存预算（MiB）', min: 0, max: 262144 },
  { key: 'queueRequests', label: '排队请求上限', min: 1, max: 16384 },
  { key: 'queueMemoryMib', label: '排队内存上限（MiB）', min: 8, max: 4096 },
  { key: 'queueWaitSeconds', label: '最长排队时间（秒）', min: 1, max: 120 },
];

export default function RuntimeCapacityPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [snapshot, setSnapshot] = useState<CapacitySnapshot | null>(null);
  const [draft, setDraft] = useState<CapacitySettings | null>(null);
  const [revision, setRevision] = useState('');
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const edited = useRef(false);

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getRuntimeCapacity();
      const data = (res?.data ?? null) as unknown as CapacitySnapshot | null;
      if (!data) return;
      setSnapshot(data);
      setNotReady(false);
      setError('');
      if (!edited.current) {
        setDraft(data.settings);
        setRevision(data.revision);
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const patchField = (key: keyof CapacitySettings, value: unknown): void => {
    edited.current = true;
    setDraft((prev) => (prev ? { ...prev, [key]: value } : prev));
  };

  const save = async (): Promise<void> => {
    if (!draft || !snapshot || busy) return;
    setBusy(true);
    setError('');
    try {
      const res = await api.saveRuntimeCapacity({ settings: draft, revision });
      const data = (res?.data ?? null) as unknown as CapacitySnapshot | null;
      if (data) {
        setSnapshot(data);
        setDraft(data.settings);
        setRevision(data.revision);
        edited.current = false;
      }
      addToast(t('运行容量已保存'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  if (loading) {
    return (
      <Card title={t('运行容量')}>
        <div className="settings-form">
          <Loader2 size={14} className="confirm-dialog-spin" /> {t('读取中')}
        </div>
      </Card>
    );
  }
  if (notReady) {
    return (
      <Card title={t('运行容量')}>
        <div className="empty-state">
          <p>{t('运行容量接口尚未就绪')}</p>
        </div>
      </Card>
    );
  }

  const changed = draft && snapshot && JSON.stringify(draft) !== JSON.stringify(snapshot.settings);
  const effective = snapshot?.effective;
  const usage = snapshot?.usage;
  return (
    <Card
      title={t('运行容量')}
      actions={
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={busy}>
          <RefreshCw size={13} /> {t('刷新')}
        </Button>
      }
    >
      <div className="settings-form">
        <p className="form-hint">{t('自动管理按可用内存调节容量；手动设置仍保留内存保护和上游账号限制。')}</p>

        <div className="form-group" style={{ maxWidth: 320 }}>
          <label>{t('运行容量')}</label>
          <Select
            value={draft?.mode || 'auto'}
            disabled={!draft || busy}
            onChange={(e) => patchField('mode', e.target.value as CapacityMode)}
          >
            <option value="auto">{t('自动管理')}</option>
            <option value="manual">{t('手动设置')}</option>
          </Select>
        </div>

        {effective && usage ? (
          <div
            style={{
              display: 'grid',
              gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))',
              gap: 12,
              border: '1px solid var(--border-color)',
              borderRadius: 8,
              padding: 12,
            }}
          >
            <div>
              <div className="form-hint">{t('正在处理')}</div>
              <strong>{usage.requests} / {effective.conversations}</strong>
            </div>
            <div>
              <div className="form-hint">{t('排队中')}</div>
              <strong>{usage.queued}</strong>
            </div>
            <div>
              <div className="form-hint">{t('后台任务')}</div>
              <strong>{usage.background} / {effective.background}</strong>
            </div>
            <div>
              <div className="form-hint">{t('运行内存')}</div>
              <strong>
                {Math.ceil(usage.memoryBytes / MIB)} / {Math.floor(effective.memoryBytes / MIB)} MiB
              </strong>
            </div>
          </div>
        ) : (
          <p className="form-hint">{t('读取中')}</p>
        )}

        {effective?.memoryPressure || effective?.cpuPressure ? (
          <p className="form-hint" style={{ color: 'var(--warning-color, #c98a00)' }}>
            {t('当前资源紧张，已暂停启动新的后台和图片任务。')}
          </p>
        ) : null}

        {draft?.mode === 'manual' ? (
          <>
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))', gap: 16 }}>
              {FIELDS.map((f) => (
                <div key={f.key} className="form-group">
                  <label>{t(f.label)}</label>
                  <Input
                    type="number"
                    min={f.min}
                    max={f.max}
                    step={1}
                    value={draft[f.key]}
                    disabled={busy}
                    onChange={(e) => patchField(f.key, Number(e.target.value))}
                  />
                </div>
              ))}
            </div>
            <p className="form-hint">
              {t('运行内存填 0 表示自动；降低并发上限不会主动中断请求，降低内存预算可能影响后续分配。')}
            </p>
          </>
        ) : null}

        {error || snapshot?.error ? (
          <div className="error-message">{error || snapshot?.error}</div>
        ) : null}

        <div className="settings-actions">
          <Button variant="primary" disabled={!changed || busy} onClick={() => void save()}>
            {busy ? t('保存中') : t('保存')}
          </Button>
        </div>
      </div>
    </Card>
  );
}
