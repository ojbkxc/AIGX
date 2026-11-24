import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ModelPicker from '../components/ModelPicker';
import type { AgentSettingsItem } from '../types';

/** 工具风险等级 → 展示文案（与后端 risk 字段对齐） */
const RISK_LABEL: Record<string, string> = {
  readonly: '只读',
  low: '低危写',
  high: '高危写（需审批）',
};

/**
 * AI 助手设置（系统设置 → 运维 → AI 助手）
 *
 * - 对话模型：与聊天页 / AI 运维页共用同一个 ModelPicker 控件
 * - 系统提示词：留空使用内置默认；非空则替换默认提示词
 * - 工具开关：取消勾选后该工具不再下发给模型（也就不会被执行）
 * - 轮数 / 审批超时 / 历史保留：Agent 运行参数
 */
export default function AgentSettingsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [enabled, setEnabled] = useState(false);
  const [model, setModel] = useState('');
  const [channel, setChannel] = useState('');
  const [channels, setChannels] = useState<Array<{ id: string; name: string; status?: string }>>([]);
  const [systemPrompt, setSystemPrompt] = useState('');
  const [effort, setEffort] = useState<'off' | 'low' | 'medium' | 'high'>('off');
  const [maxTurns, setMaxTurns] = useState('12');
  const [approvalTimeout, setApprovalTimeout] = useState('300');
  const [maxHistory, setMaxHistory] = useState('20');
  const [tools, setTools] = useState<AgentSettingsItem['tools']>([]);
  const [disabledTools, setDisabledTools] = useState<Set<string>>(new Set());

  useEffect(() => {
    void (async () => {
      try {
        const [res, chRes] = await Promise.all([api.getAgentSettings(), api.listChannels()]);
        const list = (chRes?.data || [])
          .filter((c) => (c.status ?? 'enabled') === 'enabled')
          .map((c) => ({ id: String(c.id), name: c.name, status: c.status }));
        setChannels(list);
        const d = res?.data;
        if (!d) return;
        setEnabled(Boolean(d.enabled));
        setModel(d.model || '');
        setChannel(d.channel || '');
        setSystemPrompt(d.systemPrompt || '');
        setEffort((['off', 'low', 'medium', 'high'] as const).includes(d.reasoning_effort as 'off')
          ? (d.reasoning_effort as 'off' | 'low' | 'medium' | 'high')
          : 'off');
        setMaxTurns(String(d.max_turns ?? 12));
        setApprovalTimeout(String(d.approval_timeout_secs ?? 300));
        setMaxHistory(String(d.max_history ?? 20));
        setTools(d.tools || []);
        setDisabledTools(new Set(d.disabledTools || []));
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const toggleTool = (name: string, on: boolean): void => {
    setDisabledTools((prev) => {
      const next = new Set(prev);
      if (on) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      const res = await api.saveAgentSettings({
        enabled,
        model: model.trim(),
        channel: channel.trim(),
        max_turns: Math.max(1, Number(maxTurns) || 12),
        approval_timeout_secs: Math.max(0, Number(approvalTimeout) || 300),
        max_history: Math.max(1, Number(maxHistory) || 20),
        reasoning_effort: effort,
        system_prompt: systemPrompt,
        disabled_tools: Array.from(disabledTools),
      });
      const d = res?.data;
      if (d) {
        setEnabled(Boolean(d.enabled));
        setModel(d.model || '');
        setChannel(d.channel || '');
        setSystemPrompt(d.systemPrompt || '');
        setDisabledTools(new Set(d.disabledTools || []));
      }
      addToast(t('AI 助手设置已保存'));
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
        <h2>{t('AI 助手')}</h2>
        <p className="card-subtitle">
          {t('AI 运维工作台：对话模型、系统提示词与工具开关；高危写操作仍需人工审批。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          {/* ── 对齐 A2A v3.5.1「新增配置 / 编辑配置」弹窗的字段与顺序 ──
              AIGX 后端暂无这些字段的数据源：控件与顺序照 A2A 落位，只读占位，
              取数待补（详见交付清单「A2A 有但 AIGX 缺数据源」）。 */}
          <section className="a2a-settings-section" aria-label="A2A 配置字段（结构对齐）">
            <h3 className="a2a-settings-title">配置</h3>
            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
              <div className="form-group">
                <label>配置名称</label>
                <input className="form-input" value="AIGX 内置助手" readOnly disabled />
                <div className="form-hint">A2A 的配置名称；AIGX 仅有单一内置配置，暂无该字段。</div>
              </div>
              <div className="form-group">
                <label>备注</label>
                <input className="form-input" value="" readOnly disabled placeholder="AIGX 后端暂无备注字段" />
                <div className="form-hint">A2A 的备注字段；AIGX 暂无数据源。</div>
              </div>
            </div>
            <div className="form-group">
              <label>接口基址</label>
              <input className="form-input" value="" readOnly disabled placeholder="AIGX 后端暂无接口基址字段" />
              <div className="form-hint">A2A 的接口基址；AIGX 的模型地址由供应商渠道决定。</div>
            </div>
            <div className="form-group">
              <label>认证方式</label>
              <select className="form-input" disabled value="gateway">
                <option value="gateway">使用网关 Key</option>
                <option value="key">API Key</option>
                <option value="env">环境变量</option>
                <option value="none">无需认证</option>
                <option value="preserve">保留现有认证</option>
              </select>
              <div className="form-hint">A2A 的认证方式；AIGX 助手固定走网关内部调用，暂无该字段。</div>
            </div>
            <div className="form-group">
              <label>高级选项 · 客户端协议</label>
              <input className="form-input" value="客户端原生配置" readOnly disabled />
              <div className="form-hint">A2A 的高级选项；AIGX 无本机客户端配置写入面，字段先落位。</div>
            </div>
          </section>

          {/* ── 模型配置（对齐 A2A 的 ModelsFields / SingleModelFields）── */}
          <section className="a2a-settings-section" aria-label="模型配置（对齐 A2A）">
            <h3 className="a2a-settings-title">模型配置</h3>
            <div className="form-group">
              <label>{t('对话模型')}</label>
              <ModelPicker value={model} onChange={setModel} />
              <div className="form-hint">{t('与聊天页共用同一模型列表；留空走全局默认模型。')}</div>
            </div>
            <div className="form-group">
              <label>{t('思考强度')}</label>
              <select
                className="form-input"
                value={effort}
                onChange={(e) => setEffort(e.target.value as 'off' | 'low' | 'medium' | 'high')}
              >
                <option value="off">{t('关（直接作答）')}</option>
                <option value="low">{t('低')}</option>
                <option value="medium">{t('中')}</option>
                <option value="high">{t('高')}</option>
              </select>
              <div className="form-hint">对应 A2A 的思考等级（effort）；AIGX 只支持关 / 低 / 中 / 高四档。</div>
            </div>
          </section>

          {/* ── AIGX 独有项（待评审）：A2A 没有的助手运行参数，保留不动 ── */}
          <section className="a2a-settings-section" aria-label="AIGX 独有项（待评审）">
            <h3 className="a2a-settings-title">AIGX 独有项（待评审）</h3>
            <div className="form-group">
              <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <input
                  type="checkbox"
                  checked={enabled}
                  onChange={(e) => setEnabled(e.target.checked)}
                />
                <span>{t('启用 AI 运维助理')}</span>
              </label>
              <div className="form-hint">{t('关闭后 AI 运维页不可用；工具与审批规则不受影响。')}</div>
            </div>

            <div className="form-group">
              <label>{t('供应商锁定')}</label>
              <select
                className="form-input"
                value={channel}
                onChange={(e) => setChannel(e.target.value)}
              >
                <option value="">{t('自动选择供应商（默认）')}</option>
                {channels.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
              <div className="form-hint">
                {t('指定后 AI 助手只通过该供应商调用模型（排障用）；留空则按优先级自动选路。')}
              </div>
            </div>

            <div className="form-group">
              <label>{t('系统提示词（留空使用内置默认）')}</label>
              <textarea
                className="form-input"
                rows={6}
                placeholder={t('内置默认：优先只读查现状，写操作仅在明确要求时执行，高危操作走人工审批。')}
                value={systemPrompt}
                onChange={(e) => setSystemPrompt(e.target.value)}
              />
            </div>

            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: 12 }}>
              <div className="form-group">
                <label>{t('单次最大轮数')}</label>
                <input
                  className="form-input"
                  type="number"
                  min={1}
                  max={50}
                  value={maxTurns}
                  onChange={(e) => setMaxTurns(e.target.value)}
                />
              </div>
              <div className="form-group">
                <label>{t('审批超时（秒）')}</label>
                <input
                  className="form-input"
                  type="number"
                  min={0}
                  max={3600}
                  value={approvalTimeout}
                  onChange={(e) => setApprovalTimeout(e.target.value)}
                />
              </div>
              <div className="form-group">
                <label>{t('历史保留条数')}</label>
                <input
                  className="form-input"
                  type="number"
                  min={1}
                  max={200}
                  value={maxHistory}
                  onChange={(e) => setMaxHistory(e.target.value)}
                />
              </div>
            </div>

            {tools.length > 0 && (
              <div className="form-group">
                <label>{t('工具开关')}</label>
                <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(260px, 1fr))', gap: 8 }}>
                  {tools.map((tool) => {
                    const on = !disabledTools.has(tool.name);
                    return (
                      <label key={tool.name} style={{ display: 'flex', gap: 8, alignItems: 'flex-start' }}>
                        <input
                          type="checkbox"
                          checked={on}
                          onChange={(e) => toggleTool(tool.name, e.target.checked)}
                        />
                        <span style={{ fontSize: 12 }}>
                          <code>{tool.name}</code>
                          <span style={{ color: 'var(--text-muted)' }}> · {t(RISK_LABEL[tool.risk] ?? tool.risk)}</span>
                          <div style={{ color: 'var(--text-muted)' }}>{tool.description}</div>
                        </span>
                      </label>
                    );
                  })}
                </div>
                <div className="form-hint">{t('取消勾选后该工具不再下发给模型；高危工具仍会触发人工审批。')}</div>
              </div>
            )}
          </section>

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
