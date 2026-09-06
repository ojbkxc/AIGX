import { useState, useEffect, useRef, type KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Search, Send, Trash2, Image, Video, AudioLines, Loader2, Bot, User } from 'lucide-react';
import { api } from '../api';

interface DebugMessage {
  role: 'user' | 'assistant';
  content: string;
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
  const { channelId, channelModels = [], initialProtocol = 'openai', compact = false } = props;
  const { t } = useTranslation();

  const [messages, setMessages] = useState<DebugMessage[]>([]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState('');
  const [protocol, setProtocol] = useState<'openai' | 'anthropic'>(initialProtocol);
  const [stream, setStream] = useState(true);
  const [systemPrompt, setSystemPrompt] = useState('');
  const [temperature, setTemperature] = useState('0.7');
  const [maxTokens, setMaxTokens] = useState('1024');
  const [query, setQuery] = useState('');
  const [pickerOpen, setPickerOpen] = useState(false);
  const pickerRef = useRef<HTMLDivElement | null>(null);
  const messagesEndRef = useRef<HTMLDivElement | null>(null);

  const [attachments, setAttachments] = useState<Array<{ kind: 'image' | 'video' | 'audio'; url: string }>>([]);
  const [attachKind, setAttachKind] = useState<'image' | 'video' | 'audio'>('image');
  const [attachUrl, setAttachUrl] = useState('');

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

  const handleSend = async (): Promise<void> => {
    const text = input.trim();
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
      const res = (await api.testChannelChat(body)) as ChatChunkResult;
      if (res.stream) {
        let acc = '';
        for (const chk of res.stream) {
          acc += chk.content || '';
          setMessages((prev) => {
            const next = prev.slice();
            if (next.length && next[next.length - 1].role === 'assistant') {
              next[next.length - 1] = { role: 'assistant', content: acc };
            } else {
              next.push({ role: 'assistant', content: acc });
            }
            return next;
          });
        }
        setMessages((prev) => {
          if (!prev.length || prev[prev.length - 1].role !== 'assistant') {
            return [...prev, { role: 'assistant', content: acc }];
          }
          return prev;
        });
      } else {
        const data = res.data || {};
        if (data.content) {
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
      setError(msg);
      setMessages((prev) => [...prev, { role: 'assistant', content: `⚠️ ${msg}` }]);
    } finally {
      setBusy(false);
    }
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
  };

  return (
    <div className={`chat-debugger ${compact ? 'chat-debugger-compact' : ''}`}>
      <div className="chat-debugger-bar">
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
                  onChange={(e) => setQuery(e.target.value)}
                />
              </div>
              <div className="chat-debugger-picker-list">
                {visibleModels.length === 0 && (
                  <div className="chat-debugger-picker-empty">{t('无匹配模型')}</div>
                )}
                {visibleModels.map((m) => (
                  <button
                    type="button"
                    key={m}
                    className={`chat-debugger-picker-item ${m === model ? 'active' : ''}`}
                    onClick={() => { setModel(m); setPickerOpen(false); setMessages([]); }}
                  >
                    {m}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>

        <select
          className="form-input chat-debugger-protocol"
          value={protocol}
          onChange={(e) => { setProtocol(e.target.value as 'openai' | 'anthropic'); setMessages([]); }}
        >
          <option value="openai">OpenAI /v1/chat/completions</option>
          <option value="anthropic">Anthropic /v1/messages</option>
        </select>

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

      <div className="chat-debugger-messages">
        {messages.length === 0 && (
          <div className="chat-debugger-empty">
            {t('输入消息开始对话。支持多轮上下文与图片/视频/音频附件（OpenAI 协议）。')}
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
              <div className="chat-debugger-msg-content">{m.content}</div>
            </div>
          </div>
        ))}
        {busy && (
          <div className="chat-debugger-msg chat-debugger-msg-assistant">
            <span className="chat-debugger-msg-icon"><Bot size={13} /></span>
            <div className="chat-debugger-msg-body">
              <Loader2 size={14} className="chat-debugger-spin" /> {t('思考中…')}
            </div>
          </div>
        )}
        <div ref={messagesEndRef} />
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="chat-debugger-input-row">
        <textarea
          className="form-input"
          rows={2}
          placeholder={t('输入消息，Enter 发送，Shift+Enter 换行')}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={busy}
        />
        <button type="button" className="btn btn-outline btn-sm" onClick={clearAll} disabled={busy || !messages.length} title={t('清空对话')}>
          <Trash2 size={14} />
        </button>
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => void handleSend()}
          disabled={busy || (!input.trim() && !attachments.length) || !model}
        >
          {busy ? <Loader2 size={14} className="chat-debugger-spin" /> : <Send size={14} />}
          {t('发送')}
        </button>
      </div>
    </div>
  );
}