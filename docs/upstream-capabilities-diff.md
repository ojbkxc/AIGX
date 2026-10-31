# Agent2API → AIGX 能力层漂移报告

生成时间：2026-11-01 03:50

本报告由 `tools/sync_agent2api_capabilities.py --report` 生成。
能力层 = 签到名单 / 模型目录实现 / 目录端点。

## 签到名单

上游：workbuddy, workbuddy-intl, raccoon, autoclaw, autoclaw-intl, qoder, qoder-intl, trae, loomy, kuku, officeace, codearts

**AIGX 缺签到**：（无）

**AIGX 多签到**：（无）

## 模型目录

**AIGX 缺目录实现**：（无）

**目录端点不一致**：

（无）

## 处理方式

差异不自动回写 —— 能力层改动要按 `docs/agent2api-porting.md` 的契约逐家移植
（签名 / 风控头 / 响应形态），不能用脚本盲改。
