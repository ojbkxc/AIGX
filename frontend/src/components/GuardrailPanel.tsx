// 护栏面板（系统设置 → 网关 → 内容护栏）。
//
// 对照 Agent2API「设置 → 内容护栏」：输入黑名单（命中→降级/拒绝）、输出黑名单
// （命中→流式错误帧收尾）、200 假答复词表（命中→按可轮换失败换候选）。
// 配置保存即热生效（后端每请求 Arc 快照）；降级状态是进程内运行期状态，
// 到次日 00:00 UTC+8 自动恢复，也可在此手动解除。
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from './Toast';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';

interface RuleSpec {
  pattern: string;
  kind: 'keyword' | 'regex';
}

interface GuardrailView {
  enabled: boolean;
  action: 'degrade' | 'reject';
  input_rules: RuleSpec[];
  output_rules: RuleSpec[];
  fake_reply_rules: RuleSpec[];
  neutral_prompt: string;
}

interface DegradeView {
  active?: boolean;
  until_ms?: number;
  until_text?: string;
}

const EMPTY_SETTINGS: GuardrailView = {
  enabled: false,
  action: 'degrade',
  input_rules: [],
  output_rules: [],
  fake_reply_rules: [],
  neutral_prompt: '',
};

/** 从后端返回值安全解析设置（字段全可缺省）。 */
function parseSettings(raw: Record<string, unknown> | undefined): GuardrailView {
  const base = { ...EMPTY_SETTINGS };
  if (!raw) return base;
  base.enabled = Boolean(raw.enabled);
  base.action = raw.action === 'reject' ? 'reject' : 'degrade';
  base.neutral_prompt = typeof raw.neutral_prompt === 'string' ? raw.neutral_prompt : '';
  const pickRules = (v: unknown): RuleSpec[] =>
    Array.isArray(v)
      ? v
          .map((r) => {
            const item = r as Record<string, unknown>;
            return {
              pattern: String(item.pattern ?? ''),
              kind: item.kind === 'regex' ? ('regex' as const) : ('keyword' as const),
            };
          })
          .filter((r) => r.pattern.trim() !== '')
      : [];
  base.input_rules = pickRules(raw.input_rules);
  base.output_rules = pickRules(raw.output_rules);
  base.fake_reply_rules = pickRules(raw.fake_reply_rules);
  return base;
}

export default function GuardrailPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [settings, setSettings] = useState<GuardrailView>(EMPTY_SETTINGS);
  const [degrade, setDegrade] = useState<DegradeView>({});
  const [clearing, setClearing] = useState(false);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getGuardrail();
      const data = res?.data ?? {};
      setSettings(parseSettings(data.settings as Record<string, unknown> | undefined));
      setDegrade((data.degrade as DegradeView) ?? {});
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
    // 正则类规则的前端预检：后端保存时也会编译校验，这里提前给可读错误。
    for (const [label, rules] of [
      [t('输入词表'), settings.input_rules],
      [t('输出词表'), settings.output_rules],
      [t('200 假答复词表'), settings.fake_reply_rules],
    ] as const) {
      for (const rule of rules) {
        if (!rule.pattern.trim()) {
          setError(t('词表里有空规则，请填写或删除'));
          return;
        }
        if (rule.kind === 'regex') {
          try {
            new RegExp(rule.pattern);
          } catch {
            setError(`${label}: ${t('正则无效')}: ${rule.pattern}`);
            return;
          }
        }
      }
    }
    setSaving(true);
    try {
      const res = await api.saveGuardrail({
        enabled: settings.enabled,
        action: settings.action,
        input_rules: settings.input_rules,
        output_rules: settings.output_rules,
        fake_reply_rules: settings.fake_reply_rules,
        neutral_prompt: settings.neutral_prompt,
      });
      if (res?.success) {
        const data = res.data ?? {};
        setSettings(parseSettings(data.settings as Record<string, unknown> | undefined));
        setDegrade((data.degrade as DegradeView) ?? {});
        addToast(t('护栏配置已保存，立即生效'));
      } else {
        setError(res?.error?.message ?? t('保存失败'));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const clearDegrade = (): void => {
    setConfirmState({
      message: t('确认解除降级？解除后客户端提示词立即恢复'),
      danger: true,
      confirmText: t('解除降级'),
      onConfirm: async () => {
        setClearing(true);
        try {
          const res = await api.clearGuardrailDegrade();
          if (res?.success) {
            const data = res.data ?? {};
            setDegrade((data.degrade as DegradeView) ?? {});
            addToast(t('已解除降级'));
          } else {
            throw new Error(res?.error?.message ?? t('操作失败'));
          }
        } catch (err) {
          throw err instanceof Error ? err : new Error(String(err));
        } finally {
          setClearing(false);
        }
      },
    });
  };

  /** 单个词表编辑区：pattern + kind 行编辑 + 增删。 */
  const rulesEditor = (key: keyof Pick<GuardrailView, 'input_rules' | 'output_rules' | 'fake_reply_rules'>) => {
    const rules = settings[key];
    const update = (next: RuleSpec[]): void => setSettings({ ...settings, [key]: next });
    return (
      <div className="settings-form">
        {rules.length === 0 && (
          <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('暂无规则（词表为空 = 不检查）')}</div>
        )}
        {rules.map((rule, idx) => (
          <div className="settings-row" key={`${key}-${idx}`}>
            <div className="form-group" style={{ flex: 3 }}>
              <input
                className="form-input"
                value={rule.pattern}
                placeholder={rule.kind === 'regex' ? 'regex: as an AI language model.*' : '作为一个人工智能语言模型'}
                onChange={(e) => {
                  const next = [...rules];
                  next[idx] = { ...rule, pattern: e.target.value };
                  update(next);
                }}
              />
            </div>
            <div className="form-group" style={{ flex: 1 }}>
              <select
                className="form-input"
                value={rule.kind}
                onChange={(e) => {
                  const next = [...rules];
                  next[idx] = { ...rule, kind: e.target.value === 'regex' ? 'regex' : 'keyword' };
                  update(next);
                }}
              >
                <option value="keyword">{t('字面子串')}</option>
                <option value="regex">{t('正则')}</option>
              </select>
            </div>
            <div className="form-group" style={{ flex: 0 }}>
              <button
                className="btn btn-outline btn-sm"
                onClick={() => update(rules.filter((_, i) => i !== idx))}
              >
                {t('删除')}
              </button>
            </div>
          </div>
        ))}
        <div className="settings-actions">
          <button className="btn btn-outline btn-sm" onClick={() => update([...rules, { pattern: '', kind: 'keyword' }])}>
            {t('+ 添加规则')}
          </button>
        </div>
      </div>
    );
  };

  return (
    <div className="card" style={{ marginTop: 16 }}>
      <div className="card-header">
        <h2>{t('内容护栏')}</h2>
        <p className="card-subtitle">
          {t('输入黑名单（命中→降级或拒绝）、输出黑名单（命中→错误帧收尾）、200 假答复门控（命中→换候选重试）；默认关闭，关闭时零行为变化。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        {loading ? (
          <div className="empty-state"><p>{t('加载中...')}</p></div>
        ) : (
          <>
            {/* 降级状态（进程内运行期状态，重启清零） */}
            <div className="settings-form">
              <div className="settings-row">
                <div className="form-group">
                  <label>{t('降级状态')}</label>
                  <div style={{ fontSize: 13 }}>
                    {degrade.active ? (
                      <>
                        <span style={{ color: 'var(--warning, #d97706)' }}>{t('降级进行中')}</span>
                        {degrade.until_text ? ` · ${t('至')} ${degrade.until_text}` : ''}
                      </>
                    ) : (
                      <span style={{ color: 'var(--text-muted)' }}>{t('未降级')}</span>
                    )}
                  </div>
                </div>
                {degrade.active && (
                  <div className="form-group">
                    <button className="btn btn-outline btn-sm" onClick={clearDegrade} disabled={clearing}>
                      {clearing ? t('操作中...') : t('手动解除降级')}
                    </button>
                  </div>
                )}
              </div>
            </div>

            {/* 总开关 + 策略 */}
            <div className="settings-form">
              <div className="settings-row">
                <label className="checkbox-label">
                  <input
                    type="checkbox"
                    checked={settings.enabled}
                    onChange={(e) => setSettings({ ...settings, enabled: e.target.checked })}
                  />
                  {t('启用护栏')}
                </label>
              </div>
              <div className="settings-row">
                <div className="form-group">
                  <label>{t('输入命中处置')}</label>
                  <select
                    className="form-input"
                    value={settings.action}
                    onChange={(e) => setSettings({ ...settings, action: e.target.value === 'reject' ? 'reject' : 'degrade' })}
                  >
                    <option value="degrade">{t('降级：换中性提示词原地重发一次')}</option>
                    <option value="reject">{t('拒绝：直接返回标准错误')}</option>
                  </select>
                </div>
                <div className="form-group">
                  <label>{t('中性提示词（降级时替换客户端 system）')}</label>
                  <textarea
                    className="form-input"
                    rows={2}
                    value={settings.neutral_prompt}
                    placeholder="You are a helpful AI assistant."
                    onChange={(e) => setSettings({ ...settings, neutral_prompt: e.target.value })}
                  />
                  <span className="form-hint">
                    {t('用于绕开上游按模板句的逐字指纹拦截，不是对抗语义审核；留空 = 使用内置默认文本。')}
                  </span>
                </div>
              </div>
            </div>

            {/* 三个词表 */}
            <h3 style={{ margin: '20px 0 8px' }}>{t('输入词表')}</h3>
            {rulesEditor('input_rules')}
            <h3 style={{ margin: '20px 0 8px' }}>{t('输出词表')}</h3>
            {rulesEditor('output_rules')}
            <h3 style={{ margin: '20px 0 8px' }}>
              {t('200 假答复词表')}
            </h3>
            <p style={{ fontSize: 13, color: 'var(--text-muted)', margin: '0 0 8px' }}>
              {t('上游把内容审核拒绝包装成 HTTP 200 时命中此词表；命中后按可轮换失败处理（等同 429 换下一候选，不记渠道成功）。')}
            </p>
            {rulesEditor('fake_reply_rules')}

            <div className="settings-actions" style={{ marginTop: 16 }}>
              <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
                {saving ? t('保存中...') : t('保存更改')}
              </button>
              <button className="btn btn-outline" onClick={() => void load()} disabled={saving}>
                {t('重置')}
              </button>
            </div>
          </>
        )}
      </div>
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
