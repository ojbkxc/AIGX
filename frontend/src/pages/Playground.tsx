import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Send, Image as ImageIcon, TerminalSquare, MessageSquare } from 'lucide-react';
import ChatDebugger from '../components/ChatDebugger';
import Tabs from '../components/ui/Tabs';
import { api } from '../api';
import type { PlaygroundRawResult } from '../types';
import './Playground.css';

type PlaygroundMode = 'chat' | 'completions' | 'images';

/**
 * Playground V2 — 三模式调试沙盒（P1 前端体验跃升）。
 *
 * - Chat：复用 ChatDebugger（真流式、多模态、多轮上下文），协议/温度/
 *   系统提示词等参数在工具条内可调。
 * - Completions：/completions 参数调节（temperature/top_p/惩罚系数/
 *   JSON mode）+ 请求响应双栏 JSON。
 * - Images：/images/generations（n/size）+ 图片网格 + JSON。
 *
 * 审美参照 open-webui playground：三模式 Tabs + 参数/响应双栏布局。
 */
export default function Playground(): JSX.Element {
  const { t } = useTranslation();
  const [mode, setMode] = useState<PlaygroundMode>('chat');

  return (
    <div className="playground-shell">
      <div className="playground-mode-bar">
        <Tabs<PlaygroundMode>
          items={[
            { key: 'chat', label: <><MessageSquare size={14} /> {t('Chat')}</> },
            { key: 'completions', label: <><TerminalSquare size={14} /> {t('Completions')}</> },
            { key: 'images', label: <><ImageIcon size={14} /> {t('Images')}</> },
          ]}
          active={mode}
          onChange={setMode}
          ariaLabel={t('Playground 模式')}
        />
      </div>
      {mode === 'chat' && (
        <div className="playground-body">
          <ChatDebugger />
        </div>
      )}
      {mode === 'completions' && <CompletionsPanel />}
      {mode === 'images' && <ImagesPanel />}
    </div>
  );
}

/** Completions 面板：参数左栏 + 响应 JSON 右栏（open-webui 双栏） */
function CompletionsPanel(): JSX.Element {
  const { t } = useTranslation();
  const [prompt, setPrompt] = useState('');
  const [model, setModel] = useState('');
  const [temperature, setTemperature] = useState('0.7');
  const [maxTokens, setMaxTokens] = useState('1024');
  const [topP, setTopP] = useState('1');
  const [presence, setPresence] = useState('0');
  const [frequency, setFrequency] = useState('0');
  const [jsonMode, setJsonMode] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [result, setResult] = useState<unknown>(null);

  const handleRun = async (): Promise<void> => {
    if (!prompt.trim() || busy) return;
    setBusy(true);
    setError('');
    setResult(null);
    try {
      const res = await api.playgroundChat({
        model: model.trim(),
        prompt: prompt.trim(),
        max_tokens: Number(maxTokens) || 1024,
        temperature: Number(temperature) || 0.7,
        top_p: Number(topP) || 1,
        presence_penalty: Number(presence) || 0,
        frequency_penalty: Number(frequency) || 0,
        json_mode: jsonMode,
      });
      const content = res.data?.content;
      if (typeof content === 'string') {
        setResult({ content, model });
      } else {
        setResult(res);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="playground-v2">
      <div className="playground-v2-form">
        <div className="form-group">
          <label>{t('模型')}</label>
          <input
            className="form-input"
            placeholder={t('留空自动选择渠道首个模型')}
            value={model}
            onChange={(e) => setModel(e.target.value)}
          />
        </div>
        <div className="form-group">
          <label>{t('提示词')}</label>
          <textarea
            className="form-input"
            rows={6}
            placeholder={t('输入 completions 提示词…')}
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
          />
        </div>
        <div className="playground-v2-params">
          <div className="form-group">
            <label>{t('温度')}</label>
            <input className="form-input" type="number" step="0.1" min="0" max="2" value={temperature} onChange={(e) => setTemperature(e.target.value)} />
          </div>
          <div className="form-group">
            <label>{t('最大输出 Token')}</label>
            <input className="form-input" type="number" min="1" value={maxTokens} onChange={(e) => setMaxTokens(e.target.value)} />
          </div>
          <div className="form-group">
            <label>top_p</label>
            <input className="form-input" type="number" step="0.1" min="0" max="1" value={topP} onChange={(e) => setTopP(e.target.value)} />
          </div>
          <div className="form-group">
            <label>{t('存在惩罚')}</label>
            <input className="form-input" type="number" step="0.1" min="-2" max="2" value={presence} onChange={(e) => setPresence(e.target.value)} />
          </div>
          <div className="form-group">
            <label>{t('频率惩罚')}</label>
            <input className="form-input" type="number" step="0.1" min="-2" max="2" value={frequency} onChange={(e) => setFrequency(e.target.value)} />
          </div>
        </div>
        <label className="playground-v2-check">
          <input type="checkbox" checked={jsonMode} onChange={(e) => setJsonMode(e.target.checked)} />
          <span>{t('JSON 模式')}</span>
        </label>
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => void handleRun()}
          disabled={busy || !prompt.trim()}
        >
          {busy ? <Loader2 size={14} className="chat-debugger-spin" /> : <Send size={14} />}
          {busy ? t('运行中…') : t('运行')}
        </button>
        {error && <div className="error-message">{error}</div>}
      </div>
      <div className="playground-v2-result">
        <div className="playground-v2-result-head">
          <span>{t('响应')}</span>
          {result != null && (
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={() => {
                const text = JSON.stringify(result, null, 2);
                void navigator.clipboard?.writeText(text);
              }}
            >
              {t('复制 JSON')}
            </button>
          )}
        </div>
        {result == null && !busy && (
          <div className="playground-v2-empty">{t('运行后这里显示上游 JSON 响应')}</div>
        )}
        {busy && (
          <div className="playground-v2-empty">
            <Loader2 size={14} className="chat-debugger-spin" /> {t('请求中…')}
          </div>
        )}
        {result != null && (
          <pre className="playground-v2-json">{JSON.stringify(result, null, 2)}</pre>
        )}
      </div>
    </div>
  );
}

/** Images 面板：参数 + 图片网格 + JSON（透传 /images/generations） */
function ImagesPanel(): JSX.Element {
  const { t } = useTranslation();
  const [prompt, setPrompt] = useState('');
  const [model, setModel] = useState('');
  const [n, setN] = useState('1');
  const [size, setSize] = useState('1024x1024');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [result, setResult] = useState<unknown>(null);

  const handleRun = async (): Promise<void> => {
    if (!prompt.trim() || busy) return;
    setBusy(true);
    setError('');
    setResult(null);
    try {
      const res = (await api.playgroundImages({
        model: model.trim(),
        prompt: prompt.trim(),
        n: Number(n) || 1,
        size,
      })) as PlaygroundRawResult;
      setResult(res.data ?? res);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const images: Array<{ url?: string; b64_json?: string }> =
    (result as { data?: Array<{ url?: string; b64_json?: string }> })?.data ?? [];

  return (
    <div className="playground-v2">
      <div className="playground-v2-form">
        <div className="form-group">
          <label>{t('模型')}</label>
          <input
            className="form-input"
            placeholder={t('留空自动选择渠道首个模型')}
            value={model}
            onChange={(e) => setModel(e.target.value)}
          />
        </div>
        <div className="form-group">
          <label>{t('提示词')}</label>
          <textarea
            className="form-input"
            rows={6}
            placeholder={t('描述你想生成的图片…')}
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
          />
        </div>
        <div className="playground-v2-params">
          <div className="form-group">
            <label>n</label>
            <input className="form-input" type="number" min="1" max="10" value={n} onChange={(e) => setN(e.target.value)} />
          </div>
          <div className="form-group">
            <label>size</label>
            <select className="form-input" value={size} onChange={(e) => setSize(e.target.value)}>
              <option value="1024x1024">1024x1024</option>
              <option value="1792x1024">1792x1024</option>
              <option value="1024x1792">1024x1792</option>
            </select>
          </div>
        </div>
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => void handleRun()}
          disabled={busy || !prompt.trim()}
        >
          {busy ? <Loader2 size={14} className="chat-debugger-spin" /> : <ImageIcon size={14} />}
          {busy ? t('生成中...') : t('生成')}
        </button>
        {error && <div className="error-message">{error}</div>}
      </div>
      <div className="playground-v2-result">
        <div className="playground-v2-result-head">
          <span>{t('结果')}</span>
          {result != null && (
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={() => void navigator.clipboard?.writeText(JSON.stringify(result, null, 2))}
            >
              {t('复制 JSON')}
            </button>
          )}
        </div>
        {result == null && !busy && (
          <div className="playground-v2-empty">{t('生成后这里显示图片与上游 JSON 响应')}</div>
        )}
        {busy && (
          <div className="playground-v2-empty">
            <Loader2 size={14} className="chat-debugger-spin" /> {t('生成中...')}
          </div>
        )}
        {images.length > 0 && (
          <div className="playground-v2-images">
            {images.map((img, i) => (
              <img
                key={i}
                src={img.url || `data:image/png;base64,${img.b64_json ?? ''}`}
                alt={`generated-${i}`}
                loading="lazy"
              />
            ))}
          </div>
        )}
        {result != null && images.length === 0 && (
          <pre className="playground-v2-json">{JSON.stringify(result, null, 2)}</pre>
        )}
      </div>
    </div>
  );
}
