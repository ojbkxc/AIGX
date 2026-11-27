/**
 * 配置项索引 —— 「管理员想配某个东西时，它在哪」的单一真相。
 *
 * 背景（线上可发现性缺陷）：AIGX 的配置面经过多轮迁移，许多配置**不再是侧栏一级菜单**，
 * 而是折进了别的页的二级/三级 Tab：
 *   - 易支付 / 用户分组 / 按次计费 / 价格同步 → 「套餐与兑换码」页的「计费设置」Tab
 *     （原「系统设置 → 计费」，v1.0.93 迁出）
 *   - OAuth 登录（GitHub / Google / LinuxDO）→ 「系统设置」页的「OAuth 登录」分类
 * 于是一个按旧记忆去「系统设置」里找「易支付」的管理员，看到的是一个**没有该配置的设置页**，
 * 结论就是「这些菜单我在线上真没看见」。侧栏一级菜单也帮不上忙 —— 那一层只有页的名字
 * （「套餐与兑换码」），不含「易支付」这个关键词。
 *
 * 这个文件只描述「配置项 → 落地路径」，不渲染、不发请求：
 *   - GlobalSearch（Ctrl+K）按它索引，输「易支付」「github」直接跳到落实位置；
 *   - SettingsOverview 按它渲染「配置总览」，一屏看到有哪些配置、各自在哪。
 *
 * 新增/迁移配置面时**在这里补一条**，两个入口同时生效，不要再各自维护一份路径表。
 */

/** 配置项归属；只用于总览分组与排序，不参与权限判定。 */
export type ConfigCategory =
  | 'payment'
  | 'auth'
  | 'gateway'
  | 'security'
  | 'limits'
  | 'ops';

export interface ConfigEntry {
  /** 稳定 id（React key / 测试锚点），不参与展示。 */
  id: string;
  /** 展示名，同时是 i18n key（本项目约定 key === 中文原文）。 */
  label: string;
  /**
   * 搜索别名：用户嘴里可能说的词，包含中英文与口语。
   * 只做匹配用，不展示、不进 i18n（同一个概念在不同语言下都要能命中）。
   */
  aliases: string[];
  /** 直达路径，带 ?tab= 精确到分类。 */
  path: string;
  category: ConfigCategory;
  /** 一句话说明「在哪」，用于总览的「位置」列与搜索结果的副标题。 */
  hint: string;
  /** 仅管理员可见（与 Sidebar 的 adminOnly 同口径）。 */
  adminOnly?: boolean;
}

/**
 * 配置项清单。顺序 = 总览里的顺序，也是「越常配越靠前」的顺序：
 * 用户报的三块（易支付、GitHub/Google/LinuxDO 对接）排最前。
 */
export const CONFIG_INDEX: ConfigEntry[] = [
  {
    id: 'epay',
    label: '易支付配置',
    aliases: [
      '易支付', 'epay', '支付', '商户', '商户参数', '彩虹易支付', '支付宝', '微信支付',
      '充值', '支付网关', 'pay', 'payment',
    ],
    path: '/products?tab=billing',
    category: 'payment',
    hint: '套餐与兑换码 ▸ 计费设置 ▸ 易支付配置',
    adminOnly: true,
  },
  {
    id: 'oauth',
    label: 'OAuth 登录',
    aliases: [
      'oauth', 'OAuth', 'github', 'GitHub', 'github登录', '谷歌', 'Google', 'google登录',
      'linuxdo', 'LinuxDO', '第三方登录', '第三方', '单点登录', 'sso', 'SSO',
      '登录方式', '登录对接', 'client id', '回调', 'callback',
    ],
    path: '/oauth',
    category: 'auth',
    hint: '系统设置 ▸ OAuth 登录（也可直链 /oauth）',
    adminOnly: true,
  },
  {
    id: 'user-groups',
    label: '用户分组',
    aliases: ['用户组', '分组', 'group', 'groups', '倍率', '用户分组', '等级'],
    path: '/products?tab=billing',
    category: 'payment',
    hint: '套餐与兑换码 ▸ 计费设置 ▸ 用户分组',
    adminOnly: true,
  },
  {
    id: 'flat-quota',
    label: '按次计费',
    aliases: ['按次', '按次计费', '固定额度', 'flat quota', 'billing_flat_quota', '计费设置'],
    path: '/products?tab=billing',
    category: 'payment',
    hint: '套餐与兑换码 ▸ 计费设置（页面下方）',
    adminOnly: true,
  },
  {
    id: 'price-sync',
    label: '价格同步',
    aliases: ['价格同步', '同步价格', 'price sync', '同步'],
    path: '/products?tab=billing',
    category: 'payment',
    hint: '套餐与兑换码 ▸ 计费设置（页面下方）',
    adminOnly: true,
  },
  {
    id: 'exchange-rate',
    label: '汇率与计价',
    aliases: ['汇率', '计价', '人民币', 'cny', 'usd', '换算'],
    path: '/settings?tab=general',
    category: 'payment',
    hint: '系统设置 ▸ 通用 ▸ 汇率与计价',
    adminOnly: true,
  },
  {
    id: 'router-timeout',
    label: '超时',
    aliases: ['超时', 'timeout', '连接超时', '首包', '重试'],
    path: '/settings?tab=timeout',
    category: 'gateway',
    hint: '系统设置 ▸ 超时',
    adminOnly: true,
  },
  {
    id: 'body-limit',
    label: '请求体大小上限',
    aliases: ['请求体', '体积上限', 'body limit', '413', '最大请求'],
    path: '/settings?tab=timeout',
    category: 'limits',
    hint: '系统设置 ▸ 超时 ▸ 请求体大小上限',
    adminOnly: true,
  },
  {
    id: 'email-whitelist',
    label: '邮箱白名单',
    aliases: ['邮箱白名单', '白名单', '注册限制', 'whitelist', '域名'],
    path: '/settings?tab=general',
    category: 'security',
    hint: '系统设置 ▸ 通用 ▸ 邮箱白名单',
    adminOnly: true,
  },
  {
    id: 'security-policy',
    label: '安全策略',
    aliases: ['安全策略', '安全', 'security', 'ip 管理', '登录保护'],
    path: '/settings?tab=security',
    category: 'security',
    hint: '系统设置 ▸ 安全',
    adminOnly: true,
  },
  {
    id: 'gateway-panel',
    label: '网关与网络层',
    aliases: ['网关', '网络层', 'cors', '监听', '伪装', 'guardrail', '内容护栏', '客户端识别'],
    path: '/settings?tab=gateway',
    category: 'gateway',
    hint: '系统设置 ▸ 网关',
    adminOnly: true,
  },
  {
    id: 'providers',
    label: '提供商',
    aliases: ['提供商', 'provider', '渠道', '上游', '密钥'],
    path: '/settings?tab=providers',
    category: 'gateway',
    hint: '系统设置 ▸ 提供商（实际维护入口在「渠道」页）',
    adminOnly: true,
  },
  {
    id: 'data-backup',
    label: '数据与备份',
    aliases: ['数据', '备份', '云同步', '日志保留', 'storage', 'backup'],
    path: '/settings?tab=data',
    category: 'ops',
    hint: '系统设置 ▸ 数据',
    adminOnly: true,
  },
  {
    id: 'system-info',
    label: '版本信息',
    aliases: ['版本', '系统信息', 'version', '构建'],
    path: '/settings?tab=system',
    category: 'ops',
    hint: '系统设置 ▸ 版本信息',
    adminOnly: true,
  },
];

/**
 * 按 `q` 过滤配置项。
 *
 * 匹配面 = 翻译后的展示名 + 原始中文名 + 全部别名 + 位置说明。
 * 三个面都要过：英文界面下用户可能仍敲中文（原始 label），中文界面下也可能敲
 * 「github」（别名），只匹配 t(label) 会让其中一半场景搜不到。
 */
export function matchConfigEntries(
  q: string,
  translate: (key: string) => string,
  isAdminUser: boolean
): ConfigEntry[] {
  const needle = q.trim().toLowerCase();
  const visible = isAdminUser ? CONFIG_INDEX : CONFIG_INDEX.filter((e) => !e.adminOnly);
  if (!needle) return visible;
  return visible.filter((entry) => {
    const haystack = [
      translate(entry.label),
      entry.label,
      entry.hint,
      translate(entry.hint),
      ...entry.aliases,
    ]
      .join(' ')
      .toLowerCase();
    return haystack.includes(needle);
  });
}