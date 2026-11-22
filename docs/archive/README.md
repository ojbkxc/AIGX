# docs/archive — 历史文档归档区

> 目的：把根目录散落的历史快照/一次性文档移出根目录，**保留信息**但不参与现行文档导航。
> 这里的文件**不再维护**，只作为项目演进的历史记录；现行权威文档在仓库根
> （[`README.md`](../../README.md)、[`AGENTS.md`](../../AGENTS.md)、[`CLAUDE.md`](../../CLAUDE.md)、
> [`DEPLOYMENT.md`](../../DEPLOYMENT.md)）与 [`docs/`](../) 下。
> 归档时间：2026-11-22（仓库卫生清理）。

| 归档文件 | 原位置 | 归档理由 |
|---|---|---|
| `WORK_SUMMARY_2024.md` | `/` | 2024 年渠道页重构工作总结，属过期进度快照；正文信息保留 |
| `PROJECT-PROGRESS-2026-09-05.md` | `/` | 2026-09-05 进度快照（"正在进行中"），已被 `docs/ROADMAP.md` + `docs/CHANGELOG-AI.md` 取代 |
| `PROJECT-FINAL-STATUS-2026-09-05.md` | `/` | 2026-09-05 "100% 完成"报告，同上 |
| `IMPLEMENTATION-START.md` | `/` | 一次性 5 天启动清单（前端现代化），计划早已执行完毕 |
| `AI-UPGRADE-PROMPT.md` | `/` | 一次性喂给 AI 的全量重构提示词，规则已固化进 `AGENTS.md` / `CLAUDE.md` |
| `README-MIGRATION.md` | `/` | UI 技术栈迁移路线，已被 `docs/FRONTEND-EVOLUTION-PLAN.md` + 根 `FRONTEND-EVOLUTION-V3.md` 取代 |
| `RUST_BACKEND_INTEGRATION.md` | `/` | 早期"Rust 内嵌前端"集成指南，落地后已由 `README.md`「从源码构建」+ `build-binaries.sh` 覆盖；仅被 `IMPLEMENTATION-START.md` 引用 |
| `说明.txt` | `/` | 2026-08-08 项目原始需求与进度笔记（唯一信息源，含参照项目清单），保留备查 |

已删除（不归档）：`.github-actions-deploy.sh`、`.github-actions-pull.sh`、`git-init.sh`、`DEPLOY.sh`、
`.github-workflow.md`、`backend_routes.txt`、`.github/workflows/pre-deploy.yml` —— 均为一次性脚手架/重复实现，
功能已由 `deploy-script.sh`、`.github/workflows/`、`tools/extract_routes.py` 覆盖（详见提交说明）。
