/**
 * /chat 的助手设置抽屉（对齐 A2A v3.5.1 `islands/assistant-settings-drawer.tsx`）。
 *
 * ── 为什么在对话页里 ────────────────────────────────────────
 * 这三块（默认行为 / 系统提示词 / 工具）都是聊着聊着就想改的东西；放在
 * 「设置 → 聊天」意味着每次调整都要离开对话页。搬进来之后不打断会话。
 *
 * ── 保存语义 ────────────────────────────────────────────────
 * 与 A2A 一致：改动即时生效（没有「保存」按钮）。AIGX 侧落在
 * `aigx_chat_assistant_cfg`（localStorage），系统提示词正文走小弹层编辑。
 *
 * ── A2A 有而 AIGX 缺的数据源 ────────────────────────────────
 *   · `/api/assistant/config`：无。配置落在本机 localStorage；
 *   · `/api/model-access/keys`：无。A2A 在这里选 API Key，AIGX 无该概念，
 *     该字段位以「网关自动选路」说明占位；
 *   · 工具执行通道：对话链路无。工具清单取自 `/api/agent/config`（只读展示，
 *     勾选状态保存在本机配置里，真实执行在「调试视图」）。
 */
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../api';
import ModelPicker from '../ModelPicker';
import ChatSettingsPanel from '../ChatSettingsPanel';
import {
  ChatBoltIcon,
  ChatCheckIcon,
  ChatShieldIcon,
  ChatSlidersIcon,
} from '../../pages/chat-icons';
import type { AssistantConfig } from '../../pages/chat-state';
import { DEFAULT_SYSTEM_PROMPT, REASONING_LEVELS } from '../../pages/chat-state';

export interface AsstAgentTool {
  name: string;
  label: string;
  risk: string;
}

export interface AsstSettingsDrawerProps {
  open: boolean;
  onClose: () => void;
  config: AssistantConfig;
  models: string[];
  modelsLoading: boolean;
  onRefreshModels: () => void;
  onSave: (next: AssistantConfig) => void;
  /** 管理员才显示「聊天设置」分节（后端管理接口） */
  admin: boolean;
  /** 打开调试视图（AIGX 独有入口的另一处） */
  onOpenDebugger: () => void;
  /** 标题总结配置（A2A 无：AIGX 的前端实现需要模型/提示词两个旋钮） */
  titleModel: string;
  titlePrompt: string;
  onSaveTitleCfg: (next: { titleModel: string; titlePrompt: string }) => void;
  onSummarizeNow: () => void;
}

/** 一块配置卡（与 A2A 的 Section 同形） */
function Section({ title, sub, extra, children }: {
  title: string;
  sub?: string;
  extra?: JSX.Element | null;
  children: ReactNode;
}): JSX.Element {
  return (
    <section className="asst-set-sect">
      <div className="asst-set-head">
        <h3>{title}</h3>
        {sub && <span>{sub}</span>}
        {extra}
      </div>
      {children}
    </section>
  );
}

function levelText(level: string | null, follow: string): string {
  if (!level) return follow;
  return level === 'xhigh' ? 'xHigh' : level.charAt(0).toUpperCase() + level.slice(1);
}

export default function AsstSettingsDrawer({
  open,
  onClose,
  config,
  models,
  modelsLoading,
  onRefreshModels,
  onSave,
  admin,
  onOpenDebugger,
  titleModel,
  titlePrompt,
  onSaveTitleCfg,
  onSummarizeNow,
}: AsstSettingsDrawerProps): JSX.Element | null {
  const { t } = useTranslation();
  const [promptOpen, setPromptOpen] = useState(false);
  const [promptDraft, setPromptDraft] = useState('');
  const [tools, setTools] = useState<AsstAgentTool[] | null>(null);
  const [toolsNote, setToolsNote] = useState('');
  const [titleDraft, setTitleDraft] = useState({ model: titleModel, prompt: titlePrompt });

  /** 刷新动作的最新引用。父组件常以内联箭头传入，写进依赖会每帧重跑
   *  （新引用 → 刷新 → setState → 父重渲染 → 又新引用）：实测把页面卡死，
   *  真机上则是抽屉一打开就打满 /api 模型列表。走 ref 后 effect 只跟 open 走。 */
  const refreshRef = useRef(onRefreshModels);
  refreshRef.current = onRefreshModels;

  useEffect(() => {
    if (open) refreshRef.current();
  }, [open]);

  useEffect(() => {
    setTitleDraft({ model: titleModel, prompt: titlePrompt });
  }, [titleModel, titlePrompt]);

  /* 工具清单：AIGX 的真实工具定义在 /agent/config（Agent 页在用）。
   * 取不到就如实说明「数据源缺失」，不伪造一份看起来可用的清单。 */
  useEffect(() => {
    if (!open || tools !== null) return;
    let mounted = true;
    void api.getAgentConfig()
      .then((res) => {
        if (!mounted) return;
        const list = res.data?.tools ?? [];
        setTools(list.map((tool) => ({ name: tool.name, label: tool.label, risk: tool.risk })));
        setToolsNote(list.length ? '' : t('后端没有返回工具清单'));
      })
      .catch((err: unknown) => {
        if (!mounted) return;
        setTools([]);
        setToolsNote(err instanceof Error ? err.message : String(err));
      });
    return () => { mounted = false; };
  }, [open, tools, t]);

  const setMode = useCallback((mode: 'confirm' | 'full') => {
    onSave({ ...config, mode });
  }, [config, onSave]);

  const toggleTool = useCallback((id: string, on: boolean) => {
    onSave({ ...config, tools: { ...config.tools, [id]: on } });
  }, [config, onSave]);

  if (!open) return null;

  const promptEnabled = config.systemPromptEnabled !== false;
  const customPrompt = Boolean(config.systemPrompt?.trim());

  return (
    <div className="asst-sheet-root" role="presentation">
      <div className="asst-sheet-mask" onClick={onClose} />
      <aside className="asst-drawer" role="dialog" aria-modal="true" aria-label={t('助手设置')}>
        <div className="asst-drawer-head">
          <h2>{t('助手设置')}</h2>
          <button type="button" className="asst-drawer-x" onClick={onClose} aria-label={t('关闭')}>✕</button>
        </div>

        <div className="asst-drawer-body">
          <div className="asst-moved">
            <ChatSlidersIcon size={13} />
            {t('原先在「设置 → 聊天」的配置都搬到这里了')}
          </div>

          {/* ① 默认行为 */}
          <Section title={t('默认行为')} sub={t('新建对话时使用')}>
            <div className="asst-field">
              <label>{t('网关选路')}</label>
              <div className="asst-field-static">
                {t('由 AIGX 网关按渠道优先级自动选路（无需 API Key）；渠道锁定是「AI 运维 Agent」页的配置，不作用于本页对话。')}
              </div>
            </div>

            <div className="asst-field">
              <label>{t('模型')}</label>
              <ModelPicker value={config.model ?? ''} onChange={(name) => onSave({ ...config, model: name })} />
              {modelsLoading && <div className="asst-hint">{t('正在读取可用模型…')}</div>}
              {!modelsLoading && models.length === 0 && (
                <div className="asst-hint bad">{t('没有可用的模型：请先在「模型管理」里启用渠道')}</div>
              )}
            </div>

            <div className="asst-field col">
              <label>{t('思考等级')}</label>
              <div className="asst-level-list">
                <button
                  type="button"
                  className={`asst-level-row${!config.reasoning ? ' on' : ''}`}
                  onClick={() => onSave({ ...config, reasoning: null })}
                >
                  <span>{t('跟随模型')}</span>
                  <span className="asst-level-hint">{t('不指定，交给上游默认')}</span>
                  {!config.reasoning && <ChatCheckIcon size={12} />}
                </button>
                {REASONING_LEVELS.map((level) => (
                  <button
                    type="button"
                    key={level}
                    className={`asst-level-row${config.reasoning === level ? ' on' : ''}`}
                    onClick={() => onSave({ ...config, reasoning: level })}
                  >
                    <span className="mono">{level}</span>
                    <span className="asst-level-hint">{levelText(level, t('跟随模型'))}</span>
                    {config.reasoning === level && <ChatCheckIcon size={12} />}
                  </button>
                ))}
              </div>
            </div>

            <div className="asst-field col">
              <label>{t('权限模式')}</label>
              <div className="asst-mode-cards">
                {(['confirm', 'full'] as const).map((value) => (
                  <button
                    type="button"
                    key={value}
                    className={`asst-mode-card${config.mode === value ? ' on' : ''}${value === 'full' ? ' warn' : ''}`}
                    onClick={() => setMode(value)}
                  >
                    <span className="asst-mc-ico">{value === 'full' ? <ChatBoltIcon size={14} /> : <ChatShieldIcon size={14} />}</span>
                    <span className="asst-mc-txt">
                      <b>{value === 'full' ? t('完全访问') : t('变更前确认')}</b>
                      <small>{value === 'full' ? t('改动直接执行') : t('改动先列清单')}</small>
                    </span>
                    {config.mode === value && <ChatCheckIcon size={12} className="asst-mc-check" />}
                  </button>
                ))}
              </div>
              <div className="asst-hint">
                {t('对话链路暂无工具执行通道；工具的真实执行与审批卡在「AI 运维 Agent」页，这里是同一份清单的只读勾选。')}
              </div>
            </div>
          </Section>

          {/* ② 系统提示词 */}
          <Section title={t('系统提示词')} sub={t('作为 system 消息注入')}>
            <div className="asst-switchline">
              <button
                type="button"
                role="switch"
                aria-checked={promptEnabled}
                className={`asst-switch${promptEnabled ? ' on' : ''}`}
                onClick={() => onSave({ ...config, systemPromptEnabled: !promptEnabled })}
              >
                <span className="asst-switch-thumb" />
              </button>
              <span>{t('注入系统提示词')}</span>
              <span className="asst-bar-spacer" />
              <button
                type="button"
                className="asst-link-btn"
                onClick={() => {
                  setPromptDraft(config.systemPrompt?.trim() ? config.systemPrompt : DEFAULT_SYSTEM_PROMPT);
                  setPromptOpen(true);
                }}
              >
                {t('编辑')}
              </button>
            </div>
            <div className="asst-prompt-preview">
              {!promptEnabled
                ? t('当前：已关闭（模型收不到任何系统提示词）')
                : customPrompt
                  ? t('当前：已开启 · 自定义内容 {{n}} 字', { n: config.systemPrompt.trim().length })
                  : t('当前：已开启 · 使用内置默认提示词')}
            </div>
          </Section>

          {/* ③ 工具 */}
          <Section
            title={t('工具')}
            sub={t('勾选即注入（执行在调试视图）')}
            extra={<span className="asst-badge">{tools === null ? '—' : `${tools.length}`}</span>}
          >
            {tools === null && <div className="asst-hint">{t('正在读取工具清单…')}</div>}
            {tools !== null && tools.length === 0 && (
              <div className="asst-hint bad">
                {t('工具清单为空或读取失败：')}{toolsNote}
              </div>
            )}
            {tools !== null && tools.length > 0 && (
              <div className="asst-tool-rows">
                {tools.map((tool) => (
                  <label className="asst-tool-row" key={tool.name}>
                    <input
                      type="checkbox"
                      checked={config.tools?.[tool.name] === true}
                      onChange={(event) => toggleTool(tool.name, event.currentTarget.checked)}
                    />
                    <span className="tt">
                      <span className="tt-name">
                        {tool.label || tool.name}
                        {tool.risk === 'high' && <span className="asst-tag bad">{t('高危')}</span>}
                      </span>
                      <span className="tt-desc mono">{tool.name}</span>
                    </span>
                  </label>
                ))}
              </div>
            )}
          </Section>

          {/* ④ AIGX 独有项（A2A 没有这三块） */}
          <Section title={t('AIGX 独有项')} sub={t('A2A 没有的能力，都保留在这里')}>
            <div className="asst-field">
              <label>{t('调试视图')}</label>
              <button type="button" className="asst-link-btn" onClick={onOpenDebugger}>
                {t('打开调试视图（SSE 报文 / tool_calls / 协议与参数）')}
              </button>
            </div>

            <div className="asst-field">
              <label>{t('标题模型（留空 = 网关默认首选）')}</label>
              <ModelPicker value={titleDraft.model} onChange={(name) => setTitleDraft((prev) => ({ ...prev, model: name }))} />
            </div>
            <div className="asst-field col">
              <label>{t('标题提示词')}</label>
              <textarea
                className="form-input"
                rows={3}
                value={titleDraft.prompt}
                placeholder={t('留空使用内置默认提示词')}
                onChange={(event) => setTitleDraft((prev) => ({ ...prev, prompt: event.currentTarget.value }))}
              />
              <div className="asst-row-actions">
                <button
                  type="button"
                  className="asst-link-btn"
                  onClick={() => onSaveTitleCfg({ titleModel: titleDraft.model, titlePrompt: titleDraft.prompt })}
                >
                  {t('保存标题配置')}
                </button>
                <button type="button" className="asst-link-btn" onClick={onSummarizeNow}>
                  {t('立即总结当前会话')}
                </button>
              </div>
            </div>
          </Section>

          {/* ⑤ 管理员：后端聊天设置（自动标题 / 供应商锁定 / 截断长度） */}
          {admin && (
            <Section title={t('后端聊天设置')} sub={t('管理员 · 保存后热生效')}>
              <ChatSettingsPanel />
            </Section>
          )}
        </div>

        <div className="asst-drawer-foot">
          <span className="asst-hint">{t('会话里的选择优先于这里的默认值')}</span>
          <span className="asst-bar-spacer" />
          <button type="button" className="asst-link-btn" onClick={onClose}>{t('完成')}</button>
        </div>
      </aside>

      {/* 系统提示词编辑弹层 */}
      {promptOpen && (
        <div className="asst-dialog-mask" onClick={() => setPromptOpen(false)}>
          <div className="asst-dialog" onClick={(event) => event.stopPropagation()}>
            <div className="asst-dialog-head">
              <h3>{t('编辑系统提示词')}</h3>
            </div>
            <div className="asst-dialog-body">
              <p className="asst-hint">
                {t('这段文本作为 system 消息排在对话最前面。工具定义由网关自动附加，不用写在这里。')}
              </p>
              <textarea
                className="form-input"
                rows={14}
                spellCheck={false}
                value={promptDraft}
                onChange={(event) => setPromptDraft(event.currentTarget.value)}
              />
              <div className="asst-hint">{t('清空 = 回到内置默认（与「恢复默认」等价）。')}</div>
            </div>
            <div className="asst-dialog-foot">
              <button type="button" className="asst-link-btn" onClick={() => setPromptDraft(DEFAULT_SYSTEM_PROMPT)}>
                {t('恢复默认')}
              </button>
              <span className="asst-bar-spacer" />
              <button type="button" className="asst-link-btn" onClick={() => setPromptOpen(false)}>{t('取消')}</button>
              <button
                type="button"
                className="asst-primary-btn"
                onClick={() => {
                  const text = promptDraft.trim() === DEFAULT_SYSTEM_PROMPT.trim() ? '' : promptDraft;
                  onSave({ ...config, systemPrompt: text });
                  setPromptOpen(false);
                }}
              >
                {t('保存')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
