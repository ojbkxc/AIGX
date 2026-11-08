/**
 * 用户工具权限（#65；#34 用户工具回路的策略配置面，设计稿 §5.1）。
 *
 * - 全局开关 `user_tools_enabled`（默认关；关闭时终端用户纯聊天，与历史逐字节等价）
 * - 组级 allowlist 编辑：组名来自倍率分组体系（api.listGroups），
 *   工具目录来自 catalog（name/label/risk/has_user_exec/命中状态）
 * - `["*"]` 通配 = 该组获 LowRisk 全集；HighRisk 永不暴露（D2 硬封顶，
 *   目录行禁用标注）；未声明 user_exec 的工具同样不可配置（D1）
 * - 后端三端点（GET/PUT policy + GET catalog）未落地前 404 → 「后端未就绪」
 *   降级空态（RouteSticky 同款模式），不崩
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, RefreshCw, ShieldCheck } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Button, Card, EmptyState, Select, SkeletonTable, Switch } from '../components/ui';
import type { UserToolCatalogItem, UserToolPolicy } from '../types';
import './UserTools.css';

const EMPTY_POLICY: UserToolPolicy = { enabled: false, groups: {} };

/** 风险徽标配色（ReadOnly=中性 / LowRisk=主色 / HighRisk=危险禁用） */
function riskBadgeClass(risk: UserToolCatalogItem['risk']): string {
  if (risk === 'high') return 'ut-risk high';
  if (risk === 'low') return 'ut-risk low';
  return 'ut-risk readonly';
}

export default function UserTools(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  /** 后端三端点 404（未落地）→ 降级空态 */
  const [degraded, setDegraded] = useState(false);
  const [error, setError] = useState('');
  const [catalog, setCatalog] = useState<UserToolCatalogItem[]>([]);
  const [groups, setGroups] = useState<string[]>([]);
  /** 编辑草稿（load 后深拷贝；保存 PUT 全量） */
  const [draft, setDraft] = useState<UserToolPolicy>(EMPTY_POLICY);
  const [selectedGroup, setSelectedGroup] = useState('');
  const [saving, setSaving] = useState(false);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    const [policyRes, catalogRes, groupsRes] = await Promise.allSettled([
      api.getUserToolPolicy(),
      api.getUserToolCatalog(),
      api.listGroups(),
    ]);
    // policy 404 = 后端三端点未落地 → 降级
    if (policyRes.status === 'rejected') {
      const err = policyRes.reason as Error & { status?: number };
      if (err?.status === 404) {
        setDegraded(true);
        setLoading(false);
        return;
      }
      setError(err instanceof Error ? err.message : String(err));
      setLoading(false);
      return;
    }
    const p = (policyRes.status === 'fulfilled' ? policyRes.value.data : null) ?? EMPTY_POLICY;
    const next: UserToolPolicy = {
      enabled: p.enabled === true,
      groups: p.groups && typeof p.groups === 'object' ? p.groups : {},
    };
    setDraft(JSON.parse(JSON.stringify(next)) as UserToolPolicy);
    if (catalogRes.status === 'fulfilled') {
      const tools = catalogRes.value?.data?.tools;
      setCatalog(Array.isArray(tools) ? tools : []);
    } else {
      setCatalog([]);
    }
    if (groupsRes.status === 'fulfilled') {
      const list = (groupsRes.value as { data?: Array<{ name?: string }> })?.data ?? [];
      setGroups(list.map((g) => String(g.name || '')).filter(Boolean));
    } else {
      setGroups([]);
    }
    setLoading(false);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 选中组的草稿 allowlist（无则初始化为空组策略） */
  const ensureGroup = (name: string): void => {
    setDraft((prev) => {
      if (prev.groups[name]) return prev;
      return { ...prev, groups: { ...prev.groups, [name]: { allowlist: [] } } };
    });
  };

  const selectGroup = (name: string): void => {
    if (!name) return;
    ensureGroup(name);
    setSelectedGroup(name);
  };

  const setAllowlist = (updater: (list: string[]) => string[]): void => {
    if (!selectedGroup) return;
    ensureGroup(selectedGroup);
    setDraft((prev) => ({
      ...prev,
      groups: {
        ...prev.groups,
        [selectedGroup]: { allowlist: updater(prev.groups[selectedGroup]?.allowlist ?? []) },
      },
    }));
  };

  /** 通配开关：开 = ["*"]；关 = 清空（从 LowRisk 目录预填可选集） */
  const wildcard = useMemo(
    () => (draft.groups[selectedGroup]?.allowlist ?? []).includes('*'),
    [draft, selectedGroup],
  );
  const toggleWildcard = (on: boolean): void => {
    setAllowlist(() => {
      if (on) return ['*'];
      // 关掉通配回落到目录里可配置的 LowRisk/ReadOnly 集（排除 HighRisk/无 user_exec）
      return catalog
        .filter((c) => c.has_user_exec && c.risk !== 'high')
        .map((c) => c.name);
    });
  };

  const toggleTool = (name: string, on: boolean): void => {
    setAllowlist((list) => {
      const set = new Set(list.filter((x) => x !== '*'));
      if (on) set.add(name);
      else set.delete(name);
      return [...set];
    });
  };

  const save = async (): Promise<void> => {
    if (saving) return;
    setSaving(true);
    setError('');
    try {
      const res = await api.saveUserToolPolicy(draft);
      const saved = res.data ?? draft;
      // PUT 回带保存后的策略（后端可归一化）：回写草稿保持一致
      setDraft(JSON.parse(JSON.stringify(saved)) as UserToolPolicy);
      addToast(t('用户工具策略已保存'));
    } catch (err) {
      // 409 超限拒绝：后端文案原样展示（组数/工具数 caps）
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const draftAllowlist = draft.groups[selectedGroup]?.allowlist ?? [];
  const isAllowed = (name: string): boolean =>
    wildcard || draftAllowlist.includes(name);
  /** 可配置工具 = 已声明 user_exec 且非 HighRisk（D1/D2 封顶在目录层已挡，双保险显示禁用） */
  const configurable = (c: UserToolCatalogItem): boolean =>
    c.has_user_exec && c.risk !== 'high';

  if (loading) {
    return (
      <div className="page-container">
        <SkeletonTable columns={4} rows={6} />
      </div>
    );
  }

  if (degraded) {
    return (
      <div className="page-container">
        <Card title={t('用户工具权限')}>
          <EmptyState
            icon="🛡️"
            message={t('用户工具策略端点尚未就绪（后端建设中）。就绪后这里配置组级允许清单与全局开关。')}
          />
        </Card>
      </div>
    );
  }

  return (
    <div className="page-container">
      <Card
        title={t('用户工具权限')}
        actions={
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
            <RefreshCw size={14} /> {t('刷新')}
          </Button>
        }
      >
        <div className="ut-switch-row">
          <div className="ut-switch-text">
            <div className="ut-switch-title">
              <ShieldCheck size={15} />
              <span>{t('终端用户工具回路')}</span>
              <Switch checked={draft.enabled} onChange={(v) => setDraft((prev) => ({ ...prev, enabled: v }))} />
            </div>
            <div className="ut-hint ut-hint-ok">{t('开启后，组内被允许的工具才会出现在终端用户的聊天工具清单中；写操作需用户在聊天中逐次确认。')}</div>
            <div className="ut-hint">
              <AlertTriangle size={12} />
              <span>{t('关闭时终端用户纯聊天，行为与历史逐字节等价。默认关闭。')}</span>
            </div>
          </div>
        </div>

        <div className="ut-group-row">
          <label className="ut-group-label">{t('分组')}</label>
          <Select
            value={selectedGroup}
            onChange={(e) => selectGroup(e.target.value)}
            aria-label={t('选择分组')}
            className="ut-group-select"
          >
            <option value="">{t('选择分组…')}</option>
            {groups.map((g) => (
              <option key={g} value={g}>{g}</option>
            ))}
          </Select>
          {selectedGroup && wildcard && (
            <span className="ut-wild-badge">{t('通配：全部低危工具')}</span>
          )}
        </div>

        {selectedGroup ? (
          <>
            <div className="ut-wild-row">
              <span className="ut-wild-label">{t('允许全部低危工具（["*"] 通配）')}</span>
              <Switch checked={wildcard} onChange={toggleWildcard} />
            </div>
            <div className="ut-matrix">
              <div className="ut-matrix-head">
                <span>{t('工具')}</span>
                <span>{t('风险')}</span>
                <span>{t('允许')}</span>
              </div>
              {catalog.length === 0 && (
                <div className="ut-matrix-empty">{t('目录为空（后端 registry 未返回工具）')}</div>
              )}
              {catalog.map((c) => {
                const cfg = configurable(c);
                return (
                  <div key={c.name} className={`ut-matrix-row${cfg ? '' : ' disabled'}`}>
                    <span className="ut-tool-cell">
                      <span className="ut-tool-label" title={c.name}>{c.label}</span>
                      <code className="ut-tool-name">{c.name}</code>
                      {c.policy_hit === true && <span className="ut-hit">{t('当前命中')}</span>}
                      {c.risk === 'high' && <span className="ut-flag danger">{t('高危（永不暴露）')}</span>}
                      {!c.has_user_exec && <span className="ut-flag">{t('未声明用户执行体')}</span>}
                    </span>
                    <span className={riskBadgeClass(c.risk)}>{c.risk}</span>
                    <span className="ut-allow-cell">
                      <input
                        type="checkbox"
                        checked={cfg && isAllowed(c.name)}
                        disabled={!cfg || wildcard || saving}
                        onChange={(e) => toggleTool(c.name, e.target.checked)}
                        aria-label={`${t('允许')}：${c.label}`}
                      />
                    </span>
                  </div>
                );
              })}
            </div>
            <div className="ut-save-row">
              {error && <span className="ut-error" role="alert">{error}</span>}
              <Button onClick={() => void save()} disabled={saving}>
                {saving ? t('保存中…') : t('保存策略')}
              </Button>
            </div>
          </>
        ) : (
          <div className="ut-pick-hint">{t('选择一个分组后配置其允许清单。')}</div>
        )}
      </Card>
    </div>
  );
}
