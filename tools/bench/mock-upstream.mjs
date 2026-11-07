// AIGX 性能基准 · mock 上游（openai_compat 形态）
// 非流式：固定 512B 级 JSON；流式：16 个 chunk 的 SSE。零依赖，node >= 18。
import http from 'node:http';

const PORT = Number(process.env.MOCK_PORT || 18900);
const HOST = process.env.MOCK_HOST || '127.0.0.1';
const DELAY_MS = Number(process.env.MOCK_DELAY_MS || 0); // 模拟上游处理延迟
let hits = 0;
const started = Date.now();

const BODY = {
  id: 'chatcmpl-mock',
  object: 'chat.completion',
  created: 1700000000,
  model: 'mock-model',
  choices: [{ index: 0, message: { role: 'assistant', content: 'x'.repeat(256) }, finish_reason: 'stop' }],
  usage: { prompt_tokens: 100, completion_tokens: 64, total_tokens: 164 },
};

http.createServer((req, res) => {
  hits++;
  let raw = '';
  req.on('data', (c) => (raw += c));
  req.on('end', () => {
    const finish = () => {
      let stream = false;
      try { stream = Boolean(JSON.parse(raw || '{}').stream); } catch {}
      if (req.url.endsWith('/models') || req.url === '/v1/models') {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ object: 'list', data: [{ id: 'mock-model', object: 'model' }] }));
        return;
      }
      if (stream) {
        res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
        for (let i = 0; i < 16; i++) {
          const chunk = { ...BODY, object: 'chat.completion.chunk' };
          chunk.choices = [{ index: 0, delta: { content: 'y'.repeat(16) }, finish_reason: null }];
          res.write(`data: ${JSON.stringify(chunk)}\n\n`);
        }
        res.write('data: [DONE]\n\n');
        res.end();
        return;
      }
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(BODY));
    };
    if (DELAY_MS > 0) setTimeout(finish, DELAY_MS); else finish();
  });
}).listen(PORT, HOST, () => {
  console.log(`mock-upstream on ${HOST}:${PORT}`);
});

// 每 5s 打点：上游 QPS（用于对账网关 RPS）
setInterval(() => {
  const now = Date.now();
  console.log(`[mock] hits=${hits} rate≈${((hits / (now - started)) * 1000).toFixed(1)}/s`);
}, 5000).unref();
