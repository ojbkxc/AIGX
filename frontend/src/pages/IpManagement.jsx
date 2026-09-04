import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog from '../components/ConfirmDialog';
import './IpManagement.css';

// IpManagement 页面：IP 白名单/黑名单管理。
// 参照 Groups.jsx / Keys.jsx 的 CRUD 界面模式。
// 规则支持单 IP 或 CIDR 表示法（如 192.168.0.0/24）。
export default function IpManagement() {
  const { t } = useTranslation();
  const addToast = useToast();

  // 全局开关与规则列表
  const [enabled, setEnabled] = useState(false);
  const [whitelist, setWhitelist] = useState([]);
  const [blacklist, setBlacklist] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [savingSwitch, setSavingSwitch] = useState(false);

  // 添加规则表单（白名单 / 黑名单共用一套状态，由 activeTab 区分）
  const [activeTab, setActiveTab] = useState('whitelist');
  const [pattern, setPattern] = useState('');
  const [note, setNote] = useState('');
  const [adding, setAdding] = useState(false);

  // 确认弹窗
  const [confirmState, setConfirmState] = useState(null);

  useEffect(() => {
    loadFilter();
  }, []);

  // 加载 IP 过滤配置
  const loadFilter = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getIpFilter();
      const data = res?.data || res || {};
      setEnabled(!!data.enabled);
      setWhitelist(Array.isArray(data.whitelist) ? data.whitelist : []);
      setBlacklist(Array.isArray(data.blacklist) ? data.blacklist : []);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  };

  // 切换全局开关
  const handleToggleEnabled = async () => {
    setSavingSwitch(true);
    setError('');
    try {
      const next = !enabled;
      await api.updateIpFilter({ enabled: next });
      setEnabled(next);
      addToast(next ? t('IP 过滤已启用') : t('IP 过滤已禁用'));
    } catch (err) {
      setError(err.message);
    } finally {
      setSavingSwitch(false);
    }
  };

  // 简单校验 IP / CIDR 格式（宽松校验，允许 IPv4、IPv6、CIDR）
  const isValidPattern = (p) => {
    if (!p) return false;
    // IPv4 CIDR
    if (/^\d{1,3}(\.\d{1,3}){3}(\/\d{1,2})?$/.test(p)) return true;
    // 含冒号视为 IPv6 / IPv6 CIDR，宽松放行
    if (p.includes(':')) return true;
    return false;
  };

  // 添加规则
  const handleAdd = async () => {
    const p = pattern.trim();
    if (!p) {
      setError(t('请输入 IP 或 CIDR'));
      return;
    }
    if (!isValidPattern(p)) {
      setError(t('格式不正确，请输入合法 IP 或 CIDR（如 192.168.0.0/24）'));
      return;
    }
    setAdding(true);
    setError('');
    try {
      if (activeTab === 'whitelist') {
        await api.addWhitelist(p, note.trim());
        addToast(t('白名单规则已添加'));
      } else {
        await api.addBlacklist(p, note.trim());
        addToast(t('黑名单规则已添加'));
      }
      setPattern('');
      setNote('');
      loadFilter();
    } catch (err) {
      setError(err.message);
    } finally {
      setAdding(false);
    }
  };

  // 删除规则
  const handleRemove = (item) => {
    const p = typeof item === 'string' ? item : (item.pattern || item.ip || '');
    const isWhite = activeTab === 'whitelist';
    setConfirmState({
      title: t('删除规则'),
      message: `${t('确定删除规则')} ${p}?`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          if (isWhite) {
            await api.removeWhitelist(p);
            addToast(t('白名单规则已删除'));
          } else {
            await api.removeBlacklist(p);
            addToast(t('黑名单规则已删除'));
          }
          loadFilter();
        } catch (err) {
          setError(err.message);
        }
      },
    });
  };

  // 渲染规则项：兼容字符串数组与对象数组两种后端返回格式
  const renderPattern = (item) => typeof item === 'string' ? item : (item.pattern || item.ip || '');
  const renderNote = (item) => typeof item === 'string' ? '' : (item.note || item.remark || '');

  const currentList = activeTab === 'whitelist' ? whitelist : blacklist;

  return (
    <div className="ipm-shell">
      <div className="page-header">
        <div>
          <h1>{t('IP 管理')}</h1>
          <p>{t('管理 IP 白名单与黑名单，支持单 IP 与 CIDR 网段（如 192.168.0.0/24）')}</p>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      {/* 全局开关 */}
      <div className="card">
        <div className="card-body ipm-switch-row">
          <div className="ipm-switch-info">
            <div className="ipm-switch-title">{t('IP 过滤总开关')}</div>
            <div className="ipm-switch-hint">{t('开启后白名单/黑名单规则才会生效')}</div>
          </div>
          <label className="ipm-toggle">
            <input
              type="checkbox"
              checked={enabled}
              onChange={handleToggleEnabled}
              disabled={savingSwitch || loading}
            />
            <span className="ipm-toggle-slider"></span>
          </label>
        </div>
      </div>

      {/* 白名单 / 黑名单 Tab */}
      <div className="card">
        <div className="card-body">
          <div className="ipm-tabs">
            <button
              className={`btn ${activeTab === 'whitelist' ? 'btn-primary' : 'btn-outline'}`}
              onClick={() => { setActiveTab('whitelist'); setPattern(''); setNote(''); setError(''); }}
            >
              {t('白名单')} ({whitelist.length})
            </button>
            <button
              className={`btn ${activeTab === 'blacklist' ? 'btn-primary' : 'btn-outline'}`}
              onClick={() => { setActiveTab('blacklist'); setPattern(''); setNote(''); setError(''); }}
            >
              {t('黑名单')} ({blacklist.length})
            </button>
          </div>

          {/* 添加规则表单 */}
          <div className="ipm-add-form">
            <div className="form-group">
              <label>{t('IP / CIDR')} *</label>
              <input
                className="form-input"
                placeholder={t('例如：192.168.0.0/24 或 10.0.0.1')}
                value={pattern}
                onChange={(e) => setPattern(e.target.value)}
                disabled={loading}
              />
            </div>
            <div className="form-group">
              <label>{t('备注')}</label>
              <input
                className="form-input"
                placeholder={t('可选备注，如：办公网段')}
                value={note}
                onChange={(e) => setNote(e.target.value)}
                disabled={loading}
              />
            </div>
            <div className="ipm-add-action">
              <button
                className="btn btn-primary"
                onClick={handleAdd}
                disabled={adding || loading || !pattern.trim()}
              >
                {adding ? t('保存中...') : t('添加规则')}
              </button>
            </div>
          </div>

          {/* 规则列表 */}
          {loading ? (
            <div className="loading">{t('加载中')}</div>
          ) : currentList.length === 0 ? (
            <div className="empty-state">
              <p>{activeTab === 'whitelist' ? t('暂无白名单规则') : t('暂无黑名单规则')}</p>
            </div>
          ) : (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('IP / CIDR')}</th>
                    <th>{t('备注')}</th>
                    <th>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {currentList.map((item, i) => {
                    const p = renderPattern(item);
                    const n = renderNote(item);
                    return (
                      <tr key={p || i}>
                        <td><code style={{ background: 'var(--card-bg)', padding: '2px 6px', borderRadius: 4 }}>{p}</code></td>
                        <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>{n || '—'}</td>
                        <td>
                          <div className="actions-cell">
                            <button className="btn btn-danger btn-sm" onClick={() => handleRemove(item)}>
                              {t('删除')}
                            </button>
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}