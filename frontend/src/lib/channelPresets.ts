/**
 * 渠道级「供应商预置」目录（对齐 Agent2API `preset-providers.js`）。
 *
 * 预置只是**新建渠道表单的预填模板**：选中一家时把名称 / 协议 / Base URL /
 * quirks / 余额方案填进表单（都可改），提交仍走 `POST /api/channels`，
 * 落库后就是一条普通渠道。
 *
 * ── quirks 的取值（与后端 `channel::ChannelQuirks` 逐字一致）──
 *   urlSuffix          原样追加到出站 URL 的查询串（GLM / MiniMax 的 Claude
 *                      兼容端点要求 ?beta=true）
 *   headers            合并到默认头上的静态额外头（Cline 的客户端伪装头、
 *                      OpenRouter 的推荐标识头）
 *   anthropicToolType  "custom" = 发 anthropic 上游时给工具补 type（MiniMax）
 *   clientEmulation    "opencode" = 按 OpenCode 官方 CLI 形状补齐出站请求
 *
 * ── balanceScheme 的取值（与后端 `channel::balance::SCHEMES` 逐字一致）──
 * sub2api / new_api / deepseek / opencode / sensenova / zhipu / zhipu_zai
 */

export type ChannelProtocol = 'openai_compatible' | 'anthropic' | 'zai' | 'gemini';

export interface ChannelQuirks {
  url_suffix?: string;
  headers?: Record<string, string>;
  anthropic_tool_type?: string;
  client_emulation?: string;
}

export interface PresetProvider {
  /** 卡片稳定标记，也是图标文件名（`/providers/<icon>`） */
  key: string;
  name: string;
  /** 图标文件名；缺省用首字母徽章 */
  icon?: string;
  protocol: ChannelProtocol;
  baseUrl: string;
  /** Base URL 栏下的备注（纯展示） */
  hint?: string;
  quirks?: ChannelQuirks;
  balanceScheme?: string;
  /** 该家需要补的额外余额凭证输入框（见 balanceScheme 的说明） */
  balanceFields?: Array<{ key: string; label: string; placeholder: string; header: string }>;
  /** 「该上游无需鉴权」的默认勾选态 */
  noAuth?: boolean;
}

const ANTHROPIC_BETA_HEADERS = {
  'Anthropic-Beta': 'claude-code-20250219,interleaved-thinking-2025-05-14',
};

export const PRESET_PROVIDERS: PresetProvider[] = [
  {
    key: 'openai', name: 'OpenAI', icon: 'openai.png',
    protocol: 'openai_compatible', baseUrl: 'https://api.openai.com/v1',
  },
  {
    key: 'anthropic', name: 'Anthropic', icon: 'anthropic.png',
    protocol: 'anthropic', baseUrl: 'https://api.anthropic.com',
    quirks: { headers: { ...ANTHROPIC_BETA_HEADERS } },
  },
  {
    key: 'openrouter', name: 'OpenRouter', icon: 'openrouter.png',
    protocol: 'openai_compatible', baseUrl: 'https://openrouter.ai/api/v1',
    hint: '一个 Key 用遍多家模型，模型名用「厂商/模型」全称（如 deepseek/deepseek-chat）',
    quirks: {
      headers: { 'HTTP-Referer': 'https://aigx.local', 'X-Title': 'AIGX' },
    },
  },
  {
    key: 'huggingface', name: 'HuggingFace', icon: 'huggingface.png',
    protocol: 'openai_compatible', baseUrl: 'https://router.huggingface.co/v1',
    hint: '走 Inference Providers 路由，Key 在 hf.co/settings/tokens 创建',
  },
  {
    key: 'nvidia', name: 'NVIDIA', icon: 'nvidia.png',
    protocol: 'openai_compatible', baseUrl: 'https://integrate.api.nvidia.com/v1',
    hint: 'NVIDIA NIM 托管端点，Key 在 integrate.api.nvidia.com 申请',
  },
  {
    key: 'deepseek', name: 'DeepSeek', icon: 'deepseek.png',
    protocol: 'openai_compatible', baseUrl: 'https://api.deepseek.com',
    hint: 'Anthropic 兼容端点在 https://api.deepseek.com/anthropic（协议换 anthropic 时填它）',
    balanceScheme: 'deepseek',
  },
  {
    key: 'deepseek-anthropic', name: 'DeepSeek（Anthropic 端点）', icon: 'deepseek.png',
    protocol: 'anthropic', baseUrl: 'https://api.deepseek.com/anthropic',
    hint: 'DeepSeek 的 Claude Code 兼容端点（协议为 anthropic）',
  },
  {
    key: 'glm', name: 'GLM Coding', icon: 'glm.png',
    protocol: 'anthropic', baseUrl: 'https://api.z.ai/api/anthropic',
    hint: '国际版 z.ai 的 Coding 套餐；OpenAI 兼容地址为 https://api.z.ai/api/coding/paas/v4',
    quirks: { url_suffix: '?beta=true', headers: { ...ANTHROPIC_BETA_HEADERS } },
    balanceScheme: 'zhipu_zai',
  },
  {
    key: 'glm-cn', name: 'GLM 中国', icon: 'glm-cn.png',
    protocol: 'openai_compatible', baseUrl: 'https://open.bigmodel.cn/api/coding/paas/v4',
    hint: '智谱 bigmodel.cn 的 Coding 套餐，Key 在 open.bigmodel.cn/usercenter/apikeys',
    balanceScheme: 'zhipu',
    balanceFields: [
      {
        key: 'token', label: '智谱控制台令牌',
        placeholder: 'bigmodel.cn 控制台登录 Cookie 里的 bigmodel_token_production',
        header: 'x-balance-token',
      },
    ],
  },
  {
    key: 'minimax', name: 'Minimax Coding', icon: 'minimax.png',
    protocol: 'anthropic', baseUrl: 'https://api.minimax.io/anthropic',
    hint: '国际站 Coding 套餐；OpenAI 兼容地址为 https://api.minimax.io/v1',
    quirks: {
      url_suffix: '?beta=true',
      headers: { ...ANTHROPIC_BETA_HEADERS },
      anthropic_tool_type: 'custom',
    },
  },
  {
    key: 'minimax-cn', name: 'Minimax 中国', icon: 'minimax-cn.png',
    protocol: 'anthropic', baseUrl: 'https://api.minimaxi.com/anthropic',
    hint: '国内站（minimaxi.com）；OpenAI 兼容地址为 https://api.minimaxi.com/v1',
    quirks: {
      url_suffix: '?beta=true',
      headers: { ...ANTHROPIC_BETA_HEADERS },
      anthropic_tool_type: 'custom',
    },
  },
  {
    key: 'siliconflow', name: 'SiliconFlow', icon: 'siliconflow.png',
    protocol: 'openai_compatible', baseUrl: 'https://api.siliconflow.com/v1',
    hint: '国内站为 https://api.siliconflow.cn/v1',
  },
  {
    key: 'volcengine-ark', name: '火山方舟', icon: 'volcengine-ark.png',
    protocol: 'openai_compatible', baseUrl: 'https://ark.cn-beijing.volces.com/api/coding/v3',
    hint: '火山引擎 Coding 套餐；通用推理接入点为 https://ark.cn-beijing.volces.com/api/v3',
  },
  {
    key: 'alicode', name: 'Alibaba', icon: 'alicode.png',
    protocol: 'openai_compatible', baseUrl: 'https://coding.dashscope.aliyuncs.com/v1',
    hint: '阿里云百炼 Coding 套餐（国内站）',
  },
  {
    key: 'alicode-intl', name: 'Alibaba Coding', icon: 'alicode-intl.png',
    protocol: 'openai_compatible', baseUrl: 'https://coding-intl.dashscope.aliyuncs.com/v1',
    hint: '阿里云百炼 Coding 套餐（国际站）',
  },
  {
    key: 'alims-intl', name: 'Alibaba Studio', icon: 'alims-intl.png',
    protocol: 'openai_compatible', baseUrl: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1',
    hint: '阿里云百炼模型服务（国际站，OpenAI 兼容模式）',
  },
  {
    key: 'alitp-intl', name: 'Alibaba Token Plan', icon: 'alitp-intl.png',
    protocol: 'openai_compatible', baseUrl: 'https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1',
    hint: '阿里云 Token Plan 套餐（国际站）',
  },
  {
    key: 'xiaomi-tokenplan', name: '小米 MiMo (Token Plan)', icon: 'xiaomi-tokenplan.png',
    protocol: 'openai_compatible', baseUrl: 'https://token-plan-sgp.xiaomimimo.com/v1',
    hint: '小米 MiMo Token Plan 套餐（新加坡站）',
  },
  {
    key: 'opencode-go', name: 'OpenCode Go', icon: 'opencode-go.png',
    protocol: 'openai_compatible', baseUrl: 'https://opencode.ai/zen/go/v1',
    hint: '订阅制；转发必须填 OpenCode Go 的 API Key（Go 面网关按端点自动补会话头，不需要开伪装）',
    balanceScheme: 'opencode',
  },
  {
    key: 'opencode-zen', name: 'OpenCode Zen', icon: 'opencode-zen.png',
    protocol: 'openai_compatible', baseUrl: 'https://opencode.ai/inference/v1',
    hint: '免费模型不用填 Key（默认勾上「无密钥」，网关自动用匿名凭证并按官方客户端形态补齐请求）；付费模型则填 Zen 的 API Key',
    quirks: { client_emulation: 'opencode' },
    noAuth: true,
  },
  {
    key: 'clinepass', name: 'Cline Pass', icon: 'cline.png',
    protocol: 'openai_compatible', baseUrl: 'https://api.cline.bot/api/v1',
    hint: 'Cline Pass 的 API Key 在 app.cline.bot 获取（订阅制）；上游按客户端形态校验，所需请求头已随卡预置',
    quirks: {
      headers: {
        'User-Agent': 'Cline/3.0.62',
        'HTTP-Referer': 'https://cline.bot',
        'X-Title': 'Cline',
        'X-IS-MULTIROOT': 'false',
        'X-CLIENT-TYPE': 'cline-sdk',
        'X-CLIENT-VERSION': '3.0.62',
        'X-PLATFORM': 'terminal',
        'X-PLATFORM-VERSION': '3.0.62',
        'X-CORE-VERSION': '0.0.70',
      },
    },
  },
  {
    key: 'ollama-local', name: 'Ollama（本地）', icon: 'ollama-local.png',
    protocol: 'openai_compatible', baseUrl: 'http://localhost:11434/v1',
    hint: '本机 Ollama 的 OpenAI 兼容端点，无需 API Key',
    noAuth: true,
  },
  {
    key: 'sub2api', name: 'Sub2API', icon: 'sub2api.png',
    protocol: 'openai_compatible', baseUrl: '',
    hint: '自建聚合面板：填你自己的站点地址。余额查询用该渠道的 API Key 打 {base}/v1/usage',
    balanceScheme: 'sub2api',
  },
  {
    key: 'newapi', name: 'NewAPI', icon: 'newapi.png',
    protocol: 'openai_compatible', baseUrl: '',
    hint: '自建聚合面板：填你自己的站点地址；余额查询要另填 NewAPI 的个人访问令牌',
    balanceScheme: 'new_api',
    balanceFields: [
      {
        key: 'token', label: 'NewAPI 访问令牌',
        placeholder: 'NewAPI 控制台 → 个人设置 → 生成访问令牌',
        header: 'x-balance-access-token',
      },
      {
        key: 'userId', label: 'NewAPI 用户 ID（可选）',
        placeholder: '数字用户 ID，部分部署需要',
        header: 'x-balance-user-id',
      },
    ],
  },
  {
    key: 'sensenova', name: '商汤日日新', icon: 'sensenova.png',
    protocol: 'openai_compatible', baseUrl: 'https://api.sensenova.cn/compatible-mode/v1',
    hint: '商汤日日新的 OpenAI 兼容端点；余额查询走控制台账号密码',
    balanceScheme: 'sensenova',
    balanceFields: [
      { key: 'username', label: '商汤账号', placeholder: 'platform.sensenova.cn 登录账号', header: 'x-balance-username' },
      { key: 'password', label: '商汤密码', placeholder: 'platform.sensenova.cn 登录密码', header: 'x-balance-password' },
    ],
  },
];

/** 按 key 取一家（找不到给 null） */
export function presetOf(key: string): PresetProvider | null {
  return PRESET_PROVIDERS.find((p) => p.key === key) || null;
}

/** 图标 URL：收录过的家出 `/providers/<file>`，没有返回 null（首字母徽章兜底） */
export function presetIconOf(key: string): string | null {
  const preset = presetOf(key);
  return preset?.icon ? `/providers/${preset.icon}` : null;
}

/** 余额方案展示名 */
export const BALANCE_SCHEME_LABELS: Record<string, string> = {
  sub2api: 'Sub2API（/v1/usage）',
  new_api: 'NewAPI（/api/user/self）',
  deepseek: 'DeepSeek（/user/balance）',
  opencode: 'OpenCode Go（/v1/usage）',
  sensenova: '商汤日日新（控制台账号密码）',
  zhipu: '智谱（bigmodel.cn 控制台令牌）',
  zhipu_zai: '智谱国际（z.ai API Key 签名）',
};
