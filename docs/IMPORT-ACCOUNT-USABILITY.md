# 导入账号可用性核查（基线：线上 v1.0.88，未含 v1.0.90 修复）

> 生成：2026-11-22（生产只读核查）。渠道：迁移渠道 `e9019d8d-646f-4b78-888e-50809ba58839`，共 158 条。
> 方法：逐 enabled 账号 `POST /api/account-sessions/catalog/sync {force:false,apply:true}` → 间隔2s → `POST /api/account-sessions/{id}/test`（内部 bridge.chat 真实对话）→ 间隔2s；严格串行；**不打印凭证**，账号仅示 id 脱敏尾号。
> 本文档为 **v1.0.88 基线**：②类 catalog 修复（catpaw/officeace/antigravity/trae/autoclaw）与 #61 静态兜底尚未部署，待 **v1.0.90 复验**后更新。

## 0. 总账

- 渠道账号合计 **158** = enabled **117** + expired **27** + disabled **14**
- 117 条 enabled 中：**真正可用 67**；其余 50 条归因如下：

| 归因 | 数量 |
|---|--:|
| 可用 | 67 |
| sync失败·待v1.0.90 | 11 |
| 余额/额度不足(充值即通) | 11 |
| 无目录发现·待#61/手填模型 | 10 |
| 需重登 | 8 |
| 构造·非法模型(待修) | 5 |
| 构造·request illegal(待修) | 3 |
| 需最新Cline客户端 | 1 |
| 上游瞬时502(重试) | 1 |

关键结论：**主因是「每账号 models 未同步」，不是 provider 坏**。本轮「逐账号 sync」把可用数从「每家 1 条抽样仅 5」提升到 **67**，证明逐账号同步是解锁关键。

## 1. 逐 provider 汇总

| provider | enabled | 可用 | 主要失败归因 |
|---|--:|--:|---|
| loomy | 55 | 48 | 余额/额度不足(充值即通)×6; 上游瞬时502(重试)×1 |
| workbuddy | 12 | 11 | 余额/额度不足(充值即通)×1 |
| trae | 9 | 0 | sync失败·待v1.0.90×9 |
| qoder | 8 | 0 | 需重登×8 |
| autoclaw | 5 | 0 | 构造·非法模型(待修)×5 |
| chatglm | 5 | 0 | 无目录发现·待#61/手填模型×5 |
| zcode | 5 | 2 | 余额/额度不足(充值即通)×3 |
| kuku | 4 | 4 | — |
| codar | 3 | 0 | 无目录发现·待#61/手填模型×3 |
| workbuddy-intl | 3 | 0 | 构造·request illegal(待修)×3 |
| xiaohuanxiong | 3 | 2 | 余额/额度不足(充值即通)×1 |
| antigravity | 1 | 0 | sync失败·待v1.0.90×1 |
| catpaw | 1 | 0 | sync失败·待v1.0.90×1 |
| cline-free | 1 | 0 | 需最新Cline客户端×1 |
| custom-454e15159b9f | 1 | 0 | 无目录发现·待#61/手填模型×1 |
| qwenwork | 1 | 0 | 无目录发现·待#61/手填模型×1 |

## 2. 未测账号（非故障）

- **expired 27 → 需重登**：officeace×9、codearts×10、tabbit×1、loomy×7
- **disabled 14**：accio-cn×5、xiaohuanxiong×4、zcode-intl×3、qoder-intl×2

## 3. 逐账号明细（enabled 117 条，脱敏尾号）

| provider | 账号尾号 | sync | test | model | 延迟ms | 归因 |
|---|---|---|---|---|--:|---|
| antigravity | …273dcb | FAIL:上游返回 403 Forbidden | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| autoclaw | …899346 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | glm-5.3 | 467 | 构造·非法模型(待修) |
| autoclaw | …899349 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | glm-5.3 | 317 | 构造·非法模型(待修) |
| autoclaw | …899408 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | glm-5.3 | 286 | 构造·非法模型(待修) |
| autoclaw | …899431 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | glm-5.3 | 275 | 构造·非法模型(待修) |
| autoclaw | …904032 | OK(2) | FAIL:upstream returned HTTP 400: {"message":"非法模型"} | glm-5.3 | 275 | 构造·非法模型(待修) |
| catpaw | …3b1b5e | FAIL:上游返回 405 Method Not Allowed | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| chatglm | …19f985 | FAIL:供应商 chatglm 无远程目录端点 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| chatglm | …75bfd8 | FAIL:供应商 chatglm 无远程目录端点 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| chatglm | …7731e9 | FAIL:供应商 chatglm 无远程目录端点 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| chatglm | …773209 | FAIL:供应商 chatglm 无远程目录端点 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| chatglm | …94e808 | FAIL:供应商 chatglm 无远程目录端点 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| cline-free | …XJ6YWC | OK(10) | FAIL:upstream returned HTTP 401: {"error":"Unauthorized: Please m | cline-pass/glm-5.3 | 28 | 需最新Cline客户端 |
| codar | …829605 | FAIL:供应商 codar 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| codar | …b97472 | FAIL:供应商 codar 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| codar | …ed567e | FAIL:供应商 codar 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| custom-454e15159b9f | …f0bd12 | FAIL:供应商 custom-454e15159b9f 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| kuku | …097639 | OK(13) | OK | ali-minimax/minimax-m3 | 4988 | 可用 |
| kuku | …489832 | OK(13) | OK | ali-minimax/minimax-m3 | 3879 | 可用 |
| kuku | …502492 | OK(13) | OK | ali-minimax/minimax-m3 | 3356 | 可用 |
| kuku | …973862 | OK(13) | OK | ali-minimax/minimax-m3 | 7038 | 可用 |
| loomy | …01ceb3 | OK(2) | OK | spark-x | 838 | 可用 |
| loomy | …0420f9 | OK(2) | OK | spark-x | 1492 | 可用 |
| loomy | …04ba81 | OK(2) | OK | spark-x | 227 | 可用 |
| loomy | …06f676 | OK(2) | OK | spark-x | 246 | 可用 |
| loomy | …0c7efe | OK(2) | OK | spark-x | 1262 | 可用 |
| loomy | …18d140 | OK(2) | OK | spark-x | 1351 | 可用 |
| loomy | …1bd943 | OK(2) | OK | spark-x | 240 | 可用 |
| loomy | …1f9fec | OK(2) | OK | spark-x | 1608 | 可用 |
| loomy | …259319 | OK(2) | OK | spark-x | 1807 | 可用 |
| loomy | …278cac | OK(2) | OK | spark-x | 6090 | 可用 |
| loomy | …2d02b5 | OK(2) | OK | spark-x | 1271 | 可用 |
| loomy | …312699 | OK(2) | FAIL:upstream returned HTTP 402: 积分余额不足，请获取积分后再试 | spark-x | 289 | 余额/额度不足(充值即通) |
| loomy | …3b4568 | OK(2) | OK | spark-x | 680 | 可用 |
| loomy | …418e29 | OK(2) | OK | spark-x | 2659 | 可用 |
| loomy | …467e90 | OK(2) | OK | spark-x | 247 | 可用 |
| loomy | …5651d1 | OK(2) | FAIL:upstream returned HTTP 402: 积分余额不足，请获取积分后再试 | spark-x | 293 | 余额/额度不足(充值即通) |
| loomy | …5a3c81 | OK(2) | FAIL:upstream returned HTTP 402: 积分余额不足，请获取积分后再试 | spark-x | 338 | 余额/额度不足(充值即通) |
| loomy | …5b6cf1 | OK(2) | OK | spark-x | 225 | 可用 |
| loomy | …5baf2b | OK(2) | OK | spark-x | 1471 | 可用 |
| loomy | …5bf263 | OK(2) | FAIL:upstream returned HTTP 402: 积分余额不足，请获取积分后再试 | spark-x | 280 | 余额/额度不足(充值即通) |
| loomy | …629307 | OK(2) | OK | spark-x | 4858 | 可用 |
| loomy | …667f2b | OK(2) | OK | spark-x | 224 | 可用 |
| loomy | …70b6b1 | OK(2) | OK | spark-x | 1815 | 可用 |
| loomy | …72b55b | OK(2) | OK | spark-x | 1350 | 可用 |
| loomy | …743843 | OK(2) | OK | spark-x | 243 | 可用 |
| loomy | …8357ef | OK(2) | OK | spark-x | 227 | 可用 |
| loomy | …852d49 | OK(2) | OK | spark-x | 1401 | 可用 |
| loomy | …8e508d | OK(2) | OK | spark-x | 1823 | 可用 |
| loomy | …a11bfa | OK(2) | OK | spark-x | 241 | 可用 |
| loomy | …a87a07 | OK(2) | OK | spark-x | 1844 | 可用 |
| loomy | …a99f6d | OK(2) | OK | spark-x | 244 | 可用 |
| loomy | …aa1d68 | OK(2) | OK | spark-x | 1783 | 可用 |
| loomy | …b1f737 | OK(2) | OK | spark-x | 224 | 可用 |
| loomy | …b806a5 | OK(2) | OK | spark-x | 244 | 可用 |
| loomy | …be1ba3 | OK(2) | OK | spark-x | 2026 | 可用 |
| loomy | …be777d | OK(2) | OK | spark-x | 780 | 可用 |
| loomy | …bfbada | OK(2) | OK | spark-x | 1767 | 可用 |
| loomy | …c31624 | OK(2) | FAIL:upstream returned HTTP 402: 积分余额不足，请获取积分后再试 | spark-x | 305 | 余额/额度不足(充值即通) |
| loomy | …c42bf6 | OK(2) | FAIL:HTTP502 error code: 502 | — | — | 上游瞬时502(重试) |
| loomy | …c8f83c | OK(2) | OK | spark-x | 9766 | 可用 |
| loomy | …ca1533 | OK(2) | OK | spark-x | 1368 | 可用 |
| loomy | …ca6803 | OK(2) | OK | spark-x | 262 | 可用 |
| loomy | …cc7e0d | OK(2) | OK | spark-x | 244 | 可用 |
| loomy | …d1d308 | OK(2) | FAIL:upstream returned HTTP 402: 积分余额不足，请获取积分后再试 | spark-x | 335 | 余额/额度不足(充值即通) |
| loomy | …e07cc9 | OK(2) | OK | spark-x | 1889 | 可用 |
| loomy | …e625b0 | OK(2) | OK | spark-x | 1366 | 可用 |
| loomy | …ed7e87 | OK(2) | OK | spark-x | 1599 | 可用 |
| loomy | …eddf5f | OK(2) | OK | spark-x | 2929 | 可用 |
| loomy | …ee6fb2 | OK(2) | OK | spark-x | 227 | 可用 |
| loomy | …ee6fef | OK(2) | OK | spark-x | 1191 | 可用 |
| loomy | …ee9e2c | OK(2) | OK | spark-x | 243 | 可用 |
| loomy | …f0c43a | OK(2) | OK | spark-x | 226 | 可用 |
| loomy | …f0cd81 | OK(2) | OK | spark-x | 1245 | 可用 |
| loomy | …f4bb87 | OK(2) | OK | spark-x | 247 | 可用 |
| loomy | …ffb5dd | OK(2) | OK | spark-x | 244 | 可用 |
| qoder | …4741e5 | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"105","m | Qwen3.8-Flash | 1039 | 需重登 |
| qoder | …5b463c | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"105","m | Qwen3.8-Flash | 916 | 需重登 |
| qoder | …73752a | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"105","m | Qwen3.8-Flash | 931 | 需重登 |
| qoder | …747223 | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"105","m | Qwen3.8-Flash | 1033 | 需重登 |
| qoder | …78cae6 | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"105","m | Qwen3.8-Flash | 1012 | 需重登 |
| qoder | …898915 | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"105","m | Qwen3.8-Flash | 1018 | 需重登 |
| qoder | …a26cfa | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"105","m | Qwen3.8-Flash | 1049 | 需重登 |
| qoder | …c5de5f | OK(10) | FAIL:upstream returned HTTP 403: Qoder 上游错误（403）：{"code":"105","m | Qwen3.8-Flash | 834 | 需重登 |
| qwenwork | …6af763 | FAIL:供应商 qwenwork 不支持模型目录发现 | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | 无目录发现·待#61/手填模型 |
| trae | …0069bc | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| trae | …179ae9 | FAIL:上游返回的模型目录为空（https://trae-api-cn.mchost.guru/api/ide/v1/get_d | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| trae | …33592a | FAIL:上游返回的模型目录为空（https://trae-api-cn.mchost.guru/api/ide/v1/get_d | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| trae | …55d538 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| trae | …5621a8 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| trae | …7748b7 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| trae | …797700 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| trae | …a069f9 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| trae | …a85f15 | FAIL:上游返回 404 Not Found | FAIL:该账号与渠道都未配置模型，请先声明渠道 models | — | 0 | sync失败·待v1.0.90 |
| workbuddy | …18f8a6 | OK(23) | OK | auto | 3312 | 可用 |
| workbuddy | …19a9fa | OK(23) | OK | auto | 1543 | 可用 |
| workbuddy | …1efbe2 | OK(23) | OK | auto | 1388 | 可用 |
| workbuddy | …2e2198 | OK(23) | OK | auto | 1852 | 可用 |
| workbuddy | …3017dc | OK(23) | OK | auto | 3089 | 可用 |
| workbuddy | …5b576c | OK(23) | OK | auto | 1341 | 可用 |
| workbuddy | …7a113c | OK(23) | OK | auto | 1392 | 可用 |
| workbuddy | …b632ea | OK(23) | OK | auto | 1394 | 可用 |
| workbuddy | …b883a0 | OK(23) | FAIL:upstream returned HTTP 429: {"error":{"data":{"code":14018," | auto | 524 | 余额/额度不足(充值即通) |
| workbuddy | …be7811 | OK(23) | OK | auto | 1594 | 可用 |
| workbuddy | …faf693 | OK(23) | OK | auto | 1178 | 可用 |
| workbuddy | …fdfa87 | OK(23) | OK | auto | 1135 | 可用 |
| workbuddy-intl | …0b5110 | OK(23) | FAIL:upstream returned HTTP 400: {"code":11128,"msg":"request ill | auto | 406 | 构造·request illegal(待修) |
| workbuddy-intl | …20ce62 | OK(23) | FAIL:upstream returned HTTP 400: {"code":11128,"msg":"request ill | auto | 245 | 构造·request illegal(待修) |
| workbuddy-intl | …8c487a | OK(23) | FAIL:upstream returned HTTP 400: {"code":11128,"msg":"request ill | auto | 253 | 构造·request illegal(待修) |
| xiaohuanxiong | …3e3467 | OK(10) | OK | raccoon-090dd3 | 6335 | 可用 |
| xiaohuanxiong | …c7a2ae | OK(10) | OK | raccoon-090dd3 | 3874 | 可用 |
| xiaohuanxiong | …c97160 | OK(10) | FAIL:upstream returned HTTP 400: 积分余额不足，请充值后重试 | raccoon-090dd3 | 224 | 余额/额度不足(充值即通) |
| zcode | …146214 | OK(11) | FAIL:upstream returned HTTP 429: 余额不足或无可用资源包,请充值。 | glm-5.3 | 273 | 余额/额度不足(充值即通) |
| zcode | …153352 | OK(11) | FAIL:upstream returned HTTP 429: 余额不足或无可用资源包,请充值。 | glm-5.3 | 307 | 余额/额度不足(充值即通) |
| zcode | …211183 | OK(11) | FAIL:upstream returned HTTP 429: 余额不足或无可用资源包,请充值。 | glm-5.3 | 353 | 余额/额度不足(充值即通) |
| zcode | …8e41b6 | OK(11) | OK | glm-5.3 | 1657 | 可用 |
| zcode | …dc497c | OK(11) | OK | glm-5.3 | 2184 | 可用 |

## 4. 待复验（v1.0.90）

- ②类 catalog 修复后复验：**catpaw / officeace / antigravity / trae / autoclaw**（officeace 现全 expired，需先重登）。
- #61 静态模型兜底后复验：**chatglm / codar / qwenwork / tabbit**。
- 余额类（loomy/workbuddy/xiaohuanxiong/zcode 共 11）与需重登（qoder 8、cline-free 1）属账号态，充值/重登后即可用。
- 复验口径同本文：迁移渠道 filter、逐 enabled 账号、sync(force=false)+test、串行≥2s。
