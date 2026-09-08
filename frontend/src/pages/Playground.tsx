import ChatDebugger from '../components/ChatDebugger';

/**
 * Playground — 在线调试沙盒。
 *
 * 与渠道管理的「对话调试」共用 ChatDebugger（同一后端入口
 * /api/channels/chat_test），行为完全一致：协议、模型、流式、
 * 多模态附件。此页不绑定渠道，自动选择启用的渠道。
 */
export default function Playground(): JSX.Element {
  return (
    <div className="playground-shell">
      <div className="playground-body">
        <ChatDebugger />
      </div>
    </div>
  );
}
