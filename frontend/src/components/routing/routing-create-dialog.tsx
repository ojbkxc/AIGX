/**
 * 新建路由组弹窗（对齐 Agent2API v3.5.1 `routing-create-dialog.tsx` 的**建组分支**）。
 *
 * A2A 的「新增路由配置」是一个二选一弹窗：模型独立路由 / 路由组。AIGX 侧
 * 「新增模型档案」是页面 rail 顶部的输入框（`pages/Routing.tsx` 的「新增模型
 * 档案名」），不属于卡片面 —— 所以这里只落建组这一支，字段与按钮照 A2A：
 * 组名 → 对外模型 ID（+ 本仓独有的组 id）→ 选择成员模型 → 「创建草稿」。
 *
 * 与 A2A 的语义差异（不合并，逐条登记）：
 * - A2A 要求「选择模型」非空才能建组，且组 ID 由 `randomUUID()` 生成；
 * - AIGX 的组可以没有任何成员（后端允许空成员表，保存时会提示「请求会明确
 *   失败」）；组 id 是语义键，必须由人填、且全仓唯一。
 * 两处都按 AIGX 的后端契约走，UI 文案沿用 A2A 的同义句。
 */
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { RoutingGroup } from '../../types/routing-profile';
import { RoutingModelPicker, type RouteModelChoice } from './routing-model-picker';
import { createRouteMembers, routeGroupFieldsError, RoutingGroupForm, type RoutingGroupFields } from './routing-group-form';

export function RoutingGroupCreateDialog({ existingIds, catalog, defaultId = '', onSave, onClose }: {
  /** 已有组 id（重名检测） */
  existingIds: string[];
  /** 模型目录候选；缺省 = 目录未接入，选择器展示空态 */
  catalog: RouteModelChoice[];
  /** 组 id 的默认建议值（A2A 用 uuid 自动生成；本仓 id 是语义键，给一个可改的初值） */
  defaultId?: string;
  onSave(group: RoutingGroup): void;
  onClose(): void;
}): JSX.Element {
  const { t } = useTranslation();
  const [fields, setFields] = useState<RoutingGroupFields>({ id: defaultId, name: '', model: '' });
  const [selected, setSelected] = useState<string[]>([]);
  const [error, setError] = useState('');
  const byKey = new Map(catalog.map((choice) => [`${choice.provider}\u0000${choice.model.toLowerCase()}`, choice]));
  const picked = selected.flatMap((key) => {
    const choice = byKey.get(key);
    return choice ? [choice] : [];
  });
  const create = (): void => {
    const message = routeGroupFieldsError(fields, existingIds);
    if (message) {
      setError(t(message));
      return;
    }
    onSave({
      id: fields.id.trim(),
      name: fields.name.trim(),
      model: fields.model.trim().toLowerCase(),
      strategy: null,
      sticky: null,
      members: createRouteMembers(picked),
    });
    onClose();
  };
  return (
    <div
      className="modal-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={t('新增路由配置')}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="modal" style={{ maxWidth: 880, width: '94%' }} onClick={(event) => event.stopPropagation()}>
        <div className="modal-header">
          <h3>{t('新增路由配置')}</h3>
          <button type="button" className="modal-close" aria-label={t('关闭')} onClick={onClose}>×</button>
        </div>
        <div className="modal-body">
          <p className="rt-config-note">
            {t('可以跨提供商、跨模型多选。同一提供商也可以加入不同模型。')}
          </p>
          <RoutingGroupForm value={fields} onChange={setFields} />
          <RoutingModelPicker choices={catalog} selected={selected} onChange={setSelected} lastUsed={undefined} />
          {error ? <p className="rt-config-error" role="alert">{error}</p> : null}
        </div>
        <div className="modal-footer">
          <button type="button" className="btn btn-outline" onClick={onClose}>{t('取消')}</button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={!fields.id.trim()}
            onClick={create}
          >
            {t('创建草稿')}
          </button>
        </div>
      </div>
    </div>
  );
}