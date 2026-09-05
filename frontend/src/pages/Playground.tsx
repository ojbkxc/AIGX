import { useState } from 'react';
import { api } from '../api';

interface PlaygroundMessage {
  role: 'user' | 'assistant' | 'system';
  content: string;
}

export default function Playground(): JSX.Element {
  const [messages, setMessages] = useState<PlaygroundMessage[]>([]);
  const [input, setInput] = useState('');
  const [loading, setLoading] = useState(false);
  const [model, setModel] = useState('gpt-3.5-turbo');

  const handleSubmit = async () => {
    if (!input.trim()) return;

    setLoading(true);
    try {
      // 添加用户消息
      const userMessage: PlaygroundMessage = { role: 'user', content: input };
      setMessages([...messages, userMessage]);

      // 调用 API
      const response = await api.chatCompletions({
        model,
        messages: [...messages, userMessage],
      });

      // 添加助手回复
      const assistantMessage: PlaygroundMessage = {
        role: 'assistant',
        content: response.data.choices[0].message.content,
      };
      setMessages([...messages, userMessage, assistantMessage]);

      setInput('');
    } catch (err) {
      console.error('聊天失败:', err);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="playground-container">
      <div className="page-header">
        <h1>对话调试</h1>
        <p>测试 AI 模型对话功能</p>
      </div>

      {/* 聊天区域 */}
      <div className="chat-area">
        {messages.length === 0 ? (
          <div className="empty-state">
            <p>开始与 AI 对话</p>
          </div>
        ) : (
          messages.map((message, index) => (
            <div key={index} className={`message message-${message.role}`}>
              <div className="message-header">
                <span>{message.role}</span>
              </div>
              <div className="message-content">
                {message.content}
              </div>
            </div>
          ))
        )}
        {loading && <div className="message message-assistant"><p>AI 正在思考...</p></div>}
      </div>

      {/* 输入区域 */}
      <div className="input-area">
        <select
          value={model}
          onChange={(e) => setModel(e.target.value)}
        >
          <option value="gpt-3.5-turbo">GPT-3.5 Turbo</option>
          <option value="gpt-4">GPT-4</option>
          <option value="claude-3-vision">Claude 3 Vision</option>
          <option value="gemini-pro">Gemini Pro</option>
        </select>
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="输入消息..."
          disabled={loading}
        />
        <button onClick={handleSubmit} disabled={loading}>
          发送
        </button>
      </div>
    </div>
  );
}
