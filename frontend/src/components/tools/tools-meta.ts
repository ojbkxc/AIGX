// Ported from Agent2API v3.5.0-beta.6 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// MCP & Skills 页的展示元数据（列表页 / 详情页共用）。
// 对照 A2A ui-islands tools-data.ts（SERVICE_NAME / SERVICE_DESC / PROTOCOL_DESC）
// 与 tools-detail.tsx（CAPABILITY_DESCRIPTIONS）。
//
// 所有文案都写成**字面量 t('中文')**：i18n 扫描器只登记字面量调用，
// 改成「中文表 + t(变量)」会让语言包永远缺这几条（与 ToolsPage 旧注释同一约束）。
// 注意：本文件是 .ts，扫描器只扫 .tsx —— 新增 key 必须手工补进 zh/en。

import type { ToolProtocol } from '../../types';

/** 翻译函数（允许插值参数，便于模块级 helper 复用） */
type TFn = (key: string, values?: Record<string, string | number>) => string;

/** 服务 id → 名称 */
export function serviceName(t: TFn, id: string): string {
  switch (id) {
    case 'web-search':
      return t('网络搜索');
    case 'vision':
      return t('视觉分析');
    case 'image-generation':
      return t('图片生成');
    default:
      return id;
  }
}

/** 服务 id → 描述 */
export function serviceDesc(t: TFn, id: string): string {
  switch (id) {
    case 'web-search':
      return t('聚合标准搜索服务及已有账号的搜索能力；网页抓取仍使用标准服务。');
    case 'vision':
      return t('把图片与提示词交给后台配置的视觉模型分析，并把模型回复返回给调用方的大模型。');
    case 'image-generation':
      return t('使用已有账号生成或编辑图片，支持任务查询与本机文件保存。');
    default:
      return '';
  }
}

/** 接入协议说明 */
export function protocolDesc(t: TFn, protocol: ToolProtocol): string {
  switch (protocol) {
    case 'skill':
      return t('打包为 Agent Skill，由面板安装到本机客户端目录。');
    case 'mcp':
      return t('作为远程 MCP 服务，在支持 MCP 的客户端配置里填写地址即可接入。');
    default:
      return t('提供 REST API，任何能发 HTTP 请求的程序都可以携带网关 Key 直接调用。');
  }
}

export const PROTOCOL_LABEL: Record<ToolProtocol, string> = {
  skill: 'Skill',
  mcp: 'MCP',
  api: 'API',
};

export function protocolTone(protocol: ToolProtocol): 'info' | 'success' | 'neutral' {
  if (protocol === 'skill') return 'info';
  if (protocol === 'mcp') return 'success';
  return 'neutral';
}

export interface CapabilityParam {
  name: string;
  type: string;
  desc: string;
  required: boolean;
}

export interface CapabilityGroup {
  title: string;
  desc: string;
  params: CapabilityParam[];
}

/**
 * 服务能力说明（详情页「服务能力」Tab）。
 *
 * 「编辑图片」是否展示由调用方按 `install-info.mcp.tools` 过滤，这里返回全集。
 */
export function capabilityGroups(t: TFn, id: string): CapabilityGroup[] {
  switch (id) {
    case 'web-search':
      return [
        {
          title: t('批量网页搜索'),
          desc: t('批量搜索 1～10 条查询，并发调用后台选中的提供商后合并重复来源。'),
          params: [
            {
              name: 'queries',
              type: 'string[]',
              desc: t('要搜索的查询，每条并发执行（1～10 条）'),
              required: true,
            },
          ],
        },
        {
          title: t('批量网页抓取'),
          desc: t('批量抓取 1～10 个 URL，使用后台选中的单个抓取提供商。'),
          params: [
            {
              name: 'urls',
              type: 'string[]',
              desc: t('要抓取并提取正文的网页 URL（1～10 个）'),
              required: true,
            },
          ],
        },
      ];
    case 'image-generation':
      return [
        {
          title: t('生成图片'),
          desc: t('按描述使用已配置的模型生图，默认一张，会消耗账号额度。'),
          params: [
            {
              name: 'prompt',
              type: 'string',
              desc: t('完整的图片要求'),
              required: true,
            },
            {
              name: 'request_id',
              type: 'string',
              desc: t('本次提交的唯一编号，断线重试使用原编号'),
              required: true,
            },
          ],
        },
        {
          title: t('编辑图片'),
          desc: t('按参考图修改，仅在配置了可用编辑模型时提供。'),
          params: [
            {
              name: 'images',
              type: 'string[]',
              desc: t('参考图链接或已生成图片 ID；本地桥接还可传文件路径'),
              required: true,
            },
          ],
        },
        {
          title: t('查询生成结果'),
          desc: t('查询原任务并取回图片，不再次生图或扣费。'),
          params: [
            {
              name: 'job_id / request_id',
              type: 'string',
              desc: t('任务编号或原提交编号，至少提供一项'),
              required: true,
            },
          ],
        },
      ];
    case 'vision':
      return [
        {
          title: t('图片分析'),
          desc: t('把 1～10 张图片与提示词交给后台配置的视觉模型，返回模型回复文本。'),
          params: [
            {
              name: 'images',
              type: 'string[]',
              desc: t('要分析的图片（1～10 张），每张为 http(s) URL 或图片 data URI'),
              required: true,
            },
            {
              name: 'prompt',
              type: 'string',
              desc: t('提示词，描述要让模型在图片中查看的内容'),
              required: true,
            },
          ],
        },
      ];
    default:
      return [];
  }
}

/**
 * 工具管理面（/api/tools/*）返回的是裸 payload（数组或对象），而其它域多用
 * `{ success, data }` 信封。两种形态都接受，避免列表/详情按错误形状解包后空白。
 */
export function toolPayload<T>(res: unknown): T | null {
  if (res === null || res === undefined) return null;
  if (typeof res === 'object' && !Array.isArray(res) && 'data' in (res as Record<string, unknown>)) {
    return (res as { data: T }).data;
  }
  return res as T;
}
