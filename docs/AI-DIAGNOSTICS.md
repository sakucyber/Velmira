# AI 失败诊断与可恢复性（v0.5.3）

本文说明整书分析、章节记忆和文本连接检测在失败时如何向用户解释原因，以及本地保留了哪些
可用于反馈的材料。目标：用户不必抓包，也能知道是哪个阶段、哪个模型、什么原因失败。

## 失败对象

失败时 `GET /api/books/{book_id}/ai/analysis` 的 `analysis.failure` 为：

~~~json
{
  "code": "output_reasoning_budget",
  "reason": "reasoning_budget",
  "stage": "memory",
  "chapter": 2,
  "model": "deepseek-flash",
  "http_status": 200,
  "finish_reason": "length",
  "usage": {"completion_tokens": 8000, "reasoning_tokens": 8000},
  "max_tokens": 8000,
  "title": "推理模型把输出预算用在了思考过程",
  "message": "第 3 章章节记忆阶段失败：… · 模型 deepseek-flash",
  "suggestion": "把最大 Token 提高到 16000 以上，或降低 reasoning effort。"
}
~~~

- `stage`：`analysis`（章节分析）、`reduction`（长章合并）、`memory`（连续性记忆）、
  `probe`（连接检测）。
- `reason`：`json_invalid`、`field_invalid`、`quote_mismatch`、`empty_content`、
  `output_truncated`、`reasoning_budget`、`timeout`、`rate_limited`、`auth` 等固定分类。
- `http_status`、`finish_reason`、`usage` 只包含状态码、固定字符串和 token 计数；
  不包含上游响应正文、提示词、URL 查询参数或密钥。
- `analysis.error` 仍然是给旧客户端的一行中文消息，两者同时保存。

数据库 `book_analysis` 增加 `error_code`、`error_stage`、`error_detail` 三列；旧库启动时
自动补列，不重建、不删除已有的章节分析。

## 诊断接口（不调用模型）

`GET /api/books/{book_id}/ai/analysis/diagnose` 返回：

- `checks[]`：文本 AI 是否启用、连接是否缺少密钥、推理模型 token 预算、任务状态与失败原因。
- `logs[]`：最近的 `task_logs`，含 `stage`、`level`、时间与安全消息。
- `samples[]`：本机失败样本的元数据。
- `config`：当前来源、模型名、最大 Token、推荐值与硬上限。
- `can_resume`：是否可直接继续。

该接口不发起任何供应商请求，因此可以在断网或余额不足时使用。

## 失败样本

每个“书籍 + 阶段”只保留一份最近的失败样本，写入
`<DATA_DIR>/diagnostics/analysis-<book>-<stage>.json`：

- 最多 16 KB 的模型输出原文（不是上游 HTTP 正文），按固定规则脱敏
  `sk-…`、`Bearer …`、`api_key=…` 等模式；模型可能回显少量正文，因此该文件仍视为本地隐私数据；
- 附带同一份失败分类对象，便于复现“哪个字段没有通过校验”；
- 单文件覆盖写入，不会无限增长。

## 一键诊断包

`GET /api/books/{book_id}/ai/analysis/diagnostics/export` 返回 zip：

- `summary.json`：版本、书籍元数据、分析状态、检查项与配置类别；
- `task-logs.json`：最多 1000 条任务日志；
- `config.json`：公开设置与模型池状态（不含密钥）；
- `samples/*.json`：脱敏后的失败样本；
- `README.txt`：说明包含与不包含的内容。

分析面板的“导出诊断包”按钮直接下载该文件。

## 分级连接检测

`POST /api/ai/probe` 现在返回逐级结果：

1. `models`：能否读取 `/models`（非致命，部分兼容服务不提供）；
2. `chat`：最小对话请求（致命）；
3. `json`：能否返回 JSON 对象（致命）；
4. `citation`：能否逐字引用给定原文（非致命，只影响引文精度）。

每级包含 `ok`、`fatal`、`detail`、`suggestion`、`http_status`、`finish_reason`、
`retryable` 与耗时。401/402/403 等配置或凭据错误不会重试三次；5xx、超时和限流才重试或切换候选。

## 引文校验的降级规则

章节记忆仍要求 `memory` 为 100–250 token，这是硬约束。单条事实的 `quote` 改为分级处理：

1. 逐字命中：保留；
2. 忽略空白、标点和引号后可命中：保留，并把存储的 `quote` 修正为原文实际范围；
3. 仍无法命中、人物未知或字段结构无效：丢弃该条并记录；
4. 丢弃比例超过 40%：带着纠正提示重试；最后一次尝试保留所有可验证事实，不因少数引文
   偏差中断整章。

严格模式（导入、直接调用 `normalize()`）行为不变。

## 推理模型预算

- 默认最大 Token 仍为 6000；模型名命中 `o1/o3/o4`、`reasoner`、`thinking`、
  `deepseek-r`、`gpt-oss` 等模式时，界面建议 16000 以上。
- 硬上限为 32768；超过上限会在本地直接拒绝并说明原因，不会静默丢弃请求。
- 若 `finish_reason=length` 且 `reasoning_tokens>0`，失败原因标记为
  `reasoning_budget`，而不是笼统的“格式无效”。
- 连接可选配置 `reasoning_effort`（`minimal/low/medium/high/max`）；留空则不下发，
  保持对其他供应商的兼容。

## 日志

桌面后端继续把 stdout/stderr 写入 `%LOCALAPPDATA%\Velmira\backend.log`。启动时若文件
超过 5 MB 会轮转为 `.1`、`.2`、`.3`；应用日志包含 INFO 级启动/停止信息、慢于 2 秒的
API 路径与方法，以及未处理异常的堆栈。查询字符串与请求体不写日志。
