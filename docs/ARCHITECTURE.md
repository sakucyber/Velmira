# Velmira 当前架构

本文描述仓库当前实现（v0.4.2 离线与同步增量），以源码和接口为准。早期
foliate-js、Vite/TypeScript 和 YAML 方案已经退出实现；它们不再是运行时依赖。

## 运行形态

Velmira 是本地优先的单用户服务：FastAPI、SQLite(WAL)、文件资源和原生
ES modules 组成一个进程。浏览器直接访问服务；Windows 和 Linux 桌面端的
Go/MyGo 壳分别使用 WebView2 和 WebKitGTK，只负责系统窗口和后端生命周期。
Linux 也可以不用桌面壳，直接用 `bash scripts/run.sh` 启动同一个 Web 服务。

```text
浏览器 / Android WebView（界面缓存 + IndexedDB 正文、图片、待同步操作）
          │ same-origin HTTP + SSE
          ▼
FastAPI 路由 ── SecurityMiddleware（本机管理 / LAN 配对）
    ├── 书库、章节、进度、书签、批注、全文检索
    ├── 设备注册、操作日志 push/pull、分页 snapshot 与冲突检测
    ├── AI 设置、连接与模型库、模型池路由、联网搜索、长期记忆
    ├── 场景 / 分镜 / 自动漫画、任务队列与分页媒体
    └── 安全导入器（TXT / EPUB / CBZ）
          │
          ├── SQLite：正文索引、AI 结构化数据、队列、缓存
          ├── data/books：用户源文件
          └── data/manga：参考图、分镜图、拼页和导出文件
```

## 前端

前端是零第三方依赖的原生 JavaScript 和 CSS，不需要 npm、Vite 或构建步骤。
`web/app.js` 负责启动认证状态和视图；`web/ui/panels.js` 统一设置导航，
设置页按需加载以下模块：

- `model-library.js` 管理供应商连接、模型能力、价格和适用任务；
- `model-routing.js` 管理文本/图像模型池，支持拖拽排序、顺序降级、轮询和随机；
- `ai-settings.js` 管理联网搜索、长期记忆和文本/图像/负面提示词；
- `security-settings.js` 管理 LAN 开关、一次性二维码邀请、备用配对码和会话；
- `offline-settings.js` 管理本设备逐书下载、存储清理、同步和冲突选择；
- `storyboards.js` 编辑结构化整页分镜，生成前保留原文依据和人物校验。

阅读器仍使用 `chapter/paragraph/offset` 定位。分页内核在
`web/ui/paging.js` 中用 CSS 多列和 translateX 渲染，批注只依赖段落文本坐标，
因此 AI 失败或服务商不可用不会阻塞阅读。

设置导航按“阅读 / AI 引擎 / 扩展功能 / 系统与网络”折叠分组；模型池和整页
漫画配置提供稳健、平衡、激进三档预设，预设只在显式选择时回填默认参数，
高级字段仍可覆盖。配置文件按功能分开保存（文本/图像路由、创作偏好、翻译、
安全和按书的漫画配置），`GET /api/config/domains` 与 `GET /api/config/presets`
只提供公开导航元数据。

## 本机副本与多设备同步

`util/local-store.js` 使用 IndexedDB 保存书目、章节、阅读进度、书签、批注和
待同步操作；`util/local-media.js` 单独保存显式下载的 EPUB/CBZ 图片 Blob。
阅读 API 仅在网络连接失败时回退到这些副本，401/403 不会被缓存冒充成功。
无持久存储时仍可临时阅读，但禁止标记离线下载成功。

浏览器由 Service Worker 预缓存静态界面，要求 HTTPS 或 localhost。Android
直接从 APK 提供同源静态文件，支持局域网 HTTP 断网启动；API 和媒体仍由
真实服务处理。缓存中不保存 API Key、模型连接设置或授权响应。

离线写入使用稳定实体 ID 和设备序号，序号在 IndexedDB 事务中分配。
服务以操作 ID 幂等应用、以游标拉取增量，并通过分页 snapshot 补全旧数据；
分页后重放首个 snapshot 游标之后的操作以收敛并发变更。原有在线 REST
修改在同步请求时被捕获进日志。批注依据 base_revision 检测冲突，删除保留
tombstone；进度按事件时间合并。用户在设置中选择冲突版本。

同步默认在启动、回网、回到前台与前台每分钟尝试，也可关闭自动同步。
书籍正文和图片只在各设备明确下载时复制。长期记忆、模型凭据和生成任务
当前不向远程设备开放同步。撤销设备会撤销其配对会话；已下载的本机副本
需要在该设备清除。更多平台条件见 [OFFLINE-SYNC.md](OFFLINE-SYNC.md)。

## 后端管线

1. `importer.py` 按 magic bytes 识别 TXT、EPUB、CBZ，限制上传大小并写入
   书籍、章节和搜索索引。
2. `ai/book_analysis.py` 从原文建立场景、人物和证据；`ai/memory.py` 按全局、
   书籍、人物作用域检索用户维护的长期记忆。
3. `ai/director.py` 先要求文本模型输出受校验的结构化总结，再由代码构造视觉
   提示词。原文证据、人物身份和结构化字段会在保存和生成前再次检查。
4. `ai/routing.py` 根据模型池策略选择连接；超时、连接错误、无效输出和
   429/5xx 会冷却故障候选并尝试用户配置的备用模型。并发、RPM 和单连接重试
   在 `ai/limits.py`、`ai/retry.py` 中统一处理。
5. `manga/storyboard.py` 保存整页分镜和配置快照；生成前返回当前生效的人物形态
   与已确认参考图，用户选择的参考图经过后端版本、身份哈希和接受状态校验；图像
   队列生成单格，再由 `manga/compose.py` 拼页。每个结果保留提示词哈希、模型和复核状态。
6. 联网搜索是显式可选的。Tavily、Brave 或 SearXNG 的结果只作为带来源的
   参考上下文，永远不覆盖小说原文，也不会自动写入长期记忆。

## 安全边界

服务默认只监听 `127.0.0.1`。绑定局域网地址前，必须在本机设置中开启 LAN
访问；“连接新设备”生成的邀请放在 URL fragment 中，5 分钟有效且只使用一次，
二维码不包含 API 密钥。远端浏览器用邀请或备用配对码换取 HttpOnly、SameSite 会话。
配对凭据磁盘只保存哈希，API 密钥不会进入前端响应。远端只允许阅读、进度、
书签和批注等白名单操作，连接、模型、搜索密钥、生成和文件管理仍要求本机。
请求不信任 `X-Forwarded-For`，写操作校验同源 Origin，媒体和下载也受保护。

## 存储与配置

SQLite 表结构的唯一来源是 `server/app/schema.sql`。常规设置保存在数据目录
的 JSON 文件并以原子替换写入；Windows API 密钥使用 DPAPI，其他平台使用
权限为 0600 的本地文件。联网搜索结果仅保留一小时缓存。`DATA_DIR` 可将
整套数据切换到隔离目录，测试不得使用用户默认数据目录。

## 格式支持与边界

- TXT：编码探测、章节切分、段落级定位。
- EPUB：读取 container.xml、OPF 和 spine；保留正文段落、章节标题，安全地保存
  包内 raster 图片并按正文锚点显示，支持重复引用与图片-only spine；基础行内
  强调以阅读器样式重建（复杂 CSS、脚本、字体和浮动布局仍不执行）。原文件始终可下载备份。
- CBZ：自然排序图片，使用受保护的媒体接口逐页读取。

EPUB 不执行 HTML、JavaScript 或外部资源；这意味着原书的复杂浮动布局、
脚注交互和字体效果不会完全复刻。导入后的 AI 和批注仍基于纯文本段落，保证
坐标稳定。

模型或漫画服务失败时，后端只保留允许列表中的 HTTP 类别（密钥、权限、余额、
限流、服务端、超时、离线和无效响应）；诊断接口不会发起新的模型请求，也不返回
上游原始响应、URL 或技术日志。

## 验证入口

```powershell
python -m unittest discover -s tests -p 'test_*.py'
python -m pytest
node tests/cdp_ai_settings.mjs
node tests/cdp_catalog.mjs
node tests/test_local_store.mjs
node tests/cdp_offline.mjs
node tests/cdp_offline_media.mjs
```

Windows 桌面构建和启动验收由 `scripts/build-desktop-bundle.ps1` 与
`scripts/measure-startup.mjs` 负责。Linux 只需安装 `server/requirements.txt`
并运行 `bash scripts/run.sh --help` 查看参数。
