import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Loading, EmptyState } from '../components/ui';

interface GenForm {
  count: number;
  quota: number;
  name: string;
  expires_at: number;
  expires_at_local: string;
}

interface RedemptionItem {
  id: string | number;
  code?: string;
  name?: string;
  quota?: number;
  status?: number;
  used_by?: string;
  created_at?: number;
  expires_at?: number;
}

export default function Redemptions(): JSX.Element {
  const [items, setItems] = useState<RedemptionItem[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const [showGen, setShowGen] = useState(false);
  const [genForm, setGenForm] = useState<GenForm>({ count: 10, quota: 100, name: '', expires_at: 0, expires_at_local: '' });
  const [generating, setGenerating] = useState(false);

  const openGen = () => {
    setGenForm({ count: 10, quota: 100, name: '', expires_at: 0, expires_at_local: '' });
    setShowGen(true);
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [page]);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listRedemptions({ page: String(page), size: '20' });
      setItems(Array.isArray(res?.data) ? (res.data as unknown as RedemptionItem[]) : []);
      setTotal(res?.total || 0);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const handleGenerate = async () => {
    if (!Number.isInteger(genForm.count) || genForm.count < 1) {
      addToast(t('数量至少为 1'), 'error');
      return;
    }
    if (!Number.isInteger(genForm.quota) || genForm.quota < 1) {
      addToast(t('面额至少为 1'), 'error');
      return;
    }
    setGenerating(true);
    setError('');
    try {
      const res = await api.batchRedemptions(genForm as unknown as Record<string, unknown>);
      const generatedCount = res?.data?.data?.length ?? 0;
      addToast(`${t('成功生成')} ${generatedCount} ${t('个兑换码')}`);
      setShowGen(false);
      void load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setGenerating(false);
    }
  };

  const handleDelete = (id: string | number) => {
    setConfirmState({
      title: t('删除兑换码'),
      message: t('确定删除此兑换码？'),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        try {
          await api.deleteRedemption(id);
          addToast(t('删除成功'));
          void load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const fmtTime = (ts: number | undefined): string => {
    if (!ts || ts === 0) return t('永不过期');
    return new Date(ts * 1000).toLocaleString();
  };

  const fmtQuota = (q: number | undefined): string => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  const copyToClipboard = (text: string) => {
    // navigator.clipboard 仅在安全上下文（HTTPS/localhost）可用；AIGX 常以
    // http://IP:9527 部署，需降级 execCommand 方案，否则复制静默失败。
    const fallbackCopy = (): boolean => {
      try {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        const ok = document.execCommand('copy');
        document.body.removeChild(ta);
        return ok;
      } catch {
        return false;
      }
    };
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(() => {
        addToast(t('已复制到剪贴板'));
      }).catch(() => {
        if (!fallbackCopy()) addToast(t('复制失败，请手动选择复制'), 'error');
        else addToast(t('已复制到剪贴板'));
      });
    } else if (fallbackCopy()) {
      addToast(t('已复制到剪贴板'));
    } else {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  const statusBadge = (r: RedemptionItem): JSX.Element => {
    const text =
      r.status === 1
        ? r.expires_at && r.expires_at > 0 && r.expires_at < Date.now() / 1000
          ? t('已过期')
          : t('未使用')
        : r.status === 2
          ? t('已使用')
          : t('已禁用');
    const tone = r.status === 1 ? 'success' : r.status === 2 ? 'neutral' : 'danger';
    return (
      <span className={`badge badge-${tone}`}>{text}</span>
    );
  };

  const totalPages = Math.ceil(total / 20);

  return (
    <div>
      <div className="page-header">
        <h1>{t('兑换码管理')}</h1>
        <p>{t('批量生成、管理与兑换充值码')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Card className="" bodyClassName="">
        <Button onClick={() => (showGen ? setShowGen(false) : openGen())}>
          {showGen ? t('取消') : t('批量生成兑换码')}
        </Button>
      </Card>

      {showGen && (
        <Card title={t('生成兑换码')} className="" bodyClassName="">
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(200px, 1fr))', gap: 16, maxWidth: 800 }}>
            <div className="form-group">
              <label>{t('数量')}</label>
              <input className="form-input" type="number" min="1" value={genForm.count}
                onChange={(e) => setGenForm({ ...genForm, count: Number(e.target.value) })} />
            </div>
            <div className="form-group">
              <label>{t('面额（配额）')}</label>
              <input className="form-input" type="number" min="1" value={genForm.quota}
                onChange={(e) => setGenForm({ ...genForm, quota: Number(e.target.value) })} />
            </div>
            <div className="form-group">
              <label>{t('名称（备注）')}</label>
              <input className="form-input" value={genForm.name}
                onChange={(e) => setGenForm({ ...genForm, name: e.target.value })} placeholder={t('可选')} />
            </div>
            <div className="form-group">
              <label>{t('过期时间')}</label>
              <input className="form-input" type="datetime-local"
                value={genForm.expires_at_local}
                onChange={(e) => {
                  const ts = e.target.value ? Math.floor(new Date(e.target.value).getTime() / 1000) : 0;
                  setGenForm({ ...genForm, expires_at: ts, expires_at_local: e.target.value });
                }} />
              <span className="form-hint">{t('留空 = 永不过期')}</span>
            </div>
          </div>
          <div style={{ marginTop: 16 }}>
            <Button onClick={handleGenerate} disabled={generating}>
              {generating ? t('生成中...') : `${t('生成')} ${genForm.count} ${t('个兑换码')}`}
            </Button>
          </div>
        </Card>
      )}

      <Card title={`${t('兑换码列表')} (${total})`}>
        {loading ? (
          <Loading text={t('加载中')} />
        ) : items.length === 0 ? (
          <EmptyState message={t('暂无兑换码')} />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>{t('兑换码')}</th>
                  <th>{t('名称')}</th>
                  <th>{t('面额')}</th>
                  <th>{t('状态')}</th>
                  <th>{t('使用者')}</th>
                  <th>{t('创建时间')}</th>
                  <th>{t('过期时间')}</th>
                  <th>{t('操作')}</th>
                </tr>
              </thead>
              <tbody>
                {items.map((r) => (
                  <tr key={r.id}>
                    <td>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                        <code className="key-value" style={{ maxWidth: 200 }}>{r.code}</code>
                        {r.code && (
                          <button className="btn btn-outline btn-sm" onClick={() => copyToClipboard(r.code as string)}>{t('复制')}</button>
                        )}
                      </div>
                    </td>
                    <td>{r.name || '—'}</td>
                    <td>{fmtQuota(r.quota)}</td>
                    <td>{statusBadge(r)}</td>
                    <td>{r.used_by || '—'}</td>
                    <td>{r.created_at ? new Date(r.created_at * 1000).toLocaleString() : '—'}</td>
                    <td>{fmtTime(r.expires_at)}</td>
                    <td>
                      {r.status === 1 && (
                        <button className="btn btn-outline btn-sm" style={{ color: 'rgb(239,68,68)' }} onClick={() => handleDelete(r.id)}>{t('删除')}</button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {totalPages > 1 && (
          <div style={{ display: 'flex', justifyContent: 'center', gap: 8, marginTop: 16, alignItems: 'center' }}>
            <button className="btn btn-outline btn-sm" disabled={page <= 1} onClick={() => setPage(page - 1)}>{t('上一页')}</button>
            <span style={{ fontSize: 13, color: 'var(--text-muted)' }}>{page} / {totalPages}</span>
            <button className="btn btn-outline btn-sm" disabled={page >= totalPages} onClick={() => setPage(page + 1)}>{t('下一页')}</button>
          </div>
        )}

        <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
      </Card>
    </div>
  );
}
