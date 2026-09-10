import { useState, useEffect, useRef, type KeyboardEvent, lazy, Suspense } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Send, Square, Trash2, Image, Video, AudioLines, Loader2, Bot,
  Zap, Mic, Plus, Link2, SlidersHorizontal, MessageSquarePlus, Paperclip,
  BarChart3, Code2, GraduationCap, ClipboardList,
} from 'lucide-react';
import { api, testChannelChatStream } from '../api';
import type { DebugMessage } from './chat/types';
import ModelPicker from './ModelPicker';
import './ChatDebugger.css';

// 懒加载 ChatBubble 组件
const ChatBubble = lazy(() => import('./chat/ChatBubble'));

// 重新导出 DebugMessage 类型（向后兼容）
export type { DebugMessage };

/** playground 空状态建议 prompt 默认图标序列（new-api 同款四图标） */
const DEFAULT_PG_ICONS = [BarChart3, ClipboardList, Code2, GraduationCap];


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
  /** 空状态建议 prompt（Open WebUI 首页 Suggestions 网格），点选直接发送。
   *  playground 模式下渲染为 new-api 游乐园式按钮卡片。 */
  suggestionPrompts?: Array<{ title: string; sub: string; content: string; icon?: unknown }>;
  /** 受控模型值：由宿主顶栏提供时，模型选择下沉到外部（/chat） */
  model?: string;
  /** 模型变化回调（受控模式下同步宿主状态） */
  onModelChange?: (model: string) => void;
  /** new-api 游乐园形态：无工具条，模型/参数/附件收进输入区底部工具行 */
  playground?: boolean;
  /** playground 形态：受控模型值（宿主 /chat 页持有） */
  playgroundModel?: string;
  /** playground 形态：模型变化回调 */
  onPlaygroundModelChange?: (model: string) => void;
  /** playground 形态：清空对话回调（宿主持久化层同步清空） */
  onClearMessages?: () => void;
  /** playground 形态：是否有消息（控制清空按钮禁用态） */
  hasMessages?: boolean;
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
    model: controlledModel,
    onModelChange,
    playground = false,
    playgroundModel,
    onPlaygroundModelChange,
    onClearMessages,
    hasMessages = false,
  } = props;
  const { t } = useTranslation();

  const [messages, setMessages] = useState<DebugMessage[]>(initialMessages);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const [, setModels] = useState<string[]>([]);
  const [internalModel, setInternalModel] = useState('');
  // 受控模型：宿主顶栏提供时以外部值为准，本地 state 仅兜底；
  // playground 形态下由 playgroundModel 接管（宿主 /chat 页持有）
  const effectiveControlled = playground
    ? (playgroundModel !== undefined ? playgroundModel : controlledModel)
    : controlledModel;
  const model = effectiveControlled !== undefined ? effectiveControlled : internalModel;
  const notifyModel = (v: string): void => {
    setInternalModel(v);
    if (playground) onPlaygroundModelChange?.(v);
    else onModelChange?.(v);
  };
  const setModel = notifyModel;
  const [protocol, setProtocol] = useState<'openai' | 'anthropic'>(initialProtocol);
  const [stream, setStream] = useState(true);
  const [systemPrompt, setSystemPrompt] = useState('');
  // 系统提示词预设：无（默认）/通用/代码/翻译
  const [promptPreset, setPromptPreset] = useState('');
  // 参数预设：平衡/严谨/创意（联动温度）
  const [tempPreset, setTempPreset] = useState('');
  const [temperature, setTemperature] = useState('0.7');
  const [maxTokens, setMaxTokens] = useState('1024');
  // B6: 扩展参数
  const [topP, setTopP] = useState('1');
  const [frequencyPenalty, setFrequencyPenalty] = useState('0');
  const [presencePenalty, setPresencePenalty] = useState('0');
  // B6: 每个参数独立开关，关闭时不发送到上游
  const [tempEnabled, setTempEnabled] = useState(true);
  const [topPEnabled, setTopPEnabled] = useState(false);
  const [freqPenEnabled, setFreqPenEnabled] = useState(false);
  const [presPenEnabled, setPresPenEnabled] = useState(false);
  const [maxTokensEnabled, setMaxTokensEnabled] = useState(false);
  // playground 形态：参数 Popover 与清空确认
  const [paramPopOpen, setParamPopOpen] = useState(false);
  const [clearConfirmOpen, setClearConfirmOpen] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement | null>(null);
  /** 流式生成中断控制器：用户点「停止」时 abort 上游请求 */
  const abortRef = useRef<AbortController | null>(null);

  const [attachments, setAttachments] = useState<Array<{ kind: 'image' | 'video' | 'audio'; url: string }>>([]);
  const [attachKind, setAttachKind] = useState<'image' | 'video' | 'audio'>('image');
  const [attachUrl, setAttachUrl] = useState('');
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);

  // ── Lxchat 式输入区状态 ──
  /** 「+」附件菜单开关 */
  const [attachMenuOpen, setAttachMenuOpen] = useState(false);
  const attachMenuRef = useRef<HTMLDivElement | null>(null);
  /** 粘贴媒体 URL：小输入行开关与草稿 */
  const [urlPromptOpen, setUrlPromptOpen] = useState(false);
  const [urlDraft, setUrlDraft] = useState('');
  /** 语音输入：录音中/转写中 */
  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const mediaRef = useRef<MediaRecorder | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  /** 组件卸载标记：录音 onstop 异步转写回调防 setState */
  const mountedRef = useRef(true);
  /** TTS 朗读：正在合成/正在播放的消息下标 */
  const [ttsIdx, setTtsIdx] = useState<number | null>(null);
  const [ttsPlaying, setTtsPlaying] = useState(false);
  const audioRef = useRef<HTMLAudioElement | null>(null);

  // /chat 会话持久化：消息每次变化都同步给宿主页面。
  // 回调走 ref，避免宿主页面每次渲染传入新函数引用导致
  // 「保存 → setState → 重渲染 → 新回调 → 再保存」的无限循环。
  const onMessagesChangeRef = useRef(onMessagesChange);
  useEffect(() => {
    onMessagesChangeRef.current = onMessagesChange;
  }, [onMessagesChange]);

  useEffect(() => {
    onMessagesChangeRef.current?.(messages);
  }, [messages]);

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
    const text = m.content + (m.reasoning || '');
    const cjk = (text.match(/[\u4e00-\u9fff]/g) || []).length;
    const other = text.length - cjk;
    return n + cjk + Math.ceil(other / 4);
  }, 0) + input.length;

  const copyMessage = (text: string, idx: number): boolean => {
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
        const ok = fallbackCopy();
        if (ok) setCopiedIdx(idx);
        setTimeout(() => setCopiedIdx(null), 1500);
        return ok;
      });
      return true;
    } else if (fallbackCopy()) {
      setCopiedIdx(idx);
      setTimeout(() => setCopiedIdx(null), 1500);
      return true;
    }
    return false;
  };

  // 合并模型：渠道模型优先，网关映射模型兜底（去重）
  // 渠道模型列表：依赖 join 后的字符串而非数组引用，避免父组件
  // 每次渲染传入新数组导致 effect 反复触发。
  const channelModelsKey = channelModels.join(',');

  useEffect(() => {
    let mounted = true;
    if (channelModels.length) {
      setModels(channelModels.slice());
      if (!model) setModel(channelModels[0]);
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
        if (list.length && !model) setModel(list[0]);
      })
      .catch(() => { /* 模型列表失败静默降级 */ });
    return () => { mounted = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [channelModelsKey]);

  useEffect(() => {
    if (messagesEndRef.current) {
      messagesEndRef.current.scrollIntoView({ behavior: 'smooth', block: 'end' });
    }
  }, [messages, busy]);

  // 点击外部关闭附件菜单
  useEffect(() => {
    const onDoc = (e: MouseEvent) => {
      if (attachMenuRef.current && !attachMenuRef.current.contains(e.target as Node)) {
        setAttachMenuOpen(false);
      }
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, []);

  // 卸载时停止录音 / 停止 TTS 播放
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (mediaRef.current && mediaRef.current.state === 'recording') {
        mediaRef.current.stop();
      }
      audioRef.current?.pause();
      audioRef.current = null;
    };
  }, []);

  // B7: 消息时间戳和响应时长追踪
  const messageTimestamps = useRef<Map<number, number>>(new Map());
  const messageDurations = useRef<Map<number, number>>(new Map());

  // B8: 删除消息处理
  const deleteMessage = (idx: number): void => {
    setMessages((prev) => prev.filter((_, i) => i !== idx));
    messageTimestamps.current.delete(idx);
    messageDurations.current.delete(idx);
  };

  const addAttachment = (): void => {
    const url = attachUrl.trim();
    if (!url) return;
    setAttachments((prev) => [...prev, { kind: attachKind, url }]);
    setAttachUrl('');
  };

  /** 「+」菜单：本地文件 → data URI 附件（图片/视频/音频按 MIME 自动分类） */
  const pickLocalFile = (accept: string): void => {
    const inputEl = document.createElement('input');
    inputEl.type = 'file';
    inputEl.accept = accept;
    inputEl.multiple = false;
    inputEl.onchange = () => {
      const file = inputEl.files?.[0];
      if (!file) return;
      const reader = new FileReader();
      reader.onload = () => {
        const url = String(reader.result || '');
        const kind: 'image' | 'video' | 'audio' = file.type.startsWith('video')
          ? 'video'
          : file.type.startsWith('audio') ? 'audio' : 'image';
        setAttachments((prev) => [...prev, { kind, url }]);
      };
      reader.readAsDataURL(file);
    };
    inputEl.click();
    setAttachMenuOpen(false);
  };

  /** 「粘贴媒体 URL」小输入行：Enter 确认加为图片附件 */
  const commitUrlDraft = (): void => {
    const url = urlDraft.trim();
    if (url) setAttachments((prev) => [...prev, { kind: 'image', url }]);
    setUrlDraft('');
    setUrlPromptOpen(false);
  };

  /** 语音输入：MediaRecorder 录音 → 上传转写 → 填入输入框 */
  const startRecording = async (): Promise<void> => {
    if (recording) {
      mediaRef.current?.stop();
      return;
    }
    if (!navigator.mediaDevices?.getUserMedia) {
      setError(t('当前浏览器不支持语音输入'));
      return;
    }
    try {
      const streamObj = await navigator.mediaDevices.getUserMedia({ audio: true });
      const mime = MediaRecorder.isTypeSupported('audio/webm') ? 'audio/webm' : '';
      const recorder = new MediaRecorder(streamObj, mime ? { mimeType: mime } : undefined);
      chunksRef.current = [];
      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) chunksRef.current.push(e.data);
      };
      recorder.onstop = async () => {
        if (!mountedRef.current) return;
        streamObj.getTracks().forEach((tr) => tr.stop());
        setRecording(false);
        const blob = new Blob(chunksRef.current, { type: mime || 'audio/webm' });
        if (blob.size < 1024) return; // 过短的空录音直接丢弃
        setTranscribing(true);
        try {
          const res = await api.playgroundTranscribe(blob, model || 'whisper');
          if (!mountedRef.current) return;
          const text = (res.text || '').trim();
          if (text) setInput((prev) => (prev ? `${prev} ${text}` : text));
          else setError(t('未识别到语音内容'));
        } catch (err) {
          if (!mountedRef.current) return;
          setError(err instanceof Error ? err.message : String(err));
        } finally {
          if (mountedRef.current) setTranscribing(false);
        }
      };
      mediaRef.current = recorder;
      recorder.start();
      setRecording(true);
      setError('');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(t('无法访问麦克风') + `: ${msg}`);
    }
  };

  /** TTS 朗读：再次点击同一消息 = 停止 */
  const speakMessage = async (idx: number): Promise<void> => {
    if (ttsPlaying && ttsIdx === idx) {
      audioRef.current?.pause();
      setTtsPlaying(false);
      setTtsIdx(null);
      return;
    }
    audioRef.current?.pause();
    const m = messages[idx];
    if (!m?.content?.trim()) return;
    setTtsIdx(idx);
    setTtsPlaying(false);
    try {
      const res = await api.playgroundTts({ model: model || 'tts', input: m.content });
      const data = res.data;
      if (!data?.audio_base64) throw new Error(t('未返回音频'));
      const audio = new Audio(`data:${data.content_type || 'audio/mpeg'};base64,${data.audio_base64}`);
      audioRef.current = audio;
      audio.onended = () => { setTtsPlaying(false); setTtsIdx(null); };
      audio.onerror = () => { setTtsPlaying(false); setTtsIdx(null); };
      await audio.play();
      setTtsPlaying(true);
    } catch (err) {
      setTtsIdx(null);
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  /** 重新生成：截断到最后一条用户消息后重发 */
  const regenerate = async (): Promise<void> => {
    if (busy) return;
    const lastUserIdx = messages.map((m) => m.role).lastIndexOf('user');
    if (lastUserIdx < 0) return;
    const userMsg = messages[lastUserIdx];
    setMessages(messages.slice(0, lastUserIdx));
    void handleSend(userMsg.content, userMsg.attachments?.slice(), messages.slice(0, lastUserIdx));
  };

  /** 编辑用户消息：进入编辑态 */
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState('');
  const beginEdit = (idx: number): void => {
    setEditingIdx(idx);
    setEditDraft(messages[idx]?.content || '');
  };
  const commitEdit = (): void => {
    if (editingIdx == null) return;
    const text = editDraft.trim();
    const attachmentsOf = messages[editingIdx]?.attachments?.slice();
    setEditingIdx(null);
    if (!text) return;
    setMessages(messages.slice(0, editingIdx));
    void handleSend(text, attachmentsOf, messages.slice(0, editingIdx));
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

  const handleSend = async (override?: string, overrideAttachments?: Array<{ kind: 'image' | 'video' | 'audio'; url: string }>, prevMessages?: DebugMessage[]): Promise<void> => {
    const text = (override ?? input).trim();
    const atts = overrideAttachments ?? attachments;
    if ((!text && !atts.length) || busy) return;
    if (!model) {
      setError(t('请先选择模型'));
      return;
    }
    const pendingAttachments = atts.slice();
    const historySource = prevMessages ?? messages;

    // 历史消息：附件展开为 content blocks；纯文本保持字符串形状。
    // 在截断后的消息数组（或当前闭包 messages）基础上追加本条用户消息，保证多轮上下文完整。
    const history: Array<{ role: string; content: string | Record<string, unknown>[] }> = historySource.map((m) => {
      if (m.role === 'user' && m.attachments?.length) {
        const blocks = blocksForAttachments(m.attachments);
        if (m.content) blocks.push({ type: 'text', text: m.content });
        return { role: 'user', content: blocks };
      }
      return { role: m.role, content: m.content };
    });
    const currentBlocks = blocksForAttachments(pendingAttachments);
    if (text) currentBlocks.push({ type: 'text', text });
    history.push({
      role: 'user',
      content: pendingAttachments.length ? currentBlocks : text,
    });

    setMessages((prev) => {
      // 重新生成/编辑重发场景：messages 已被截断到此条之前，直接拼接
      const hasPendingUser = prev.length > 0 && prev[prev.length - 1].role === 'user'
        && prev[prev.length - 1].content === text;
      if (hasPendingUser) return prev;
      const userIdx = prev.length;
      messageTimestamps.current.set(userIdx, Date.now());
      return [...prev, { role: 'user', content: text, attachments: pendingAttachments }];
    });
    if (override == null) setInput('');
    setAttachments([]);
    setBusy(true);
    setError('');

    const body: Record<string, unknown> = {
      channel_id: channelId || '',
      protocol,
      model,
      message: text,
      history,
      stream,
    };
    if (protocol === 'openai') {
      if (tempEnabled) body.temperature = Number(temperature) || 0.7;
      if (maxTokensEnabled) body.max_tokens = Number(maxTokens) || 1024;
      if (topPEnabled) body.top_p = Number(topP) || 1;
      if (freqPenEnabled) body.frequency_penalty = Number(frequencyPenalty) || 0;
      if (presPenEnabled) body.presence_penalty = Number(presencePenalty) || 0;
    }
    if (systemPrompt.trim()) {
      body.system_prompt = systemPrompt.trim();
    }
    // 多模态附件：OpenAI 协议下把 message 换成 content blocks
    if (pendingAttachments.length) {
      body.message = currentBlocks;
    }

    const requestStartTime = Date.now();
    try {
      if (stream) {
        const controller = new AbortController();
        abortRef.current = controller;
        // 真·流式：占位一条 assistant 消息，逐增量拼接渲染
        setMessages((prev) => {
          const assistantIdx = prev.length;
          messageTimestamps.current.set(assistantIdx, requestStartTime);
          return [...prev, { role: 'assistant', content: '…' }];
        });
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
        const res = (await api.testChannelChat(body)) as unknown as ChatChunkResult;
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
        setMessages((prev) => {
          const next = prev.slice();
          const last = next[next.length - 1];
          // 停在「…」占位阶段时直接替换占位，避免留下空白气泡
          if (last && last.role === 'assistant' && (last.content === '…' || !last.content.trim())) {
            next[next.length - 1] = { ...last, content: `⏹ ${t('已停止生成')}` };
          } else {
            next.push({ role: 'assistant', content: `⏹ ${t('已停止生成')}` });
          }
          return next;
        });
        abortRef.current = null;
        return;
      }
      setError(msg);
      setMessages((prev) => [...prev, { role: 'assistant', content: `⚠️ ${msg}` }]);
    } finally {
      const duration = Date.now() - requestStartTime;
      setMessages((prev) => {
        const idx = prev.length - 1;
        if (idx >= 0 && prev[idx].role === 'assistant') {
          messageDurations.current.set(idx, duration);
        }
        return prev;
      });
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
    onClearMessages?.();
  };

  // A10: 使用统一的 ModelPicker 组件替换内联 picker
  const modelPicker = (
    <ModelPicker
      value={model}
      onChange={(m) => {
        setModel(m);
        if (!playground) setMessages([]);
      }}
      channelModels={channelModels}
      compact={compact}
    />
  );

  // playground 形态：new-api 游乐园式模型选择（输入区底部工具行，切换不丢消息）
  const playgroundModelPicker = (
    <ModelPicker
      value={model}
      onChange={setModel}
      channelModels={channelModels}
      compact
    />
  );

  /** new-api 同款参数控件表（6 参数：滑块/数字输入 + 独立开关） */
  const PARAM_CONTROLS: Array<{
    key: string;
    label: string;
    desc: string;
    type: 'slider' | 'number';
    min: number;
    max: number;
    step: number;
    enabled: boolean;
    onEnabled: (v: boolean) => void;
    value: string;
    onValue: (v: string) => void;
    streamToggle?: boolean;
  }> = [
    { key: 'temperature', label: t('温度'), desc: t('控制随机性与创意'), type: 'slider', min: 0, max: 2, step: 0.1, enabled: tempEnabled, onEnabled: setTempEnabled, value: temperature, onValue: setTemperature },
    { key: 'top_p', label: 'Top P', desc: t('限制候选 token 的概率质量'), type: 'slider', min: 0, max: 1, step: 0.01, enabled: topPEnabled, onEnabled: setTopPEnabled, value: topP, onValue: setTopP },
    { key: 'frequency_penalty', label: t('频率惩罚'), desc: t('减少字词重复'), type: 'slider', min: -2, max: 2, step: 0.1, enabled: freqPenEnabled, onEnabled: setFreqPenEnabled, value: frequencyPenalty, onValue: setFrequencyPenalty },
    { key: 'presence_penalty', label: t('存在惩罚'), desc: t('鼓励引入新话题'), type: 'slider', min: -2, max: 2, step: 0.1, enabled: presPenEnabled, onEnabled: setPresPenEnabled, value: presencePenalty, onValue: setPresencePenalty },
    { key: 'max_tokens', label: t('最大输出 Token'), desc: t('限制响应长度'), type: 'number', min: 1, max: 200000, step: 1, enabled: maxTokensEnabled, onEnabled: setMaxTokensEnabled, value: maxTokens, onValue: setMaxTokens },
  ];

  const paramActiveCount = PARAM_CONTROLS.filter((c) => c.enabled).length + (stream ? 1 : 0);

  /** playground 参数 Popover（new-api PlaygroundParameterPanel 观感） */
  const playgroundParamPanel = (
    <div className="pg-param-pop">
      <div className="pg-param-head">
        <div className="pg-param-title">{t('参数设置')}</div>
        <div className="pg-param-sub">{t('仅启用的参数会随请求发送')}</div>
      </div>
      <div className="pg-param-list">
        {PARAM_CONTROLS.map((c) => (
          <div key={c.key} className={`pg-param-item ${(!c.enabled || busy) ? 'disabled' : ''}`}>
            <div className="pg-param-item-head">
              <div className="pg-param-item-text">
                <div className="pg-param-item-label">
                  <span>{c.label}</span>
                  <code>{c.value}</code>
                </div>
                <div className="pg-param-item-desc">{c.desc}</div>
              </div>
              <label className="pg-switch">
                <input
                  type="checkbox"
                  checked={c.enabled}
                  disabled={busy}
                  onChange={(e) => c.onEnabled(e.target.checked)}
                />
                <span />
              </label>
            </div>
            {c.type === 'slider' ? (
              <input
                type="range"
                className="pg-param-slider"
                min={c.min}
                max={c.max}
                step={c.step}
                value={c.value}
                disabled={busy || !c.enabled}
                onChange={(e) => c.onValue(e.target.value)}
              />
            ) : (
              <input
                type="number"
                className="form-input pg-param-number"
                min={c.min}
                max={c.max}
                step={c.step}
                value={c.value}
                disabled={busy || !c.enabled}
                onChange={(e) => c.onValue(e.target.value)}
              />
            )}
          </div>
        ))}
        {/* 流式开关：new-api 也作为参数面板里的一项 */}
        <div className={`pg-param-item ${busy ? 'disabled' : ''}`}>
          <div className="pg-param-item-head">
            <div className="pg-param-item-text">
              <div className="pg-param-item-label">
                <span>{t('流式')}</span>
                <code>{stream ? 'true' : 'false'}</code>
              </div>
              <div className="pg-param-item-desc">{t('逐字流式渲染响应')}</div>
            </div>
            <label className="pg-switch">
              <input
                type="checkbox"
                checked={stream}
                disabled={busy}
                onChange={(e) => setStream(e.target.checked)}
              />
              <span />
            </label>
          </div>
        </div>
      </div>
    </div>
  );

  return (
    <div className={`chat-debugger ${compact ? 'chat-debugger-compact' : ''} ${playground ? 'chat-debugger-playground' : ''}`}>
      {!hideToolbar && !playground && (
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
            <label className="chat-debugger-slider" title={t('温度')}>
              <input
                type="checkbox"
                checked={tempEnabled}
                onChange={(e) => setTempEnabled(e.target.checked)}
              />
              <span>Temp</span>
              <input
                type="range" step="0.1" min="0" max="2"
                value={temperature}
                disabled={!tempEnabled}
                onChange={(e) => setTemperature(e.target.value)}
              />
              <output>{temperature}</output>
            </label>
            <label className="chat-debugger-slider" title={t('Top P')}>
              <input
                type="checkbox"
                checked={topPEnabled}
                onChange={(e) => setTopPEnabled(e.target.checked)}
              />
              <span>TopP</span>
              <input
                type="range" step="0.01" min="0" max="1"
                value={topP}
                disabled={!topPEnabled}
                onChange={(e) => setTopP(e.target.value)}
              />
              <output>{topP}</output>
            </label>
            <label className="chat-debugger-slider" title={t('频率惩罚')}>
              <input
                type="checkbox"
                checked={freqPenEnabled}
                onChange={(e) => setFreqPenEnabled(e.target.checked)}
              />
              <span>Freq</span>
              <input
                type="range" step="0.1" min="-2" max="2"
                value={frequencyPenalty}
                disabled={!freqPenEnabled}
                onChange={(e) => setFrequencyPenalty(e.target.value)}
              />
              <output>{frequencyPenalty}</output>
            </label>
            <label className="chat-debugger-slider" title={t('存在惩罚')}>
              <input
                type="checkbox"
                checked={presPenEnabled}
                onChange={(e) => setPresPenEnabled(e.target.checked)}
              />
              <span>Pres</span>
              <input
                type="range" step="0.1" min="-2" max="2"
                value={presencePenalty}
                disabled={!presPenEnabled}
                onChange={(e) => setPresencePenalty(e.target.value)}
              />
              <output>{presencePenalty}</output>
            </label>
            <label className="chat-debugger-slider" title={t('最大输出 Token')}>
              <input
                type="checkbox"
                checked={maxTokensEnabled}
                onChange={(e) => setMaxTokensEnabled(e.target.checked)}
              />
              <span>Max</span>
              <input
                className="form-input chat-debugger-max"
                type="number"
                min="1"
                style={{ width: 70 }}
                value={maxTokens}
                disabled={!maxTokensEnabled}
                onChange={(e) => setMaxTokens(e.target.value)}
              />
            </label>
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

      {/* 受控模型（/chat 顶栏 ModelPicker 提供）时不再渲染任何内部模型选择器：
       * 悬浮条与空状态大选择器都会与顶栏入口重复，唯一入口在宿主。 */}
      {hideToolbar && !playground && floatingModelBar && controlledModel === undefined && (
        <div className="chat-debugger-bar chat-debugger-bar-min">
          {modelPicker}
        </div>
      )}

      {!hideToolbar && !playground && (
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
        {messages.length === 0 && playground && (
          /* 空状态：图标 + 标题 + 提示词按钮网格 */
          <div className="pg-empty">
            <div className="pg-empty-icon"><MessageSquarePlus size={20} /></div>
            <h2 className="pg-empty-title">{t('有什么可以帮忙的？')}</h2>
            <p className="pg-empty-sub">{t('选择一个常用任务快速开始，或在下方直接输入你的请求。')}</p>
            <div className="pg-empty-prompts">
              {suggestionPrompts.map((s, idx) => {
                const Icon = (s.icon as typeof Zap | undefined) ?? DEFAULT_PG_ICONS[idx % DEFAULT_PG_ICONS.length];
                return (
                  <button
                    key={`${s.title}-${idx}`}
                    type="button"
                    className="pg-empty-prompt"
                    onClick={() => { void handleSend(s.content); }}
                  >
                    <Icon size={15} />
                    <span>{s.title}</span>
                  </button>
                );
              })}
            </div>
          </div>
        )}
        {messages.length === 0 && !playground && (
          <div className="chat-debugger-empty">
            {hideToolbar && !floatingModelBar && controlledModel === undefined ? (
              <div className="chat-debugger-empty-model">{modelPicker}</div>
            ) : null}
            <div className="chat-debugger-empty-title">{model || t('开始对话')}</div>
            <div className="chat-debugger-empty-sub">
              {t('输入消息开始对话。支持多轮上下文与图片/视频/音频附件（OpenAI 协议）。')}
            </div>
            {suggestionPrompts.length > 0 && (
              <>
                <div className="chat-debugger-empty-suggestions-label">
                  <Zap size={13} />
                  {t('推荐提示词')}
                </div>
                <div className="chat-debugger-empty-suggestions">
                  {suggestionPrompts.map((s, idx) => (
                    <button
                      key={`${s.title}-${idx}`}
                      type="button"
                      className="chat-debugger-suggestion"
                      style={{ animationDelay: `${idx * 45}ms` }}
                      onClick={() => {
                        void handleSend(s.content);
                      }}
                    >
                      <span className="chat-debugger-suggestion-title">{s.title}</span>
                      <span className="chat-debugger-suggestion-sub">{s.sub || t('提示词')}</span>
                    </button>
                  ))}
                </div>
              </>
            )}
          </div>
        )}
        <Suspense fallback={<div className="chat-debugger-loading">{t('加载中…')}</div>}>
        {messages.map((m, i) => (
            <ChatBubble
              key={i}
              message={m}
              index={i}
              isLast={i === messages.length - 1}
              isBusy={busy}
              isEditing={editingIdx === i}
              editDraft={editDraft}
              isCopied={copiedIdx === i}
              isTtsPlaying={ttsPlaying}
              isTtsTarget={ttsIdx === i}
              timestamp={messageTimestamps.current.get(i)}
              responseDuration={messageDurations.current.get(i)}
              onEditDraftChange={setEditDraft}
              onBeginEdit={() => beginEdit(i)}
              onCommitEdit={commitEdit}
              onCancelEdit={() => setEditingIdx(null)}
              onCopy={() => copyMessage(m.reasoning ? `${m.reasoning}\n\n${m.content}` : m.content, i)}
              onSpeak={() => void speakMessage(i)}
              onRegenerate={() => void regenerate()}
              onDelete={() => deleteMessage(i)}
            />
        ))}
        </Suspense>
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
          {urlPromptOpen && (
            <div className="chat-debugger-url-row">
              <Link2 size={13} />
              <input
                autoFocus
                placeholder={t('粘贴图片 URL，Enter 确认')}
                value={urlDraft}
                onChange={(e) => setUrlDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') { e.preventDefault(); commitUrlDraft(); }
                  if (e.key === 'Escape') { e.preventDefault(); setUrlDraft(''); setUrlPromptOpen(false); }
                }}
                onBlur={() => { setUrlDraft(''); setUrlPromptOpen(false); }}
              />
            </div>
          )}
          {attachments.length > 0 && (
            <div className="chat-debugger-attach-chips">
              {attachments.map((a, i) => (
                <span key={i} className="chat-debugger-chip" title={a.url.length > 48 ? a.url.slice(0, 200) : a.url}>
                  {a.kind === 'image' && <Image size={12} />}
                  {a.kind === 'video' && <Video size={12} />}
                  {a.kind === 'audio' && <AudioLines size={12} />}
                  {a.kind === 'image' ? t('图片') : a.kind === 'video' ? t('视频') : t('音频')}
                  <button type="button" onClick={() => setAttachments((prev) => prev.filter((_, j) => j !== i))}>×</button>
                </span>
              ))}
            </div>
          )}
          <textarea
            rows={2}
            placeholder={recording ? t('录音中…再次点击麦克风结束') : transcribing ? t('语音转文字中…') : t('输入消息，Enter 发送，Shift+Enter 换行')}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={busy}
            onPaste={(e) => {
              // 粘贴图片（截图）自动变附件
              const items = e.clipboardData?.items;
              if (!items) return;
              for (const item of items) {
                if (item.type.startsWith('image/')) {
                  const file = item.getAsFile();
                  if (!file) continue;
                  e.preventDefault();
                  const reader = new FileReader();
                  reader.onload = () => {
                    const url = String(reader.result || '');
                    if (url) setAttachments((prev) => [...prev, { kind: 'image', url }]);
                  };
                  reader.readAsDataURL(file);
                }
              }
            }}
          />
          <div className="chat-debugger-input-meta">
            <div className="chat-debugger-input-left">
              <div className="chat-debugger-attach-menu" ref={attachMenuRef}>
                <button
                  type="button"
                  className="chat-debugger-icon-btn"
                  title={t('添加附件')}
                  onClick={() => setAttachMenuOpen((v) => !v)}
                  disabled={busy}
                >
                  {playground ? <Paperclip size={15} /> : <Plus size={15} />}
                </button>
                {attachMenuOpen && (
                  <div className="chat-debugger-attach-pop">
                    <button type="button" onClick={() => pickLocalFile('image/*')}>
                      <Image size={13} /> {t('上传图片')}
                    </button>
                    <button type="button" onClick={() => pickLocalFile('video/*')}>
                      <Video size={13} /> {t('上传视频')}
                    </button>
                    <button type="button" onClick={() => pickLocalFile('audio/*')}>
                      <AudioLines size={13} /> {t('上传音频')}
                    </button>
                    <button
                      type="button"
                      onClick={() => { setAttachKind('image'); setAttachMenuOpen(false); setUrlPromptOpen(true); }}
                    >
                      <Link2 size={13} /> {t('粘贴媒体 URL')}
                    </button>
                  </div>
                )}
              </div>

              {playground ? (
                /* new-api 游乐园工具组：附件 + 参数(Popover) + 清空 */
                <div className="pg-tools">
                  <div className="pg-param-menu">
                    <button
                      type="button"
                      className="pg-tool-btn"
                      title={t('参数设置')}
                      disabled={busy}
                      onClick={() => setParamPopOpen((v) => !v)}
                    >
                      <SlidersHorizontal size={15} />
                      {paramActiveCount > 0 && (
                        <span className="pg-tool-badge">{paramActiveCount}</span>
                      )}
                    </button>
                    {paramPopOpen && (
                      <div className="pg-param-pop-anchor">
                        {playgroundParamPanel}
                      </div>
                    )}
                  </div>
                  <button
                    type="button"
                    className="pg-tool-btn pg-tool-danger"
                    title={t('清空对话')}
                    disabled={busy || !hasMessages}
                    onClick={() => setClearConfirmOpen(true)}
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              ) : (
                <div className="chat-debugger-context" title={t('上下文 ≈')}>
                  {t('上下文 ≈')} {contextEstimate.toLocaleString()} tokens
                </div>
              )}
            </div>
            <div className="chat-debugger-input-actions">
              {playground && (
                /* new-api 游乐园：模型选择器在输入区底部工具行（清空按钮右侧、发送按钮左侧） */
                <div className="pg-tools-right">
                  {playgroundModelPicker}
                </div>
              )}
              {!playground && (
                <button type="button" className="btn btn-outline btn-sm" onClick={clearAll} disabled={busy || !messages.length} title={t('清空对话')}>
                  <Trash2 size={14} />
                </button>
              )}
              {/* Lxchat 式圆形三态按钮：空输入=语音，有输入=发送，生成中=停止 */}
              <button
                type="button"
                className={`chat-debugger-send-fab ${recording ? 'recording' : ''} ${transcribing ? 'busy' : ''}`}
                onClick={() => {
                  if (busy) { stopStreaming(); return; }
                  if (!input.trim() && !attachments.length) { void startRecording(); return; }
                  void handleSend();
                }}
                disabled={transcribing || (!busy && !input.trim() && !attachments.length && !navigator.mediaDevices?.getUserMedia)}
                title={busy ? t('停止生成') : (!input.trim() && !attachments.length) ? t('语音输入') : t('发送')}
              >
                {busy ? <Square size={14} /> : (!input.trim() && !attachments.length) ? <Mic size={15} /> : <Send size={15} />}
              </button>
            </div>
          </div>

          {/* playground 清空确认弹窗 */}
          {clearConfirmOpen && (
            <div className="pg-confirm-overlay" onClick={() => setClearConfirmOpen(false)}>
              <div className="pg-confirm" onClick={(e) => e.stopPropagation()}>
                <div className="pg-confirm-title">{t('清空对话记录？')}</div>
                <div className="pg-confirm-desc">{t('浏览器中保存的当前对话将被移除，且无法撤销。')}</div>
                <div className="pg-confirm-actions">
                  <button type="button" className="btn btn-outline btn-sm" onClick={() => setClearConfirmOpen(false)}>
                    {t('取消')}
                  </button>
                  <button
                    type="button"
                    className="btn btn-danger btn-sm"
                    onClick={() => { clearAll(); setClearConfirmOpen(false); }}
                  >
                    {t('清空')}
                  </button>
                </div>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
