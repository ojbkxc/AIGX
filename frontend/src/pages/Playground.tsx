import React, { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Playground.css';

// Playground 页面：交互式对话调试。
// 复用 Channels.jsx 中 chat_test 的 SSE 解析模式，但独立为全页面布局：
// 左侧参数面板（模型 / temperature / max_tokens / stream），右侧对话区。
export default function Playground() {
  const { t } = useTranslation();
  const addToast = useToast();

  // 模型列表
  const [models, setModels] = useState([]);
  const [modelsLoading, setModelsLoading] = useState(true);

  // 对话状态
  const [messages, setMessages] = useState([]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  // 参数
  const [model, setModel] = useState('');
  const [temperature, setTemperature] = useState('0.7');
  const [maxTokens, setMaxTokens] = useState('1024');
  const [stream, setStream] = useState(true);
  // P2：系统提示词模板与参数预设
  const [systemPrompt, setSystemPrompt] = useState('');

  const messagesEndRef = useRef(null);

  // 初始化：拉取网关可用模型列表
  useEffect(() => {
    let mounted = true;
    setModelsLoading(true);
    api.listModels()
      .then((res) => {
        if (!mounted) return;
        // /v1/models 可能返回 { data: [{ id }] } 或字符串数组
        const list = Array.isArray(res) ? res
          : Array.isArray(res?.data) ? res.data.map((m) => (typeof m === 'string' ? m : m.id)).filter(Boolean)
          : [];
        setModels(list);
        if (list.length) setModel(list[0]);
      })
      .catch((err) => {
        if (!mounted) return;
        setError(err.message);
      })
      .finally(() => {
        if (mounted) setModelsLoading(false);
      });
    return () => { mounted = false; };
  }, []);

  // 对话区自动滚动到底部
  useEffect(() => {
    if (messagesEndRef.current) {
      messagesEndRef.current.scrollIntoView({ behavior: 'smooth', block: 'end' });
    }
  }, [messages, busy]);

  // 流式发送：直接 fetch /api/playground/chat，复用 testChannelChat 的 SSE 解析模式
  const sendStream = async (payload, userText) => {
    const token = localStorage.getItem('token');
    const res = await fetch('/api/playground/chat', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({ ...payload, stream: true }),
    });
    if (res.status === 401) {
      throw new Error('Unauthorized');
    }
    const contentType = res.headers.get('Content-Type') || '';
    if (!contentType.includes('text/event-stream')) {
      // 后端未走流式：按普通 JSON 处理
      const text = await res.text();
      let parsed = null;
      if (text) {
        try { parsed = JSON.parse(text); } catch { /* ignore */ }
      }
      if (!res.ok) {
        const errMsg = (parsed && (parsed.error || parsed.message)) || text || `Request failed with status ${res.status}`;
        throw new Error(errMsg);
      }
      const content = parsed?.content || parsed?.data?.content || '';
      if (content) {
        setMessages((prev) => [...prev, { role: 'assistant', content }]);
      }
      return;
    }

    // 流式：逐帧解析 SSE，累积增量
    const text = await res.text();
    let acc = '';
    let buf = '';
    const flush = () => {
      if (buf && buf !== '[DONE]') {
        try {
          const parsed = JSON.parse(buf);
          const content =
            (parsed.choices && parsed.choices[0] && parsed.choices[0].delta &&
              (parsed.choices[0].delta.content || parsed.choices[0].delta.text)) ||
            (parsed.delta && parsed.delta.text) ||
            (parsed.content && parsed.content[0] && parsed.content[0].text) || '';
          if (content) {
            acc += content;
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
        } catch {
          // 忽略非 JSON 帧
        }
      }
      buf = '';
    };
    for (const line of text.split('\n')) {
      const trimmed = line.trim();
      if (trimmed.startsWith('data:')) {
        buf += trimmed.slice(5).trim();
      } else if (trimmed === '') {
        flush();
      } else {
        buf += trimmed;
      }
    }
    flush();
    // 确保最后有一条 assistant 消息
    setMessages((prev) => {
      if (!prev.length || prev[prev.length - 1].role !== 'assistant') {
        return [...prev, { role: 'assistant', content: acc }];
      }
      return prev;
    });
  };

  // 非流式发送：走 api.playgroundChat
  const sendNormal = async (payload) => {
    const res = await api.playgroundChat({ ...payload, stream: false });
    const content = res?.content || res?.data?.content || '';
    if (content) {
      setMessages((prev) => [...prev, { role: 'assistant', content }]);
    } else if (res?.error) {
      setMessages((prev) => [...prev, { role: 'assistant', content: `⚠️ ${res.error}` }]);
    }
  };

  const handleSend = async () => {
    const text = input.trim();
    if (!text || busy) return;
    if (!model) {
      addToast(t('请先选择模型'), 'error');
      return;
    }
    // 追加用户消息（函数式更新，避免与流式累积竞态）
    setMessages((prev) => [...prev, { role: 'user', content: text }]);
    setInput('');
    setBusy(true);
    setError('');
    try {
      // 后端契约：playground 请求体为 messages 数组（旧 message/history 字段已废弃）
      const outgoing = [];
      if (systemPrompt.trim()) {
        outgoing.push({ role: 'system', content: systemPrompt.trim() });
      }
      outgoing.push(
        ...messages.map((m) => ({ role: m.role, content: m.content })),
        { role: 'user', content: text }
      );
      const payload = {
        model,
        messages: outgoing,
        temperature: Number(temperature) || 0.7,
        max_tokens: Number(maxTokens) || 1024,
      };
      if (stream) {
        await sendStream(payload, text);
      } else {
        await sendNormal(payload);
      }
    } catch (err) {
      setMessages((prev) => [...prev, { role: 'assistant', content: `⚠️ ${err.message}` }]);
    } finally {
      setBusy(false);
    }
  };

  const handleKeyDown = (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleClear = () => {
    setMessages([]);
    setError('');
  };

  // 参数预设：直接写入当前表单状态
  const SYSTEM_PRESETS = [
    { value: '', labelKey: '无（默认）' },
    { value: 'You are a helpful assistant. Answer concisely and accurately.', labelKey: '通用助手' },
    { value: 'You are a senior software engineer. Provide clear, correct code with brief explanations.', labelKey: '代码助手' },
    { value: 'You are a professional translator. Translate faithfully and preserve tone.', labelKey: '翻译助手' },
  ];

  const TEMPERATURE_PRESETS = [
    { value: '0.2', labelKey: '严谨' },
    { value: '0.7', labelKey: '平衡' },
    { value: '1.2', labelKey: '创意' },
  ];

  const MAX_TOKENS_PRESETS = ['256', '1024', '4096'];

  return (
    <div className="playground-shell">
      <div className="page-header">
        <div>
          <h1>{t('Playground')}</h1>
          <p>{t('交互式对话调试，选择模型并直接与 AI 网关对话验证效果')}</p>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="playground-body">
        {/* 左侧参数面板 */}
        <div className="card playground-params">
          <div className="card-header">
            <h2>{t('参数设置')}</h2>
          </div>
          <div className="card-body">
            <div className="form-group">
              <label>{t('系统提示词')}</label>
              <textarea
                className="form-input playground-system-input"
                rows="3"
                placeholder={t('设置系统提示词（可选）')}
                value={systemPrompt}
                onChange={(e) => setSystemPrompt(e.target.value)}
                disabled={busy}
              />
              <span className="form-hint">{t('系统提示词预设')}</span>
              <div className="playground-preset-row">
                {SYSTEM_PRESETS.map((p) => (
                  <button
                    key={p.labelKey}
                    type="button"
                    className="btn btn-outline btn-sm"
                    disabled={busy}
                    onClick={() => setSystemPrompt(p.value)}
                  >
                    {t(p.labelKey)}
                  </button>
                ))}
              </div>
            </div>

            <div className="form-group">
              <label>{t('模型')}</label>
              <select
                className="form-input"
                value={model}
                onChange={(e) => { setModel(e.target.value); setMessages([]); }}
                disabled={modelsLoading}
              >
                {modelsLoading && <option value="">{t('加载中')}</option>}
                {!modelsLoading && models.length === 0 && <option value="">{t('暂无可用模型')}</option>}
                {models.map((m) => (
                  <option key={m} value={m}>{m}</option>
                ))}
              </select>
            </div>

            <div className="form-group">
              <label>{t('温度 (temperature)')}</label>
              <input
                className="form-input"
                type="number"
                step="0.1"
                min="0"
                max="2"
                value={temperature}
                onChange={(e) => setTemperature(e.target.value)}
              />
              <span className="form-hint">{t('采样温度，越高越随机，0-2 之间')}</span>
              <div className="playground-preset-row">
                {TEMPERATURE_PRESETS.map((p) => (
                  <button
                    key={p.labelKey}
                    type="button"
                    className="btn btn-outline btn-sm"
                    disabled={busy}
                    onClick={() => setTemperature(p.value)}
                  >
                    {t(p.labelKey)}
                  </button>
                ))}
              </div>
            </div>

            <div className="form-group">
              <label>{t('最大输出 Token')}</label>
              <input
                className="form-input"
                type="number"
                min="1"
                value={maxTokens}
                onChange={(e) => setMaxTokens(e.target.value)}
              />
              <div className="playground-preset-row">
                {MAX_TOKENS_PRESETS.map((v) => (
                  <button
                    key={v}
                    type="button"
                    className="btn btn-outline btn-sm"
                    disabled={busy}
                    onClick={() => setMaxTokens(v)}
                  >
                    {v}
                  </button>
                ))}
              </div>
            </div>

            <div className="form-group playground-stream-row">
              <label className="playground-switch">
                <input
                  type="checkbox"
                  checked={stream}
                  onChange={(e) => setStream(e.target.checked)}
                />
                <span>{t('流式响应 (stream)')}</span>
              </label>
              <span className="form-hint">{t('开启后逐字输出，体验更流畅')}</span>
            </div>

            <div className="playground-actions">
              <button className="btn btn-outline btn-sm" onClick={handleClear} disabled={busy || messages.length === 0}>
                {t('清空对话')}
              </button>
            </div>
          </div>
        </div>

        {/* 右侧对话区 */}
        <div className="card playground-chat">
          <div className="card-header">
            <h2>{t('对话')} {model && <span className="playground-model-tag">{model}</span>}</h2>
          </div>
          <div className="card-body playground-chat-body">
            <div className="chat-messages">
              {messages.length === 0 && (
                <div className="chat-empty">{t('输入消息开始对话，验证网关与模型是否正常工作')}</div>
              )}
              {messages.map((m, i) => (
                <div key={i} className={`chat-msg chat-msg-${m.role}`}>
                  <span className="chat-msg-role">{m.role === 'user' ? t('用户') : t('助手')}</span>
                  <div className="chat-msg-content">{m.content}</div>
                </div>
              ))}
              {busy && <div className="chat-busy">{t('思考中...')}</div>}
              <div ref={messagesEndRef} />
            </div>
            <div className="chat-input-row">
              <textarea
                className="form-input chat-input"
                rows="3"
                placeholder={t('输入消息，Enter 发送，Shift+Enter 换行')}
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKeyDown}
                disabled={busy}
              />
              <button
                className="btn btn-primary"
                onClick={handleSend}
                disabled={busy || !input.trim() || !model}
              >
                {busy ? t('思考中...') : t('发送')}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
