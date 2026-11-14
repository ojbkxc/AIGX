// 浏览器 E2E 用的 loopback mock 上游（openai_compatible 协议）。
//
// 对齐 499_disconnect_billing.rs 内嵌 mock 的行为口径：
// - /v1/chat/completions 同时支持流式（SSE chunk + usage + [DONE]）与非流式 JSON
// - 每条响应带 usage，供网关计费链路真实执行
// 模型名固定 deepseek-chat —— run-e2e.mjs 的模型搜索断言搜 'deepseek' 片段。
//
// 协议：stdout 打印一行 JSON {"port": N} 供父进程（run-browser-e2e.mjs）取端口。
import http from 'node:http';

const CONTENT = '你好，我是 AIGX 测试回复。';
const USAGE = { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 };

const server = http.createServer((req, res) => {
  if (!req.url || !req.url.startsWith('/v1/')) {
    res.writeHead(404, { 'content-type': 'application/json' });
    return res.end(JSON.stringify({ error: 'not found' }));
  }
  let body = '';
  req.on('data', (c) => (body += c));
  req.on('end', () => {
    let model = 'deepseek-chat';
    let stream = false;
    try {
      const j = JSON.parse(body || '{}');
      if (j.model) model = j.model;
      stream = !!j.stream;
    } catch {
      // 保持默认
    }
    if (stream) {
      res.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
      });
      for (let i = 0; i < 4; i++) {
        res.write(
          `data: ${JSON.stringify({
            id: 'chatcmpl-mock',
            object: 'chat.completion.chunk',
            created: 1700000000,
            model,
            choices: [{ index: 0, delta: { content: CONTENT }, finish_reason: null }],
          })}\n\n`,
        );
      }
      res.write(
        `data: ${JSON.stringify({
          id: 'chatcmpl-mock',
          object: 'chat.completion.chunk',
          created: 1700000000,
          model,
          choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
          usage: USAGE,
        })}\n\n`,
      );
      res.write('data: [DONE]\n\n');
      res.end();
    } else {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          id: 'chatcmpl-mock',
          object: 'chat.completion',
          created: 1700000000,
          model,
          choices: [
            { index: 0, message: { role: 'assistant', content: CONTENT }, finish_reason: 'stop' },
          ],
          usage: USAGE,
        }),
      );
    }
  });
});

server.listen(0, '127.0.0.1', () => {
  const { port } = server.address();
  console.log(JSON.stringify({ port }));
  process.stdout.write('', () => {});
});

// 父进程意外退出时不要留孤儿
process.on('disconnect', () => process.exit(0));
