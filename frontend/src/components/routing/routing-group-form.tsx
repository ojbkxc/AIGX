/**
 * 建组 / 改组表单（对齐 Agent2API v3.5.1 `routing-group-form.tsx`）。
 *
 * 字段与顺序照 A2A：**组名 → 对外模型 ID**。A2A 的组 id 是内部 uuid（界面上
 * 不出现），AIGX 的组 id 是**语义键**（档案「默认路由组」与请求头都用它引用，
 * 见 `types/routing-profile.ts`）—— 所以这里多一格「组 id」，并在 hint 里
 * 写明改 id 会让已有引用失效。这是本仓独有项，不是 A2A 的字段。
 *
 * ⚠ 语义差异（不合并成一句话糊过去）：A2A 的「对外模型 ID」是**必填**的
 * 入口名（`findGroup` 按它路由）；AIGX 的 `model` 是**限定/留空即不限**
 * （后端判据 `g.model.is_empty() || g.model == requested_model`）。
 * 所以校验只对 id 提硬要求，model 允许空 —— 空 = 任何模型都能用此组。
 */
import { useId } from 'react';
import { useTranslation } from 'react-i18next';
import { ROUTING_CAPS, type RoutingGroup, type RoutingMember } from '../../types/routing-profile';

export type RoutingGroupFields = Pick<RoutingGroup, 'id' | 'name' | 'model'>;

/** 成员上限与组上限（与后端 `routing_policy::caps` 同源，不另写一份数字） */
export const ROUTING_MEMBER_LIMIT = ROUTING_CAPS.MAX_MEMBERS;

/** 成员身份键：同一组内 (provider, model) 唯一（AIGX 的成员没有独立 id） */
export const routeModelKey = (member: Pick<RoutingMember, 'provider' | 'model'>): string =>
  `${member.provider}\u0000${member.model.toLowerCase()}`;

/** 校验：id 必填、格式合法、与其它组不重名（大小写不敏感） */
export function routeGroupFieldsError(
  fields: RoutingGroupFields,
  otherIds: string[],
  originalId = '',
): string {
  const id = fields.id.trim();
  if (!id) return '请填写组 id';
  // 空白或控制字符会让「请求头 / 档案引用」这种纯文本引用失效；这里不用正则，
  // 避免 `\u0000-\u001f` 触发 no-control-regex（同一判据，写法不同）。
  const hasBadChar = [...id].some((ch) => {
    const code = ch.codePointAt(0) ?? 0;
    return code <= 0x20 || code === 0x7f;
  });
  if (id.length > 64 || hasBadChar) return '组 id 不能包含空白或控制字符';
  if (
    otherIds.some(
      (value) => value.toLowerCase() === id.toLowerCase() && value.toLowerCase() !== originalId.toLowerCase(),
    )
  ) {
    return '组 id 已被占用，请换一个名称';
  }
  return '';
}

/** 由模型候选生成成员（账号留空 = 该槽位用该提供商的全部账号） */
export function createRouteMembers(choices: Array<{ provider: string; model: string }>): RoutingMember[] {
  return choices.map((choice) => ({
    provider: choice.provider,
    model: choice.model,
    included: true,
    accounts: [],
  }));
}

export function RoutingGroupForm({ value, onChange, disabled = false }: {
  value: RoutingGroupFields;
  onChange(value: RoutingGroupFields): void;
  disabled?: boolean;
}): JSX.Element {
  const { t } = useTranslation();
  const id = useId();
  return (
    <div className="rt-group-form">
      <div className="rt-subfield">
        <label htmlFor={`${id}-name`}>{t('组名')}</label>
        <input
          id={`${id}-name`}
          className="form-input"
          maxLength={64}
          disabled={disabled}
          value={value.name}
          placeholder={t('例如：日常省钱')}
          onChange={(event) => onChange({ ...value, name: event.currentTarget.value })}
        />
      </div>
      <div className="rt-subfield">
        <label htmlFor={`${id}-model`}>{t('对外模型 ID')}</label>
        <input
          id={`${id}-model`}
          className="form-input"
          maxLength={256}
          disabled={disabled}
          value={value.model}
          placeholder="budget-chat"
          onChange={(event) => onChange({ ...value, model: event.currentTarget.value })}
        />
        <span className="form-hint">{t('留空 = 任何模型都能用此组；填了之后只有请求名等于它的请求能命中。')}</span>
      </div>
      <div className="rt-subfield">
        <label htmlFor={`${id}-id`}>{t('组 id')}</label>
        <input
          id={`${id}-id`}
          className="form-input"
          maxLength={64}
          disabled={disabled}
          value={value.id}
          placeholder="group-1"
          onChange={(event) => onChange({ ...value, id: event.currentTarget.value })}
        />
        <span className="form-hint">
          {t('档案的「默认路由组」与请求头都用它引用本组。改 id 会让已有引用失效。')}
        </span>
      </div>
    </div>
  );
}