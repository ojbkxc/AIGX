/**
 * 档案字段 → 人话（provenance 表 / 工作台覆盖对比共用）。
 *
 * 唯一口径：生效值一律来自后端解析结果（`profile` / `inheritedProfile`），
 * 本文件只负责**把值排版成可读文本**，不做任何选路推算。
 */
import { SORT_ON_LABELS } from './ProfileForm';
import {
  type ProfileField,
  type RoutingProfile,
  type RequestNeeds,
} from '../../types/routing-profile';

export type TFn = (key: string, opts?: Record<string, unknown>) => string;

/** 能力需求三项的中文标签（预览请求体与档案表共用） */
export const NEEDS_FIELDS: Array<[keyof RequestNeeds, string]> = [
  ['tools', '工具调用'],
  ['images', '图片理解'],
  ['video', '视频'],
];

/** 档案六字段的标签（表头 / 行序与 `PROFILE_FIELDS` 对齐） */
export const PROFILE_FIELD_LABELS: Record<ProfileField, string> = {
  scope: '参与名单',
  strategy: '选路策略',
  sticky: '会话粘性',
  needs: '能力需求',
  sort_rules: '账号排序规则',
  default_group: '默认路由组',
};

/** 单个字段值的可读文本（null = 该层没表态，显示「（未设置）」） */
export function profileFieldText(p: RoutingProfile, f: ProfileField, t: TFn): string {
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
}