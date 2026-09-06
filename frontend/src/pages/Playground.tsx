import { useTranslation } from 'react-i18next';
import ChatDebugger from '../components/ChatDebugger';

/**
 * Playground — 在线调试沙盒。
 *
 * 与渠道管理的「对话调试」共用 ChatDebugger（同一后端入口
 * /api/channels/chat_test），行为完全一致：协议、模型、流式、
 * 多模态附件。此页不绑定渠道，自动选择启用的渠道。
 */
export default function Playground(): JSX.Element {
  const { t } = useTranslation();

  return (
    <div className="playground-shell">
      <div className="page-header">
        <div>
          <h1>{t('Playground')}</h1>
          <p>{t('在线调试沙盒：像聊天一样测试你的 AI 网关——模型、协议、多模态附件，与渠道管理里的「对话调试」完全一致。')}</p>
        </div>
      </div>
      <div className="playground-body">
        <ChatDebugger />
      </div>
    </div>
  );
}