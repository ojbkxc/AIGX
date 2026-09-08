import { useState, useEffect, useRef, type KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Search, Send, Square, Trash2, Image, Video, AudioLines, Loader2, Bot, User, Copy, Check } from 'lucide-react';
import { api, testChannelChatStream } from '../api';
import MessageViewer from './MessageViewer';
import './ChatDebugger.css';

export interface DebugMessage {
  role: 'user' | 'assistant';
  content: string;
  /** DeepSeek 式深度思考（SSE 的 reasoning_content，与正文分离） */
  reasoning?: string;
  /** 用户消息可选的多模态附件（URL 或 base64 data URI） */
  attachments?: Array<{ kind: 'image' | 'video' | 'audio'; url: string }>;
}

export interface ChatDebuggerProps {
  /** 指定渠道 ID：调试固定渠道；留空走「自动选择启用渠道」（Playground） */
  channelId?: string;
  /** 指定渠道可用模型列表 */
  channelModels?: string[];
  /** 初始协议 */
  initialProtocol?: 'openai' | 'anthropic';
  /** 紧凑模式（渠道弹窗内嵌） */
  compact?: boolean;
  /** 初始消息（/chat 会话恢复用）；配合 key 重挂载生效 */
  initialMessages?: DebugMessage[];
  /** 消息变化回调（/chat 会话持久化用） */
  onMessagesChange?: (messages: DebugMessage[]) => void;
  /** 隐藏调试工具条（协议/系统提示词/附件区），/chat 终端用户形态 */
  hideToolbar?: boolean;
  /** 顶部悬浮模型 pill（hideToolbar 时）。Open WebUI 首页形态传 false，
   *  模型选择下沉到空状态大标题上方，避免与居中问候重叠。 */
  floatingModelBar?: boolean;
  /** 空状态建议 prompt（Open WebUI 首页 Suggestions 网格），点选直接发送 */
  suggestionPrompts?: Array<{ title: string; sub: string; content: string }>;
}

interface ChatChunkResult {
  stream?: Array<{ content?: string }>;
  data?: { content?: string; error?: string; usage?: unknown };
  error?: string;
  success?: boolean;
}

/**
 * ChatDebugger — 统一对话调试器。
 *
 * Playground 页与渠道管理的「对话调试」共用本组件，两者都走同一个
 * 后端入口 /api/channels/chat_test（OpenAI/Anthropic 协议 + SSE 流式），
 * 保证调试行为与数据面代理一致。
 *
 * 多模态：OpenAI 协议下图片/视频/音频附件以 content 数组块透传
 * （image_url / video_url / audio_url 形状）。
 */
export default function ChatDebugger(props: ChatDebuggerProps): JSX.Element {
  const {
    channelId,
    channelModels = [],
    initialProtocol = 'openai',
    compact = false,
    initialMessages = [],
    onMessagesChange,
    hideToolbar = false,
    floatingModelBar = true,
    suggestionPrompts = [],
  } = props;
  const { t } = useTranslation();

  const [messages, setMessages] = useState<DebugMessage[]>(initialMessages);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState('');
  const [protocol, setProtocol] = useState<'openai' | 'anthropic'>(initialProtocol);
  const [stream, setStream] = useState(true);
  const [systemPrompt, setSystemPrompt] = useState('');
  // 系统提示词预设：无（默认）/通用/代码/翻译
  const [promptPreset, setPromptPreset] = useState('');
  // 参数预设：平衡/严谨/创意（联动温度）
  const [tempPreset, setTempPreset] = useState('');
  const [temperature, setTemperature] = useState('0.7');
  const [maxTokens, setMaxTokens] = useState('1024');
  const [query, setQuery] = useState('');
  const [pickerIdx, setPickerIdx] = useState(0);
  const [pickerOpen, setPickerOpen] = useState(false);
  const pickerRef = useRef<HTMLDivElement | null>(null);
  const messagesEndRef = useRef<HTMLDivElement | null>(null);
  /** 流式生成中断控制器：用户点「停止」时 abort 上游请求 */
  const abortRef = useRef<AbortController | null>(null);

  const [attachments, setAttachments] = useState<Array<{ kind: 'image' | 'video' | 'audio'; url: string }>>([]);
  const [attachKind, setAttachKind] = useState<'image' | 'video' | 'audio'>('image');
  const [attachUrl, setAttachUrl] = useState('');
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);

  // /chat 会话持久化：消息每次变化都同步给宿主页面
  useEffect(() => {
    onMessagesChange?.(messages);
  }, [messages, onMessagesChange]);

  // 系统提示词预设（与 i18n 词条保持一致）
  const PROMPT_PRESETS: Record<string, string> = {
    general: '你是一个乐于助人的 AI 助手，请用简洁清晰的语言回答。',
    code: '你是一位资深软件工程师。请给出可运行的代码示例，优先使用主流最佳实践，并简要解释关键点。',
    translate: '你是一名专业翻译。请把用户输入准确翻译为目标语言，保持原意、语气与格式。',
  };

  // 参数预设：温度（严谨 0.2 / 平衡 0.7 / 创意 1.3）
  const TEMP_PRESETS: Record<string, string> = {
    precise: '0.2',
    balanced: '0.7',
    creative: '1.3',
  };

  // 上下文估算：中文按 1 字≈1 token，其他按 4 字符≈1 token，仅作展示
  const contextEstimate = messages.reduce((n, m) => {
    const text = m.content || '';
    const cjk = (text.match(/[\u4e00-\u9fff]/g) || []).length;
    const other = text.length - cjk;
    return n + cjk + Math.ceil(other / 4);
  }, 0) + input.length;

  const copyMessage = (text: string, idx: number): void => {
    const fallbackCopy = (): boolean => {
      try {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        const ok = document.execCommand('copy');
        document.body.removeChild(ta);
        return ok;
      } catch {
        return false;
      }
    };
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(() => {
        setCopiedIdx(idx);
        setTimeout(() => setCopiedIdx(null), 1500);
      }).catch(() => {
        if (fallbackCopy()) {
          setCopiedIdx(idx);
          setTimeout(() => setCopiedIdx(null), 1500);
        }
      });
    } else if (fallbackCopy()) {
      setCopiedIdx(idx);
      setTimeout(() => setCopiedIdx(null), 1500);
    }
  };

  // 合并模型：渠道模型优先，网关映射模型兜底（去重）
  useEffect(() => {
    let mounted = true;
    if (channelModels.length) {
      setModels(channelModels.slice());
      setModel((prev) => prev || channelModels[0]);
      return () => { mounted = false; };
    }
    setModels([]);
    api.listModels()
      .then((res) => {
        if (!mounted) return;
        const raw = Array.isArray(res) ? res : res?.data;
        const list: string[] = Array.isArray(raw)
          ? (raw as Array<string | { id?: string }>)
            .map((m) => (typeof m === 'string' ? m : m.id))
            .filter((v): v is string => Boolean(v))
          : [];
        setModels(list);
        if (list.length) setModel((prev) => prev || list[0]);
      })
      .catch(() => { /* 模型列表失败静默降级 */ });
    return () => { mounted = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [channelModels.join(',')]);

  useEffect(() => {
    if (messagesEndRef.current) {
      messagesEndRef.current.scrollIntoView({ behavior: 'smooth', block: 'end' });
    }
  }, [messages, busy]);

  // 点击外部关闭模型选择器
  useEffect(() => {
    const onDoc = (e: MouseEvent) => {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setPickerOpen(false);
      }
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, []);

  const visibleModels = models.filter((m) =>
    m.toLowerCase().includes(query.trim().toLowerCase()),
  );

  const addAttachment = (): void => {
    const url = attachUrl.trim();
    if (!url) return;
    setAttachments((prev) => [...prev, { kind: attachKind, url }]);
    setAttachUrl('');
  };

  const blocksForAttachments = (atts: Array<{ kind: string; url: string }>): Array<Record<string, unknown>> => {
    const blocks: Array<Record<string, unknown>> = [];
    for (const a of atts) {
      if (a.kind === 'image') blocks.push({ type: 'image_url', image_url: { url: a.url } });
      if (a.kind === 'video') blocks.push({ type: 'video_url', video_url: { url: a.url } });
      if (a.kind === 'audio') blocks.push({ type: 'audio_url', audio_url: { url: a.url } });
    }
    return blocks;
  };

  const handleSend = async (override?: string): Promise<void> => {
    const text = (override ?? input).trim();
    if ((!text && !attachments.length) || busy) return;
    if (!model) {
      setError(t('请先选择模型'));
      return;
    }
    setMessages((prev) => [...prev, { role: 'user', content: text, attachments: attachments.slice() }]);
    setInput('');
    setAttachments([]);
    setBusy(true);
    setError('');

    // 历史消息：附件展开为 content blocks；纯文本保持字符串形状
    const history = messages.map((m) => {
      if (m.role === 'user' && m.attachments?.length) {
        const blocks = blocksForAttachments(m.attachments);
        if (m.content) blocks.push({ type: 'text', text: m.content });
        return { role: 'user', content: blocks };
      }
      return { role: m.role, content: m.content };
    });

    const body: Record<string, unknown> = {
      channel_id: channelId || '',
      protocol,
      model,
      message: text,
      history,
      stream,
    };
    if (protocol === 'openai') {
      body.temperature = Number(temperature) || 0.7;
      body.max_tokens = Number(maxTokens) || 1024;
    }
    if (systemPrompt.trim()) {
      body.system_prompt = systemPrompt.trim();
    }
    // 多模态附件：OpenAI 协议下把 message 换成 content blocks
    if (attachments.length || messages.some((m) => m.attachments?.length)) {
      const finalBlocks = blocksForAttachments(attachments);
      if (text) finalBlocks.push({ type: 'text', text });
      body.message = finalBlocks;
    }

    try {
      if (stream) {
        const controller = new AbortController();
        abortRef.current = controller;
        // 真·流式：占位一条 assistant 消息，逐增量拼接渲染
        setMessages((prev) => [...prev, { role: 'assistant', content: '…' }]);
        await testChannelChatStream(body, (delta) => {
          setMessages((prev) => {
            const next = prev.slice();
            const last = next[next.length - 1];
            if (last && last.role === 'assistant') {
              // reasoning 与正文分流：思考走折叠面板，正文清占位「…」
              const patch: DebugMessage = { ...last };
              if (delta.kind === 'reasoning') {
                patch.reasoning = (last.reasoning ?? '') + delta.content;
              } else {
                const base = last.content === '…' ? '' : last.content;
                patch.content = delta.isEnd ? base : base + delta.content;
              }
              next[next.length - 1] = patch;
            }
            return next;
          });
        }, controller.signal);
        abortRef.current = null;
        // 空流兜底提示（避免界面出现永久空白气泡）
        setMessages((prev) => {
          const next = prev.slice();
          const last = next[next.length - 1];
          if (last && last.role === 'assistant' && !last.reasoning && (last.content === '…' || !last.content.trim())) {
            next[next.length - 1] = { ...last, content: `⚠️ ${t('上游未返回内容')}` };
          }
          return next;
        });
      } else {
        const res = (await api.testChannelChat(body)) as ChatChunkResult;
        const data = res.data || {};
        if (res.stream && res.stream.length) {
          const acc = res.stream.map((c) => c.content || '').join('');
          setMessages((prev) => [...prev, { role: 'assistant', content: acc }]);
        } else if (data.content) {
          setMessages((prev) => [...prev, { role: 'assistant', content: data.content ?? '' }]);
        } else if (data.error) {
          setMessages((prev) => [...prev, { role: 'assistant', content: `⚠️ ${data.error}` }]);
        } else if (res.error) {
          setMessages((prev) => [...prev, { role: 'assistant', content: `⚠️ ${res.error}` }]);
        } else {
          setMessages((prev) => [...prev, { role: 'assistant', content: `⚠️ ${t('上游未返回内容')}` }]);
        }
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (msg === 'Aborted' || msg === 'AbortError' || /abort/i.test(msg)) {
        // 用户主动停止：补一句生成已停止，不弹出错误
        setMessages((prev) => [...prev, { role: 'assistant', content: `⏹ ${t('已停止生成')}` }]);
        abortRef.current = null;
        return;
      }
      setError(msg);
      setMessages((prev) => [...prev, { role: 'assistant', content: `⚠️ ${msg}` }]);
    } finally {
      setBusy(false);
      abortRef.current = null;
    }
  };

  /** 停止当前流式生成（AbortController 中断上游请求） */
  const stopStreaming = (): void => {
    abortRef.current?.abort();
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>): void => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      void handleSend();
    }
  };

  const clearAll = (): void => {
    setMessages([]);
    setError('');
    setPromptPreset('');
    setTempPreset('');
  };

  // 模型选择器（完整工具条与 /chat 精简条共用同一份 JSX）
  const modelPicker = (
    <div className="chat-debugger-model" ref={pickerRef}>
          <button
            type="button"
            className="form-input chat-debugger-model-btn"
            onClick={() => setPickerOpen((v) => !v)}
          >
            <Bot size={14} />
            <span className="chat-debugger-model-name">{model || t('选择模型')}</span>
          </button>
          {pickerOpen && (
            <div className="chat-debugger-picker">
              <div className="chat-debugger-search">
                <Search size={13} />
                <input
                  className="form-input"
                  autoFocus
                  placeholder={t('搜索模型…')}
                  value={query}
                  onChange={(e) => { setQuery(e.target.value); setPickerIdx(0); }}
                  onKeyDown={(e) => {
                    // 键盘导航：↑/↓ 选择，Enter 确认，Esc 关闭
                    if (e.key === 'ArrowDown') {
                      e.preventDefault();
                      setPickerIdx((i) => Math.min(i + 1, visibleModels.length - 1));
                    } else if (e.key === 'ArrowUp') {
                      e.preventDefault();
                      setPickerIdx((i) => Math.max(i - 1, 0));
                    } else if (e.key === 'Enter') {
                      e.preventDefault();
                      const pick = visibleModels[pickerIdx];
                      if (pick) { setModel(pick); setPickerOpen(false); setMessages([]); setQuery(''); }
                    } else if (e.key === 'Escape') {
                      setPickerOpen(false); setQuery('');
                    }
                  }}
                />
                {visibleModels.length > 0 && (
                  <span className="chat-debugger-picker-count">{visibleModels.length}</span>
                )}
              </div>
              <div className="chat-debugger-picker-list">
                {visibleModels.length === 0 && (
                  <div className="chat-debugger-picker-empty">{t('无匹配模型')}</div>
                )}
                {visibleModels.map((m, i) => (
                  <button
                    type="button"
                    key={m}
                    className={`chat-debugger-picker-item ${m === model ? 'active' : ''} ${i === pickerIdx ? 'hover' : ''}`}
                    onMouseEnter={() => setPickerIdx(i)}
                    onClick={() => { setModel(m); setPickerOpen(false); setMessages([]); setQuery(''); }}
                  >
                    {m}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>
  );

  return (
    <div className={`chat-debugger ${compact ? 'chat-debugger-compact' : ''}`}>
      {!hideToolbar && (
      <div className="chat-debugger-bar">
        {modelPicker}

        <select
          className="form-input chat-debugger-protocol"
          value={protocol}
          onChange={(e) => { setProtocol(e.target.value as 'openai' | 'anthropic'); setMessages([]); }}
        >
          <option value="openai">OpenAI /v1/chat/completions</option>
          <option value="anthropic">Anthropic /v1/messages</option>
        </select>

        {!compact && (
          <>
            <select
              className="form-input chat-debugger-protocol"
              style={{ minWidth: 150 }}
              value={promptPreset}
              onChange={(e) => {
                const v = e.target.value;
                setPromptPreset(v);
                setSystemPrompt(PROMPT_PRESETS[v] || '');
              }}
              title={t('系统提示词预设')}
            >
              <option value="">{t('系统提示词预设')}</option>
              <option value="general">{t('通用助手')}</option>
              <option value="code">{t('代码助手')}</option>
              <option value="translate">{t('翻译助手')}</option>
            </select>
            <select
              className="form-input chat-debugger-protocol"
              style={{ minWidth: 130 }}
              value={tempPreset}
              onChange={(e) => {
                const v = e.target.value;
                setTempPreset(v);
                if (TEMP_PRESETS[v]) setTemperature(TEMP_PRESETS[v]);
              }}
              title={t('参数预设')}
            >
              <option value="">{t('参数预设')}</option>
              <option value="balanced">{t('平衡')}</option>
              <option value="precise">{t('严谨')}</option>
              <option value="creative">{t('创意')}</option>
            </select>
          </>
        )}

        <label className="chat-debugger-stream">
          <input
            type="checkbox"
            checked={stream}
            onChange={(e) => setStream(e.target.checked)}
          />
          <span>{t('流式')}</span>
        </label>

        {!compact && (
          <>
            <input
              className="form-input chat-debugger-temp"
              type="number"
              step="0.1"
              min="0"
              max="2"
              title={t('温度')}
              value={temperature}
              onChange={(e) => setTemperature(e.target.value)}
            />
            <input
              className="form-input chat-debugger-max"
              type="number"
              min="1"
              title={t('最大输出 Token')}
              value={maxTokens}
              onChange={(e) => setMaxTokens(e.target.value)}
            />
          </>
        )}

        {!compact && (
          <input
            className="form-input chat-debugger-system"
            placeholder={t('系统提示词（可选）')}
            value={systemPrompt}
            onChange={(e) => setSystemPrompt(e.target.value)}
          />
        )}
      </div>
      )}

      {hideToolbar && floatingModelBar && (
        <div className="chat-debugger-bar chat-debugger-bar-min">
          {modelPicker}
        </div>
      )}

      {!hideToolbar && (
      <div className="chat-debugger-attachments">
        <select
          className="form-input"
          value={attachKind}
          onChange={(e) => setAttachKind(e.target.value as 'image' | 'video' | 'audio')}
        >
          <option value="image">{t('图片 URL')}</option>
          <option value="video">{t('视频 URL')}</option>
          <option value="audio">{t('音频 URL')}</option>
        </select>
        <input
          className="form-input"
          placeholder={t('粘贴媒体 URL（图片/视频/音频）')}
          value={attachUrl}
          onChange={(e) => setAttachUrl(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); addAttachment(); } }}
        />
        <button type="button" className="btn btn-outline btn-sm" onClick={addAttachment}>
          {t('添加')}
        </button>
        {attachments.length > 0 && (
          <div className="chat-debugger-attach-chips">
            {attachments.map((a, i) => (
              <span key={i} className="chat-debugger-chip" title={a.url}>
                {a.kind === 'image' && <Image size={12} />}
                {a.kind === 'video' && <Video size={12} />}
                {a.kind === 'audio' && <AudioLines size={12} />}
                {a.url.slice(0, 32)}…
                <button type="button" onClick={() => setAttachments((prev) => prev.filter((_, j) => j !== i))}>×</button>
              </span>
            ))}
          </div>
        )}
      </div>
      )}

      <div className="chat-debugger-messages">
        {messages.length === 0 && (
          <div className="chat-debugger-empty">
            {hideToolbar && !floatingModelBar ? (
              <div className="chat-debugger-empty-model">{modelPicker}</div>
            ) : null}
            <div className="chat-debugger-empty-title">{model || t('开始对话')}</div>
            <div className="chat-debugger-empty-sub">
              {t('输入消息开始对话。支持多轮上下文与图片/视频/音频附件（OpenAI 协议）。')}
            </div>
            {suggestionPrompts.length > 0 && (
              <div className="chat-debugger-empty-suggestions">
                {suggestionPrompts.map((s) => (
                  <button
                    key={s.title}
                    type="button"
                    className="chat-debugger-suggestion"
                    onClick={() => {
                      void handleSend(s.content);
                    }}
                  >
                    <span className="chat-debugger-suggestion-title">{s.title}</span>
                    <span className="chat-debugger-suggestion-sub">{s.sub}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`chat-debugger-msg chat-debugger-msg-${m.role}`}>
            <span className="chat-debugger-msg-icon">
              {m.role === 'user' ? <User size={13} /> : <Bot size={13} />}
            </span>
            <div className="chat-debugger-msg-body">
              {m.attachments?.map((a, j) => (
                <div key={j} className="chat-debugger-msg-media">
                  {a.kind === 'image' && <img src={a.url} alt="attachment" loading="lazy" />}
                  {a.kind === 'video' && <video src={a.url} controls muted />}
                  {a.kind === 'audio' && <audio src={a.url} controls />}
                </div>
              ))}
              <div className="chat-debugger-msg-content">
                <MessageViewer content={m.content} reasoning={m.reasoning} />
              </div>
              <button
                type="button"
                className="chat-debugger-copy-btn"
                title={t('复制消息')}
                onClick={() => copyMessage(m.reasoning ? `${m.reasoning}\n\n${m.content}` : m.content, i)}
              >
                {copiedIdx === i ? <Check size={12} /> : <Copy size={12} />}
              </button>
            </div>
          </div>
        ))}
        {busy &&
          (stream ? null : (
            <div className="chat-debugger-msg chat-debugger-msg-assistant">
              <span className="chat-debugger-msg-icon"><Bot size={13} /></span>
              <div className="chat-debugger-msg-body">
                <Loader2 size={14} className="chat-debugger-spin" /> {t('思考中…')}
              </div>
            </div>
          ))}
        <div ref={messagesEndRef} />
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="chat-debugger-input-row">
        <div className="chat-debugger-input-shell">
          <textarea
            rows={2}
            placeholder={t('输入消息，Enter 发送，Shift+Enter 换行')}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={busy}
          />
          <div className="chat-debugger-input-meta">
            <div className="chat-debugger-context" title={t('上下文 ≈')}>
              {t('上下文 ≈')} {contextEstimate.toLocaleString()} tokens
            </div>
            <div className="chat-debugger-input-actions">
              <button type="button" className="btn btn-outline btn-sm" onClick={clearAll} disabled={busy || !messages.length} title={t('清空对话')}>
                <Trash2 size={14} />
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={busy ? stopStreaming : () => void handleSend()}
                disabled={(!busy && ((!input.trim() && !attachments.length) || !model))}
                title={busy ? t('停止生成') : t('发送')}
              >
                {busy ? <Square size={14} /> : <Send size={14} />}
                {busy ? t('停止') : t('发送')}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
