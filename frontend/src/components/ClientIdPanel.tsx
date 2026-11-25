// 客户端识别规则面板（系统设置 → 网关 → 客户端识别）。
//
// 对照 Agent2API「设置 → 客户端识别」：内置客户端目录 + 自定义规则 CRUD +
// 样例试识别。规则保存后下一个请求立即生效（后端原子替换进程快照）。
// 识别只读请求头、不改转发行为；未识别 = 空名（不写 `unknown` 之类的占位词，
// 预览里空名只显示 `—`）。
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from './Toast';

interface RuleForm {
  id?: string;
  name: string;
  enabled: boolean;
  target: 'ua' | 'header';
  header: string;
  matchMode: 'prefix' | 'contains' | 'regex';
  pattern: string;
  clientName: string;
  priority: number;
}

const EMPTY_FORM: RuleForm = {
  name: '',
  enabled: true,
  target: 'ua',
  header: '',
  matchMode: 'contains',
  pattern: '',
  clientName: '',
  priority: 100,
};

export default function ClientIdPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [view, setView] = useState<Record<string, unknown>>({});
  const [rules, setRules] = useState<Array<Record<string, unknown>>>([]);
  const [builtins, setBuiltins] = useState<Array<Record<string, unknown>>>([]);
  const [form, setForm] = useState<RuleForm>(EMPTY_FORM);
  const [previewUa, setPreviewUa] = useState('');
  const [previewResult, setPreviewResult] = useState<Record<string, unknown> | null>(null);
  const [previewing, setPreviewing] = useState(false);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getClientId();
      const data = res?.data ?? {};
      setView(data);
      setRules(Array.isArray(data.rules) ? data.rules as Array<Record<string, unknown>> : []);
      setBuiltins(Array.isArray(data.builtins) ? data.builtins as Array<Record<string, unknown>> : []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (): Promise<void> => {
    setError('');
    if (!form.name.trim()) {
      setError(t('规则名称不能为空'));
      return;
    }
    if (!form.pattern.trim()) {
      setError(t('匹配内容不能为空'));
      return;
    }
    if (!form.clientName.trim()) {
      setError(t('命中后的客户端名不能为空'));
      return;
    }
    setSaving(true);
    try {
      const payload: Record<string, unknown> = {
        name: form.name.trim(),
        enabled: form.enabled,
        target: form.target,
        header: form.target === 'header' ? form.header.trim() : '',
        matchMode: form.matchMode,
        pattern: form.pattern.trim(),
        clientName: form.clientName.trim(),
        priority: Number(form.priority) || 0,
      };
      const res = form.id
        ? await api.updateClientRule(form.id, payload)
        : await api.createClientRule(payload);
      if (res?.success) {
        const data = res.data ?? {};
        const next = Array.isArray(data.rules) ? data.rules as Array<Record<string, unknown>> : rules;
        setRules(next);
        setForm(EMPTY_FORM);
        addToast(t('识别规则已保存，下一个请求立即生效'));
      } else {
        setError(res?.error?.message ?? t('保存失败'));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (id: string): Promise<void> => {
    if (!window.confirm(t('删除该识别规则？'))) return;
    setError('');
    try {
      const res = await api.deleteClientRule(id);
      if (res?.success) {
        setRules(Array.isArray(res.data?.rules) ? res.data.rules as Array<Record<string, unknown>> : []);
        addToast(t('规则已删除'));
      } else {
        setError(res?.error?.message ?? t('删除失败'));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const preview = async (): Promise<void> => {
    if (!previewUa.trim()) return;
    setPreviewing(true);
    try {
      const res = await api.previewClientId({ userAgent: previewUa.trim() });
      setPreviewResult(res?.data ?? null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPreviewing(false);
    }
  };

  const formField = (
    <div className="settings-form">
      <div className="settings-row">
        <div className="form-group">
          <label>{t('规则名称')}</label>
          <input className="form-input" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
        </div>
        <div className="form-group">
          <label>{t('匹配来源')}</label>
          <select className="form-input" value={form.target} onChange={(e) => setForm({ ...form, target: e.target.value as RuleForm['target'] })}>
            <option value="ua">{t('User-Agent')}</option>
            <option value="header">{t('请求头')}</option>
          </select>
        </div>
        <div className="form-group">
          <label>{form.target === 'header' ? t('请求头名称') : t('请求头名称（UA 模式忽略）')}</label>
          <input
            className="form-input"
            disabled={form.target !== 'header'}
            value={form.header}
            onChange={(e) => setForm({ ...form, header: e.target.value })}
            placeholder="x-app-name"
          />
        </div>
        <div className="form-group">
          <label>{t('匹配方式')}</label>
          <select className="form-input" value={form.matchMode} onChange={(e) => setForm({ ...form, matchMode: e.target.value as RuleForm['matchMode'] })}>
            <option value="prefix">{t('前缀')}</option>
            <option value="contains">{t('包含')}</option>
            <option value="regex">{t('正则')}</option>
          </select>
        </div>
        <div className="form-group">
          <label>{t('匹配内容')}</label>
          <input className="form-input" value={form.pattern} onChange={(e) => setForm({ ...form, pattern: e.target.value })} placeholder="codex_cli" />
        </div>
        <div className="form-group">
          <label>{t('目标客户端名')}</label>
          <input className="form-input" value={form.clientName} onChange={(e) => setForm({ ...form, clientName: e.target.value })} />
        </div>
        <div className="form-group">
          <label>{t('优先级（数值小者先）')}</label>
          <input
            className="form-input"
            type="number"
            value={form.priority}
            onChange={(e) => setForm({ ...form, priority: Number(e.target.value) || 0 })}
          />
        </div>
        <label className="checkbox-label">
          <input type="checkbox" checked={form.enabled} onChange={(e) => setForm({ ...form, enabled: e.target.checked })} />
          {t('启用')}
        </label>
      </div>
      <div className="settings-actions">
        <button className="btn btn-primary" onClick={save} disabled={saving}>
          {saving ? t('保存中...') : form.id ? t('更新规则') : t('新建规则')}
        </button>
        {form.id && (
          <button className="btn btn-outline" onClick={() => setForm(EMPTY_FORM)}>
            {t('取消编辑')}
          </button>
        )}
      </div>
    </div>
  );

  return (
    <div className="card" style={{ marginTop: 16 }}>
      <div className="card-header">
        <h2>{t('客户端识别')}</h2>
        <p className="card-subtitle">
          {t('只读请求头判定请求来自哪个客户端，写入请求日志与报表；不改变转发行为。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        {loading ? (
          <div className="empty-state"><p>{t('加载中...')}</p></div>
        ) : (
          <>
            {/* 内置客户端目录 */}
            <h3 style={{ margin: '16px 0 8px' }}>{t('内置客户端')}</h3>
            <table className="top-rank-table" style={{ width: '100%' }}>
              <thead>
                <tr>
                  <th>{t('客户端')}</th>
                  <th>{t('声明别名')}</th>
                  <th>{t('UA 产品令牌')}</th>
                </tr>
              </thead>
              <tbody>
                {builtins.map((item) => (
                  <tr key={String(item.id)}>
                    <td style={{ whiteSpace: 'nowrap' }}>{String(item.name)}</td>
                    <td style={{ fontSize: 12, color: 'var(--text-muted)', wordBreak: 'break-all' }}>
                      {Array.isArray(item.aliases) ? (item.aliases as unknown[]).map(String).join(', ') : ''}
                    </td>
                    <td style={{ fontSize: 12, color: 'var(--text-muted)', wordBreak: 'break-all' }}>
                      {Array.isArray(item.products) ? (item.products as unknown[]).map(String).join(', ') : ''}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>

            {/* 样例试识别 */}
            <h3 style={{ margin: '20px 0 8px' }}>{t('样例试识别')}</h3>
            <div className="settings-form">
              <div className="form-group">
                <label>{t('User-Agent')}</label>
                <textarea
                  className="form-input"
                  rows={2}
                  value={previewUa}
                  onChange={(e) => setPreviewUa(e.target.value)}
                  placeholder="claude-cli/2.0.4 anthropic-sdk/python 3.0"
                />
              </div>
              <div className="settings-actions">
                <button className="btn btn-outline" onClick={preview} disabled={previewing}>
                  {previewing ? t('试识别中...') : t('试识别')}
                </button>
                {previewResult && (
                  <span style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                    {previewResult.clientName ? String(previewResult.clientName) : '—'}
                    {previewResult.clientVersion ? ` ${String(previewResult.clientVersion)}` : ''}
                    {previewResult.source ? ` · ${String(previewResult.source)}` : ''}
                  </span>
                )}
              </div>
            </div>

            {/* 自定义规则 */}
            <h3 style={{ margin: '20px 0 8px' }}>
              {t('自定义规则')}（{rules.length} / {Number(view.maxRules ?? 0) || '—'}）
            </h3>
            <table className="top-rank-table" style={{ width: '100%' }}>
              <thead>
                <tr>
                  <th>{t('名称')}</th>
                  <th>{t('来源')}</th>
                  <th>{t('方式')}</th>
                  <th>{t('匹配内容')}</th>
                  <th>{t('目标')}</th>
                  <th>{t('优先级')}</th>
                  <th>{t('状态')}</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {rules.length === 0 && (
                  <tr><td colSpan={8} style={{ color: 'var(--text-muted)' }}>{t('暂无规则')}</td></tr>
                )}
                {rules.map((rule) => (
                  <tr key={String(rule.id)}>
                    <td style={{ whiteSpace: 'nowrap' }}>{String(rule.name)}</td>
                    <td style={{ fontSize: 12 }}>{rule.target === 'header' ? `${t('请求头')} ${String(rule.header ?? '')}` : t('User-Agent')}</td>
                    <td style={{ fontSize: 12 }}>{String(rule.matchMode ?? '')}</td>
                    <td style={{ fontSize: 12, wordBreak: 'break-all' }}>{String(rule.pattern ?? '')}</td>
                    <td style={{ fontSize: 12 }}>{String(rule.clientName ?? '')}</td>
                    <td className="num">{Number(rule.priority ?? 0)}</td>
                    <td style={{ fontSize: 12 }}>{rule.enabled ? t('启用') : t('停用')}</td>
                    <td>
                      <button
                        className="btn btn-outline btn-sm"
                        onClick={() => setForm({
                          id: String(rule.id),
                          name: String(rule.name ?? ''),
                          enabled: Boolean(rule.enabled),
                          target: rule.target === 'header' ? 'header' : 'ua',
                          header: String(rule.header ?? ''),
                          matchMode: String(rule.matchMode ?? 'contains') as RuleForm['matchMode'],
                          pattern: String(rule.pattern ?? ''),
                          clientName: String(rule.clientName ?? ''),
                          priority: Number(rule.priority ?? 0),
                        })}
                      >
                        {t('编辑')}
                      </button>
                      <button className="btn btn-outline btn-sm" style={{ marginLeft: 4 }} onClick={() => void remove(String(rule.id))}>
                        {t('删除')}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>

            <h3 style={{ margin: '20px 0 8px' }}>{form.id ? t('编辑规则') : t('新建规则')}</h3>
            {formField}
          </>
        )}
      </div>
    </div>
  );
}
