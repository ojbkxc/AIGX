/**
 * GroupEditor —— 路由组面板（对齐 Agent2API v3.5.1 `routing-group-panel.tsx`
 * + `routing-member-card.tsx` + `routing-model-picker.tsx` 的卡片面）。
 *
 * 路由组 = 管理员配的**候选集**：一组 (提供商, 模型) 槽位，槽位里可以再点名账号。
 * 组的 `id` 是唯一查找键（后端 `RoutingProfiles::find_group`）。
 *
 * ⚠ 三条必须写在界面上、否则用户一定会配错的语义（依据后端实现）：
 *
 * 1. **`model` 是「限定」不是「继承」**：后端 `find_group(id, requested_model)`
 *    的判据是 `g.model.is_empty() || g.model == requested_model` ——
 *    留空 = 任何模型都能用这个组；写了模型名 = 只有请求名等于它的请求能用。
 *    （A2A 的「对外模型 ID」是必填入口名，语义不同，见交付清单。）
 * 2. **组的 strategy / sticky 是三态**（后端字段是 `Option`）：`null` = 继承
 *    上层（组不覆盖策略）；有值 = 组内显式覆盖。
 * 3. 组的 strategy / sticky **只在这里维护**（不在分层档案里再开一处）。
 *
 * 卡片结构（照 A2A）：组头（跨模型组合 / 组名 / 更多）→ 对外模型 ID →
 * 路由策略 + 会话粘滞 → 参与模型列（搜索 / 折叠全部 / 添加模型 / 全部移出 /
 * 成员卡 / 未参与折叠区 / 空态）。
 */
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronsDownUp, ChevronsUpDown, Copy, Plus } from 'lucide-react';
import Button from '../ui/Button';
import ConfirmDialog, { type ConfirmState } from '../ConfirmDialog';
import { useToast } from '../Toast';
import {
  ROUTING_CAPS,
  type RoutingGroup,
  type RoutingMember,
} from '../../types/routing-profile';
import type { RouteStrategy, StickyMode } from '../../types';
import { RoutingGroupDialog, RoutingMemberAccounts, RoutingMemberDialog } from './routing-group-dialogs';
import { RoutingGroupCreateDialog } from './routing-create-dialog';
import type { RouteModelChoice } from './routing-model-picker';
import { RoutingMemberCard } from './routing-member-card';
import { RoutingSelect } from './routing-select';
import { routeModelKey } from './routing-group-form';
import { RoutingDragOverlay, reorderRoute, useRoutingDrag, type RoutingDrop } from './routing-drag';
import { RoutingActions } from './routing-actions';
import './RoutingCards.css';

const STRATEGY_LABELS: Record<RouteStrategy, string> = {
  weighted: '加权随机',
  round_robin: '组内轮询',
  ranked: '多因子调度',
};

const STICKY_LABELS: Record<StickyMode, string> = {
  auto: '自动',
  session: '会话内',
  turn: '每轮',
  off: '关闭',
};

/** 新组 id：在既有 id 上取一个不冲突的默认值（可改） */
function nextGroupId(groups: RoutingGroup[]): string {
  const used = new Set(groups.map((g) => g.id));
  for (let i = 1; i < 10_000; i += 1) {
    const id = `group-${i}`;
    if (!used.has(id)) return id;
  }
  return `group-${used.size + 1}`;
}

export default function GroupEditor({
  groups,
  onChange,
  focusId = null,
  /** 模型目录候选（provider + model）；缺省 = 目录未接入，选择器展示空态 */
  catalog = [],
}: {
  groups: RoutingGroup[];
  onChange: (next: RoutingGroup[]) => void;
  /**
   * 左栏 rail 点中的组 id（对齐 A2A：点为 rail 条目，右侧只展开那一个组）。
   * null / '' = 不聚焦，全部展开（此时行为与旧版逐字节一致）。
   * 折叠只是显示层：groups 数组照样全量渲染项参与 patch，索引不会错位。
   */
  focusId?: string | null;
  catalog?: RouteModelChoice[];
}): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  /** 用户手动展开的非聚焦组（focusId 只决定「默认展开谁」，不是只读过滤） */
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  /** 搜索（对齐 A2A「搜索提供商或模型」）：只筛显示，不改数据 */
  const [search, setSearch] = useState('');
  /** 成员卡展开集合（键 = `${组 id}:${成员键}`） */
  const [opened, setOpened] = useState<Record<string, boolean>>({});
  /** 当前弹窗 */
  const [dialog, setDialog] = useState<{ kind: 'group' | 'add' | 'example' | 'create'; group: string } | null>(null);
  const [accountTarget, setAccountTarget] = useState<{ group: string; member: string } | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const keyword = search.trim().toLowerCase();
  const locked = false;

  const dupIds = new Set(groups.map((g) => g.id).filter((id, i, arr) => arr.indexOf(id) !== i));
  const overGroups = groups.length > ROUTING_CAPS.MAX_GROUPS;

  const patchGroup = (index: number, patch: Partial<RoutingGroup>): void =>
    onChange(groups.map((group, idx) => (idx === index ? { ...group, ...patch } : group)));

  const patchMember = (groupIndex: number, memberKey: string, mutate: (member: RoutingMember) => void): void => {
    const group = groups[groupIndex];
    patchGroup(groupIndex, {
      members: group.members.map((member) => {
        if (routeModelKey(member) !== memberKey) return member;
        const next = { ...member, accounts: [...member.accounts] };
        mutate(next);
        return next;
      }),
    });
  };

  const findGroupIndex = (id: string): number => groups.findIndex((group) => group.id === id);

  /** 成员落点：只有参与中的成员参与排序，其余（未参与）保持在后 */
  const applyDrop = (drop: RoutingDrop): void => {
    if (drop.kind === 'member') {
      const index = findGroupIndex(drop.provider);
      if (index < 0) return;
      const group = groups[index];
      const ids = group.members.filter((member) => member.included).map((member) => routeModelKey(member));
      reorderRoute(ids, drop.id, drop.target, drop.after);
      const byKey = new Map(group.members.map((member) => [routeModelKey(member), member]));
      const ordered = [
        ...ids.flatMap((key) => (byKey.has(key) ? [byKey.get(key) as RoutingMember] : [])),
        ...group.members.filter((member) => !member.included),
      ];
      patchGroup(index, { members: ordered });
      return;
    }
    // 账号落点：找到该成员，重排它的 accounts
    const groupIndex = groups.findIndex((group) =>
      group.members.some((member) => routeModelKey(member) === drop.provider));
    if (groupIndex < 0) return;
    patchMember(groupIndex, drop.provider, (member) => {
      reorderRoute(member.accounts, drop.id, drop.target, drop.after);
    });
  };

  const drag = useRoutingDrag(!locked, keyword, applyDrop);

  const moveMember = (groupIndex: number, memberKey: string, direction: number): void => {
    const group = groups[groupIndex];
    const peers = group.members.filter((member) => member.included);
    const at = peers.findIndex((member) => routeModelKey(member) === memberKey);
    const target = peers[at + direction];
    if (!target) return;
    applyDrop({
      kind: 'member',
      provider: group.id,
      id: memberKey,
      target: routeModelKey(target),
      after: direction > 0,
    });
  };

  const removeAll = (groupIndex: number): void => {
    setConfirmState({
      title: t('全部移出本路由'),
      message: t('只移出本路由，账号状态和其他路由不变；保存后此入口将没有可用路由。'),
      confirmText: t('全部移出'),
      danger: true,
      onConfirm: () => {
        patchGroup(groupIndex, {
          members: groups[groupIndex].members.map((member) => ({ ...member, included: false })),
        });
      },
    });
  };

  const copy = async (value: string): Promise<void> => {
    try {
      await navigator.clipboard.writeText(value);
      addToast(t('已复制'));
    } catch {
      addToast(t('复制失败，请手动选择并复制'), 'error');
    }
  };

  const snippetOf = (group: RoutingGroup): string => {
    const request = JSON.stringify(
      { model: group.model || '<请求模型>', messages: [{ role: 'user', content: 'Hello' }], stream: true },
      null,
      2,
    );
    return `POST /v1/chat/completions\nAuthorization: Bearer <YOUR_GATEWAY_KEY>\nContent-Type: application/json\n\n${request}`;
  };

  /** 「展开全部 / 折叠全部」的判据：所有可见成员卡都已展开 */
  const allExpanded = useMemo(() => {
    const keys = groups.flatMap((group) => group.members
      .filter((member) => !keyword || `${member.model} ${member.provider}`.toLowerCase().includes(keyword))
      .map((member) => `${group.id}:${routeModelKey(member)}`));
    return keys.length > 0 && keys.every((key) => opened[key]);
  }, [groups, keyword, opened]);

  /** 聚焦组默认展开；id 重复的组一律展开（那是要立刻看见的错） */
  const isCollapsed = (group: RoutingGroup, index: number): boolean =>
    Boolean(focusId) && group.id !== focusId && !dupIds.has(group.id) && !expanded.has(`${group.id}-${index}`);

  const dialogGroup = dialog ? groups[findGroupIndex(dialog.group)] : undefined;
  const accountMember = accountTarget
    ? groups[findGroupIndex(accountTarget.group)]?.members.find(
      (member) => routeModelKey(member) === accountTarget.member,
    )
    : undefined;
  const accountGroupIndex = accountTarget ? findGroupIndex(accountTarget.group) : -1;

  return (
    <div className="rt-group-panel">
      <div className="rt-notice rt-notice-info">
        {t('组的「路由策略 / 会话粘滞」是可继承的三态：留「继承上层」表示本组不改这两项，跟随全局/模型级；选了具体值才覆盖。')}
      </div>
      <div className="form-hint" style={{ marginTop: 6 }}>
        {t('组目前通过档案的「默认路由组」或请求显式指定命中；把组登记为可直接调用的模型（出现在 /v1/models 供客户端选择）依赖后端后续能力，本期未提供。')}
      </div>

      <div className="rt-group-toolbar">
        <input
          className="form-input"
          placeholder={t('搜索提供商或模型')}
          aria-label={t('搜索提供商或模型')}
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
        <Button
          variant="outline"
          size="sm"
          disabled={Boolean(keyword) || groups.length === 0}
          onClick={() => {
            if (allExpanded) {
              setOpened({});
              return;
            }
            const next: Record<string, boolean> = {};
            for (const group of groups) {
              for (const member of group.members) next[`${group.id}:${routeModelKey(member)}`] = true;
            }
            setOpened(next);
          }}
        >
          {allExpanded ? <ChevronsDownUp size={13} /> : <ChevronsUpDown size={13} />}
          {allExpanded ? t('折叠全部') : t('展开全部')}
        </Button>
        <span className={`rt-count${overGroups ? ' rt-count-over' : ''}`}>
          {t('{{n}} / 上限 {{cap}} 个组', { n: groups.length, cap: ROUTING_CAPS.MAX_GROUPS })}
        </span>
      </div>

      {groups.length === 0 ? (
        <div className="rt-config-note rt-state-note">
          {t('还没有路由组。没有组时请求只走 global 与模型级档案 —— 需要「某模型专走某批供应商/账号」时才建组。')}
        </div>
      ) : null}

      {groups.map((group, groupIndex) => {
        const members = group.members;
        const active = members.filter((member) => member.included);
        const visible = members.filter((member) => {
          if (!keyword) return true;
          if (member.included) {
            return `${member.model} ${member.provider}`.toLowerCase().includes(keyword)
              || member.accounts.some((id) => id.toLowerCase().includes(keyword));
          }
          return `${member.model} ${member.provider}`.toLowerCase().includes(keyword);
        });
        const activeVisible = visible.filter((member) => member.included);
        const excludedVisible = visible.filter((member) => !member.included);
        const overMembers = members.length > ROUTING_CAPS.MAX_MEMBERS;
        const dup = dupIds.has(group.id);
        if (isCollapsed(group, groupIndex)) {
          return (
            <div className="rt-config-card rt-group-collapsed" key={`${group.id}-${groupIndex}`}>
              <div className="rt-group-head">
                <div className="rt-group-title">
                  <span className="rt-group-id">{group.name || group.id || t('（未命名 id）')}</span>
                  <span className="rt-count">{t('{{n}} 个成员槽位', { n: members.length })}</span>
                </div>
                <button
                  type="button"
                  className="btn btn-secondary btn-sm"
                  onClick={() => setExpanded((prev) => new Set(prev).add(`${group.id}-${groupIndex}`))}
                >
                  {t('展开此组')}
                </button>
              </div>
            </div>
          );
        }
        return (
          <section className="rt-config-card" data-route-source key={`${group.id}-${groupIndex}`}>
            <header className="rt-config-head">
              <div>
                <span className="rt-config-eyebrow">{t('跨模型组合')}</span>
                <h2>{group.name || group.id || t('（未命名 id）')}</h2>
              </div>
              <RoutingActions
                label={t('路由配置操作')}
                actions={[
                  { label: t('编辑路由组'), disabled: locked, onClick: () => setDialog({ kind: 'group', group: group.id }) },
                  { label: t('调用示例'), disabled: locked, onClick: () => setDialog({ kind: 'example', group: group.id }) },
                  {
                    label: t('删除路由组'),
                    destructive: true,
                    disabled: locked,
                    onClick: () => onChange(groups.filter((_, idx) => idx !== groupIndex)),
                  },
                ]}
              />
            </header>
            {dup ? (
              <div className="rt-notice rt-notice-warn" style={{ margin: '0 16px 12px' }}>
                {t('有两处用了同一个组 id。后端按 id 查找，重复 id 会让命中的是哪一个组变得不确定 —— 请改成唯一 id。')}
              </div>
            ) : null}
            <div className="rt-config-alias">
              <span>{t('对外模型 ID')}</span>
              <code title={group.model}>{group.model || t('未限定（任何模型都能用此组）')}</code>
              <button
                type="button"
                className="rt-link-btn"
                title={t('复制')}
                aria-label={t('复制')}
                onClick={() => void copy(group.model)}
              >
                <Copy size={13} aria-hidden="true" />
              </button>
            </div>
            <div className="rt-config-rules">
              <div className="rt-rule-field">
                <label>{t('路由策略')}</label>
                <RoutingSelect
                  label={t('路由策略')}
                  value={group.strategy ?? ''}
                  disabled={locked}
                  options={[
                    ['', t('继承上层（不覆盖）')],
                    ...(Object.keys(STRATEGY_LABELS) as RouteStrategy[]).map(
                      (key): [string, string] => [key, t(STRATEGY_LABELS[key])],
                    ),
                  ]}
                  onChange={(value) => patchGroup(groupIndex, { strategy: (value || null) as RouteStrategy | null })}
                />
              </div>
              <div className="rt-rule-field">
                <label>{t('会话粘滞')}</label>
                <RoutingSelect
                  label={t('会话粘滞')}
                  value={group.sticky ?? ''}
                  disabled={locked}
                  options={[
                    ['', t('继承上层（不覆盖）')],
                    ...(Object.keys(STICKY_LABELS) as StickyMode[]).map(
                      (key): [string, string] => [key, t(STICKY_LABELS[key])],
                    ),
                  ]}
                  onChange={(value) => patchGroup(groupIndex, { sticky: (value || null) as StickyMode | null })}
                />
              </div>
            </div>
            <p className="rt-config-note rt-config-note-pad">
              {group.sticky === 'auto' || group.sticky === 'session'
                ? t('同一会话优先保持模型与账号，不可用时才顺延。')
                : t('每轮重新选路，不保留跨请求的账号绑定。')}
            </p>
            <button
              type="button"
              className="rt-link-btn rt-group-example"
              onClick={() => setDialog({ kind: 'example', group: group.id })}
            >
              {t('调用示例')}
            </button>

            <div className="rt-groups-column">
              <div className="rt-column-head">
                <b>{t('成员模型')}</b>
                <span className="rt-count">{t('{{n}} 项参与', { n: active.length })}</span>
                <div className="right">
                  <RoutingActions
                    label={t('路由参与操作')}
                    actions={[
                      { label: t('添加模型'), icon: <Plus size={14} />, disabled: locked, onClick: () => setDialog({ kind: 'add', group: group.id }) },
                      { label: t('全部移出'), disabled: locked || !active.length, onClick: () => removeAll(groupIndex) },
                    ]}
                  />
                </div>
              </div>
              <p className="rt-groups-hint">{t('拖动把手或按 Alt + 方向键调整顺序，更多操作在右侧菜单。')}</p>
              <div className="rt-config-listcol" data-route-scroll>
                <div className="rt-config-participants" data-route-list>
                  {activeVisible.map((member) => {
                    const memberKey = routeModelKey(member);
                    const openKey = `${group.id}:${memberKey}`;
                    const peers = active;
                    return (
                      <RoutingMemberCard
                        key={memberKey}
                        member={member}
                        label={member.provider}
                        name={member.model}
                        index={peers.findIndex((item) => routeModelKey(item) === memberKey)}
                        first={peers[0] === member}
                        last={peers[peers.length - 1] === member}
                        open={Boolean(keyword) || Boolean(opened[openKey])}
                        locked={locked}
                        query={keyword}
                        drag={drag}
                        accounts={[]}
                        onOpen={(next) => setOpened((current) => ({ ...current, [openKey]: next }))}
                        onJoin={(on) => patchMember(groupIndex, memberKey, (item) => { item.included = on; })}
                        onMove={(direction) => moveMember(groupIndex, memberKey, direction)}
                        onAccounts={() => setAccountTarget({ group: group.id, member: memberKey })}
                        onToggleAccount={(id) => patchMember(groupIndex, memberKey, (item) => {
                          item.accounts = item.accounts.includes(id)
                            ? item.accounts.filter((value) => value !== id)
                            : [...item.accounts, id];
                        })}
                        onMoveAccount={(id, direction) => patchMember(groupIndex, memberKey, (item) => {
                          const target = item.accounts[item.accounts.indexOf(id) + direction];
                          if (target) reorderRoute(item.accounts, id, target, direction > 0);
                        })}
                      />
                    );
                  })}
                </div>
                {!active.length ? (
                  <div className="rt-state-note rt-state-inherit">
                    <b>{t('还没有参与模型')}</b>
                    <span>{t('可以保存空路由，但请求会明确失败，不会回退全局。')}</span>
                  </div>
                ) : null}
                {excludedVisible.length ? (
                  <div className="rt-config-excluded">
                    <div className="rt-excluded-trigger">
                      {t('未参与本路由')} · {excludedVisible.length}
                    </div>
                    <div data-route-list>
                      {excludedVisible.map((member) => {
                        const memberKey = routeModelKey(member);
                        const openKey = `${group.id}:${memberKey}`;
                        return (
                          <RoutingMemberCard
                            key={memberKey}
                            member={member}
                            label={member.provider}
                            name={member.model}
                            index={-1}
                            first={false}
                            last={false}
                            open={Boolean(opened[openKey])}
                            locked={locked}
                            query={keyword}
                            drag={drag}
                            accounts={[]}
                            onOpen={(next) => setOpened((current) => ({ ...current, [openKey]: next }))}
                            onJoin={(on) => patchMember(groupIndex, memberKey, (item) => { item.included = on; })}
                            onMove={() => undefined}
                            onAccounts={() => setAccountTarget({ group: group.id, member: memberKey })}
                            onToggleAccount={(id) => patchMember(groupIndex, memberKey, (item) => {
                              item.accounts = item.accounts.includes(id)
                                ? item.accounts.filter((value) => value !== id)
                                : [...item.accounts, id];
                            })}
                            onMoveAccount={(id, direction) => patchMember(groupIndex, memberKey, (item) => {
                              const target = item.accounts[item.accounts.indexOf(id) + direction];
                              if (target) reorderRoute(item.accounts, id, target, direction > 0);
                            })}
                          />
                        );
                      })}
                    </div>
                  </div>
                ) : null}
                {keyword && !visible.length ? (
                  <div className="rt-state-note rt-state-inherit">{t('没有匹配的候选')}</div>
                ) : null}
                <div className="rt-group-actions">
                  <button
                    type="button"
                    className="btn btn-secondary btn-sm rt-add-member"
                    disabled={locked}
                    onClick={() => setDialog({ kind: 'add', group: group.id })}
                  >
                    <Plus size={13} /> {t('添加模型')}
                  </button>
                  <span className={`rt-count${overMembers ? ' rt-count-over' : ''}`}>
                    {t('{{n}} / 上限 {{cap}} 个成员', { n: members.length, cap: ROUTING_CAPS.MAX_MEMBERS })}
                  </span>
                </div>
              </div>
            </div>
          </section>
        );
      })}

      <div className="rt-group-toolbar">
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          onClick={() => setDialog({ kind: 'create', group: '' })}
        >
          <Plus size={13} /> {t('新增路由组')}
        </button>
      </div>

      <RoutingDragOverlay state={drag.state} />

      {dialog?.kind === 'create' ? (
        <RoutingGroupCreateDialog
          existingIds={groups.map((group) => group.id)}
          catalog={catalog}
          defaultId={nextGroupId(groups)}
          onSave={(created) => onChange([...groups, created])}
          onClose={() => setDialog(null)}
        />
      ) : null}

      {dialog?.kind === 'group' && dialogGroup ? (
        <RoutingGroupDialog
          fields={{ id: dialogGroup.id, name: dialogGroup.name, model: dialogGroup.model }}
          otherIds={groups.filter((group) => group.id !== dialogGroup.id).map((group) => group.id)}
          onSave={(fields) => patchGroup(findGroupIndex(dialogGroup.id), fields)}
          onClose={() => setDialog(null)}
        />
      ) : null}
      {dialog?.kind === 'add' && dialogGroup ? (
        <RoutingMemberDialog
          choices={catalog.filter(
            (choice) => !dialogGroup.members.some(
              (member) => member.provider === choice.provider && member.model.toLowerCase() === choice.model.toLowerCase(),
            ),
          )}
          members={dialogGroup.members}
          onAdd={(added) => patchGroup(findGroupIndex(dialogGroup.id), { members: [...dialogGroup.members, ...added] })}
          onClose={() => setDialog(null)}
        />
      ) : null}
      {dialog?.kind === 'example' && dialogGroup ? (
        <div
          className="modal-overlay"
          role="dialog"
          aria-modal="true"
          aria-label={t('调用示例')}
          onClick={(event) => {
            if (event.target === event.currentTarget) setDialog(null);
          }}
        >
          <div className="modal" style={{ maxWidth: 620, width: '92%' }} onClick={(event) => event.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('调用示例')}</h3>
              <button type="button" className="modal-close" aria-label={t('关闭')} onClick={() => setDialog(null)}>×</button>
            </div>
            <div className="modal-body">
              <p className="rt-config-note">
                {t('保存路由后，在客户端填写此模型 ID。密钥使用你自己的网关 Key。')}
              </p>
              <pre className="rt-group-code">{snippetOf(dialogGroup)}</pre>
            </div>
            <div className="modal-footer">
              <button type="button" className="btn btn-outline" onClick={() => setDialog(null)}>{t('关闭')}</button>
              <button type="button" className="btn btn-primary" onClick={() => void copy(snippetOf(dialogGroup))}>
                {t('复制')}
              </button>
            </div>
          </div>
        </div>
      ) : null}
      {accountMember && accountGroupIndex >= 0 ? (
        <RoutingMemberAccounts
          member={accountMember}
          onSave={(ids) => patchMember(accountGroupIndex, routeModelKey(accountMember), (item) => { item.accounts = ids; })}
          onClose={() => setAccountTarget(null)}
        />
      ) : null}
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}