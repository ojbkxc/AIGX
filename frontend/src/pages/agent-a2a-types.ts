/**
 * A2A（Agent2API v3.5.1）Agent 页类型与文案口径。
 *
 * 严格对齐 `desktop-tauri/ui-islands/src/islands/agent-types.ts` 的字段名与语义，
 * 只取 AIGX 页面实际用到的子集。AIGX 后端暂无「客户端检测 / 配置写入」接口，
 * 相关字段先按 A2A 结构落控件并允许占位，数据源缺失记入交付清单。
 */

/** A2A AgentModel（模型目录项与配置内模型共用同一形状） */
export type A2aModel = {
  id: string;
  name: string;
  contextWindow?: number | null;
  maxOutputTokens?: number | null;
  supportsTools?: boolean | null;
  supportsImages?: boolean | null;
  supportsVideo?: boolean | null;
  supportsReasoning?: boolean | null;
  reasoningLevels: string[];
  effort: string;
};

/** A2A AgentProfile（一套可写入客户端的配置） */
export type A2aProfile = {
  id: string;
  name: string;
  kind: string;
  protocol: string;
  baseUrl: string;
  model: string;
  provider: string;
  effort: string;
  enabled: boolean;
  default: boolean;
  note: string;
  models: A2aModel[];
  claudeModels: { opus: string; sonnet: string; haiku: string; subagent: string };
  authMode: string;
  apiKey: string;
  envKey: string;
  gatewayKeyId: string;
  hasApiKey?: boolean;
  schemaVersion?: number;
  sourceId?: string;
  inFile?: boolean;
  validationError?: string | null;
  editable?: boolean;
  deletable?: boolean;
  updatedAt?: number;
  liveId?: string | null;
  savedIds?: string[];
  status?: 'active' | 'enabled' | 'disabled' | 'available' | 'inactive';
};

/** A2A AgentClient（一个本机客户端） */
export type A2aClient = {
  id: string;
  name: string;
  vendor: string;
  hue: number;
  letter: string;
  icon: string;
  iconFull: boolean;
  configPath: string;
  resolvedPath: string;
  pathOverride: string;
  configLayout?: 'legacy' | 'profile' | '';
  format: string;
  configMode: 'single' | 'providers' | 'models' | 'readonly';
  protocol: string;
  protocols: string[];
  installed: boolean;
  detected: boolean;
  detectionState: 'missing' | 'uninitialized' | 'read_error' | 'unsupported' | 'ready';
  evidence: string[];
  version: string;
  writable: boolean;
  supportsModels: boolean;
  modelFields?: Array<keyof A2aModel>;
  supportsEffort: boolean;
  supportsDefault: boolean;
  supportsToggle: boolean;
  supportsGateway: boolean;
  supportsEnvKey: boolean;
  supportNote: string;
  docsUrl: string;
  needsRestart: boolean;
  configurations: A2aProfile[];
  profiles: A2aProfile[];
  live: A2aProfile[];
  readError: string | null;
  storeError: string | null;
  viaGateway: boolean;
  activeId: string | null;
  revision: string;
  nativeRevision?: string | null;
  applied: {
    canUndo?: boolean;
    canDisconnect?: boolean;
    modifiedExternally?: boolean;
    pendingRecovery?: boolean;
    appliedAt?: number;
    backupId?: string;
    error?: string;
  };
};

/** A2A AgentPreview（写入前的差异预览） */
export type A2aPreview = {
  token: string;
  hasChanges: boolean;
  profileId: string;
  warnings: string[];
  files: Array<{
    path: string;
    before: string;
    text: string;
    changed: boolean;
    created: boolean;
    deleted: boolean;
    diff: Array<{ kind: 'same' | 'added' | 'removed'; text: string; oldLine?: number; newLine?: number }>;
  }>;
};

/** A2A 应用操作类型（对齐 agent-types.ts 的 AgentOperation） */
export type A2aOperation =
  | 'apply'
  | 'default'
  | 'restore'
  | 'disconnect'
  | 'native-edit'
  | 'native-delete'
  | 'quick-connect'
  | 'config-use'
  | 'config-disable'
  | 'config-edit'
  | 'config-delete';

/** A2A 客户端筛选档位（对齐 agent-page.tsx 的 SegmentedControl） */
export const A2A_FILTER_KEYS = ['all', 'via', 'direct', 'missing'] as const;
export type A2aFilterKey = (typeof A2A_FILTER_KEYS)[number];

/** A2A 页内分段页签（对齐 agent-page.tsx 的四个 SegmentedControl 选项） */
export const A2A_TAB_KEYS = ['config', 'prompt', 'skills', 'mcp'] as const;
export type A2aTabKey = (typeof A2A_TAB_KEYS)[number];

/** 中文文案单一来源：全部与 A2A v3.5.1 同义同词 */
export const A2A_TEXT = {
  search: '搜索客户端 / 配置文件…',
  searchAria: '搜索客户端',
  filterAll: '全部',
  filterVia: '接入本网关',
  filterDirect: '未接入',
  filterMissing: '未检测到',
  detecting: '正在检测…',
  redetect: '重新检测',
  updating: '后台更新中…',
  detectedAt: '上次检测 {time}',
  railClients: '客户端清单',
  railDetected: '已检测到',
  railMissing: '未检测到',
  noMatchClients: '没有匹配的客户端',
  clearFilters: '清除筛选',
  detailLoading: '正在检测本机客户端与配置文件…',
  noMatchDetail: '没有找到匹配的客户端',
  noMatchDetailHint: '试试调整搜索关键词，或切换到其他筛选分类',
  noClientData: '暂无客户端数据',
  readingFailed: '读取配置失败',
  notDetectedRaw: '尚未安装',
  pendingRecovery: '上次应用待恢复',
  connecting: '接入中',
  notEnabled: '尚未启用配置',
  quickConnect: '一键接入',
  addConfig: '新增配置',
  clientOptions: '客户端选项',
  quickConnectGateway: '一键接入本网关',
  choosePath: '指定配置路径',
  undoLast: '撤销上次应用',
  disconnectGateway: '解除接入本网关',
  tabs: { config: '模型配置', prompt: '提示词', skills: 'Skill', mcp: 'MCP' },
  machineNote: '操作目标：网关所在机器',
  notDetected: '未找到客户端程序或配置文件',
  notDetectedWritable: '可先新增配置，安装后再使用',
  panelConfig: '配置列表',
  noConfigs: '还没有配置',
  noConfigsHint: '未读取到可识别的客户端配置',
  addConfigManual: '手动新增配置',
  profileName: '配置名称',
  profileNote: '备注',
  connectionMode: '连接方式',
  modeGateway: '本网关',
  modeOfficial: '官方直连',
  modeRelay: '第三方中转',
  modeLocal: '本机服务',
  baseUrl: '接口基址',
  authMode: '认证方式',
  authGatewayKey: '使用网关 Key',
  authApiKey: 'API Key',
  authEnv: '环境变量',
  authNone: '无需认证',
  authPreserve: '保留现有认证',
  gatewayKey: '网关 Key',
  gatewayKeyFirst: '第一把启用的 Key',
  apiKeyField: 'API Key',
  envKeyField: '环境变量名',
  advanced: '高级选项',
  protocol: '客户端协议',
  protocolNote: '这里只选择客户端实际支持的协议，不改变上游提供商协议。',
  modelList: '模型列表',
  modelId: '模型 ID',
  modelName: '模型展示名',
  contextWindow: '上下文窗口',
  outputLabel: '输出上限',
  maxOutput: '最大输出 Token',
  capabilityTools: '工具调用',
  capabilityImages: '图片输入',
  capabilityVideo: '视频输入',
  capabilityReasoning: '思考能力',
  capUnset: '未声明 / 保留',
  capOn: '支持',
  capOff: '不支持',
  addModel: '添加模型',
  manualAddModel: '手动添加模型',
  remove: '移除',
  defaultModel: '默认模型',
  chooseFromCatalog: '从网关目录选择模型',
  defaultModelThinking: '默认模型思考等级',
  thinkingLevel: '思考等级',
  modelDefault: '模型默认',
  selectModel: '选择模型',
  removeModel: '移除模型：{name}',
  configLocation: '配置位置与说明',
  resolvedPath: '配置已解析路径',
  evidence: '证据',
  supportNote: '支持说明',
  statusLoadNote: '状态来自配置文件，已打开的会话可能需要重新加载',
  externalModified: '客户端文件已有外部修改，自动恢复会拒绝覆盖。',
  newConfig: '新增配置',
  editConfig: '编辑配置',
  save: '保存',
  saving: '保存中…',
  cancel: '取消',
  confirm: '确认',
  deleteConfig: '删除配置',
  useConfig: '使用此配置',
  addToClient: '添加到客户端',
  disableConfig: '停用',
  toggleOn: '启用',
  togglingOn: '启用此配置',
  preview: '查看文件改动',
  previewHeadline: '密钥已隐藏；绿色为新增，红色为删除。所有实际写入文件都列在这里。',
  regenerate: '重新生成预览',
  apply: '确认应用',
  applyDefault: '应用并设为默认',
  applyToClient: '应用到客户端',
  confirmDelete: '确认删除',
  confirmDisable: '确认停用',
  confirmUse: '确认使用',
  restore: '撤销上次应用',
  disconnect: '解除接入本网关',
  pathDialogTitle: '指定配置路径',
  pathLabel: '完整配置文件路径',
  restoreAuto: '恢复自动检测',
  savePath: '保存路径',
  browseFile: '浏览文件',
  choosing: '选择中…',
  quickConnectTitle: '一键接入',
  quickConnectDesc: '选择要接入的模型；确认前展示文件改动，写入前自动备份。',
  endpoint: '接入端点',
  writeFile: '写入文件',
  connectClient: '接入客户端',
  confirmConnect: '确认接入',
  backToEdit: '返回编辑',
  noModels: '没有可添加的模型',
  noMatchModels: '没有匹配的模型',
  atLeastOneModel: '请至少添加一个模型',
  clearSearch: '清除搜索',
  allSearch: '全选搜索结果',
  clearPick: '清空选择',
  pickedCount: '已选择 {count} / {total} 个',
  maxModels: '最多选择 {n} 个模型',
  modelCatalogEmpty: '所选 Key 下没有可用的对话模型，请先启用模型或更换 Key。',
  missingFields: '请补全配置后使用',
  current: '使用中',
  enabled: '已启用',
  disabled: '已停用',
  statusActive: '状态来自配置文件，已打开的会话可能需要重新加载',
} as const;
