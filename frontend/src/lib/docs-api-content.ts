/**
 * docs-api-content — 「接入文档」正文数据（7 章 + AIGX 独有能力 1 章）。
 *
 * 口径：**只写 AIGX 真实存在的端点与字段**，逐条对着源码校对过：
 * - 路由表 `src/main.rs`（`openai_routes` / `anthropic_routes` / 无前缀兼容双挂）；
 * - 模型清单条目形状 `src/api/openai/models.rs::list_item` + `anthropic_models_view`；
 * - 请求字段 `src/api/openai/{chat,responses,embeddings,rerank,completions,images,audio}.rs`
 *   与 `src/api/anthropic.rs`；
 * - 错误码全集 `openai_error_response("…")` / `anthropic_error("…")` 调用点；
 * - 鉴权口径 `src/api/openai/resolve.rs::{verify_api_key_full,verify_api_key_listing}`、
 *   `src/api/common.rs::extract_api_key*`。
 *
 * 与 Agent2API v3.5.1 `docs-api-content.ts` 的差异（A2A 有、AIGX 没有的**不写**）：
 * - A2A 的 `kind` / `supports_image_generation` / `supports_image_editing` /
 *   `max_input_tokens` / `output_modalities` 在 AIGX 清单条目里**不存在**，
 *   换成 AIGX 实际字段（`capabilities` / `context_length` / `supports_video` /
 *   `supports_embedding` / `supports_rerank` / `reasoning_levels` / `cap_sources`）；
 * - A2A 临时图片 `GET /v1/images/files/{id}` → AIGX `GET /v1/images/assets/:name`
 *   （且需鉴权、只在「生图资产落盘」开关打开时有文件）；
 * - A2A `/health` 返回 `status=ok|degraded` → AIGX `/health` 返回
 *   `{ 模型名: 0..3 }` 健康档位表（另有 `/livez` `/readyz`）；
 * - A2A `count_tokens` 返回 `input_tokens` → AIGX 返回 `count`；
 * - AIGX 独有端点补进来：`/v1/completions`、`/v1/embeddings`、`/v1/rerank`、
 *   `/v1/audio/{transcriptions,translations,speech}`、`/v1/models/{model}`、
 *   无前缀兼容双挂、工具服务 `/api/web-search/*` + `/skill/web-search/*` + `/mcp/*`。
 *
 * i18n：正文用中文字面量存这里（`.ts` 不进 `i18n-fill-missing.mjs` 的扫描面），
 * 渲染时由组件统一 `t(…)`——缺 key 时回落到中文原文，英文界面不会空白。
 */

/** 一个请求/响应示例（代码块 + 一键复制） */
export interface DocsExample {
  /** 该示例的标题（复制时也用它做 toast 文案） */
  label: string;
  /** 代码原文（着色按 language 粗分，不做语法高亮库依赖） */
  code: string;
  language: 'bash' | 'json' | 'python' | 'text';
}

/** 字段/状态表的一行 */
export interface DocsField {
  name: string;
  type: string;
  description: string;
}

/** 正文的「接口地址」条目（独立章节，地址由页面按实际 origin 拼） */
export interface DocsEndpoint {
  id: string;
  method: string;
  path: string;
  name: string;
  note: string;
  group: string;
  /** 是否携带 API Key 才能调用（AIGX 数据面除探针外全部需要） */
  auth: string;
}

/** 一章正文 */
export interface DocsChapter {
  id: string;
  title: string;
  /** 标题下的一行摘要（目录搜索也匹配它） */
  summary: string;
  /** 端点路径行（可空；纯说明章留空） */
  path?: string;
  /** 目录分组名 */
  group: string;
  paragraphs?: string[];
  fields?: DocsField[];
  points?: string[];
  warning?: string;
  examples?: DocsExample[];
}

/** 目录分组（侧栏一级） */
export interface DocsGroup {
  id: string;
  title: string;
}

/** 侧栏分组顺序（章节按 group 归属；顺序即目录顺序） */
export const DOCS_GROUPS: DocsGroup[] = [
  { id: 'start', title: '接入指南' },
  { id: 'chat', title: '对话协议' },
  { id: 'media', title: '向量 / 重排 / 图片 / 音频' },
  { id: 'tools', title: '工具服务' },
  { id: 'aigx', title: 'AIGX 独有能力' },
  { id: 'errors', title: '排错与限制' },
];

/**
 * 接口地址表（对齐 `src/main.rs` 真实路由）。
 *
 * `path` 是相对 Base URL 的路径；`/v1` 前缀版与无前缀兼容版都列出来，
 * 因为 AIGX 两条都挂（客户端 base_url 填根地址还是填 `…/v1` 都能用）。
 */
export const DOCS_ENDPOINTS: DocsEndpoint[] = [
  {
    id: 'ep-base',
    method: '',
    path: '/v1',
    name: 'Base URL',
    note: 'OpenAI 兼容客户端填以 /v1 结尾的地址；Anthropic 官方 SDK 填根地址（它自己追加 /v1/messages）',
    group: '根地址',
    auth: '—',
  },
  {
    id: 'ep-chat',
    method: 'POST',
    path: '/v1/chat/completions',
    name: 'OpenAI Chat Completions',
    note: '多数 OpenAI 兼容客户端的默认协议；支持流式（SSE）',
    group: '对话协议',
    auth: '网关 Key',
  },
  {
    id: 'ep-responses',
    method: 'POST',
    path: '/v1/responses',
    name: 'OpenAI Responses',
    note: 'Codex / 新版 OpenAI SDK；优先原样透传上游，渠道不支持时降级转 Chat',
    group: '对话协议',
    auth: '网关 Key',
  },
  {
    id: 'ep-messages',
    method: 'POST',
    path: '/v1/messages',
    name: 'Anthropic Messages',
    note: 'Claude Code / Anthropic SDK；与 OpenAI 入口共用模型目录与渠道',
    group: '对话协议',
    auth: '网关 Key（x-api-key 优先）',
  },
  {
    id: 'ep-count',
    method: 'POST',
    path: '/v1/messages/count_tokens',
    name: 'Token 数估算',
    note: '按字符数估算（chars/3.5），不走上游分词器；返回 count + estimated=true',
    group: '对话协议',
    auth: '网关 Key',
  },
  {
    id: 'ep-completions',
    method: 'POST',
    path: '/v1/completions',
    name: 'OpenAI 文本补全（legacy）',
    note: '顶层 prompt 的旧式补全，body 原样透传上游',
    group: '补全 / 向量',
    auth: '网关 Key',
  },
  {
    id: 'ep-embeddings',
    method: 'POST',
    path: '/v1/embeddings',
    name: '向量嵌入',
    note: 'input 支持字符串或字符串数组；响应含 data + usage',
    group: '补全 / 向量',
    auth: '网关 Key',
  },
  {
    id: 'ep-rerank',
    method: 'POST',
    path: '/v1/rerank',
    name: '重排序',
    note: 'Cohere / Jina 兼容形状：model + query + documents[].content（+ 可选 top_n）',
    group: '补全 / 向量',
    auth: '网关 Key',
  },
  {
    id: 'ep-images-gen',
    method: 'POST',
    path: '/v1/images/generations',
    name: '图片生成',
    note: 'JSON 请求，body 透传上游图片通道',
    group: '图片 / 音频',
    auth: '网关 Key',
  },
  {
    id: 'ep-images-edits',
    method: 'POST',
    path: '/v1/images/edits',
    name: '图片编辑',
    note: 'multipart/form-data；必填 image 文件与 prompt，支持可选 mask',
    group: '图片 / 音频',
    auth: '网关 Key',
  },
  {
    id: 'ep-images-assets',
    method: 'GET',
    path: '/v1/images/assets/:name',
    name: '生成资产下载',
    note: '仅在「生图资产落盘」开启时有文件；需有效 Key，单文件 ≤32 MiB',
    group: '图片 / 音频',
    auth: '网关 Key（不校验模型白名单）',
  },
  {
    id: 'ep-audio-tr',
    method: 'POST',
    path: '/v1/audio/transcriptions',
    name: '语音转写',
    note: 'multipart/form-data 上传音频；响应 {text}',
    group: '图片 / 音频',
    auth: '网关 Key',
  },
  {
    id: 'ep-audio-tl',
    method: 'POST',
    path: '/v1/audio/translations',
    name: '语音翻译',
    note: 'multipart/form-data 上传音频；响应 {text}',
    group: '图片 / 音频',
    auth: '网关 Key',
  },
  {
    id: 'ep-audio-speech',
    method: 'POST',
    path: '/v1/audio/speech',
    name: '语音合成',
    note: 'JSON 请求：input + voice（+ model）',
    group: '图片 / 音频',
    auth: '网关 Key',
  },
  {
    id: 'ep-models',
    method: 'GET',
    path: '/v1/models',
    name: '模型列表',
    note: '需有效 Key；带 anthropic-version 或 x-api-key 时返回 Anthropic 原生形态',
    group: '模型与探针',
    auth: '网关 Key（跳过模型白名单）',
  },
  {
    id: 'ep-model-one',
    method: 'GET',
    path: '/v1/models/:model',
    name: '模型详情',
    note: '单模型条目；模型不存在或不在该 Key 白名单内返回 404 / model_not_allowed',
    group: '模型与探针',
    auth: '网关 Key（含模型白名单）',
  },
  {
    id: 'ep-websearch',
    method: 'POST',
    path: '/api/web-search/search · /api/web-search/fetch',
    name: '网络搜索 / 抓取（REST）',
    note: '工具服务数据面，复用网关 Key；另有 /skill/web-search/* 同功能别名',
    group: '工具服务',
    auth: '网关 Key（跳过模型白名单）',
  },
  {
    id: 'ep-health',
    method: 'GET',
    path: '/health',
    name: '健康探针（模型档位表）',
    note: '返回 {模型名: 0..3}；不需要 Key',
    group: '模型与探针',
    auth: '免鉴权',
  },
  {
    id: 'ep-livez',
    method: 'GET',
    path: '/livez · /readyz',
    name: '存活 / 就绪探针',
    note: '编排器探针；/readyz 额外做一次存储读探测',
    group: '模型与探针',
    auth: '免鉴权',
  },
];

/** 拼接示例用的 base（去尾部斜杠）。origin 为空时用占位符。 */
function baseOf(origin: string): string {
  const trimmed = (origin || '').replace(/\/+$/, '');
  return trimmed || 'http://127.0.0.1:9527';
}

const json = (body: unknown): string => JSON.stringify(body, null, 2);

/** 生成全部章节（origin = 页面实际 Base URL） */
export function docsApiChapters(origin: string): DocsChapter[] {
  const base = baseOf(origin);
  const post = (path: string, body: unknown, streaming = false, anthropic = false): string =>
    [
      `curl${streaming ? ' -N' : ''} -X POST "${base}${path}" \\`,
      anthropic
        ? '  -H "x-api-key: sk-xxx" \\'
        : '  -H "Authorization: Bearer sk-xxx" \\',
      ...(anthropic ? ['  -H "anthropic-version: 2023-06-01" \\'] : []),
      '  -H "Content-Type: application/json" \\',
      `  -d '${json(body)}'`,
    ].join('\n');

  const ex = (label: string, code: string, language: DocsExample['language'] = 'bash'): DocsExample => ({
    label,
    code,
    language,
  });

  const chatBody = {
    model: 'deepseek-chat',
    messages: [
      { role: 'system', content: 'You are a helpful assistant.' },
      { role: 'user', content: 'Hello!' },
    ],
    stream: false,
  };

  return [
    /* ── 1. 接入与鉴权 ─────────────────────────────────────────── */
    {
      id: 'start',
      group: 'start',
      title: '接入与鉴权',
      summary: '先定 Base URL、拿到网关 Key、再选客户端支持的协议。',
      path: 'Base URL + Authorization / x-api-key',
      paragraphs: [
        '本文档写的是**数据面**（客户端调模型的 /v1 接口 + 工具服务），不是管理面 /api 接口：管理面要的是面板登录会话，不能拿来调模型。',
        'OpenAI 兼容客户端的 base_url 填「接口地址」里以 /v1 结尾的地址，不要填完整的 /chat/completions。Anthropic 官方 SDK 与 Claude Code 的 ANTHROPIC_BASE_URL 填网关根地址（不带 /v1），SDK 自己追加 /v1/messages。',
        'AIGX 同时挂了**无前缀兼容路由**（/chat/completions、/messages、/models 等，见路由表双挂）：客户端 base_url 只填到根地址也不至于 405。不确定填哪个时，优先用带 /v1 的写法。',
        '从别的设备访问时，127.0.0.1 指向那台设备本身，必须换成实际可达的网关地址（管理员在「系统设置 → 网关」维护监听地址与端口）。',
      ],
      fields: [
        {
          name: 'Authorization: Bearer sk-xxx',
          type: 'HTTP header',
          description:
            'OpenAI 系端点的标准写法。Key 来自「API 密钥」页，必须处于启用状态且未过期、未超额。',
        },
        {
          name: 'x-api-key: sk-xxx',
          type: 'HTTP header',
          description:
            'Anthropic 系端点的标准写法。AIGX 两种头都收：OpenAI 面 Authorization 优先、Anthropic 面 x-api-key 优先，任填一种即可，不要同时塞两把不同的 Key。',
        },
        {
          name: 'Content-Type',
          type: 'HTTP header',
          description:
            'JSON 请求用 application/json；图片编辑与语音转写/翻译是 multipart/form-data（curl -F 会自动写 boundary，不要手写 Content-Type 覆盖它）。',
        },
        {
          name: 'anthropic-version',
          type: 'HTTP header',
          description:
            'Anthropic SDK 自动携带；手写 Messages 请求可用 2023-06-01。GET /v1/models 带它（或带 x-api-key）时返回 Anthropic 原生列表形态。',
        },
      ],
      points: [
        '模型调用始终需要一把启用且可用的网关 Key；面板登录会话不能代替它。',
        '一把启用的 Key 都没有时网关不做鉴权（默认只监听 127.0.0.1；在设置页开启「局域网访问」后监听所有网卡，鉴权与安全闸门随之启用）。这是本机自用形态，不要把这种部署暴露到公网。',
        '每把 Key 可以配「模型白名单」和「提供商白名单」：两者都只收窄、不放宽。调用未列入白名单的模型会被拒（403）。',
        '示例里的 sk-xxx 是占位符，替换成你自己那把 Key；任何真实 Key 都不要写进脚本、日志或文档。',
      ],
      examples: [
        ex(
          'SDK 示例用的环境变量',
          `export AIGX_BASE_URL="${base}"
export AIGX_KEY="sk-xxx"`,
        ),
        ex('读取可用的模型（需要 Key）', `curl "${base}/v1/models" \\\n  -H "Authorization: Bearer sk-xxx"`),
      ],
    },

    /* ── 2. 模型发现与健康检查 ─────────────────────────────────── */
    {
      id: 'models',
      group: 'start',
      title: '模型发现与健康检查',
      summary: '用 /v1/models 拿条目与能力位；/health 只说明模型健康档位，不代表额度可用。',
      path: 'GET /v1/models · GET /v1/models/{model} · GET /health',
      paragraphs: [
        '模型清单是**聚合视图**：启用渠道声明的模型 + 账号型渠道的模型目录 + 全局模型映射 + 自定义模型，去重后按名字广告一条。与控制台里模型下拉是同一份实现（同一个聚合函数），客户端拉得到就等于控制台选得到。',
        '清单**需要有效 Key**（与 A2A 的「不带 Key 也能读」不同）：/v1/models 走列举鉴权 —— 跳过模型白名单，但启用 / 过期 / 额度 / IP / 用户余额照查；没带 Key 直接 401 auth_error。',
        '未定价的模型会被剔出清单：数据面会以「no price configured」拒绝调用，保留在清单里只会让客户端选到必然失败的模型。',
        '不携带 Anthropic 相关头时返回 OpenAI 形态（object=list、data 数组、meta 摘要）；携带 anthropic-version 或 x-api-key 任一时返回 Anthropic 原生形态（data / has_more / first_id / last_id），条目里保留全部能力扩展字段。',
      ],
      fields: [
        {
          name: 'data[].id',
          type: 'string',
          description: '业务请求 model 字段要填的值。别名映射后的名字以清单为准（清单广告的就是能调通的）。',
        },
        {
          name: 'data[].owned_by',
          type: 'string',
          description: '这一条在聚合视图里的归属家（渠道类型或插件 provider）。归属只用于展示与能力来源，不保证每次请求都由它承载——同名模型可能有多条路由。',
        },
        {
          name: 'data[].context_length',
          type: 'number | null',
          description: '目录声明的上下文长度。可能为 null（上游没申报），不等于「无限制」。',
        },
        {
          name: 'data[].capabilities',
          type: 'object',
          description: '上游 / models.dev / 用户覆盖逐键合并后的能力对象（键名如 supportsToolCall、supportsImages），与 cap_sources 同源。',
        },
        {
          name: 'data[].supports_tool_call / supports_images / supports_video / supports_reasoning / supports_embedding / supports_rerank',
          type: 'boolean',
          description: '六个出口布尔位。缺省 false = 未声明支持；未声明不等于一定失败，但以声明为准挑模型更稳。',
        },
        {
          name: 'data[].input_modalities',
          type: 'string[]',
          description: '与 supports_images / supports_video 同源推导：恒含 text，声明支持图片/视频时追加。上游没声明的模态不猜。',
        },
        {
          name: 'data[].reasoning_levels',
          type: 'string[] | null',
          description: '该模型的思考档位（客户端据此决定 reasoning_effort 发哪一档）。空/缺省表示未申报。',
        },
        {
          name: 'data[].cap_sources',
          type: 'object',
          description: '每个能力键的来源（user / upstream / modelsdev / null），控制台用于展示可信度，客户端可忽略。',
        },
        {
          name: 'data[].tool_calling_mode',
          type: 'string（可选）',
          description: '仅在模型元信息里带该键时输出；不同上游语义不同（如 function / native）。缺省不输出该键。',
        },
        {
          name: 'meta.source / meta.count',
          type: 'string / number',
          description: 'OpenAI 形态的摘要（source 恒为 aggregate）。Anthropic 形态没有 meta，改用 has_more / first_id / last_id。',
        },
        {
          name: 'GET /health',
          type: 'JSON object',
          description: '返回 `{ "<模型名>": 0|1|2|3 }` 的健康档位表（0 最健康）。它不是 status=ok 那种存活探针：存活请看 /livez、就绪看 /readyz。',
        },
      ],
      points: [
        '清单只说明「这个网关广告了哪些模型」，不保证此刻有可用账号或额度：真正调用仍可能 503 no_bridge / 429 rate_limit_exceeded。',
        '/v1/models/{model} 走完整校验（含模型白名单）：Key 白名单里没有该模型时，详情也会被拒。',
        'AIGX 不做「配 Key 之前先拉一遍模型」的免鉴权路径；配置客户端时先建好 Key。',
      ],
      examples: [
        ex('OpenAI 形态的清单结构', json({
          object: 'list',
          data: [
            {
              id: 'deepseek-chat',
              object: 'model',
              created: 1735689600,
              owned_by: 'deepseek',
              context_length: 65536,
              capabilities: { supportsToolCall: true, supportsImages: false },
              supports_tool_call: true,
              supports_images: false,
              supports_video: false,
              supports_reasoning: false,
              supports_embedding: false,
              supports_rerank: false,
              input_modalities: ['text'],
              reasoning_levels: null,
              cap_sources: { supportsToolCall: 'upstream', supportsImages: 'upstream' },
            },
          ],
          meta: { source: 'aggregate', count: 1 },
        }), 'json'),
        ex('Anthropic 形态（带 x-api-key 或 anthropic-version）', json({
          data: [
            {
              id: 'deepseek-chat',
              object: 'model',
              created: 1735689600,
              owned_by: 'deepseek',
              type: 'model',
              display_name: 'deepseek-chat',
              created_at: '2025-01-01T00:00:00Z',
              supports_tool_call: true,
              supports_images: false,
            },
          ],
          has_more: false,
          first_id: 'deepseek-chat',
          last_id: 'deepseek-chat',
        }), 'json'),
        ex('查看健康档位表', `curl "${base}/health"`),
      ],
    },

    /* ── 3. OpenAI Chat Completions ───────────────────────────── */
    {
      id: 'chat',
      group: 'chat',
      title: 'OpenAI Chat Completions',
      summary: '通用对话入口，支持流式；历史由客户端每次携带。',
      path: 'POST /v1/chat/completions',
      paragraphs: [
        '非流式读 choices[].message；流式读 choices[].delta。网关不会凭上一条响应的 id 自动恢复对话：多轮请把 history 整段带上。',
        'AIGX 在转发前会做一层「请求治理」（破甲控制台，见后文）：system 合并/去重、字面改写、同义替换、思考档位压降都可能改变实际发往上游的文本。响应结构与协议字段不受影响。',
      ],
      fields: [
        {
          name: 'model',
          type: 'string',
          description: '推荐显式填 /v1/models 里的 id。完全不带 model 时回落到网关默认模型（管理员配置）；显式空串按缺失处理，返回 400 invalid_model。',
        },
        {
          name: 'messages',
          type: 'array（必填）',
          description: 'role + content。常见角色 system / user / assistant，工具结果用 tool。文本可给字符串，多模态给内容块数组。清洗后没有任何有效消息返回 400 invalid_messages。',
        },
        {
          name: 'stream',
          type: 'boolean',
          description: '默认 false。true 返回 text/event-stream；curl 看流式要加 -N，客户端要按完整 SSE 事件解析，不要按网络分块切。',
        },
        {
          name: 'stream_options.include_usage',
          type: 'object',
          description: '仅流式时提取并透传（非流式忽略）。上游是否真的给终帧 usage 由上游决定。',
        },
        {
          name: 'max_tokens',
          type: 'integer',
          description: '输出上限。max_completion_tokens 也会透传（二选一，别同时给冲突值）。',
        },
        {
          name: 'temperature / top_p / stop',
          type: 'number / array',
          description: '采样与停止序列，原样透传上游。协议兼容不代表每家上游都支持全部调参项。',
        },
        {
          name: 'tools / tool_choice',
          type: 'array / string|object',
          description: '函数工具声明与选择策略。结果为 role=tool 的消息，tool_call_id 必须对应，别丢并行调用的结果。',
        },
        {
          name: 'response_format / seed / presence_penalty / frequency_penalty / n / parallel_tool_calls / logit_bias / service_tier',
          type: '透传白名单',
          description: '这些字段被显式提取进上游请求体（白名单透传）。是否生效取决于上游与目标模型。',
        },
        {
          name: 'reasoning_effort',
          type: 'string',
          description: '思考档位。未填时按模型映射里的档位补齐，再经破甲控制台的降档钳制（high/medium/xhigh 可能被压到 reasoning_floor）。',
        },
        {
          name: 'web_search_options',
          type: 'object',
          description: '透传上游（若该渠道支持）。不是网关统一能力。',
        },
      ],
      points: [
        'Chat 流式通常以 data: [DONE] 收尾；工具参数可能跨多个增量拼接，需要自己累积。',
        'usage 里的 prompt_tokens / completion_tokens / total_tokens 是计费口径的用量，不是货币价格。',
        '流式一旦开始，HTTP 状态码无法再改：即使先收到 200，也要检查流内错误与是否正常收尾。',
        '渠道 failover 在网关内完成：某条渠道报可重试错误时自动切下一条，客户端拿到的是最终那条的结果（换渠道仅限「明确未完成」的错误，避免重复计费）。',
      ],
      examples: [
        ex('非流式对话请求', post('/v1/chat/completions', chatBody)),
        ex(
          '流式对话请求',
          post('/v1/chat/completions', { ...chatBody, stream: true, stream_options: { include_usage: true } }, true),
        ),
        ex(
          '非流式响应结构',
          json({
            id: 'chatcmpl-EXAMPLE',
            object: 'chat.completion',
            model: 'deepseek-chat',
            choices: [
              { index: 0, message: { role: 'assistant', content: 'Hello!' }, finish_reason: 'stop' },
            ],
            usage: { prompt_tokens: 12, completion_tokens: 4, total_tokens: 16 },
          }),
          'json',
        ),
      ],
    },

    /* ── 4. Responses ─────────────────────────────────────────── */
    {
      id: 'responses',
      group: 'chat',
      title: 'OpenAI Responses',
      summary: '兼容 Responses 请求与事件格式；优先透传，渠道不支持时降级转 Chat。',
      path: 'POST /v1/responses',
      paragraphs: [
        '处理顺序：先原样透传给上游的 /v1/responses；只有会话型渠道报出「不支持该端点」这类配置错误时，才降级成 Chat Completions 转换再拼回 Responses 形状。传输类错误照旧 failover，不会偷偷改协议。',
        '网关不需要（也不保存）服务端会话状态：历史由客户端每次带上。',
      ],
      fields: [
        {
          name: 'model',
          type: 'string',
          description: '与 Chat 同一份模型目录、同一套 Key 授权规则；完全不带时回落网关默认模型。',
        },
        {
          name: 'input',
          type: 'string | array',
          description: '单轮可以是字符串；多轮用消息 / 工具项数组。文本块 input_text、图片块 input_image。',
        },
        {
          name: 'instructions',
          type: 'string',
          description: '系统说明，对应 Chat 的 system 消息；每次请求都要重新带。',
        },
        {
          name: 'stream',
          type: 'boolean',
          description: '默认 false。true 返回 Responses 命名事件（response.created / response.output_text.delta / response.completed …）。',
        },
        {
          name: 'max_output_tokens',
          type: 'integer',
          description: '输出预算；达到上限时以 incomplete 状态收尾（看 incomplete_details）。',
        },
        {
          name: 'tools / tool_choice',
          type: 'array',
          description: 'Responses 形态的函数工具声明；结果用 function_call_output 带相同 call_id 回传。',
        },
      ],
      points: [
        '降级链路是「网关侧转换」，不等价于上游原生 Responses：上游原生才有的字段在降级后可能被忽略。',
        '非流式返回 object=response、status、output 数组与 output_text。output 里也可能有 reasoning / function_call，别只读第一项。',
        '流式错误可能以 response.failed 结束；输出预算不足时检查 incomplete 状态。',
      ],
      examples: [
        ex(
          'Responses 请求',
          post('/v1/responses', {
            model: 'deepseek-chat',
            instructions: 'You are a helpful assistant.',
            input: 'Hello!',
            max_output_tokens: 1024,
            stream: false,
          }),
        ),
        ex(
          'Responses 响应结构',
          json({
            id: 'resp_EXAMPLE',
            object: 'response',
            status: 'completed',
            model: 'deepseek-chat',
            output: [
              { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Hello!' }] },
            ],
            output_text: 'Hello!',
            usage: { input_tokens: 12, output_tokens: 4, total_tokens: 16 },
          }),
          'json',
        ),
      ],
    },

    /* ── 5. Anthropic Messages ─────────────────────────────────── */
    {
      id: 'messages',
      group: 'chat',
      title: 'Anthropic Messages',
      summary: '兼容 Anthropic 消息块与 SSE 事件；本地 Token 估算端点单独可用。',
      path: 'POST /v1/messages · POST /v1/messages/count_tokens',
      paragraphs: [
        '模型仍从网关目录里选，不要求模型名以 claude 开头；跨协议转发由网关转换消息块（text / image / tool_use / tool_result）。',
        'count_tokens 是**本地估算**：按字符数 / 3.5 取整、返回 estimated=true，不调上游分词器，也不消耗额度。Claude Code 启动时会探测它。',
      ],
      fields: [
        {
          name: 'model',
          type: 'string',
          description: '完全不带时回落网关默认模型；都没有则 400 invalid_request_error（Missing model field）。',
        },
        {
          name: 'messages',
          type: 'array（必填）',
          description: 'user / assistant 角色；content 可以是字符串或 text / image / tool_use / tool_result 块数组。',
        },
        {
          name: 'system',
          type: 'string | array',
          description: '顶层字段，不要塞进 messages 的 system 角色；支持文本或 text 块数组。',
        },
        {
          name: 'max_tokens',
          type: 'integer',
          description: 'Anthropic 协议要求显式给出；上游实际预算可能更低。',
        },
        {
          name: 'stream / stop_sequences',
          type: 'boolean / string[]',
          description: 'stream=true 返回 Anthropic 事件流（message_start → content_block_* → message_delta → message_stop）；不要按 Chat 的 [DONE] 解析。',
        },
        {
          name: 'tools / tool_choice',
          type: 'array / object',
          description: '函数工具声明含 name / description / input_schema；结果放在后续 user 消息的 tool_result 块里，带对应 tool_use_id。',
        },
        {
          name: 'metadata.user_id',
          type: 'string',
          description: 'Anthropic SDK 标准字段，AIGX 额外用它做会话粘性标识（同一 user_id 在粘滞窗口内优先落到同一渠道）。不填则用 Key 所属用户。',
        },
        {
          name: 'count_tokens 响应',
          type: 'JSON',
          description: '返回 `{ "count": N, "estimated": true }`。字段名与 Anthropic 官方 input_tokens 不同，按 count 读。',
        },
      ],
      points: [
        'count_tokens 跳过模型白名单（不转发上游），但仍校验启用 / 过期 / 额度 / IP / 用户余额。',
        '鉴权/限流错误用 Anthropic 信封 `{type:"error", error:{type, message}}`；count_tokens 在 Key 缺失时返回 type=authentication_error（401），额度耗尽返回 402。',
        '非流式返回 type=message、content 块数组、stop_reason 与 usage；content 里可能同时有文本、思考与工具调用，按 type 遍历。',
      ],
      examples: [
        ex(
          'Messages 请求',
          post(
            '/v1/messages',
            {
              model: 'deepseek-chat',
              max_tokens: 1024,
              system: 'You are a helpful assistant.',
              messages: [{ role: 'user', content: 'Hello!' }],
              stream: false,
            },
            false,
            true,
          ),
        ),
        ex(
          'Messages 响应结构',
          json({
            id: 'msg_EXAMPLE',
            type: 'message',
            role: 'assistant',
            model: 'deepseek-chat',
            content: [{ type: 'text', text: 'Hello!' }],
            stop_reason: 'end_turn',
            usage: { input_tokens: 12, output_tokens: 4 },
          }),
          'json',
        ),
        ex(
          '估算输入 Token 数',
          post(
            '/v1/messages/count_tokens',
            { model: 'deepseek-chat', system: 'You are a helpful assistant.', messages: [{ role: 'user', content: 'Hello!' }] },
            false,
            true,
          ),
        ),
        ex('估算响应', json({ count: 42, estimated: true }), 'json'),
      ],
    },

    /* ── 6. 看图文与工具调用 ───────────────────────────────────── */
    {
      id: 'vision-tools',
      group: 'chat',
      title: '看图与工具调用',
      summary: '图片输入是「理解图片」，工具调用是「客户端执行动作」，两者都依赖模型能力位。',
      paragraphs: [
        '看图用对话模型，先确认该模型条目 supports_images=true。Chat 用 text + image_url 内容块；图片值可为上游支持的 HTTP(S) URL 或 data URL。Anthropic 面用 image 块（source.type=base64 或 url）。',
        '自定义函数工具由客户端声明、客户端执行：模型返回 tool_calls（Anthropic 是 tool_use）后，校验参数、执行函数，再把「助手调用 + 结果」一起放进下一轮 messages；tool_call_id / tool_use_id 必须对应，并行调用的结果一条都不能漏。',
        '网关只做协议转换与转发，不替客户端执行函数，也不提供托管工具。',
      ],
      warning:
        '图片输入受模型能力与上游限制（格式、体积、张数）。大图失败先缩小或转 data URL 重试，别把「协议字段被收下」当成「上游一定能看图」。',
      examples: [
        ex(
          '对话模型看图请求',
          post('/v1/chat/completions', {
            model: 'deepseek-chat',
            messages: [
              {
                role: 'user',
                content: [
                  { type: 'text', text: 'Describe this image.' },
                  { type: 'image_url', image_url: { url: 'https://YOUR_IMAGE_HOST/reference.png' } },
                ],
              },
            ],
            stream: false,
          }),
        ),
        ex(
          '声明函数工具的请求体',
          json({
            model: 'deepseek-chat',
            messages: [{ role: 'user', content: 'What is the weather in Beijing?' }],
            tools: [
              {
                type: 'function',
                function: {
                  name: 'get_weather',
                  description: 'Get the weather for a city.',
                  parameters: {
                    type: 'object',
                    properties: { city: { type: 'string' } },
                    required: ['city'],
                    additionalProperties: false,
                  },
                },
              },
            ],
            tool_choice: 'auto',
            stream: false,
          }),
          'json',
        ),
        ex(
          '执行工具后回传结果',
          json({
            model: 'deepseek-chat',
            messages: [
              { role: 'user', content: 'What is the weather in Beijing?' },
              {
                role: 'assistant',
                content: null,
                tool_calls: [
                  {
                    id: 'call_EXAMPLE',
                    type: 'function',
                    function: { name: 'get_weather', arguments: '{"city":"Beijing"}' },
                  },
                ],
              },
              { role: 'tool', tool_call_id: 'call_EXAMPLE', content: '{"temperature_c":22}' },
            ],
            stream: false,
          }),
          'json',
        ),
      ],
    },

    /* ── 7. 补全 / 向量 / 重排 ─────────────────────────────────── */
    {
      id: 'completions-embeddings',
      group: 'media',
      title: '文本补全、向量与重排',
      summary: '三个非对话端点：legacy 补全、embeddings、rerank。',
      path: 'POST /v1/completions · POST /v1/embeddings · POST /v1/rerank',
      paragraphs: [
        '这三个端点都要求 model 字段（缺了返回 400 invalid_model），都走同一套 Key 鉴权 + 渠道 failover + 用量计费。',
        'rerank 用 Cohere / Jina 兼容形状：documents 是对象数组，每项带 content。',
      ],
      fields: [
        {
          name: '/v1/completions · prompt',
          type: 'string | string[]',
          description: '旧式补全的顶层 prompt（不是 messages），body 原样透传给上游 /completions。',
        },
        {
          name: '/v1/embeddings · input',
          type: 'string | string[]（必填）',
          description: '要向量化的文本。形状不对返回 400 invalid_input；响应含 data 与 usage（prompt_tokens / total_tokens）。',
        },
        {
          name: '/v1/rerank · query',
          type: 'string（必填）',
          description: '查询文本；缺失返回 400 invalid_query。',
        },
        {
          name: '/v1/rerank · documents',
          type: 'array（必填）',
          description: '对象数组，每项 `{ "content": "…" }`；形状不对返回 400 invalid_documents。',
        },
        {
          name: '/v1/rerank · top_n',
          type: 'integer（可选）',
          description: '只要前 N 条结果。响应为 `results: [{index, relevance_score}]` + usage。',
        },
      ],
      points: [
        '向量与重排模型在 /v1/models 里以 supports_embedding / supports_rerank 标注；拿对话模型打这两个端点通常 503 no_bridge。',
        '这三个端点同样受「网络层停用」闸门约束：管理员关掉网络层后返回 503 network_layer_disabled。',
      ],
      examples: [
        ex('向量嵌入', post('/v1/embeddings', { model: 'text-embedding-3-small', input: ['第一条', '第二条'] })),
        ex(
          '重排序',
          post('/v1/rerank', {
            model: 'bge-reranker',
            query: '什么是向量检索',
            documents: [{ content: '向量检索用近似最近邻找相似文本。' }, { content: '今天天气不错。' }],
            top_n: 1,
          }),
        ),
      ],
    },

    /* ── 8. 图片生成与编辑 ────────────────────────────────────── */
    {
      id: 'images',
      group: 'media',
      title: '图片生成、编辑与资产下载',
      summary: '生成走 JSON 透传；编辑是 multipart（必填 image + prompt，可选 mask）。',
      path: 'POST /v1/images/generations · POST /v1/images/edits · GET /v1/images/assets/{name}',
      paragraphs: [
        '生成端点把请求体原样透传给该渠道的图片通道：size / quality / style / response_format / n 这些字段是否被接受、取值范围如何，由上游图片模型决定，网关不额外改写参数（除了按渠道映射改写 model）。',
        '编辑端点必须同时带 image 文件与 prompt（缺任一项 400 invalid_request，文案 Missing prompt field）。multipart 表单项上限 128 个，总请求体受「最大请求体」设置约束。',
        '生成资产下载只在管理员打开「生图资产落盘」后有文件：服务把响应里的 b64_json 存进数据目录的 generated_images/，并按总容量 LRU 清理；接口不生成「临时下载令牌」那种无鉴权链接。',
      ],
      fields: [
        {
          name: 'model',
          type: 'string（必填）',
          description: '图片模型 id。缺了返回 400 invalid_model；未在该 Key 白名单内被拒。',
        },
        {
          name: 'prompt',
          type: 'string（编辑必填）',
          description: '描述主体、构图、修改目标。当前编辑，想保留的内容与要改的内容都要写清楚。',
        },
        {
          name: 'image / image[]',
          type: 'multipart file（编辑必填）',
          description: '单图用 image=@ref.png，多图重复 image[]。表单里其他普通参数不要重复提交。',
        },
        {
          name: 'mask',
          type: 'multipart file（可选）',
          description: '可选遮罩文件，原样透传上游；上游是否支持由图片模型决定。',
        },
        {
          name: 'size / quality / style / response_format / n',
          type: '透传',
          description: '原样透传上游。response_format=b64_json 时响应里是可直接解码的 base64（不带 data URL 前缀）。',
        },
        {
          name: 'GET /v1/images/assets/{name}',
          type: '下载',
          description: '需有效 Key（列举鉴权，跳过模型白名单）。文件名只认安全形态；单文件 ≤32 MiB；破例返回 404 asset_not_found / 413 request_too_large / 400 invalid_request。',
        },
      ],
      points: [
        'multipart 的 Content-Type 由 curl -F 自动生成带 boundary 的版本，不要手写覆盖。',
        '生图可能消耗账号额度：先小尺寸、n=1 确认能力；上游「可能已完成」的超时不要自动重发，避免重复扣费。',
        '资产落盘关闭时 /v1/images/assets/* 一律 404——需要长期保存就在客户端把 b64_json 自行落盘。',
      ],
      warning: '不要把 base64 图片塞回对话文本、日志或 Git；那不是网关的存储，也不受 LRU 清理保护。',
      examples: [
        ex(
          '图片生成请求（JSON）',
          post('/v1/images/generations', {
            model: 'IMAGE_MODEL_ID',
            prompt: 'A small raccoon reading a book, flat illustration.',
            n: 1,
            size: '1024x1024',
            response_format: 'b64_json',
          }),
        ),
        ex(
          '参考图编辑（multipart）',
          [
            `curl -X POST "${base}/v1/images/edits" \\`,
            '  -H "Authorization: Bearer sk-xxx" \\',
            '  -F "model=IMAGE_MODEL_ID" \\',
            '  -F "image=@reference.png" \\',
            '  --form-string "prompt=Replace the background with a sunny garden." \\',
            '  -F "n=1" \\',
            '  -F "response_format=b64_json"',
          ].join('\n'),
        ),
        ex(
          '图片响应结构（b64）',
          json({ created: 1700000000, data: [{ b64_json: 'BASE64_IMAGE_DATA' }] }),
          'json',
        ),
      ],
    },

    /* ── 9. 音频 ──────────────────────────────────────────────── */
    {
      id: 'audio',
      group: 'media',
      title: '语音转写、翻译与合成',
      summary: '转写/翻译是 multipart 上传音频；合成是 JSON（input + voice）。',
      path: 'POST /v1/audio/transcriptions · /v1/audio/translations · /v1/audio/speech',
      paragraphs: [
        '转写与翻译都要 multipart 上传音频文件并带 model；读不到请求体返回 400 read_error，上游失败按 api_error 返回。成功响应只有 `{ "text": "…" }`。',
        '语音合成走 JSON：input（要念的文本）与 voice 必填，model 可选；上游返回的音频按上游的 Content-Type 原样给客户端。',
      ],
      fields: [
        {
          name: 'file',
          type: 'multipart file（转写/翻译）',
          description: '音频文件；格式与体积上限由上游音频模型决定。',
        },
        {
          name: 'model',
          type: 'string（必填）',
          description: '音频模型 id；缺了返回 400 invalid_model。',
        },
        {
          name: 'input / voice',
          type: 'string（合成必填）',
          description: '要合成的文本与音色名；缺任一项 400 invalid_request。',
        },
      ],
      points: [
        '音频端点同样受网络层闸门与限流约束（503 network_layer_disabled / 429 rate_limit_exceeded）。',
        '转写结果只有 text，不返回分段/时间戳结构；需要逐句时间轴请换支持该能力的上游直连。',
      ],
      examples: [
        ex(
          '语音转写',
          [
            `curl -X POST "${base}/v1/audio/transcriptions" \\`,
            '  -H "Authorization: Bearer sk-xxx" \\',
            '  -F "model=WHISPER_MODEL_ID" \\',
            '  -F "file=@sample.mp3"',
          ].join('\n'),
        ),
        ex('语音合成', post('/v1/audio/speech', { model: 'TTS_MODEL_ID', input: '你好，这是一段测试语音。', voice: 'alloy' })),
      ],
    },

    /* ── 10. 工具服务 ─────────────────────────────────────────── */
    {
      id: 'tool-services',
      group: 'tools',
      title: '工具服务（网络搜索 / 抓取 / MCP）',
      summary: '复用网关 Key 的工具数据面：REST 与 MCP 两种形态，路径与上游逐字对齐。',
      path: 'POST /api/web-search/search · /api/web-search/fetch · /skill/web-search/* · /mcp/web-search · /mcp/vision',
      paragraphs: [
        '工具服务不是 /v1 对话协议，也不是管理面接口：它同样是「拿网关 Key 就能调」的数据面，鉴权跳过模型白名单（工具调用没有具体模型），但保留启用 / 过期 / 额度 / IP / 用户余额检查。',
        'REST 形态两套路径同功能：/api/web-search/* 与 /skill/web-search/*（后者与上游客户端写死的地址逐字一致，改路径会影响已接入方）。元数据端点（GET /api/web-search、GET /skill/web-search）免鉴权。',
        'MCP 形态是 Streamable HTTP：POST /mcp/web-search 与 POST /mcp/vision，同一个 Key。',
        '抓取侧关闭了自动重定向——「公网白名单」只校验初始地址，跟随重定向会让公网域名跳到 127.0.0.1 绕过检查。',
      ],
      fields: [
        {
          name: 'GET /api/web-search',
          type: '元数据（免鉴权）',
          description: '返回该网关支持的工具与版本信息，便于客户端自发现。',
        },
        {
          name: 'POST /api/web-search/search',
          type: '批量搜索',
          description: '按上游协议提交查询；账号型搜索遵守 Key 的提供商白名单。',
        },
        {
          name: 'POST /api/web-search/fetch',
          type: '批量抓取',
          description: '抓取给定 URL 的正文；只接受公网地址，本机/内网/重定向到内网一律拒绝。',
        },
        {
          name: 'POST /mcp/web-search · POST /mcp/vision',
          type: 'MCP（Streamable HTTP）',
          description: '按 MCP 协议做 JSON-RPC 交互；vision 提供图片理解工具。',
        },
      ],
      points: [
        '工具服务与管理面 `/api/tools*` 是两回事：后者要管理员会话，用于配置与连通性测试。',
        '页面上的「工具服务」开关决定这些端点是否可用；关闭后调它们会被拒。',
      ],
      examples: [
        ex('读取工具元数据（免鉴权）', `curl "${base}/api/web-search"`),
        ex(
          '一次搜索调用',
          `curl -X POST "${base}/api/web-search/search" \\
  -H "Authorization: Bearer sk-xxx" \\
  -H "Content-Type: application/json" \\
  -d '{"queries":["AIGX 网关"]}'`,
        ),
      ],
    },

    /* ── 11. AIGX 独有能力 ────────────────────────────────────── */
    {
      id: 'aigx-only',
      group: 'aigx',
      title: 'AIGX 独有能力（客户端可见的部分）',
      summary: '账号池、破甲治理、渠道 failover、插件 provider、会话粘性——客户端会真实感受到的差异。',
      paragraphs: [
        '这一章只写**客户端调用会感知到**的行为，全部对应真实实现。纯管理面能力（签到、账目、渠道锁定等）不在这里列，它们不影响 /v1 请求语义；渠道锁定只作用于「AI 运维 Agent」页自身的对话，不作用于外部客户端。',
        '① 账号池与故障转移：同一个模型名可能挂在多个渠道/账号上。某条候选报「明确未完成」的错误时网关自动切下一条，客户端只看到最终结果；「在途丢失」这类可能已计费的错误不重发，避免重复扣费。候选全部不可用时返回 503 no_bridge。',
        '② 破甲控制台（请求治理）：数据面转发前可做 system 合并去重、字面改写、同义词替换、末条 user 追加结构化要求、按关键词注入技能片段、以及思考档位降档（reasoning_effort 被钳到 reasoning_floor）。开关关闭时请求逐字节不变。默认词表只含中性改写示例，是否开启由管理员决定。',
        '③ 插件 provider：插件渠道可以往 /v1/models 里追加模型（带自己的 provider 归属与能力位），对这些模型发起请求会走插件桥接；插件没接入相应能力时该模型不可用。',
        '④ 会话粘性：同一 Key（或 Anthropic metadata.user_id）在粘滞窗口内优先落到同一渠道，减少同会话跨渠道抖动；窗口可按设置调整，关掉后每请求独立选路。',
        '⑤ 模型别名与映射：管理员可给模型起别名、把客户端模型名映射到上游真实名。映射后模型不在清单里（或清单名字不同）时，以清单广告的名字为准最稳。',
        '⑥ 未定价模型拒绝：没配价格的模型在 /v1/models 里不出现，直接调也会被拒（model_not_priced）——这是「清单能看到就能调通」的配套约束。',
      ],
      points: [
        '破甲与安全护栏是两层：安全护栏（guardrail）命中时返回 400 content_blocked / guardrail_blocked；破甲是请求治理，不改协议结构。',
        '网络层可被管理员整体停用：数据面返回 503 network_layer_disabled，管理面不受影响。',
        '「签到」「账号额度」这类站点运营能力属于管理面，客户端不需要（也无法）调用。',
      ],
      warning: '遇到 503 no_bridge 先看是不是所有候选渠道都不可用（未配置、被熔断、冷却中），而不是逐字改请求体重试。',
    },

    /* ── 12. 错误与限额 ───────────────────────────────────────── */
    {
      id: 'errors',
      group: 'errors',
      title: '错误、限额与兼容边界',
      summary: '按 HTTP 状态 + error.code 判断；不要盲目重试收费请求。',
      path: 'error.code · HTTP status',
      paragraphs: [
        'OpenAI 系端点的错误信封是 `{"error":{"message","type","param","code"}}`：type 固定 invalid_request_error（即使是鉴权失败），真正的分类看 code。Anthropic 系端点用 `{"type":"error","error":{"type","message"}}`，type 取 authentication_error / invalid_request_error / rate_limit_error / service_unavailable。',
        '本文档只写 AIGX 实际提供的端点。没列出的（Files、Batch、文件上传、服务端会话存储、图片 variations 等）不要假设存在。',
      ],
      fields: [
        {
          name: '400 · invalid_model / invalid_messages / invalid_input / invalid_query / invalid_documents / invalid_request',
          type: 'HTTP 400',
          description: '请求形状不对：缺 model、messages 清洗后为空、embeddings 的 input 形状不对、rerank 缺 query 或 documents、multipart 缺 image/prompt 等。按 message 修请求后再发。',
        },
        {
          name: '400 · content_blocked / guardrail_blocked',
          type: 'HTTP 400',
          description: '命中安全护栏（输入侧 reject 或输出侧拦截）。降级模式下网关会换中性提示词原地重发一次，仍被拒才回这个错。',
        },
        {
          name: '401 · auth_error',
          type: 'HTTP 401',
          description: '没带 Key、Key 不存在或已停用、Key 已被撤销。检查 Authorization / x-api-key 头与「API 密钥」页状态。',
        },
        {
          name: '403 · auth_error / model_not_allowed / ip_blocked',
          type: 'HTTP 403',
          description: 'Key 有效但没权限：过期、超额、模型不在白名单、IP 不在允许表，或绑定用户余额耗尽（这些在 auth_error 的 message 里区分；模型白名单单独给 model_not_allowed）。',
        },
        {
          name: '403 · insufficient_quota',
          type: 'HTTP 402/403',
          description: '额度不足（含用户余额耗尽）。充值或在「API 密钥」页调整额度上限后才可能恢复。',
        },
        {
          name: '404 · model_not_found',
          type: 'HTTP 404',
          description: '模型不存在、已关闭、不在该 Key 授权范围，或路径拼错；GET /v1/models/{model} 与对话端点同口径。',
        },
        {
          name: '404 · asset_not_found',
          type: 'HTTP 404',
          description: '生成资产不存在、已被 LRU 清理、落盘开关没开，或文件名不是安全形态。',
        },
        {
          name: '400 · model_not_priced',
          type: 'HTTP 400',
          description: '该模型没配价格，数据面拒绝转发（避免免费用量）。让管理员补价格，或换清单里已定价的模型。',
        },
        {
          name: '404/503 · no_bridge',
          type: 'HTTP 503',
          description: '没有任何可用候选渠道（未配置、被禁用、冷却、熔断）。这是网关侧状态，重发同样的请求通常仍失败。',
        },
        {
          name: '413 · request_too_large',
          type: 'HTTP 413',
          description: '请求体或资产超过上限。缩小图片/音频、减少参考图数量后重试；原样重发不会解决。',
        },
        {
          name: '429 · rate_limit_exceeded（Anthropic 面 rate_limit_error）',
          type: 'HTTP 429',
          description: '限流。message 里带 Retry after 秒数；按它退避，不要并发重试把额度打空。',
        },
        {
          name: '503 · network_layer_disabled',
          type: 'HTTP 503',
          description: '管理员关闭了网络层（推理转发整体停用）。管理面仍可用，数据面请求一律被拒。',
        },
      ],
      warning:
        '超时、断线、客户端取消不代表上游撤销了任务：图片这类可能已计费的请求不要自动整条重发（网关已停止重试的错误再次提交就是重复扣费）。',
      points: [
        '三种协议都要检查「流内」的结束状态：HTTP 200 本身不足以证明成功（Chat 看 [DONE]、Responses 看 response.completed、Anthropic 看 message_stop）。',
        '协议兼容不保证能力齐全：工具、思考、结构化输出、图片/音频参数支持与否，先看模型条目能力位与上游版本。',
        '网关的重试（换渠道）与客户端 SDK 的重试是两层独立行为，可能叠加；对收费端点建议客户端把自动重试关掉。',
        '请求日志里能看承载渠道、错误摘要与是否被破甲改写——排错优先看日志而不是猜协议。',
      ],
      examples: [
        ex(
          'OpenAI 系错误结构',
          json({
            error: {
              message: "Model 'UNKNOWN_MODEL' not found",
              type: 'invalid_request_error',
              param: null,
              code: 'model_not_found',
            },
          }),
          'json',
        ),
        ex(
          'Anthropic 系错误结构',
          json({ type: 'error', error: { type: 'authentication_error', message: 'Missing API key' } }),
          'json',
        ),
      ],
    },
  ];
}

/** 章节搜索匹配：空格分词后要求全部命中（标题 / 摘要 / 路径 / 正文 / 字段 / 示例） */
export function docsChapterMatches(chapter: DocsChapter, query: string): boolean {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return true;
  const haystack = [
    chapter.title,
    chapter.summary,
    chapter.path ?? '',
    ...(chapter.paragraphs ?? []),
    chapter.warning ?? '',
    ...(chapter.points ?? []),
    ...(chapter.fields ?? []).flatMap((field) => [field.name, field.type, field.description]),
    ...(chapter.examples ?? []).flatMap((example) => [example.label, example.code]),
  ]
    .join('\n')
    .toLowerCase();
  return terms.every((term) => haystack.includes(term));
}
