/**
 * 路由（#40 多规则体系）—— 路由配置独立成页。
 *
 * 对照 Agent2API：路由域是**单页多模式**（配置 / 回放 / 粘滞），而不是塞在
 * 「系统设置」的某个 tab 里 —— 因为「我改了 global 为什么没生效」这类问题
 * 需要配置、来源、预演三样东西在同一屏上下文里才能自查。
 *
 * 页面结构：
 *   ① 多规则总开关 —— 关闭时档案**全部短路**，行为与旧版逐字节一致
 *   ② 基础策略     —— 复用既有 RoutingPanel（策略/粘性/断路器/权重）
 *   ③ 分层档案     —— global 一层 + 按模型名一层（三态逐字段覆盖）+ 路由组
 *   ④ 预览与来源   —— 候选（走数据面同一套筛选）+ 命中层（appliedLayers）
 *
 * ── 两条硬约束（别在实现里破坏）──────────────────────────────
 * 1. **不在这里算生效结果**：候选裁剪与命中层一律取后端返回。本仓 #31 的
 *    事故就是「界面自己投影了一套形状，与真实转发不一致」。
 * 2. **provenance 不许猜**：后端没给来源时显示「来源未知」，绝不按推算值
 *    冒充权威来源 —— 否则用户拿它排障，排出来的是假的。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Copy, Loader2, Plus, RefreshCw, Trash2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, EmptyState, Loading, Switch, Tabs } from '../components/ui';
import RouteReplayPanel from '../components/RouteReplay';
import RouteSticky from '../components/RouteSticky';
import RoutingPanel from '../components/RoutingPanel';
import ProfileForm, { SORT_ON_LABELS } from '../components/routing/ProfileForm';
import GroupEditor from '../components/routing/GroupEditor';
import { LayerChip } from '../components/routing/TriStateField';
import {
  EMPTY_NEEDS,
  EMPTY_SCOPE,
  PROFILE_FIELDS,
  ROUTING_CAPS,
  emptyProfile,
  isProfileEmpty,
  mergeProfiles,
  profileOverrideCount,
  type Layer,
  type ProfileField,
  type RequestNeeds,
  type RoutingGroup,
  type RoutingPolicyWithProfiles,
  type RoutingPreviewItem,
  type RoutingProfile,
  type RoutingProfiles,
  type RoutingQueueWithLayers,
} from '../types/routing-profile';
import type { RoutingQueueEntryItem, RoutingQueueItem } from '../types';
import './Routing.css';

type RoutingTab = 'config' | 'replay' | 'sticky';

/** 层栏选中项：`global` / `m:<模型名>` / `groups` */
type LayerSel = 'global' | 'groups' | `m:${string}`;

const STATUS_HINT: Record<string, string> = {
  open: '断路器打开',
  halfopen: '半开试探',
  closed: '正常',
};

/** 预览请求体的能力需求初值 */
const NEEDS_FIELDS: Array<[keyof RequestNeeds, string]> = [
  ['tools', '工具调用'],
  ['images', '图片理解'],
  ['video', '视频'],
];

/** 空档案容器（后端未返回 profiles 时的本地草稿起点） */
function emptyProfiles(): RoutingProfiles {
  return { global: emptyProfile(), models: {}, groups: [] };
}

function statusOf(err: unknown): number | undefined {
  return (err as { status?: number } | null)?.status;
}

export default function Routing(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [tab, setTab] = useState<RoutingTab>('config');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [policy, setPolicy] = useState<RoutingPolicyWithProfiles | null>(null);
  /** 后端未返回 profiles（#40 未就绪的旧版本） */
  const [noProfiles, setNoProfiles] = useState(false);

  const [enabledBusy, setEnabledBusy] = useState(false);

  // 档案草稿（本页编辑，未落库）
  const [profiles, setProfiles] = useState<RoutingProfiles | null>(null);
  const [savedProfiles, setSavedProfiles] = useState<RoutingProfiles | null>(null);
  const [savingProfiles, setSavingProfiles] = useState(false);
  /** 档案保存端点是否就绪（首次 404/405 后置 false，避免反复试错） */
  const [profilesEndpointReady, setProfilesEndpointReady] = useState(true);
  const [showJson, setShowJson] = useState(false);

  // 层选择
  const [layerSel, setLayerSel] = useState<LayerSel>('global');
  const [newModel, setNewModel] = useState('');

  // 预览：候选与命中层（走既有 /api/routing/queue，今日即可用）
  const [queueModel, setQueueModel] = useState('');
  const [queue, setQueue] = useState<(RoutingQueueItem & RoutingQueueWithLayers) | null>(null);
  const [queueLoading, setQueueLoading] = useState(false);

  // 预览：能力需求 / 指定组（走 #40 新增 POST /api/routing/preview）
  const [pvModel, setPvModel] = useState('');
  const [pvNeeds, setPvNeeds] = useState<RequestNeeds>({ ...EMPTY_NEEDS });
  const [pvGroup, setPvGroup] = useState('');
  const [pv, setPv] = useState<RoutingPreviewItem | null>(null);
  const [pvLoading, setPvLoading] = useState(false);
  /** 预览端点未就绪（404/405）—— 不谎报，直接告知 */
  const [pvNotReady, setPvNotReady] = useState(false);

  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const applyPolicy = useCallback((p: RoutingPolicyWithProfiles): void => {
    setPolicy(p);
    if (p.profiles) {
      setProfiles(p.profiles);
      setSavedProfiles(p.profiles);
      setNoProfiles(false);
    } else {
      // 后端没返回 profiles（#40 未就绪的旧版本）：给一份空草稿，让编辑器
      // 仍可用（改的是本地态），并明确告知服务端读写不了档案。
      const blank = emptyProfiles();
      setProfiles(blank);
      setSavedProfiles(blank);
      setNoProfiles(true);
    }
  }, []);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getRoutingPolicy();
      if (!mounted.current) return;
      if (res?.data) applyPolicy(res.data);
    } catch (err) {
      if (!mounted.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, [applyPolicy]);

  useEffect(() => {
    void load();
  }, [load]);

  /** 总开关：PUT /api/routing 是读-改-写，只传 enabled 不碰其他字段 */
  const toggleEnabled = async (next: boolean): Promise<void> => {
    setEnabledBusy(true);
    try {
      const res = await api.saveRoutingEnabled(next);
      if (res?.data) applyPolicy(res.data as RoutingPolicyWithProfiles);
      addToast(
        next
          ? t('多规则已启用：下一请求即按档案选路')
          : t('多规则已关闭：档案全部短路，行为与旧版一致'),
      );
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      if (mounted.current) setEnabledBusy(false);
    }
  };

  const dirty = useMemo(
    () => JSON.stringify(profiles) !== JSON.stringify(savedProfiles),
    [profiles, savedProfiles],
  );

  const groupIds = useMemo(() => (profiles?.groups ?? []).map((g) => g.id), [profiles]);

  /**
   * 「内置默认」档案：值是后端 `resolve_profile` 的**基座**。
   * strategy / sticky 取自基础策略现值（后端基座就是 policy 的值，不是枚举默认），
   * 其余五项是结构默认（无名单 / 无需求 / 无排序 / 无默认组）。
   * 仅用于「继承会得到什么」的提示，不是判定口径。
   */
  const baseProfile: RoutingProfile = useMemo(
    () => ({
      scope: { ...EMPTY_SCOPE },
      strategy: policy?.strategy ?? 'weighted',
      sticky: policy?.sticky ?? 'auto',
      needs: { ...EMPTY_NEEDS },
      sort_rules: [],
      default_group: null,
    }),
    [policy?.strategy, policy?.sticky],
  );

  const modelNames = useMemo(
    () => Object.keys(profiles?.models ?? {}).sort((a, b) => a.localeCompare(b)),
    [profiles],
  );

  const modelKey = layerSel.startsWith('m:') ? layerSel.slice(2) : null;

  /** 当前选中层要编辑的档案 */
  const currentProfile: RoutingProfile | null = useMemo(() => {
    if (!profiles) return null;
    if (layerSel === 'global') return profiles.global;
    if (modelKey != null) return profiles.models[modelKey] ?? emptyProfile();
    return null;
  }, [profiles, layerSel, modelKey]);

  /** 下层合并结果：全局层之下是内置默认；模型级之下是「内置默认 + global」 */
  const inheritedProfile: RoutingProfile = useMemo(() => {
    if (layerSel === 'global' || !profiles) return baseProfile;
    return mergeProfiles(baseProfile, profiles.global);
  }, [layerSel, profiles, baseProfile]);

  const patchGroupList = (next: RoutingGroup[]): void => {
    if (!profiles) return;
    setProfiles({ ...profiles, groups: next });
  };

  const setModelProfile = (name: string, next: RoutingProfile): void => {
    if (!profiles) return;
    setProfiles({ ...profiles, models: { ...profiles.models, [name]: next } });
  };

  const addModel = (): void => {
    const name = newModel.trim();
    if (!profiles || !name) return;
    if (profiles.models[name]) {
      addToast(t('该模型名已存在档案'), 'error');
      return;
    }
    if (modelNames.length >= ROUTING_CAPS.MAX_MODELS) {
      addToast(t('模型档案数已达上限 {{cap}}', { cap: ROUTING_CAPS.MAX_MODELS }), 'error');
      return;
    }
    setProfiles({ ...profiles, models: { ...profiles.models, [name]: emptyProfile() } });
    setNewModel('');
    setLayerSel(`m:${name}`);
  };

  const removeModel = (name: string): void => {
    if (!profiles) return;
    const next = { ...profiles.models };
    delete next[name];
    setProfiles({ ...profiles, models: next });
    setLayerSel('global');
  };

  const saveProfiles = async (): Promise<void> => {
    if (!profiles) return;
    setSavingProfiles(true);
    try {
      await api.saveRoutingProfiles(profiles);
      setSavedProfiles(profiles);
      setProfilesEndpointReady(true);
      addToast(t('路由档案已保存（下一请求即生效）'));
      void load();
    } catch (err) {
      const st = statusOf(err);
      // 404 = 端点未上线；405 = 方法未允许。都属「后端 #40 第 2 步未就绪」，
      // 明确告知而非谎报成功 —— 其余错误照常显示后端文案。
      if (st === 404 || st === 405) {
        setProfilesEndpointReady(false);
        addToast(t('档案保存端点尚未就绪（后端 #40 第 2 步）：本次修改只在本页，未写入服务端'), 'error');
      } else {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      }
    } finally {
      if (mounted.current) setSavingProfiles(false);
    }
  };

  const copyJson = async (): Promise<void> => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(profiles, null, 2));
      addToast(t('档案 JSON 已复制到剪贴板'));
    } catch {
      addToast(t('复制失败：浏览器未授权剪贴板'), 'error');
    }
  };

  /** 候选与命中层：复用网页版预演端点（数据面同一序列，不自己算） */
  const loadQueue = async (): Promise<void> => {
    const m = queueModel.trim();
    if (!m) return;
    setQueueLoading(true);
    try {
      const res = await api.getRoutingQueue(m);
      setQueue(res?.data ?? null);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      if (mounted.current) setQueueLoading(false);
    }
  };

  /** 能力需求 / 指定组预演：POST /api/routing/preview（后端 #40 第 2 步） */
  const runPreview = async (): Promise<void> => {
    const m = pvModel.trim();
    if (!m) return;
    setPvLoading(true);
    try {
      const res = await api.previewRouting({
        model: m,
        needs: pvNeeds,
        group_id: pvGroup.trim() || null,
      });
      setPv(res?.data ?? null);
      setPvNotReady(false);
    } catch (err) {
      const st = statusOf(err);
      if (st === 404 || st === 405) {
        setPvNotReady(true);
        setPv(null);
      } else {
        addToast(err instanceof Error ? err.message : String(err), 'error');
      }
    } finally {
      if (mounted.current) setPvLoading(false);
    }
  };

  /** 生效值 → 人话（provenance 表用） */
  const fmtValue = (p: RoutingProfile, f: ProfileField): string => {
    switch (f) {
      case 'strategy':
        return p.strategy ?? '—';
      case 'sticky':
        return p.sticky ?? '—';
      case 'scope': {
        if (!p.scope) return '—';
        const s = p.scope;
        if (s.provider_scope === 'listed') {
          return t('仅列出的 {{n}} 个提供商', { n: s.providers.length });
        }
        const ex = s.disabled_providers.length + s.excluded_accounts.length;
        return ex ? t('全部提供商（排除 {{n}} 项）', { n: ex }) : t('全部提供商');
      }
      case 'needs': {
        if (!p.needs) return '—';
        const on = NEEDS_FIELDS.filter(([k]) => p.needs?.[k]).map(([, label]) => t(label));
        return on.length ? on.join('、') : t('无要求');
      }
      case 'sort_rules':
        return p.sort_rules?.length
          ? p.sort_rules
              .map((r) => `${t(SORT_ON_LABELS[r.on])} ${r.dir === 'asc' ? t('升序') : t('降序')}`)
              .join(' → ')
          : t('（无）');
      case 'default_group':
        return p.default_group || t('（无）');
    }
  };

  const FIELD_LABELS: Record<ProfileField, string> = {
    scope: t('参与名单'),
    strategy: t('选路策略'),
    sticky: t('会话粘性'),
    needs: t('能力需求'),
    sort_rules: t('账号排序规则'),
    default_group: t('默认路由组'),
  };

  if (loading) return <Loading text={t('读取路由配置')} />;

  const enabled = policy?.enabled === true;

  const renderCandidate = (c: RoutingQueueEntryItem, i: number): JSX.Element => (
    <li key={c.id}>
      {i === 0 ? <strong>{`${i + 1}. ${c.name}${t('（队首）')}`}</strong> : `${i + 1}. ${c.name}`}
      <span className="rt-muted form-hint">
        {` · ${c.provider} · ${t(STATUS_HINT[c.state] ?? c.state)}`}
      </span>
    </li>
  );

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('路由')}</h1>
          <p>
            {t('供应商选路的策略、分层档案（全局 / 按模型 / 路由组）与预演；保存即热生效，无需重启。')}
          </p>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <Button variant="outline" onClick={() => void load()} disabled={loading}>
            <RefreshCw size={13} /> {t('刷新')}
          </Button>
        </div>
      </div>

      <Tabs<RoutingTab>
        items={[
          { key: 'config', label: t('配置') },
          { key: 'replay', label: t('回放') },
          { key: 'sticky', label: t('粘滞会话') },
        ]}
        active={tab}
        onChange={setTab}
        ariaLabel={t('路由分区')}
      />

      {error && <div className="error-message">{error}</div>}

      {tab === 'replay' ? (
        <div style={{ marginTop: 12 }}>
          <RouteReplayPanel />
        </div>
      ) : tab === 'sticky' ? (
        <div style={{ marginTop: 12 }}>
          <RouteSticky />
        </div>
      ) : (
        <div style={{ marginTop: 12 }}>
          {/* ① 总开关 */}
          <Card
            title={t('多规则路由')}
            actions={
              <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <Badge tone={enabled ? 'success' : 'neutral'}>
                  {enabled ? t('已启用') : t('未启用')}
                </Badge>
                <Switch
                  checked={enabled}
                  disabled={enabledBusy}
                  ariaLabel={t('多规则路由总开关')}
                  onChange={(v) => void toggleEnabled(v)}
                />
              </div>
            }
          >
            {enabled ? (
              <div className="rt-notice rt-notice-info">
                {t('多规则已启用：请求先按「全局 → 模型级 → 路由组」逐字段合并出生效档案，再按它裁剪候选。')}
              </div>
            ) : (
              <div className="rt-notice rt-notice-warn">
                {t('多规则未启用：行为与旧版完全一致 —— 下面所有档案都被忽略（这是可验证的等价，不是「大概一样」）。档案可以照常编辑，启用后才生效。')}
              </div>
            )}
            {noProfiles ? (
              <div className="rt-notice rt-notice-warn" style={{ marginTop: 8 }}>
                {t('当前后端未返回 profiles 字段（#40 未就绪）：本页只能编辑本地草稿，无法读写服务端档案。')}
              </div>
            ) : null}
          </Card>

          {/* ② 基础策略（复用既有 RoutingPanel：策略 / 粘性 / 断路器 / 权重） */}
          <div style={{ marginTop: 12 }}>
            <RoutingPanel />
          </div>

          {/* ③ 分层档案 */}
          <div style={{ marginTop: 12 }}>
            <Card
              title={t('分层档案')}
              actions={
                dirty ? <span className="rt-dirty">{t('有未保存的修改')}</span> : undefined
              }
            >
              <div className="rt-notice">
                {t('合并口径（逐字段三态）：自左向右取最右侧「不是继承」的那一层的值。全局 → 模型级 → 路由组，后面的层可以只覆盖其中几个字段，其余继续继承。')}
              </div>

              <div className="rt-layout" style={{ marginTop: 12 }}>
                {/* 左栏：选层 */}
                <div className="rt-rail">
                  <div className="rt-rail-title">{t('层')}</div>
                  <button
                    type="button"
                    className={`rt-rail-item${layerSel === 'global' ? ' is-on' : ''}`}
                    onClick={() => setLayerSel('global')}
                  >
                    <span className="rt-rail-name">{t('全局（所有模型）')}</span>
                    {profiles ? (
                      <span className="rt-count">{profileOverrideCount(profiles.global)}</span>
                    ) : null}
                  </button>

                  <div className="rt-rail-title" style={{ marginTop: 8 }}>
                    {t('模型级（{{n}}）', { n: modelNames.length })}
                  </div>
                  {modelNames.length === 0 ? (
                    <div className="rt-count" style={{ padding: '0 6px 4px' }}>
                      {t('还没有模型级档案')}
                    </div>
                  ) : null}
                  {modelNames.map((name) => (
                    <button
                      key={name}
                      type="button"
                      className={`rt-rail-item${layerSel === `m:${name}` ? ' is-on' : ''}`}
                      title={name}
                      onClick={() => setLayerSel(`m:${name}`)}
                    >
                      <span className="rt-rail-name">{name}</span>
                      <span className="rt-count">
                        {profileOverrideCount(profiles?.models[name] ?? emptyProfile())}
                      </span>
                    </button>
                  ))}
                  <div className="rt-rail-add">
                    <input
                      className="form-input"
                      placeholder={t('新增模型档案名')}
                      value={newModel}
                      onChange={(e) => setNewModel(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') addModel();
                      }}
                    />
                    <Button variant="outline" size="sm" onClick={addModel} disabled={!newModel.trim()}>
                      <Plus size={13} />
                    </Button>
                  </div>

                  <button
                    type="button"
                    className={`rt-rail-item${layerSel === 'groups' ? ' is-on' : ''}`}
                    style={{ marginTop: 8 }}
                    onClick={() => setLayerSel('groups')}
                  >
                    <span className="rt-rail-name">{t('路由组')}</span>
                    <span className="rt-count">{profiles?.groups.length ?? 0}</span>
                  </button>
                </div>

                {/* 右栏：编辑选中层 */}
                <div>
                  {layerSel === 'groups' ? (
                    profiles ? (
                      <GroupEditor groups={profiles.groups} onChange={patchGroupList} />
                    ) : null
                  ) : currentProfile && profiles ? (
                    <>
                      {layerSel === 'global' ? (
                        <div className="rt-notice">
                          {t('全局层是所有模型的兜底：这里配的值会被模型级/路由组按字段覆盖。')}
                        </div>
                      ) : (
                        <div className="rt-notice rt-notice-info">
                          {t('模型名请填客户端的请求名 / 别名（不是上游真名）。后端先按请求名精确匹配，未命中再用全局映射名兜底 —— 用别名暴露、上游真名不同的场景下，两个名字都可以建档案。')}
                          {modelKey ? (
                            <div style={{ marginTop: 6 }}>
                              <Button
                                variant="outline"
                                size="sm"
                                onClick={() => modelKey && removeModel(modelKey)}
                              >
                                <Trash2 size={13} /> {t('删除「{{name}}」的档案', { name: modelKey })}
                              </Button>
                            </div>
                          ) : null}
                        </div>
                      )}

                      <div style={{ marginTop: 10 }}>
                        <ProfileForm
                          value={currentProfile}
                          onChange={(next) => {
                            if (layerSel === 'global') {
                              setProfiles({ ...profiles, global: next });
                            } else if (modelKey != null) {
                              setModelProfile(modelKey, next);
                            }
                          }}
                          inherited={inheritedProfile}
                          inheritSource={layerSel === 'global' ? t('内置默认') : t('全局')}
                          groupIds={groupIds}
                          /* 逐字段来源只由后端预览端点提供：未跑预演 → 不显示徽章；
                             跑过则显示真实来源（不再显示「来源未知」占位） */
                          provenance={pv?.provenance}
                        />
                      </div>

                      {isProfileEmpty(currentProfile) ? (
                        <div className="rt-state-note rt-state-inherit" style={{ marginTop: 10 }}>
                          {t('本层全部字段都是「继承」：这一层等价于不存在（不产生任何覆盖）。')}
                        </div>
                      ) : null}
                    </>
                  ) : null}

                  {/* 保存条 */}
                  <div className="rt-savebar">
                    <Button
                      variant="primary"
                      onClick={() => void saveProfiles()}
                      disabled={!dirty || savingProfiles || !profiles}
                    >
                      {savingProfiles ? <Loader2 size={13} className="spin" /> : null}
                      {t('保存档案')}
                    </Button>
                    <Button
                      variant="outline"
                      onClick={() => {
                        setProfiles(savedProfiles);
                        addToast(t('已放弃未保存的修改'));
                      }}
                      disabled={!dirty}
                    >
                      {t('放弃更改')}
                    </Button>
                    <Button variant="outline" onClick={() => void copyJson()} disabled={!profiles}>
                      <Copy size={13} /> {t('复制 JSON')}
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => setShowJson(!showJson)}>
                      {showJson ? t('收起 JSON') : t('查看 JSON')}
                    </Button>
                    {!profilesEndpointReady ? (
                      <span className="rt-dirty">
                        {t('保存端点未就绪：修改只在本页，刷新即丢 —— 可先「复制 JSON」留档')}
                      </span>
                    ) : null}
                  </div>

                  {showJson && profiles ? (
                    <textarea
                      className="form-input rt-json"
                      style={{ marginTop: 10 }}
                      readOnly
                      value={JSON.stringify(profiles, null, 2)}
                    />
                  ) : null}
                </div>
              </div>
            </Card>
          </div>

          {/* ④ 预览与来源 */}
          <div style={{ marginTop: 12 }}>
            <Card title={t('候选与命中层')}>
              <div className="rt-notice">
                {t('预演与真实转发走同一套判定，所以这里的结果就是下一个请求会走的路 —— 界面不会自己再算一遍候选。')}
              </div>
              <div style={{ display: 'flex', gap: 8, marginTop: 10, flexWrap: 'wrap' }}>
                <input
                  className="form-input"
                  style={{ maxWidth: 320 }}
                  placeholder={t('输入模型名（请求名），如 gpt-4o')}
                  value={queueModel}
                  onChange={(e) => setQueueModel(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void loadQueue();
                  }}
                />
                <Button variant="outline" onClick={() => void loadQueue()} disabled={queueLoading || !queueModel.trim()}>
                  {queueLoading ? <Loader2 size={13} className="spin" /> : null}
                  {t('预演')}
                </Button>
              </div>

              {queue ? (
                <div style={{ marginTop: 12 }}>
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                    <span className="rt-muted">{t('命中层：')}</span>
                    {queue.appliedLayers?.length ? (
                      queue.appliedLayers.map((l: Layer) => <LayerChip key={l} layer={l} size="md" />)
                    ) : (
                      <span className="rt-muted">{t('（未命中任何档案层，只走内置默认）')}</span>
                    )}
                    <span className="rt-muted" style={{ marginLeft: 8 }}>
                      {t('生效策略：')}
                      {queue.strategy}
                      {` · `}
                      {t('粘性：')}
                      {queue.sticky}
                    </span>
                    {queue.skipped &&
                    (queue.skipped.provider_not_allowed || queue.skipped.provider_disabled) ? (
                      <span className="rt-muted">
                        {t('被档案裁掉：提供商不在名单 {{a}} 个 · 提供商被排除 {{b}} 个', {
                          a: queue.skipped.provider_not_allowed ?? 0,
                          b: queue.skipped.provider_disabled ?? 0,
                        })}
                      </span>
                    ) : null}
                  </div>

                  {queue.ordered?.length ? (
                    <ul style={{ margin: '8px 0', paddingLeft: 18 }}>
                      {queue.ordered.map(renderCandidate)}
                    </ul>
                  ) : (
                    <EmptyState message={t('没有可用候选')} />
                  )}

                  {queue.blocked && queue.blocked.length > 0 ? (
                    <>
                      <div className="form-hint">{t('被挡在门外：')}</div>
                      <ul style={{ margin: '4px 0', paddingLeft: 18 }}>
                        {queue.blocked.map((c) => (
                          <li key={c.id}>{`${c.name} — ${c.reason ?? ''}`}</li>
                        ))}
                      </ul>
                    </>
                  ) : null}

                  {queue.strategy === 'round_robin' ? (
                    <div className="form-hint">
                      {t('组内轮询下实际首挑由游标决定，预演以静态排序为准。')}
                    </div>
                  ) : null}
                </div>
              ) : null}
            </Card>
          </div>

          <div style={{ marginTop: 12 }}>
            <Card title={t('能力需求 / 指定组预演与逐字段来源')}>
              <div className="rt-notice">
                {t('按请求的能力需求（工具/图片/视频）与指定路由组预演，并列出每个字段的取值来自哪一层 —— 「改了全局为什么没生效」看这张表即可自查。')}
              </div>

              <div style={{ display: 'flex', gap: 8, marginTop: 10, flexWrap: 'wrap', alignItems: 'center' }}>
                <input
                  className="form-input"
                  style={{ maxWidth: 280 }}
                  placeholder={t('模型名（请求名）')}
                  value={pvModel}
                  onChange={(e) => setPvModel(e.target.value)}
                />
                {NEEDS_FIELDS.map(([k, label]) => (
                  <label key={k} style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 12.5 }}>
                    <input
                      type="checkbox"
                      checked={pvNeeds[k]}
                      onChange={(e) => setPvNeeds({ ...pvNeeds, [k]: e.target.checked })}
                    />
                    <span>{t(label)}</span>
                  </label>
                ))}
                <input
                  className="form-input"
                  style={{ maxWidth: 200 }}
                  list="rt-group-ids-preview"
                  placeholder={t('指定组 id（可选）')}
                  value={pvGroup}
                  onChange={(e) => setPvGroup(e.target.value)}
                />
                <datalist id="rt-group-ids-preview">
                  {groupIds.map((g) => (
                    <option key={g} value={g} />
                  ))}
                </datalist>
                <Button variant="outline" onClick={() => void runPreview()} disabled={pvLoading || !pvModel.trim()}>
                  {pvLoading ? <Loader2 size={13} className="spin" /> : null}
                  {t('预演')}
                </Button>
              </div>

              {pvNotReady ? (
                <div className="rt-notice rt-notice-warn" style={{ marginTop: 10 }}>
                  {t('预演端点尚未就绪（后端 #40 第 2 步）。就绪后这里会显示：命中层、逐字段来源、候选与被裁原因 —— 全部取自后端的同一次判定，界面不复算。上方「候选与命中层」已可看到候选与命中层。')}
                </div>
              ) : pv ? (
                <div style={{ marginTop: 12 }}>
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
                    <span className="rt-muted">{t('命中层：')}</span>
                    {pv.appliedLayers?.length ? (
                      pv.appliedLayers.map((l: Layer) => <LayerChip key={l} layer={l} size="md" />)
                    ) : (
                      <span className="rt-muted">{t('（未命中任何档案层，只走内置默认）')}</span>
                    )}
                    {pv.groupId ? (
                      <Badge tone="info">{t('生效组：{{id}}', { id: pv.groupId })}</Badge>
                    ) : null}
                  </div>

                  {pv.profile ? (
                    <div className="rt-table-wrap" style={{ marginTop: 10 }}>
                      <table className="rt-table">
                        <thead>
                          <tr>
                            <th>{t('字段')}</th>
                            <th>{t('生效值')}</th>
                            <th>{t('来源层')}</th>
                          </tr>
                        </thead>
                        <tbody>
                          {PROFILE_FIELDS.map((f) => (
                            <tr key={f}>
                              <td>{FIELD_LABELS[f]}</td>
                              <td>{fmtValue(pv.profile as RoutingProfile, f)}</td>
                              <td>
                                <LayerChip
                                  layer={
                                    pv.provenance
                                      ? pv.provenance[f]
                                      : undefined /* 后端未给来源 → 显示「来源未知」而非推测 */
                                  }
                                />
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  ) : null}

                  {pv.ordered?.length ? (
                    <ul style={{ margin: '10px 0 0', paddingLeft: 18 }}>
                      {pv.ordered.map(renderCandidate)}
                    </ul>
                  ) : null}
                </div>
              ) : null}
            </Card>
          </div>
        </div>
      )}
    </div>
  );
}
