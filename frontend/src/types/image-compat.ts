/**
 * 会话图片压缩与图片兼容设置（#7；独立文件理由：types/index.ts 正被并行编辑，
 * 避免共享大文件互卷在制品 —— 惯例同 report-tz / routing-replay）。
 *
 * GET 响应 camelCase；PUT body 也用 camelCase（后端收 camelCase 别名，
 * snake_case 为主键，本面板统一 camelCase 与响应一致）。缺省字段保持原值。
 */

/** 图片压缩与视觉兼容设置（GET/PUT /api/image-compat/settings 的 data） */
export interface ImageCompatSettingsItem {
  /** 压缩开关（默认开）：关闭后数据面不做任何扫描/改写 */
  enabled: boolean;
  /** 压缩长边上限（像素，默认 1568；越界在生效时夹到 [min,max]） */
  maxEdge: number;
  /** 重编码质量（jpeg，默认 85） */
  quality: number;
  /** 触发阈值（原图 KB，默认 256；0 = 每张都尝试压缩） */
  thresholdKb: number;
  /** 图片兼容总开关（辅助识图 / 切换视觉模型），默认关：关闭时请求逐字节不变 */
  visionEnabled: boolean;
  /** 模式：describe（辅助识图，默认）| switch（切换视觉模型） */
  mode: 'describe' | 'switch';
  /** 辅助视觉模型（describe 用；switch 模式下即目标视觉模型）。空 = 不生效 */
  visionModel: string;
  /** 视觉模型备选链：视觉模型调用失败后按顺序回退 */
  fallbackModels: string[];
  /** 辅助识图提示词（空 = 用后端内置默认提示词） */
  describePrompt: string;
  // ── 边界值随响应下发（前端输入框直接用，避免前后端各写一套常量）──
  maxEdgeMin: number;
  maxEdgeMax: number;
  qualityMin: number;
  qualityMax: number;
  thresholdKbMin: number;
  thresholdKbMax: number;
  /** 后端内置默认提示词（describePrompt 留空时的实际生效值） */
  defaultDescribePrompt: string;
}

/** PUT 请求体：全量传当前表单（后端逐字段校验越界返回 400） */
export type ImageCompatSettingsBody = Partial<
  Pick<
    ImageCompatSettingsItem,
    | 'enabled'
    | 'maxEdge'
    | 'quality'
    | 'thresholdKb'
    | 'visionEnabled'
    | 'mode'
    | 'visionModel'
    | 'fallbackModels'
    | 'describePrompt'
  >
>;
