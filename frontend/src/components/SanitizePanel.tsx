import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

/**
 * 指纹脱敏（系统设置 → 运维 → 网关）
 *
 * 对照 Agent2API「设置 → 网关 → 指纹脱敏」：网关把发往上游的请求头记进
 * 调试报文时会脱敏凭据类头（`autorization` / `cookie` / `x-*-token` 等内置
 * 名单），但各家上游的自定义头名会变。这里让管理员补充额外名单：
 *
 * - 完整头名：`x-foo-token`
 * - 尾部通配：`x-foo-*`（匹配一切以 `x-foo-` 开头的头）
 *
 * 保存即热生效：下一请求的调试报文即按新名单脱敏。
 */
export default function SanitizePanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [rows, setRows] = useState<string[]>([]);
  const [draft, setDraft] = useState('');

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getSanitize();
        setRows(res?.data?.extra_headers ?? []);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const addRow = (): void => {
    const name = draft.trim().toLowerCase();
    if (!name) return;
    if (rows.includes(name)) {
      setDraft('');
      return;
    }
    setRows([...rows, name]);
    setDraft('');
  };

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      // 未提交的草稿一并带上：避免「填了没点添加就点保存」白填
      const pending = draft.trim().toLowerCase();
      const payload = pending && !rows.includes(pending) ? [...rows, pending] : rows;
      const res = await api.saveSanitize({ extra_headers: payload });
      setRows(res?.data?.extra_headers ?? payload);
      setDraft('');
      addToast(t('脱敏头名单已保存（下一请求即生效）'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('指纹脱敏')}</h2>
        <p className="card-subtitle">
          {t('补充需要脱敏的上游请求头；调试报文里命中的头会显示为 [redacted]。保存即热生效。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-hint" style={{ marginBottom: 12 }}>
            {t('内置已覆盖：authorization / cookie / x-api-key，以及一切以 -token、-secret、-key 结尾的头。')}
          </div>

          {rows.length === 0 ? (
            <div className="empty-state">
              <p>{t('暂无额外脱敏头（仅有内置名单生效）')}</p>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8, marginBottom: 12 }}>
              {rows.map((name) => (
                <div key={name} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <code style={{ flex: 1 }}>{name}</code>
                  <button
                    type="button"
                    className="btn btn-secondary"
                    onClick={() => setRows(rows.filter((r) => r !== name))}
                  >
                    {t('删除')}
                  </button>
                </div>
              ))}
            </div>
          )}

          <div className="form-group">
            <label>{t('新增头名（支持尾部 * 通配）')}</label>
            <div style={{ display: 'flex', gap: 8 }}>
              <input
                className="form-input"
                value={draft}
                placeholder="x-foo-token 或 x-foo-*"
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    addRow();
                  }
                }}
              />
              <button type="button" className="btn btn-secondary" onClick={addRow}>
                {t('添加')}
              </button>
            </div>
          </div>

          <div className="settings-actions">
            <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
              {saving ? t('保存中...') : t('保存更改')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
