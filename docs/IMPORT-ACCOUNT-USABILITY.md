# 导入账号可用性核查（线上 v1.0.90，含 #56/#58/#61 修复）

> 生成：2026-11-23（生产只读核查）。渠道：迁移渠道 `e9019d8d-646f-4b78-888e-50809ba58839`，共 **162** 条。
> 方法：逐 enabled 账号 `POST /api/account-sessions/catalog/sync {force:false,apply:true}` → 间隔2s → `POST /api/account-sessions/{id}/test`（内部 bridge.chat 真实对话）→ 间隔2s；严格串行；**不打印凭证**，仅示 id 脱敏尾号。
> 上一版基线见同文件历史（v1.0.88：可用 67/117）。

## 0. 总账（enabled 121）

| 档位 | 数量 |
|---|--:|
| 可用 | 75 |
| 需充值(余额) | 4 |
| 需重登 | 8 |
| 需最新客户端 | 1 |
| 仍不可用 | 33 |
| **合计** | **121** |

- 合计 162 = enabled **121** + expired **27** + disabled **14**（未测，见 §4）。
- vs v1.0.88：可用 **67 → 75**（+8；loomy 余额恢复 48→55、workbuddy 11→12）。

## 1. 逐 provider

| provider | enabled | 可用 | 需充值 | 需重登 | 客户端 | 仍不可用 |
|---|--:|--:|--:|--:|--:|--:|
| loomy | 55 | 55 | 0 | 0 | 0 | 0 |
| workbuddy | 12 | 12 | 0 | 0 | 0 | 0 |
| trae | 9 | 0 | 0 | 0 | 0 | 9 |
| qoder | 8 | 0 | 0 | 8 | 0 | 0 |
| autoclaw | 5 | 0 | 0 | 0 | 0 | 5 |
| chatglm | 5 | 0 | 0 | 0 | 0 | 5 |
| zcode | 5 | 2 | 3 | 0 | 0 | 0 |
| kuku | 4 | 4 | 0 | 0 | 0 | 0 |
| codar | 3 | 0 | 0 | 0 | 0 | 3 |
| teleagent | 3 | 0 | 0 | 0 | 0 | 3 |
| workbuddy-intl | 3 | 0 | 0 | 0 | 0 | 3 |
| xiaohuanxiong | 3 | 2 | 1 | 0 | 0 | 0 |
| antigravity | 1 | 0 | 0 | 0 | 0 | 1 |
| catpaw | 1 | 0 | 0 | 0 | 0 | 1 |
| cline-free | 1 | 0 | 0 | 0 | 1 | 0 |
| custom-454e15159b9f | 1 | 0 | 0 | 0 | 0 | 1 |
| doubaowork | 1 | 0 | 0 | 0 | 0 | 1 |
| qwenwork | 1 | 0 | 0 | 0 | 0 | 1 |

## 2. ②类 / #58 / #61 修复前后对比（v1.0.88 → v1.0.90）

| provider | v1.0.88 现象 | v1.0.90 结果 | 结论 |
|---|---|---|---|
| antigravity | sync 403 Forbidden | **sync OK(18)**；test 上游 400（chat 侧） | sync 已修，chat 仍不可用 |
| catpaw | sync 405 Method Not Allowed | sync 目录为空(ai.catpaw.meituan…) | method 已修，目录仍空 |
| autoclaw | model glm-5.3 → 非法模型 | model **zaicoding_glm-5.3**（别名已接线）→ 仍非法模型 | 别名生效，口径仍不符 |
| trae | sync 9 全失败(404/空) | sync 失败 7，2 条 sync 成功但 chat 400 | 部分改善，仍不可用 |
| chatglm/codar/qwenwork/custom | sync 无端点/不支持 | 报错改为「无远程模型目录或内置静态清单，请在账号…**手动声明**」 | #61 到位，需手填模型 |
| officeace/codearts | sync 失败 | 账号现全 **expired**（需重登，未测） | 账号态，重登后复验 |

## 3. 逐账号明细（enabled 121 条，脱敏尾号）

| provider | 账号尾号 | sync | test | model | 延迟ms | 归因 |
|---|---|---|---|---|--:|---|
| antigravity | …273dcb | OK(18) | FAIL:upstream returned HTTP 400: Antigravity 返回 HTTP 400：{  | claude-opus-4-6-thinking | 12 | 仍不可用·sync已修/chat400 |
| autoclaw | …899346 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | zaicoding_glm-5.3 | 517 | 仍不可用·别名后仍非法模型 |
| autoclaw | …899349 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | zaicoding_glm-5.3 | 280 | 仍不可用·别名后仍非法模型 |
| autoclaw | …899408 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | zaicoding_glm-5.3 | 274 | 仍不可用·别名后仍非法模型 |
| autoclaw | …899431 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | zaicoding_glm-5.3 | 281 | 仍不可用·别名后仍非法模型 |
| autoclaw | …904032 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | zaicoding_glm-5.3 | 418 | 仍不可用·别名后仍非法模型 |
| catpaw | …3b1b5e | FAIL:上游返回的模型目录为空（https://ai.catpaw.meituan.com/api/agent/ma | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| chatglm | …19f985 | FAIL:供应商 chatglm 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| chatglm | …75bfd8 | FAIL:供应商 chatglm 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| chatglm | …7731e9 | FAIL:供应商 chatglm 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| chatglm | …773209 | FAIL:供应商 chatglm 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| chatglm | …94e808 | FAIL:供应商 chatglm 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| cline-free | …XJ6YWC | OK(10) | FAIL:upstream returned HTTP 401: {"error":"Unauthorized: Pl | cline-pass/glm-5.3 | 28 | 需最新Cline客户端 |
| codar | …829605 | FAIL:供应商 codar 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| codar | …b97472 | FAIL:供应商 codar 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| codar | …ed567e | FAIL:供应商 codar 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| custom-454e15159b9f | …f0bd12 | FAIL:供应商 custom-454e15159b9f 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| doubaowork | …2f0bc1 | FAIL:供应商 doubaowork 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| kuku | …097639 | OK(13) | OK | ali-minimax/minimax-m3 | 4429 | 可用 |
| kuku | …489832 | OK(13) | OK | ali-minimax/minimax-m3 | 4975 | 可用 |
| kuku | …502492 | OK(13) | OK | ali-minimax/minimax-m3 | 4041 | 可用 |
| kuku | …973862 | OK(13) | OK | ali-minimax/minimax-m3 | 5917 | 可用 |
| loomy | …01ceb3 | OK(2) | OK | spark-x | 255 | 可用 |
| loomy | …0420f9 | OK(2) | OK | spark-x | 1491 | 可用 |
| loomy | …04ba81 | OK(2) | OK | spark-x | 433 | 可用 |
| loomy | …06f676 | OK(2) | OK | spark-x | 265 | 可用 |
| loomy | …0c7efe | OK(2) | OK | spark-x | 1884 | 可用 |
| loomy | …18d140 | OK(2) | OK | spark-x | 7769 | 可用 |
| loomy | …1bd943 | OK(2) | OK | spark-x | 253 | 可用 |
| loomy | …1f9fec | OK(2) | OK | spark-x | 6285 | 可用 |
| loomy | …259319 | OK(2) | OK | spark-x | 2347 | 可用 |
| loomy | …278cac | OK(2) | OK | spark-x | 2213 | 可用 |
| loomy | …2d02b5 | OK(2) | OK | spark-x | 2118 | 可用 |
| loomy | …312699 | OK(2) | OK | spark-x | 2971 | 可用 |
| loomy | …3b4568 | OK(2) | OK | spark-x | 273 | 可用 |
| loomy | …418e29 | OK(2) | OK | spark-x | 1353 | 可用 |
| loomy | …467e90 | OK(2) | OK | spark-x | 255 | 可用 |
| loomy | …5651d1 | OK(2) | OK | spark-x | 2994 | 可用 |
| loomy | …5a3c81 | OK(2) | OK | spark-x | 1437 | 可用 |
| loomy | …5b6cf1 | OK(2) | OK | spark-x | 240 | 可用 |
| loomy | …5baf2b | OK(2) | OK | spark-x | 11815 | 可用 |
| loomy | …5bf263 | OK(2) | OK | spark-x | 1396 | 可用 |
| loomy | …629307 | OK(2) | OK | spark-x | 1374 | 可用 |
| loomy | …667f2b | OK(2) | OK | spark-x | 246 | 可用 |
| loomy | …70b6b1 | OK(2) | OK | spark-x | 9268 | 可用 |
| loomy | …72b55b | OK(2) | OK | spark-x | 1808 | 可用 |
| loomy | …743843 | OK(2) | OK | spark-x | 255 | 可用 |
| loomy | …8357ef | OK(2) | OK | spark-x | 253 | 可用 |
| loomy | …852d49 | OK(2) | OK | spark-x | 1905 | 可用 |
| loomy | …8e508d | OK(2) | OK | spark-x | 1504 | 可用 |
| loomy | …a11bfa | OK(2) | OK | spark-x | 248 | 可用 |
| loomy | …a87a07 | OK(2) | OK | spark-x | 1442 | 可用 |
| loomy | …a99f6d | OK(2) | OK | spark-x | 233 | 可用 |
| loomy | …aa1d68 | OK(2) | OK | spark-x | 18828 | 可用 |
| loomy | …b1f737 | OK(2) | OK | spark-x | 252 | 可用 |
| loomy | …b806a5 | OK(2) | OK | spark-x | 246 | 可用 |
| loomy | …be1ba3 | OK(2) | OK | spark-x | 1437 | 可用 |
| loomy | …be777d | OK(2) | OK | spark-x | 233 | 可用 |
| loomy | …bfbada | OK(2) | OK | spark-x | 22590 | 可用 |
| loomy | …c31624 | OK(2) | OK | spark-x | 4022 | 可用 |
| loomy | …c42bf6 | OK(2) | OK | spark-x | 255 | 可用 |
| loomy | …c8f83c | OK(2) | OK | spark-x | 1776 | 可用 |
| loomy | …ca1533 | OK(2) | OK | spark-x | 1678 | 可用 |
| loomy | …ca6803 | OK(2) | OK | spark-x | 250 | 可用 |
| loomy | …cc7e0d | OK(2) | OK | spark-x | 254 | 可用 |
| loomy | …d1d308 | OK(2) | OK | spark-x | 1376 | 可用 |
| loomy | …e07cc9 | OK(2) | OK | spark-x | 7608 | 可用 |
| loomy | …e625b0 | OK(2) | OK | spark-x | 1714 | 可用 |
| loomy | …ed7e87 | OK(2) | OK | spark-x | 1344 | 可用 |
| loomy | …eddf5f | OK(2) | OK | spark-x | 1958 | 可用 |
| loomy | …ee6fb2 | OK(2) | OK | spark-x | 239 | 可用 |
| loomy | …ee6fef | OK(2) | OK | spark-x | 1315 | 可用 |
| loomy | …ee9e2c | OK(2) | OK | spark-x | 253 | 可用 |
| loomy | …f0c43a | OK(2) | OK | spark-x | 257 | 可用 |
| loomy | …f0cd81 | OK(2) | OK | spark-x | 4255 | 可用 |
| loomy | …f4bb87 | OK(2) | OK | spark-x | 309 | 可用 |
| loomy | …ffb5dd | OK(2) | OK | spark-x | 251 | 可用 |
| qoder | …4741e5 | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"1 | qfmodel | 925 | 需重登 |
| qoder | …5b463c | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"1 | qfmodel | 931 | 需重登 |
| qoder | …73752a | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"1 | qfmodel | 1035 | 需重登 |
| qoder | …747223 | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"1 | qfmodel | 1025 | 需重登 |
| qoder | …78cae6 | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"1 | qfmodel | 848 | 需重登 |
| qoder | …898915 | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"1 | qfmodel | 727 | 需重登 |
| qoder | …a26cfa | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"1 | qfmodel | 938 | 需重登 |
| qoder | …c5de5f | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"1 | qfmodel | 917 | 需重登 |
| qwenwork | …6af763 | FAIL:供应商 qwenwork 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(手动声明/静态兜底) |
| teleagent | …211e28 | FAIL:供应商 teleagent 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| teleagent | …2fb613 | FAIL:供应商 teleagent 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| teleagent | …33c6a5 | FAIL:供应商 teleagent 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| trae | …0069bc | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| trae | …179ae9 | OK(19) | FAIL:upstream returned HTTP 400: Trae 返回 HTTP 400：{"code":4 | Doubao-Seed-2.1-Turbo | 307 | 仍不可用·上游其他 |
| trae | …33592a | OK(19) | FAIL:upstream returned HTTP 400: Trae 返回 HTTP 400：{"code":4 | Doubao-Seed-2.1-Turbo | 248 | 仍不可用·上游其他 |
| trae | …55d538 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| trae | …5621a8 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| trae | …7748b7 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| trae | …797700 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| trae | …a069f9 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| trae | …a85f15 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 仍不可用·无模型(sync/parser) |
| workbuddy | …18f8a6 | OK(23) | OK | auto | 1367 | 可用 |
| workbuddy | …19a9fa | OK(23) | OK | auto | 1207 | 可用 |
| workbuddy | …1efbe2 | OK(23) | OK | auto | 1489 | 可用 |
| workbuddy | …2e2198 | OK(23) | OK | auto | 1344 | 可用 |
| workbuddy | …3017dc | OK(23) | OK | auto | 1101 | 可用 |
| workbuddy | …5b576c | OK(23) | OK | auto | 1586 | 可用 |
| workbuddy | …7a113c | OK(23) | OK | auto | 1145 | 可用 |
| workbuddy | …b632ea | OK(23) | OK | auto | 1277 | 可用 |
| workbuddy | …b883a0 | OK(23) | OK | auto | 1787 | 可用 |
| workbuddy | …be7811 | OK(23) | OK | auto | 1400 | 可用 |
| workbuddy | …faf693 | OK(23) | OK | auto | 1296 | 可用 |
| workbuddy | …fdfa87 | OK(23) | OK | auto | 1684 | 可用 |
| workbuddy-intl | …0b5110 | OK(23) | FAIL:upstream returned HTTP 400: {"code":11128,"msg":"reque | auto | 397 | 仍不可用·request illegal |
| workbuddy-intl | …20ce62 | OK(23) | FAIL:upstream returned HTTP 400: {"code":11128,"msg":"reque | auto | 290 | 仍不可用·request illegal |
| workbuddy-intl | …8c487a | OK(23) | FAIL:upstream returned HTTP 400: {"code":11128,"msg":"reque | auto | 281 | 仍不可用·request illegal |
| xiaohuanxiong | …3e3467 | OK(10) | OK | raccoon-090dd3 | 3012 | 可用 |
| xiaohuanxiong | …c7a2ae | OK(10) | OK | raccoon-090dd3 | 2634 | 可用 |
| xiaohuanxiong | …c97160 | OK(10) | FAIL:upstream returned HTTP 400: 积分余额不足，请充值后重试 | raccoon-090dd3 | 217 | 需充值(余额不足) |
| zcode | …146214 | OK(11) | FAIL:upstream returned HTTP 429: 余额不足或无可用资源包,请充值。 | glm-5.3 | 315 | 需充值(余额不足) |
| zcode | …153352 | OK(11) | FAIL:upstream returned HTTP 429: 余额不足或无可用资源包,请充值。 | glm-5.3 | 267 | 需充值(余额不足) |
| zcode | …211183 | OK(11) | FAIL:upstream returned HTTP 429: 余额不足或无可用资源包,请充值。 | glm-5.3 | 286 | 需充值(余额不足) |
| zcode | …8e41b6 | OK(11) | OK | glm-5.3 | 945 | 可用 |
| zcode | …dc497c | OK(11) | OK | glm-5.3 | 844 | 可用 |

## 4. 未测（非故障）

- **expired 27 → 需重登**：officeace×9、codearts×10、tabbit×1、loomy×7
- **disabled 14**：accio-cn×5、xiaohuanxiong×4、zcode-intl×3、qoder-intl×2
- teleagent×3 / doubaowork×1：插件型「不支持模型目录发现」，需对应插件/手填模型（已计入 enabled 仍不可用）。

## 5. 待办/阻断（如实）

- 仍不可用 33 = 无模型 22（trae/chatglm/codar/catpaw/custom/qwenwork/teleagent/doubaowork）+ 非法模型 5（autoclaw）+ request illegal 3（workbuddy-intl）+ antigravity chat400 1 + trae 上游400 2。
- 需充值 4（zcode3/xiaohuanxiong1）、需重登 8（qoder）、需最新客户端 1（cline-free）——均为账号态，充值/重登/升级客户端后可复跑。
- DeepSeek 网页 wasm（#62）宿主已就绪；迁移账号无 deepseek-web 账号，未硬测（插件能力层已通过）。
