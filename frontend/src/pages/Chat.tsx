import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  BarChart3, Code2, GraduationCap, ClipboardList,
} from 'lucide-react';
import ChatDebugger, { type DebugMessage } from '../components/ChatDebugger';
import './Chat.css';

/** 读取「已启用」提示词并映射为空状态建议卡片（无则用 new-api 同款默认四项） */
function loadSuggestionPrompts(): Array<{ title: string; sub: string; content: string }> {
  try {
    const raw = localStorage.getItem('aigx_prompts');
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((p): p is { id: string; name: string; content: string; tags?: string[]; enabled?: boolean } =>
        Boolean(p && typeof p === 'object' && (p as { enabled?: boolean }).enabled !== false))
      .slice(0, 6)
      .map((p) => ({
        title: p.name || 'Prompt',
        sub: (p.tags ?? []).slice(0, 3).join(' · '),
        content: p.content,
      }))
      .filter((p) => p.content);
  } catch {
    return [];
  }
}

const DEFAULT_PROMPTS: Array<{ icon: typeof BarChart3; content: string }> = [
  { icon: BarChart3, content: '帮我分析一组数据的趋势并给出结论' },
  { icon: ClipboardList, content: '帮我总结一段长文本的要点' },
  { icon: Code2, content: '写一个可运行的代码示例并解释关键点' },
  { icon: GraduationCap, content: '我想学习一个新领域，给我一条完整的学习路径' },
];

/**
 * Chat — new-api 游乐园（Playground）布局。
 *
 * 无侧栏、无会话列表、无模式切换：单一对话流 + 底部输入区。
 * 输入区底部工具行对齐 new-api：
 *   [附件 + 参数(带启用数徽标) + 清空] ··· [模型选择器 + 发送/停止]
 * 消息持久化走 localStorage 单会话（debounce 保存）。
 */
export default function Chat(): JSX.Element {
  const { t } = useTranslation();

  const [messages, setMessages] = useState<DebugMessage[]>([]);
  const [model, setModel] = useState('');
  const [loading, setLoading] = useState(true);
  const saveTimer = useRef<number | null>(null);
  const latestRef = useRef<DebugMessage[]>([]);
  const loadedRef = useRef(false);

  // 恢复上次会话（setTimeout 0，避免首帧闪烁旧消息后又被清空的竞态）
  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      try {
        const raw = localStorage.getItem('aigx_playground_messages');
        const parsed = raw ? JSON.parse(raw) as unknown : [];
        if (!cancelled && Array.isArray(parsed)) {
          setMessages(parsed.filter((m): m is DebugMessage =>
            Boolean(m && typeof m === 'object' && (m as DebugMessage).role)));
        }
      } catch { /* 损坏数据静默丢弃 */ }
      loadedRef.current = true;
      setLoading(false);
    }, 0);
    return () => { cancelled = true; window.clearTimeout(timer); };
  }, []);

  // debounce 持久化
  useEffect(() => {
    latestRef.current = messages;
    if (!loadedRef.current) return;
    if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      saveTimer.current = null;
      try {
        localStorage.setItem('aigx_playground_messages', JSON.stringify(latestRef.current));
      } catch { /* 存储写满静默降级 */ }
    }, 500);
  }, [messages]);

  // 卸载时立即落盘
  useEffect(() => () => {
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      try {
        localStorage.setItem('aigx_playground_messages', JSON.stringify(latestRef.current));
      } catch { /* noop */ }
    }
  }, []);

  const suggestions = useMemo(() => {
    const custom = loadSuggestionPrompts();
    if (custom.length) {
      return custom.map((s, i) => ({
        icon: DEFAULT_PROMPTS[i % DEFAULT_PROMPTS.length].icon,
        title: s.title,
        sub: s.sub,
        content: s.content,
      }));
    }
    return DEFAULT_PROMPTS.map((p) => ({ ...p, title: p.content, sub: t('提示词') }));
  }, [t]);

  const handleMessagesChange = (next: DebugMessage[]): void => {
    setMessages(next);
  };

  const handleClear = (): void => {
    setMessages([]);
    try {
      localStorage.removeItem('aigx_playground_messages');
    } catch { /* noop */ }
  };

  return (
    <div className="pg-shell">
      <ChatDebugger
        playground
        playgroundModel={model}
        onPlaygroundModelChange={setModel}
        initialMessages={loading ? [] : messages}
        onMessagesChange={handleMessagesChange}
        suggestionPrompts={suggestions}
        onClearMessages={handleClear}
        hasMessages={messages.length > 0}
      />
    </div>
  );
}
