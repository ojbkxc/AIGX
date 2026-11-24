/**
 * GroupEditor —— 路由组编辑器（#40 契约 §1 `RoutingGroup`）。
 *
 * 路由组 = 管理员配的**候选集**：一组 (提供商, 模型) 槽位，槽位里可以再点名账号。
 * 组的 `id` 是唯一查找键（后端 `RoutingProfiles::find_group`）。
 *
 * ⚠ 三条必须写在界面上、否则用户一定会配错的语义（依据后端实现）：
 *
 * 1. **`model` 是「限定」不是「继承」**：后端 `find_group(id, requested_model)`
 *    的判据是 `g.model.is_empty() || g.model == requested_model` ——
 *    留空 = 任何模型都能用这个组；写了模型名 = 只有请求名等于它的请求能用。
 * 2. **组的 strategy / sticky 是三态**（后端字段是 `Option`）：`null` = 继承
 *    上层（组不覆盖策略）；有值 = 组内显式覆盖。早先「组总会覆盖上层」的
 *    口径已随后端改成 `Option` 而废弃 —— 本组件必须给「继承」档，否则用户
 *    会误以为「组没配策略 = 强制 weighted/auto」。
 * 3. 组的 strategy / sticky **只在这里维护**（不在分层档案里再开一处，
 *    避免同一字段两处可改 —— 本仓既有约定）。
 */
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Plus, Trash2 } from 'lucide-react';
import { Button } from '../ui';
import {
  ROUTING_CAPS,
  type RoutingGroup,
  type RoutingMember,
} from '../../types/routing-profile';
import type { RouteStrategy, StickyMode } from '../../types';

const STRATEGY_LABELS: Record<RouteStrategy, string> = {
  weighted: '加权随机',
  round_robin: '组内轮询',
  ranked: '多因子调度',
};

const STICKY_LABELS: Record<StickyMode, string> = {
  auto: '自动',
  session: '长会话',
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

/** 成员账号输入：逗号/空格分隔（比多行文本更适合表格行内） */
function parseAccounts(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of text.split(/[,\s]+/)) {
    const v = raw.trim();
    if (!v || seen.has(v)) continue;
    seen.add(v);
    out.push(v);
  }
  return out;
}

function emptyGroup(id: string): RoutingGroup {
  return {
    id,
    name: '',
    model: '',
    strategy: null,
    sticky: null,
    members: [],
  };
}

export default function GroupEditor({
  groups,
  onChange,
  focusId = null,
}: {
  groups: RoutingGroup[];
  onChange: (next: RoutingGroup[]) => void;
  /**
   * 左栏 rail 点中的组 id（对齐 A2A：点为 rail 条目，右侧只展开那一个组）。
   * null / '' = 不聚焦，全部展开（此时行为与旧版逐字节一致）。
   * 折叠只是显示层：groups 数组照样全量渲染项参与 patch，索引不会错位。
   */
  focusId?: string | null;
}): JSX.Element {
  const { t } = useTranslation();
  /** 用户手动展开的非聚焦组（focusId 只决定「默认展开谁」，不是只读过滤） */
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  /** 搜索（对齐 A2A routing-group-panel 的「搜索提供商或模型」）：只筛显示，不改数据 */
  const [search, setSearch] = useState('');

  /** 重复 id 检测：后端按 id 查找，重复会让命中结果不确定 */
  const dupIds = new Set(
    groups.map((g) => g.id).filter((id, i, arr) => arr.indexOf(id) !== i),
  );

  const patchGroup = (i: number, p: Partial<RoutingGroup>): void =>
    onChange(groups.map((g, idx) => (idx === i ? { ...g, ...p } : g)));

  const patchMember = (gi: number, mi: number, p: Partial<RoutingMember>): void => {
    const g = groups[gi];
    patchGroup(gi, {
      members: g.members.map((m, idx) => (idx === mi ? { ...m, ...p } : m)),
    });
  };

  const overGroups = groups.length > ROUTING_CAPS.MAX_GROUPS;

  /** 「展开全部 / 折叠全部」的判据：所有组（除聚焦组）都在手动展开集合里 */
  const allExpanded =
    groups.length > 0
    && groups.every((g, i) => g.id === focusId || expanded.has(`${g.id}-${i}`));

  return (
    <div>
      <div className="rt-notice rt-notice-info">
        {t('组的「路由策略 / 会话粘滞」是可继承的三态：留「继承上层」表示本组不改这两项，跟随全局/模型级；选了具体值才覆盖。')}
      </div>
      <div className="form-hint" style={{ marginTop: 6 }}>
        {t('组目前通过档案的「默认路由组」或请求显式指定命中；把组登记为可直接调用的模型（出现在 /v1/models 供客户端选择）依赖后端后续能力，本期未提供。')}
      </div>

      <div className="rt-group-actions" style={{ margin: '10px 0' }}>
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          onClick={() => onChange([...groups, emptyGroup(nextGroupId(groups))])}
        >
          <Plus size={13} /> {t('新增路由组')}
        </button>
        <span className={`rt-count${overGroups ? ' rt-count-over' : ''}`}>
          {t('{{n}} / 上限 {{cap}} 个组', { n: groups.length, cap: ROUTING_CAPS.MAX_GROUPS })}
        </span>
      </div>

      {/* 工具条（对齐 A2A routing-group-panel：搜索 + 折叠全部/展开全部） */}
      <div className="rt-group-toolbar">
        <input
          className="form-input"
          placeholder={t('搜索提供商或模型')}
          aria-label={t('搜索提供商或模型')}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        <Button
          variant="outline"
          size="sm"
          disabled={Boolean(search.trim()) || groups.length === 0}
          onClick={() => {
            if (allExpanded) {
              setExpanded(new Set());
              return;
            }
            setExpanded(new Set(groups.map((g, i) => `${g.id}-${i}`)));
          }}
        >
          {allExpanded ? t('折叠全部') : t('展开全部')}
        </Button>
      </div>

      {groups.length === 0 ? (
        <div className="rt-state-note rt-state-inherit">
          {t('还没有路由组。没有组时请求只走 global 与模型级档案 —— 需要「某模型专走某批供应商/账号」时才建组。')}
        </div>
      ) : null}

      {groups.map((g, gi) => {
        const overMembers = g.members.length > ROUTING_CAPS.MAX_MEMBERS;
        const dup = dupIds.has(g.id);
        // 聚焦模式下非目标组默认收成一行（点 rail 组条目 = 只看这一个组）；
        // 能手动展开，id 重复的组一律展开（那是要立刻看见的错）。
        const rowKey = `${g.id}-${gi}`;
        // 搜索只筛成员行（对齐 A2A：组卡片照旧显示，被筛掉的是候选模型行）；
        // 保留原始下标 mi，编辑/删除仍按真实位置改，不会因过滤错位。
        const keyword = search.trim().toLowerCase();
        const visibleMembers = g.members
          .map((m, mi) => ({ m, mi }))
          .filter(
            ({ m }) =>
              !keyword
              || m.provider.toLowerCase().includes(keyword)
              || m.model.toLowerCase().includes(keyword),
          );
        const collapsed = Boolean(focusId) && g.id !== focusId && !dup && !expanded.has(rowKey);
        if (collapsed) {
          return (
            <div className="rt-group-card rt-group-collapsed" key={rowKey}>
              <div className="rt-group-head">
                <div className="rt-group-title">
                  <span className="rt-group-id">{g.name || g.id || t('（未命名 id）')}</span>
                  <span className="rt-count">
                    {t('{{n}} 个成员槽位', { n: g.members.length })}
                  </span>
                </div>
                <button
                  type="button"
                  className="btn btn-secondary btn-sm"
                  onClick={() => setExpanded((prev) => new Set(prev).add(rowKey))}
                >
                  {t('展开此组')}
                </button>
              </div>
            </div>
          );
        }
        return (
          <div className="rt-group-card" key={`${g.id}-${gi}`}>
            <div className="rt-group-head">
              <div className="rt-group-title">
                <span className="rt-group-id">{g.id || t('（未命名 id）')}</span>
                {dup ? (
                  <span className="rt-chip rt-chip-default rt-chip-sm">
                    {t('id 重复')}
                  </span>
                ) : null}
                <span className="rt-count">
                  {t('{{n}} 个成员槽位', { n: g.members.length })}
                </span>
              </div>
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                disabled={!g.members.some((m) => m.included)}
                onClick={() =>
                  patchGroup(gi, { members: g.members.map((m) => ({ ...m, included: false })) })
                }
              >
                {t('全部移出')}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                onClick={() => onChange(groups.filter((_, idx) => idx !== gi))}
              >
                <Trash2 size={13} /> {t('删除此组')}
              </button>
            </div>

            {dup ? (
              <div className="rt-notice rt-notice-warn" style={{ marginTop: 8 }}>
                {t('有两处用了同一个组 id。后端按 id 查找，重复 id 会让命中的是哪一个组变得不确定 —— 请改成唯一 id。')}
              </div>
            ) : null}

            <div className="rt-grid3" style={{ marginTop: 10 }}>
              <div className="rt-subfield">
                <label>{t('组 id')}</label>
                <input
                  className="form-input"
                  value={g.id}
                  placeholder="gpt4o-fast"
                  onChange={(e) => patchGroup(gi, { id: e.target.value })}
                />
                <span className="form-hint">
                  {t('档案的「默认路由组」与请求头都用它引用本组。改 id 会让已有引用失效。')}
                </span>
              </div>
              <div className="rt-subfield">
                <label>{t('组名')}</label>
                <input
                  className="form-input"
                  value={g.name}
                  onChange={(e) => patchGroup(gi, { name: e.target.value })}
                />
              </div>
              <div className="rt-subfield">
                <label>{t('限定模型（留空 = 不限）')}</label>
                <input
                  className="form-input"
                  value={g.model}
                  placeholder={t('留空 = 任何模型都能用此组')}
                  onChange={(e) => patchGroup(gi, { model: e.target.value })}
                />
                <span className="form-hint">
                  {t('填了模型名后，只有请求名等于它的请求才能命中此组（不是上游真名）。')}
                </span>
              </div>
              <div className="rt-subfield">
                <label>{t('路由策略')}</label>
                <select
                  className="form-input"
                  value={g.strategy ?? ''}
                  onChange={(e) =>
                    patchGroup(gi, {
                      strategy: (e.target.value || null) as RouteStrategy | null,
                    })
                  }
                >
                  <option value="">{t('继承上层（不覆盖）')}</option>
                  {(Object.keys(STRATEGY_LABELS) as RouteStrategy[]).map((k) => (
                    <option key={k} value={k}>
                      {t(STRATEGY_LABELS[k])}
                    </option>
                  ))}
                </select>
              </div>
              <div className="rt-subfield">
                <label>{t('会话粘滞')}</label>
                <select
                  className="form-input"
                  value={g.sticky ?? ''}
                  onChange={(e) =>
                    patchGroup(gi, {
                      sticky: (e.target.value || null) as StickyMode | null,
                    })
                  }
                >
                  <option value="">{t('继承上层（不覆盖）')}</option>
                  {(Object.keys(STICKY_LABELS) as StickyMode[]).map((k) => (
                    <option key={k} value={k}>
                      {t(STICKY_LABELS[k])}
                    </option>
                  ))}
                </select>
              </div>
            </div>

            <div style={{ marginTop: 12 }}>
              <div className="rt-rail-title" style={{ padding: '0 0 6px' }}>
                {t('成员模型')}
                <span className="rt-count">
                  {t('{{n}} 项参与', { n: g.members.filter((m) => m.included).length })}
                </span>
              </div>
              {g.members.length === 0 ? (
                <div className="rt-state-note rt-state-inherit">
                  <b>{t('还没有参与模型')}</b>
                  {t('可以保存空路由，但请求会明确失败，不会回退全局。')}
                </div>
              ) : null}
              {visibleMembers.map(({ m, mi }) => (
                <div className="rt-member-row" key={mi} style={{ marginBottom: 8 }}>
                  <input
                    className="form-input"
                    placeholder={t('提供商，如 openai')}
                    value={m.provider}
                    onChange={(e) => patchMember(gi, mi, { provider: e.target.value })}
                  />
                  <input
                    className="form-input"
                    placeholder={t('模型名（上游真名）')}
                    value={m.model}
                    onChange={(e) => patchMember(gi, mi, { model: e.target.value })}
                  />
                  <input
                    className="form-input"
                    placeholder={t('点名账号，逗号分隔（留空 = 该槽位全部账号）')}
                    value={m.accounts.join(', ')}
                    onChange={(e) => patchMember(gi, mi, { accounts: parseAccounts(e.target.value) })}
                  />
                  <label
                    style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 12.5 }}
                    title={t('取消勾选 = 该槽位参与选路时被排除（保留记录便于再启用）')}
                  >
                    <input
                      type="checkbox"
                      checked={m.included}
                      onChange={(e) => patchMember(gi, mi, { included: e.target.checked })}
                    />
                    <span>{t('参与')}</span>
                  </label>
                  <button
                    type="button"
                    className="btn btn-secondary btn-sm"
                    onClick={() =>
                      patchGroup(gi, { members: g.members.filter((_, idx) => idx !== mi) })
                    }
                  >
                    {t('删除')}
                  </button>
                </div>
              ))}
              {keyword && !visibleMembers.length ? (
                <div className="rt-state-note rt-state-inherit">
                  {t('没有匹配的候选')}
                </div>
              ) : null}
              <div className="rt-group-actions" style={{ marginTop: 6 }}>
                <button
                  type="button"
                  className="btn btn-secondary btn-sm"
                  onClick={() =>
                    patchGroup(gi, {
                      members: [
                        ...g.members,
                        { provider: '', model: '', included: true, accounts: [] },
                      ],
                    })
                  }
                >
                  <Plus size={13} /> {t('添加模型')}
                </button>
                <span className={`rt-count${overMembers ? ' rt-count-over' : ''}`}>
                  {t('{{n}} / 上限 {{cap}} 个成员', { n: g.members.length, cap: ROUTING_CAPS.MAX_MEMBERS })}
                </span>
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
