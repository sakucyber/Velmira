# API 契约 v0.4（Velmira · 前后端唯一真相）

> 所有 Agent 动手前必须先读本文件。字段名、路径、状态码以本文为准，任何一方不得单方面改动。
> **v0.2 = v0.1 + 第 7 节 M1 增量**（EPUB/CBZ 导入、漫画分页媒体、注释高亮、品牌/版本）。
> **v0.5 = v0.4 + 第 10 节**（模型池、模型库、联网搜索、长期记忆、提示词与 LAN 配对）。v0.4 = v0.3 + 第 9 节；冲突时以较新章节为准。
> 端口：127.0.0.1:8765（Linux 可用 scripts/run.sh；绑定 0.0.0.0 前必须完成 LAN 配对设置）（测试用 8799 或 --port 随机，数据目录互相隔离）
> 索引约定：章节 idx **0-based**（前端展示 +1）；段落下标 0-based。

---

## 1. 通用约定

- 全部 JSON 使用 UTF-8，Content-Type: application/json; charset=utf-8
- 成功：2xx + 直接返回数据对象；创建：201
- 失败：{"error": {"code": "snake_case", "message": "人类可读"}}，配 400/404/409/500
- 默认本机管理；LAN 访问必须用本机生成配对码并换取 HttpOnly SameSite 会话。远端只读阅读/进度/书签/批注白名单；连接、模型、密钥、生成和文件管理要求本机。写请求必须通过同源 Origin 校验。
- 时间戳：Unix 秒（整数）
- 环境变量 DATA_DIR 覆盖数据目录（默认 项目根/data），测试必须用它做隔离

---

## 2. 数据库 DDL（权威，A 落成 server/app/schema.sql，C 只读不改）

~~~sql
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS books (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  author TEXT,
  format TEXT NOT NULL DEFAULT 'txt',
  file_path TEXT NOT NULL,
  sha256 TEXT NOT NULL UNIQUE,
  encoding TEXT,
  total_chars INTEGER NOT NULL DEFAULT 0,
  chapter_count INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS chapters (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  idx INTEGER NOT NULL,
  title TEXT NOT NULL,
  chars INTEGER NOT NULL,
  paras_json TEXT NOT NULL,
  UNIQUE(book_id, idx)
);
CREATE INDEX IF NOT EXISTS idx_chapters_book ON chapters(book_id, idx);

CREATE TABLE IF NOT EXISTS progress (
  book_id TEXT PRIMARY KEY,
  chapter INTEGER NOT NULL DEFAULT 0,
  para INTEGER NOT NULL DEFAULT 0,
  offset INTEGER NOT NULL DEFAULT 0,
  percent REAL NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS bookmarks (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  chapter INTEGER NOT NULL,
  para INTEGER NOT NULL,
  offset INTEGER NOT NULL DEFAULT 0,
  label TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS segments (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  chapter INTEGER NOT NULL,
  idx INTEGER NOT NULL,
  strategy TEXT NOT NULL,
  params_hash TEXT NOT NULL,
  start_para INTEGER NOT NULL,
  end_para INTEGER NOT NULL,
  text TEXT NOT NULL,
  features_json TEXT NOT NULL,
  score REAL NOT NULL,          -- 语料内百分位 0..1
  raw REAL NOT NULL DEFAULT 0,  -- 原始加权分
  selected INTEGER NOT NULL DEFAULT 0,
  UNIQUE(book_id, chapter, idx, params_hash)
);
CREATE INDEX IF NOT EXISTS idx_segments_lookup ON segments(book_id, params_hash, chapter, score);

CREATE TABLE IF NOT EXISTS search_index_state (
  book_id TEXT PRIMARY KEY,
  chapters_indexed INTEGER NOT NULL DEFAULT 0,
  degraded INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS para_fts USING fts5(
  text,
  book_id UNINDEXED,
  chapter_idx UNINDEXED,
  para_idx UNINDEXED,
  tokenize='trigram'
);
~~~

---

## 3. server/app/db.py 必须导出的接口（C 依赖）

~~~python
DB_PATH: pathlib.Path
def init_db() -> None            # 幂等：建目录、执行 schema.sql
def get_conn() -> sqlite3.Connection   # row_factory=sqlite3.Row，WAL
~~~

server/app/main.py 中 C 的路由必须可选挂载（A 写）：

~~~python
try:
    from .api.segments import router as segments_router
    app.include_router(segments_router)
except ImportError:
    pass
try:
    from .api.search import router as search_router
    app.include_router(search_router)
except ImportError:
    pass
~~~

C 的两个模块各自导出 router: APIRouter，**路径自带 /api 前缀**，main 挂载时不加 prefix。
静态托管放最后：app.mount("/", StaticFiles(directory=WEB_DIR, html=True))，WEB_DIR = 项目根/web。

---

## 4. 端点

### 4.1 GET /api/health
~~~json
{"ok": true, "version": "0.1.0", "data_dir": "D:/project/ai reading plus/data"}
~~~

### 4.2 POST /api/books?filename=xxx.txt&force=0
请求体 = 原始文件字节（Content-Type: application/octet-stream 或 text/plain，不解析 multipart）。
- filename 必填（无扩展名也可，用于标题兜底）
- 同 sha256 已存在：force=0 → 200 + 已有 book + "deduped":true；force=1 → 覆盖重建
- 成功 201：

~~~json
{
  "book": {
    "id": "b_3f9a2c", "title": "示例小说", "author": null,
    "format": "txt", "encoding": "utf-8",
    "total_chars": 516256, "chapter_count": 234, "created_at": 1762934000
  },
  "import_ms": 812,
  "paragraphs": 15245,
  "deduped": false
}
> 注：示例数字仅供参考，断言以「干净剥离书头/简介/章首标签行」后的实测为准；
> 段落口径 = 非空段落 − 章节标题行，不得为了凑数字保留简介块。
> 本书实测（已核对字节）：**无 BOM**，encoding 必须报 "utf-8"，不得为对齐旧文档伪报 utf-8-sig。
> total_chars = 解码后全文长度（含换行与书头，516256）；**正文总字数 = Σ chapters.chars = 482081**，
> 两者不可混用，percent 的分母是后者。
~~~
- 错误：400 unsupported_encoding / empty_file；413 file_too_large（>64MB）

### 4.3 GET /api/books
~~~json
{"books": [ { "id": "b_3f9a2c", "title": "...", "total_chars": 516256, "chapter_count": 234,
              "created_at": 1762934000, "last_chapter": 33, "percent": 0.12 } ]}
~~~

### 4.4 GET /api/books/{book_id}
~~~json
{"book": { ...同 4.2 字段..., "para_count": 15245 }}
~~~
404 book_not_found

### 4.5 GET /api/books/{book_id}/toc
~~~json
{"book_id": "b_3f9a2c", "chapters": [{"idx": 0, "title": "第1章 穿越", "chars": 1980}]}
~~~

### 4.6 GET /api/books/{book_id}/chapters/{idx}
~~~json
{"book_id": "b_3f9a2c", "idx": 0, "title": "第1章 穿越", "chars": 1980,
 "paragraphs": ["第一段正文……", "第二段……"]}
~~~
404 chapter_not_found。**不返回全书、不返回相邻章**。

### 4.7 GET /api/books/{book_id}/progress
~~~json
{"progress": {"chapter": 0, "para": 0, "offset": 0, "percent": 0.0, "updated_at": 1762934000}}
~~~
无记录时返回 chapter=0 的默认值（不要 404）。

### 4.8 PUT /api/books/{book_id}/progress
请求：{"chapter": 33, "para": 2, "offset": 7}（percent 由服务端推算，客户端传了也忽略）
percent 定义：percent = (Σ chapters.chars[idx < chapter] + 章内比例 × chapters.chars[chapter]) / Σ chapters.chars
- 分母是**正文总字数**（本书 482,081），不是 books.total_chars（516,256，含换行与书头）
- 最后一章最后一段应得 percent = 1.0（±0.001），不得停在 0.93
响应：同 4.7。校验 chapter 越界 → 400 invalid_locator。

### 4.9 书签（可选，实现则按此）
- GET /api/books/{book_id}/bookmarks → {"bookmarks": [...]}
- POST /api/books/{book_id}/bookmarks {"chapter","para","offset","label"} → 201 {"bookmark": {...}}
- DELETE /api/books/{book_id}/bookmarks/{id} → 200 {"ok": true}

### 4.10 POST /api/books/{book_id}/segment（C）
请求：
~~~json
{"strategy": "paragraph_window",
 "params": {"target_chars": 900, "overlap_paras": 1, "min_chars": 400, "max_chars": 1500},
 "per_chapter": 1,
 "force": false}
~~~
响应：
~~~json
{"book_id": "b_3f9a2c", "strategy": "paragraph_window", "params_hash": "9f2c1ab0",
 "segments": 475, "selected": 234, "picked_chars": 214184, "elapsed_ms": 850, "cached": false}
~~~
- 同 (book_id, params_hash) 已存在且 force=false → cached=true，不重算
- **M0 限定「每本书单套分段结果」**：POST 时先删该书旧 segments 再写新结果（ids 全局主键，多套参数暂不并存）；GET /segments 因此不需要 params_hash 参数。多策略对比留到 M1（届时 GET 增 params_hash）。
- 幂等可重入；写入用 REPLACE
- 标定参照 docs/spike_report.json，允许因书头/简介剥离产生 ≤0.1% 偏差（例如 picked_chars 214184→214167）；
  偏差必须在报告里注明原因，**不得为了对齐数字保留被污染的片段**

### 4.11 GET /api/books/{book_id}/segments?chapter=32&selected=1&limit=50（C）
~~~json
{"segments": [{"id": "c33-s0", "chapter": 32, "idx": 0, "start_para": 0, "end_para": 12,
               "chars": 919, "raw": 3.414, "score": 0.999, "selected": 1,
               "title": "第33章 示例章节", "head": "（示例段落）"}]}
~~~
chapter 可选；selected 可选（1/0）；按 chapter 升序、score 降序。**不返回 text 全文**（省流量，需要时前端用章节段落拼）。
limit 默认 50，实现上限 1000（本书仅 475 段，越界不报错只钳制）。

### 4.12 GET /api/books/{book_id}/search?q=主角A&limit=20（C）
~~~json
{"book_id": "b_3f9a2c", "q": "主角A", "elapsed_ms": 12,
 "hits": [{"chapter": 32, "para": 3, "text": "……主角A……", "snippet": "……<b>主角A</b>……", "score": -3.2}],
 "index": {"chapters_indexed": 234, "degraded": false}}
~~~
- q 长度 < 1 → 400 empty_query
- **MATCH 语义（sqlite 3.45 实测）：trigram 的 MATCH 只支持 ≥3 字符。**
  - ≥3 字：MATCH + bm25 排序
  - 2 字（如「主角A」）：走 para_fts 的 LIKE 查询（真实命中，无 bm25，按 chapter/para 排序）
  - 1 字：走 chapters 表 LIKE，并置 degraded=true
  - 任何长度都不得报错，且必须返回可定位的 chapter/para
- 响应里的 q 字段是**原样回显**，前端必须用 textContent 渲染它；hits 里的 text/snippet 已做 HTML 转义
- 索引按需增量：首次查询/新章导入后补齐，单本 234 章建索引 < 2s
- 只返回命中段落，前端用 Locator 跳转并高亮

---

## 5. 前端约定（B 执行，D 检查）

- 路由：/ → 书库（导入、列表、进度）；/?book=<book_id> → 阅读器；章节/段落用 query 参数 ch/p 同步（history.replaceState，便于刷新恢复）
- 纯 ESM，无打包、无第三方库；每个模块 < 400 行
- 单章数据只在需要时请求；任何时候不在内存里保留 2 章以上正文
- 大章切块：单章字符 > 8000 时前端按段落切 chunk（≤8000 字/块），一次只渲染一块
- 事件用委托，组件销毁时 abort 全部 fetch
- 切章先销毁旧 DOM 再渲染新章，禁止双向保留
- localStorage 只存 {"book","chapter","para","offset","theme","fontSize"}，< 2KB
- 移动端 390×844 无横向溢出；键盘 ←/→ 翻页，触摸左右点击区翻页
- 可选功能（分镜片段面板、搜索）在接口 404 时必须静默降级，不得报错阻塞阅读

---

## 6. 运行与测试

~~~powershell
# 启动（项目根）
python -m uvicorn server.app.main:app --host 127.0.0.1 --port 8765
# 或 scripts/run.ps1

# 导入实测
curl.exe -X POST "http://127.0.0.1:8765/api/books?filename=book.txt" --data-binary "@path/to/book.txt" -H "Content-Type: application/octet-stream"
~~~

测试独占端口与数据目录：设 DATA_DIR=data/_test_<随机>、--port 8799。
不许 pip install / npm install；只用 Python 3.11 + fastapi 0.141 + uvicorn 0.54 + sqlite3 标准库。

---

# 7. M1 增量契约（v0.2）

## 7.1 品牌与版本

- 产品名：**Velmira**（UI 标题、document.title、README、健康检查统一）
- `server/app/config.py: VERSION = "0.5.1"`（当前版本，见第 9 节）
- GET /api/health 变为：
~~~json
{"ok": true, "name": "Velmira", "version": "0.5.1"}
~~~
- 前端不得再出现旧品牌名；浏览器标签页格式：`<书名> · Velmira`

## 7.2 新增 DDL（A 落进 schema.sql，幂等）

~~~sql
CREATE TABLE IF NOT EXISTS book_pages (          -- 仅 cbz 使用
  book_id TEXT NOT NULL,
  idx INTEGER NOT NULL,
  path TEXT NOT NULL,                             -- 相对 DATA_DIR 的路径
  width INTEGER NOT NULL DEFAULT 0,
  height INTEGER NOT NULL DEFAULT 0,
  bytes INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (book_id, idx)
);

CREATE TABLE IF NOT EXISTS annotations (          -- 高亮/笔记
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  chapter INTEGER NOT NULL,
  para INTEGER NOT NULL,
  start_offset INTEGER NOT NULL,
  end_offset INTEGER NOT NULL,
  text TEXT NOT NULL,
  note TEXT,
  color TEXT NOT NULL DEFAULT 'yellow',          -- yellow | green | blue | pink
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_annotations_book ON annotations(book_id, chapter, para);
~~~

## 7.3 导入：格式自动识别（沿用 POST /api/books）

- `filename` 扩展名仅作提示，**以字节为准**：
  - zip 且含 `META-INF/container.xml` 或 mimetype=application/epub+zip → **epub**
  - zip 且非 epub，内含 ≥1 张图片（jpg/jpeg/png/webp/gif）→ **cbz**
  - 其它 → txt（沿用 v0.1 编码探测）
- `books.format` ∈ `txt` | `epub` | `cbz`
- **epub**：解析 container.xml → OPF → spine 顺序 → 每个 XHTML 抽正文为段落（剥标签、去脚本样式、合并连续空行），生成 chapters；encoding 固定 utf-8；title/author 取 OPF 的 dc:title/dc:creator（缺失回退文件名）。正文引用的 JPEG/PNG/GIF/WebP 以章节归属保存，GET /api/books/{id}/epub/assets/{asset_id} 通过受保护接口读取；strong/em/u/s/code 的字符范围作为 paragraph_formats 返回。脚本、SVG、data URL、外链、复杂 CSS 和字体不执行。
- **cbz**：zip 内图片按**自然排序**（1,2,10 而非 1,10,2）解压到 `DATA_DIR/media/<book_id>/`，写 book_pages（width/height 用 Pillow 读，失败填 0）；`chapter_count = page_count`；不写 chapters 行
- 响应沿用 v0.1 结构，新增/变化字段：
~~~json
{"book": {"id": "b_x", "title": "...", "format": "cbz",
          "total_chars": 0, "chapter_count": 24, "page_count": 24, "encoding": null,
          "created_at": 1762934000},
 "import_ms": 420, "paragraphs": 0, "deduped": false}
~~~
- `GET /api/books/{id}` 对 cbz 增加 `"page_count"`；对 txt/epub 增加 `"para_count"`（原有）

## 7.4 漫画分页与媒体

- `GET /api/books/{book_id}/pages/{idx}` → 200
~~~json
{"book_id": "b_x", "idx": 0, "count": 24, "url": "/media/b_x/0", "width": 1200, "height": 1800}
~~~
  404 `page_not_found`；非 cbz 书 400 `not_comic`
- `GET /api/books/{book_id}/pages?from=0&limit=12` → `{"pages": [{idx,url,width,height}], "count": 24, "from": 0}`（limit 上限 50，默认 12）
- `GET /media/{book_id}/{idx}` → 图片字节，Content-Type 按扩展名，`Cache-Control: public, max-age=31536000, immutable`；404 不存在
- 漫画进度复用 progress：`chapter = 页码`、`para = 0`、`offset = 0`；percent = (page+1)/page_count（服务端对 cbz 按页计算，不再用字数公式）
  - **无进度记录的默认值也必须遵守该公式**：未读过时 GET /progress 返回 chapter=0 且 percent = 1/page_count（例如 5 页书 0.2），不能返回 0.0

## 7.5 注释/高亮

- `GET /api/books/{book_id}/annotations?chapter=32&limit=200&offset=0`
~~~json
{"annotations": [{"id": "an_x", "chapter": 32, "para": 3, "start_offset": 2, "end_offset": 8,
                  "text": "主角A", "note": "主角", "color": "yellow",
                  "created_at": 1762934000, "updated_at": 1762934000}]}
~~~
  chapter 省略 → 全书（limit 上限 500）；按 chapter, para, start_offset 升序
- `POST /api/books/{book_id}/annotations` body `{chapter, para, start_offset, end_offset, text, note?, color?}` → 201 `{"annotation": {...}}`
  校验：chapter 在范围内、0 ≤ start < end、`len(text) == end-start`、para 在章节段落范围内 → 否则 400 `invalid_annotation`
- `PATCH /api/books/{book_id}/annotations/{id}` body `{note?, color?}` → 200 `{"annotation": {...}}`；404 `annotation_not_found`
- `DELETE /api/books/{book_id}/annotations/{id}` → 200 `{"ok": true}`
- color 只允许 `yellow|green|blue|pink`，其它值 400

## 7.6 前端约定（M1）

- 路由不变：`/?book=<id>`。app.js 仍只负责显示容器并 import reader.js；**reader.js 取到 book 后若 format=cbz，改 import `./ui/comic.js` 并调用**
~~~js
export async function mountComic({ bookId, params, book })  // 自行接管 #reader-main
~~~
  comic.js 可在 `<body>` 上加 `comic-mode` 类隐藏文本阅读器顶栏/底栏（样式放 web/styles/comic.css）
- 文本阅读器新增：书签（加/删/列表/跳转）、选中文字→浮动菜单（4 色高亮 / 写笔记 / 复制）、笔记抽屉、目录过滤框、触摸滑动翻页（阈值 40px，不改现有键盘与点击区）
- **双页模式为可选加分项**：仅 ≥1024px 生效，默认关闭，设置里可切换；做不好可以不做，但不得破坏单页模式
- 高亮渲染：用 `<mark class="ann ann-yellow" data-ann="<id>">` 包裹原文文本节点，切章重渲染后必须按注释列表重建；不得改写纯文本内容
- 内存硬约束（M1 追加）：
  - 注释只在当前章保存到内存（单章查询），切章清空
  - 漫画模式**只保留 1 个 `<img>` 元素**（换页改 src，不 new Image 预加载超过 1 张，不用 canvas）
  - 前端 JS 总量 < 230KB（v0.4 当前预算，含漫画与按需加载模块；M1 的 150KB 是历史预算）
  - 首屏请求：**文本模式 ≤ 9，漫画模式 ≤ 12**（漫画必拉 comic.js/comic.css/pages/media）

---

# 8. v0.3 增量：实时书库 + 内容脱敏

## 8.1 内容脱敏（硬性规则，优先级最高）

- **产品界面、README、演示数据、截图、提示文案中不得出现任何具体小说内容**：书名、角色名、原文句子、情节/章节标题描述一律不得出现。
- 书库内容完全由用户导入决定，**产品不预置任何书籍**，也不得预置带小说原文的示例注释/书签。
- 仓库只能提供中性示例：`samples/demo-text.txt`（通用占位文本）与 `samples/demo-comic.cbz`（程序绘制的占位分镜）。
- 后端/前端代码中同样不得硬编码具体书名或角色名；测试/脚本若必须使用本地测试书，用「取 book/ 目录第一本」这种方式定位，不写死标题。

## 8.2 GET /api/events（SSE 实时事件流）

- Content-Type: `text/event-stream`，UTF-8，长连接
- 事件类型 `books_changed`：
~~~
event: books_changed
data: {"revision": 7, "count": 3}

~~~
  - `revision`：进程内单调递增整数（书库每次变化 +1）
  - `count`：当前书籍数量
- 心跳：每 15 秒发送一行 `: ping`（注释行），保活
- 广播时机：新书导入成功（201）、同文件 force 重建成功
- 客户端断开后服务端必须释放队列（不得泄漏）
- 单进程内广播即可（部署假设 uvicorn 单 worker；多 worker 时退化为轮询，前端已有兜底）

## 8.3 前端书库实时刷新

- `/` 书库页挂载时订阅 `EventSource('/api/events')`
- 收到 `books_changed` → 重新 `GET /api/books`，**原地更新列表**：保留滚动位置、不整页刷新、不闪烁；新增书可有一次轻微高亮
- EventSource 不可用或报错 → 退化为 **5 秒轮询** `GET /api/books`，比对数据后更新
- 离开书库（进入阅读器/页面卸载）→ 关闭 EventSource 与轮询；不得累积监听
- 保留「刷新」按钮作为手动兜底；书库标题旁显示「实时」状态点（连接中/已连接/轮询中/离线）
- 内存：任意时刻 1 个 EventSource、1 个定时器；列表 DOM 只重建必要节点

## 8.4 演示数据

- 仓库提供 `samples/demo-text.txt`：通用中文占位文本（≥2000 字，无具体小说内容），用于演示分页/高亮/注释。
- 不提交 `data/` 运行数据；用户首次启动为**空书库**，由用户导入。

# 9. v0.4 增量：批注预览与分发

- 当前 `GET /api/health` 返回 `name="Velmira"`、`version="0.5.1"`；书库标识为 v0.4，APK 版本为 0.5.1。
- 注释 API 和数据库结构不变。`note` 为非空文字时，原文 `mark.ann` 额外带 `ann-note` 类，用双下划线标记；普通高亮维持四色背景。
- 鼠标悬停批注文字直接展示笔记，点击可固定弹层；手机单击、键盘 Enter/空格也可查看。弹层保留改色、编辑、删除操作。
- 笔记用文本节点渲染，保留换行；弹层限宽/限高，长内容可滚动。点空白、Escape、翻页、切章、调整窗口时收起，不触发额外翻页。
- 桌面分发包包含目录模式的 `backend/Velmira-backend/`（PyInstaller），优先启动内置后端，避免每次启动解压单文件后端；源代码模式仍可使用 Python。打包模式默认数据目录为 `%LOCALAPPDATA%\Velmira\data`，`DATA_DIR` 可覆盖。
- 桌面窗口禁用浏览器默认右键菜单。书库页整页接收文件拖放，侧栏、搜索框、封面或空白处松开文件均沿用现有导入校验与流程；普通文字拖拽不触发文件导入。

# 10. M2 增量：文本抽取与人物卡

- `GET /api/books/{book_id}/ai/config` → 当前文本 provider 配置；默认 `mock`，密钥只保存环境变量名。
- `PATCH /api/books/{book_id}/ai/config` body `{provider, base_url?, model?, api_key_env?, temperature?, prompt_version?, enabled?}`。
- `POST /api/books/{book_id}/ai/extract` body `{segment_ids?: string[], provider?: string, force?: boolean, prompt_version?: number}` → scene/人物卡抽取统计。省略 `segment_ids` 时只处理 `selected=1` 的候选片段；默认 Mock 离线可运行。
- `GET /api/books/{book_id}/ai/scenes`、`GET /api/books/{book_id}/ai/characters` → 结构化 scene 与人物卡；scene 只包含 `summary/location/time_of_day/characters/key_props/drawable/reason/continuity`。
- `GET/PATCH /api/books/{book_id}/ai/characters/{id}` → 人工查看/编辑；`POST .../{id}/freeze` 固定确定性英文 `identity_block` 并保存 `identity_hash`；`POST .../{id}/unfreeze` 必须 `confirm=true`，升版本并保留旧身份快照。
- 自动抽取幂等键包含书籍、片段、提示词版本以及正文和 provider 配置的哈希；切换模型后重新抽取。provider 输出非法时只将该片段标为 `needs_review`，不会中断整批。
- 人物卡视觉字段必须有原文 `evidence[{field,value,quote,ch}]`；冻结后自动流程不覆盖 identity 字段，只能追加证据/变体。

# 11. 书架管理与阅读设置增量

- `GET /api/books` 默认仅返回在架书籍；`GET /api/books?archived=true` 仅返回已移出书籍。列表额外提供 `has_progress:boolean`、`last_read_at:int`（未读为 0）、`archived_at:int|null`。漫画默认第一页的百分比不代表已读，阅读状态以 `has_progress` 为准。
- `PATCH /api/books/{book_id}` 接收非空 JSON 对象 `{title?: string, archived?: boolean}`。书名去首尾空白后须为 1–200 字符，`archived` 仅接受布尔值，未知字段返回 400；书籍不存在返回 404。成功返回 200 `{"book": {...}}`，与详情端点的 book 字段一致。
- 新增幂等表 `library_state(book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE, archived_at INTEGER NOT NULL)`；有记录表示已移出。移出/恢复只改变此表，正文、进度、书签、注释与 M2 记录均保留。旧数据库启动时自动添加此表。
- 重新导入已移出的相同文件会删除移出标记，返回额外的 `restored:true`，保留原书 ID 与阅读数据；正常导入返回 `restored:false`。原有去重和 `force` 语义不变。
- 成功重命名、移出、恢复、重新导入恢复均广播 `books_changed`；事件的 `count` 为在架书籍数，广播失败不影响已提交的操作。
- 书架密度与排序存在独立本机键 `velmira.shelf`；阅读主题与字号继续使用 `velmira.r1`，漫画适页/适宽继续使用 `velmira.comic.fit`。不改变阅读位置与注释坐标契约。
- 阅读“设置”集中显示主题、字号与当前模式实际支持的快捷键，打开时暂停翻页快捷键；关闭可用 Escape 并恢复触发按钮焦点。移除底部常驻快捷键提示，保留页码与全书进度。
- `velmira.r1.fontSize` 现在保存实际像素值（12–48，精度 0.1，默认 19）。读取旧值 0/1/2/3 时自动转换为 17/19/22/25 px，后续保存使用像素值，不改变主题或阅读坐标。字号支持连续滑块、数值输入与恢复默认；连续输入合并并串行重排，完成后重建批注。

# 12. M3 单格出图

以下路径均以 `/api/books/{book_id}/manga` 为前缀，错误保持 `{"error":{"code","message"}}`。

- `GET/PATCH /config`：图像 provider 配置；`provider=comfyui|openai-images|mock`，包含 `base_url/model/unet/clip/vae/edit_lora/width/height/steps/cfg/sampler/scheduler/seed/style_preset/style/negative/face_threshold/max_attempts/timeout/face_python/face_models`。宽高须为 16 的倍数；最多 3 次尝试；默认人脸阈值 0.55。校验器路径为空时使用环境变量或本机默认值。GET 另返回 `styles`（预设 id 到 `{label,prompt}` 的映射）。
- `style_preset` 默认 `anime`，另支持 `manga/watercolor/chinese/semi_realistic/cinematic/custom`。anime 默认提示词明确要求日系二维彩色漫画、清晰线稿、动漫五官与赛璐璐阴影，默认负面约束排除真实摄影、皮肤毛孔和 3D 渲染。显式选择预设时使用其固定提示词；只修改 `style` 时转为 custom。旧版内置半写实默认及旧 anime 默认更新为新的二次元模板；已有自定义风格保留。生成任务保存实际风格和预设快照。ComfyUI 单独使用 negative 编码，OpenAI Images 兼容接口将负面约束附加到实际发送的 prompt，避免接口忽略 negative 字段。
- `POST /probe`：检查节点和模型，返回 `{ok,provider,missing,models?,reference_mode}`。Mock 只验证流程，不代表人物一致性通过。
- `GET /forms`：本书人物形态，含版本、冻结身份及 `refs[]`。基础形态从 M2 人物卡同步；修改基础身份沿用 M2 编辑/解冻/冻结接口。
- `POST /forms`：`{character_id,form_key,label,card,start_ch?,start_para?,reversible?}` 新增形态；生效锚点为从 0 开始的章节、段落，重复锚点拒绝。`PATCH /forms/{id}` 编辑未冻结的派生形态，`POST /forms/{id}/actions/freeze|unfreeze` 冻结/解冻（解冻须 `confirm=true`，升版本并标旧结果过期）。
- `POST /forms/{id}/refs`：原始图片字节，最大 16 MB，转换为本地 PNG，默认未确认。`PATCH /refs/{id}` `{accepted:boolean}` 人工确认/撤销；卡版本不一致时拒绝确认，撤销使使用该参考图的任务过期。
- `POST /jobs`：`{kind?:panel|reference,form_id?,character_id?,scene_id?,shot?,action?,seed?,flashback?,para?,request_id?,plan_id?}`，201 返回 `{job}`。单人格必须属于所选 M2 场景，身份须冻结，参考图须已确认且版本一致。多人场景直接生成时拒绝；提供有效 `plan_id` 时使用总结聚焦所选人物的单人镜头。显式旧形态须标记闪回，未生效形态始终拒绝。`request_id` 为书内幂等键，重复键搭配不同快照返回 409。
- `GET /jobs?limit=30` 和 `GET /jobs/{id}`：返回配置快照、prompt、config hash、每次尝试的 seed/workflow/prompt id/score/图片 URL。
- `POST /jobs/{id}/cancel|retry|reroll|approve|reference`：取消、续查失败任务、换种子新建任务、人工确认分镜、存为参考图候选。`reroll` 可传 `{seed}`；`reference` 创建的候选仍须确认。
- `GET /assets/{id}`：本书内图片。拒绝跨书访问，不接受任意文件路径。

任务状态：`queued → running → done|needs_review|error|stale|cancelled`。仅单人脸测量分数达到阈值才标 `done`；无人脸、多人脸或校验器不可用都进入 `needs_review`，人工确认后为独立状态 `approved`，不虚构分数。参考图候选始终需要人工确认。进程重启将 `running` 恢复入队，有 prompt id 时续查原任务；ComfyUI 提交前保存 UUID，避免超时后无记录地重复提交。部署使用一个后端进程和一个串行生成队列。

# 13. 上下文总结与提示词

- `GET /api/ai/settings` 返回公开设置和 `key_configured`，不回显文本连接密钥。`PATCH /api/ai/settings` 接收开关、自动分析、地址、模型、限流、JSON 模式和 `api_key`；空密钥保留旧值，地址支持不含凭据和查询参数的 HTTP/HTTPS（包括局域网 HTTP）。漫画设置使用文本模型池；地址、模型与密钥字段保留用于旧版配置兼容。
- `GET /api/ai/models` 从已保存的地址读取模型名称；`POST /api/ai/probe` 发起小型 JSON 完成请求，返回 `{ok:true,model}`。
- 默认文本模型 `gpt-oss-120b`，128K 上下文，只发送文字。请求按 UTF-8 字节数保守估算 token 上界，预留 3000 输出 token 和 1024 消息开销；这是保守预算，不是精确 tokenizer 计数。整书按章节和分段处理，无需把全书一次塞入上下文。模型具备工具调用能力，本流程由应用串联请求和图像队列，不向模型开放额外工具。
- `POST /api/books/{book_id}/manga/plan` 接收 `{scene_id,form_id,shot?,action?,para?,flashback?}`，返回 `{plan,cached}`。镜头默认 `portrait`，界面明确传入当前选项。分析完成前 `min(10,总章节数)` 章且目标章节已分析即可生成；否则自动入队并返回 409 `book_analysis_pending`。就绪依据当前分析 run 的已完成章节前缀，旧缓存不能越过门槛；暂停/失败任务需显式继续。读取目标片段（最多 6000 字符）、片段附近本章文字（最多 14000 字符）、上一章结尾（最多 3000 字符），结合相关章节摘要和人物证据，调用文本模型生成结构化中文总结和英文单格提示词。生成目标章节时不引用未来章节资料，也不把参考图发给文本模型。
- 总结字段为 `summary/location/time_of_day/moment/visible_action/emotion/key_props/continuity/uncertainties`。模型输出须通过 JSON、类型和长度校验；每个文本阶段的网络请求和结构化校验共用最多 3 次尝试（含首次），提示词夹带中文也计入同一预算；失败返回 502，不提交出图任务。
- `plan` 保存来源哈希、范围字符数、模型标识、总结和提示词；缓存键包括来源、地址、模型及方案版本。场景/人物/镜头/上下文在分析期间改变，或旧方案被用于新选择时，返回 409 `text_plan_stale`。
- 有效 `plan_id` 的出图任务在快照中保存 `text_plan`，将英文提示词与冻结身份、参考图约束一起交给 MOSS。参考图确认、人物版本、生效段落和人脸阈值检查继续适用；执行前和完成时重新验证源上下文，换种子保留总结方案。
- 本机共享文本 AI 启用后，仍使用默认 Mock 配置的 M2 书籍分析会使用共享 API；显式选择 provider 的请求继续遵循其选择。

# 14. 整书阅读与通用图像 API

- `GET /api/books/{book_id}/ai/analysis` 返回 `{analysis,chapters}`。analysis 含 `status/total/completed/current_chapter/current_part/total_parts/error/updated_at`；失败时另有 `failure:{code,reason,stage,chapter,model,http_status,finish_reason,usage,max_tokens,title,message,suggestion}`。chapters 按章节排序，含 `chapter/title/summary/character_ids/entities/parts`。未开始时为 idle。
- `POST /api/books/{book_id}/ai/analysis` 开始或继续，返回同样结构。只接收 TXT/EPUB。相同来源和模型的已完成任务直接复用；更改内容、模型、地址或分析版本会新建 run，复用仍有效的缓存。
- `POST /api/books/{book_id}/ai/analysis/pause` 暂停 queued/running 任务；正在进行的 HTTP 请求可能完成，但晚到响应不会写入已暂停任务。状态为 `queued/running/done/paused/error`。关闭程序后 running 在下次启动恢复为 queued；已完成章节、分段和摘要合并结果持久化复用。
- `GET /api/books/{book_id}/ai/analysis/diagnose` 返回 `{analysis,checks,logs,samples,config,can_resume,failure}`，不调用任何模型；用于查看阶段、原因、Token 预算与最近任务时间线。详见 `AI-DIAGNOSTICS.md`。
- `GET /api/books/{book_id}/ai/analysis/diagnostics/export` 返回 zip（summary/task-logs/config/samples/README），只含脱敏后的失败样本、任务日志和公开配置，不含密钥、提示词或上游响应正文。
- `auto_analyze` 默认 true；共享文本 AI 启用后，每 15 秒发现未分析的未归档小说并排队，含已有书库和后续导入。关闭自动发现不取消已创建任务；面板可以单独暂停、继续。API 错误或无效 JSON 在 3 次尝试都失败后停止任务；401/402/403 等凭据或配置错误只请求一次，不静默跳章、不伪造完成、不自动无限重试。
- 每段最多 6000 字符，包含超长单段的全部文字。多段章节按每组最多 8 份摘要递归合并，保留每章大意；前章摘要参与下一章阅读。人物视觉证据必须逐字出现在该段，章节编号由应用写入；人物卡与场景在同一事务落库，冻结身份不覆盖。新增人物卡仍需冻结并确认参考图后生成分镜。
- 每章另由文本模型生成 100–250 token 胶囊及带原文引用和字符位置的事实；自动分镜上下文按最终序列化结果限制为 1800 token。只检索当前位置之前的事实及此前章节胶囊，来源哈希排除过期资料。已知编码采用 tiktoken，未知编码明确标记估计。人物外貌以生效位置的冻结形态和参考图为准，详见 `CHAPTER-MEMORY.md`。
- 场景列表支持 `offset` 和 `limit`，返回 `has_more`；漫画面板加载全部有效场景后按章节选择，不受原默认 100 条限制。文本 AI 启用时，界面“加入生成队列”也会先补齐总结和提示词。
- `GET/PATCH /api/image-ai/settings` 保留独立生图密钥和模型、限流设置用于旧版兼容；地址支持 HTTP/HTTPS。当前漫画设置使用生图模型池，画风、尺寸和 ComfyUI 参数仍在每本书的 `/manga/config` 配置。

### 自动漫画

- `GET /api/books/{book_id}/manga/auto` 返回 `{auto}`：开关、阶段、起始/当前章节、已完成章节和页数、待处理分格、错误提示。
- `PATCH .../auto` 接收 `{enabled:boolean,chapter?:number}`。首次开启从当前章开始；暂停后开启恢复原 run 和起点。完成后重新开启创建新 run，历史草稿可继续查看。仅支持 TXT/EPUB，使用已配置的文本及图像 API。
- 后台分析完成前 10 章（短书则全部章节）后，按章节及原文片段顺序规划分页，每页编排 1–7 格连续分镜；同一章节或分析片段可以对应多页，分页原文范围按句子连续覆盖。整书分析继续运行；生成追上分析进度时等待目标章就绪。把上一页/格内容带入下一次编排和总结。对白校验为原文逐字引用，人物必须来自片段；没有具名人物的片段可以生成环境空镜。
- 依照当前章节以前的外貌证据创建冻结形态，复用同一形态的参考图。自动选用的参考图记录于 `auto_manga_refs`，不把 `accepted` 写成人工确认。撤销自动参考会使相关任务失效，不能由后台重新选回。
- 自动页写入独立的 `auto_manga_pages`，明确标为草稿，`needs_review` 和真实人脸分数保持不变；不绕过 M4 正式拼页/导出的复核门槛。`GET .../auto/pages?offset=0&limit=12&run_id=...&chapter=...` 分页读取草稿（最多30页），省略 run_id 取本次，chapter 可选且从 0 开始、用于正文旁按章节查看；`GET .../auto/runs` 查看历史记录，`GET .../auto/pages/{id}/image` 读取页图。
- 关闭开关后不再创建下一张任务，已提交的一张保留；自动流程自行启动的整书分析也暂停。状态写入 SQLite，重启可续跑；重复开启幂等。文本和图像请求共尝试 3 次后仍失败则停机并显示可处理错误，不无限重试；章节、人物、参考图变化会阻止旧快照继续。中途修改模型/画风须恢复当前页原配置后继续。
- `openai-images` 使用 OpenAI Images 兼容协议：参考图生成调用 `/images/generations`（JSON），带参考图的分镜调用 `/images/edits`（multipart PNG）。需要 `data[0].b64_json` 输出，返回 URL 的接口不在当前适配范围内。所选服务须支持相应模型、尺寸和编辑接口；`/probe` 只验证模型列表可访问，不能证明编辑能力。ComfyUI/MOSS 继续使用原有 HTTP API。
- 图像 API 每次请求最多尝试 3 次，复用同一 Idempotency-Key，提交前持久化请求标识；服务商须支持该键才可确保远端不重复计费。ComfyUI 查询失败或超时继续查询原 prompt；明确执行失败后才重新提交。相似度未达标的换种子重画仍由 max_attempts 控制，与连接失败重试独立。服务中断/退出后无法确认同步请求结果时，续查不会再次提交；用户核对服务端记录后可显式换种子新建任务。实际图像仍须通过参考图及人脸检查，缺少校验或低分保留 needs_review。

# 15. M4 拼页与漫画版

- `GET /api/books/{book_id}/manga/page-panels` 返回本书分镜候选；只有 `done` 或 `approved` 且源快照仍有效的分镜可以成页。
- `GET/POST /api/books/{book_id}/manga/pages` 列出或创建页面；页面设置包含尺寸、白边、字体和 1–7 个分格。自动模板按实际数量选择无空白占位的布局，新增 `1+2+2`、`2+2+1`、`2+3`、`3+4`、`2+2+2+1` 等不等列布局。`PATCH /pages/{id}` 使用 `revision` 乐观锁，`PUT /pages/order` 要求整组页 ID 和版本一致。页面公开结果的 `source_map` 含每格的章节、段落与可用的原文范围，原文范围取自生成快照。
- `DELETE /pages/{id}?revision=N` 是软删除，`POST /pages/{id}/restore` 恢复；`GET /pages/{id}/image?revision=N` 只返回指定版本的不可变 PNG。
- `POST /api/books/{book_id}/manga/pages/auto` 接收 1–120 个分镜 ID，按章节分组并保留勾选顺序，不跨章节合并。自动布局还保留不同 `board_id` 的已规划页面边界；无分页规划的图像每页最多七格。显式选择固定模板时按其容量分页。
- `POST /api/books/{book_id}/manga/exports` 校验所有有效页并生成 CBZ，包含 `ComicInfo.xml` 与 `pages/0001.png` 等有序文件；`GET /exports/{id}/download` 下载。导出快照记录页 revision、图片哈希、章节和段落定位，后续编辑不会改变旧漫画版。
- 书籍详情中的 `reading_links` 提供小说与漫画版的双向关系及页到章节／段落映射；漫画版继续使用普通书籍进度接口，和原小说进度分开保存。

- `POST /api/ai/probe` 分级返回 `{ok,model,stages,note,elapsed_ms}`：模型列表、最小对话、JSON 输出、原文引文四级，含 `fatal/retryable/detail/suggestion`；`ok` 只由致命级（对话、JSON）决定。
- `GET .../auto/logs?after=0&limit=100&run_id=...` 返回 `{logs:[{id,stage,level,message,created_at}],run_id,cursor}`。首次返回最近的日志窗口（按 id 正序）；后续用 after=cursor 增量读取，limit 最大 200。当前任务包含其整书分析日志，历史 run 只查对应漫画日志；按书隔离，每本保留最近 1000 条。不记录密钥、小说正文、提示词或原始服务商错误。
- 自动漫画页面实时显示日志；已完成页在正文右侧按当前章节展示（窄屏在正文下方）。漫画与画风配置统一收进阅读器设置，自动漫画和漫画工作台入口均直达该设置页。
- `GET /api/books/{book_id}/manga/reading-map?chapter=0&run_id=...` 返回 `{book_id,run_id,offset_unit:"unicode_codepoints",range_end:"exclusive",panels}`。省略 run_id 使用当前自动任务；同时合并有效手工页并按 job_id 去重。每格含 `chapter,start,end,source_quote,job_id,page_id,panel_index,page_url,url,rect,caption,dialogue,speaker,draft,pending_review`；`rect` 为页内归一化 `[x,y,width,height]`，`url` 直接读取单格图片。原文完整哈希或引用不符、资源缺失以及缺少可信来源的旧图片不参与映射。
- 阅读页手动、自动播放和语音朗读共享字符进度，支持暂停、继续、调速和跨章；语音边界从浏览器 UTF-16 位置转换为 Unicode 字符位置，缺少细粒度边界时按朗读片段结束推进。


## 整页分镜（2026-11-13）

新增 `/api/books/{book_id}/manga/storyboards`：

- `POST`：`{"scene_id":"..."}`，一次文本调用生成1–7格结构化方案，按原文/前情/模型/版本缓存。
- `GET`：返回本书最近100个方案标识、scene_id、revision和更新时间。
- `GET /{id}`：返回完整方案与原文材料，原文或场景变化返回409。
- `PATCH /{id}`：`{"revision":1,"panels":[...]}`，全量替换分镜并升版；版本冲突返回409。已开始出图的当前自动页禁止编辑。
- `POST /{id}/render`：`{"revision":1,"panels":[0,1]}`，以零基序号选择分镜，省略panels则生成整页；同配置、参考图和seed重复请求复用任务。任何一格不具备条件则整个请求回滚。
- `POST /{id}/approve`：`{"revision":1}`，确认自动漫画当前页的分镜编排；不确认参考图或最终图像质量。

分镜字段：`kind=solo|duo|environment`；`shot=portrait|medium|wide|three_quarter|reaction|detail`；`subjects=[{character_id,position:left|right|center,pose}]`；以及 `action,environment,lighting,emotion,caption,dialogue,speaker,source_quote,start_sentence,end_sentence,source_start,source_end`。新编排使用句子编号连续分组并完整覆盖当前页，`end_sentence` 含末句，字符 `source_end` 为半开区间。字符范围及完整 `source_quote` 由原句组确定；对白须来自本组。动作/环境/姿势采用英文；caption可为空。solo/duo/environment分别要求1/2/0个人物；同框人物和位置不可重复。旧方案编辑仍兼容只有精确原文引用的格式。

新任务snapshot保留 `board_id,panel_index,panel,cast,source_hash`。cast里的每个人物均包含形态版本和参考图哈希，任何一个失效都会使任务过期。双人和空镜进入needs_review，不能凭单人脸分数自动通过；单人保留原有校验。旧snapshot和单格API继续兼容。

图像配置新增 `reference_api=edits|generations`、`rpm=0..1000`、`max_concurrency=1..8`、`review_storyboards:boolean`。generations模式将参考图作为JSON `image`数组传递，edits模式多图采用multipart `image[]`。ComfyUI不支持双参考图，返回409；其并发固定为1。

文本设置新增 `json_mode:boolean`、`rpm`、`max_concurrency`。关闭json_mode仅省略上游response_format字段，本地输出校验仍然执行。输出预算6000 tokens，供需要推理预算的模型完成完整JSON。

`GET /api/ai/connections`仅返回本机已保存连接的公开元数据；`POST /api/ai/connections/activate`接受 `{id,model,kind:"text"|"image"}`，启用相应连接。密钥从本机配置读取，不在响应或数据库快照中返回。连接切换不会打开自动扫描全书库的开关。

## 模型降级与负载均衡

- `GET /api/ai/routing/{kind}`：kind为 `text|image`，返回 `{config,status}`。
- `PATCH /api/ai/routing/{kind}`：接受 `enabled:boolean,strategy:"ordered"|"round_robin"|"random",models:[{connection_id,model}],timeout:5..600,cooldown:0..3600,temperature:0..2,max_tokens:256..32768,revision?`。字段可部分更新；启用须有1–20个同类有效候选，不允许重复。revision冲突409；类型、字段、范围错误400。返回更新后的 `{config,status}`。
- `POST /api/ai/connections`：`{id?,label,kind,base_url,models:[string],api_key?,rpm?,max_concurrency?,json_mode?,reference_api?}`，返回201 `{connection}`；可选id用于修改已有连接，不能改变类型或删除仍在池中的模型。空密钥保留已有凭据；地址支持局域网 HTTP。响应只含公开元数据、key_configured，不返回凭据。
- `status`：`requests,successes,request_failures,fallbacks,attempts,failures,last_model,available,models`。requests为逻辑池调用数，attempts/ failures为实际候选调用/失败数；models含 `connection_id,model,label,attempts,successes,failures,avg_ms,last_error,cooldown_remaining`。窗口为最近24小时；available=false代表统计暂不可读，不影响路由。
- ordered每次从首个可用候选开始，round_robin线程安全轮换起点，random每次随机排列。故障/无效输出触发下一候选，各失败候选在本轮最多请求一次；忙槽不触发故障冷却，全部忙时有界等待；全部故障或冷却报错，原有外层最多3轮重试保持有界。
- 图像候选保留原提示词、尺寸和参考图顺序，按各候选协议调用，不丢弃参考图；每个候选具有稳定独立幂等键。ComfyUI/mock和显式env provider不使用全局池。图像 `/probe` 在池模式并发读取候选 `/models`，不发送生成请求，不改变路由统计或冷却。

## 10. 设置、模型库与安全（v0.5）

- GET /api/auth/status 返回 {enabled,authenticated,local}，不返回配对码或密钥。
- POST /api/auth/login 接收 {token}，成功设置 HttpOnly、SameSite=strict 的会话 Cookie；POST /api/auth/logout 撤销当前会话。配对码只保存哈希，旋转旧码会撤销旧会话。
- GET /api/security/settings 返回 {settings:{enabled,paired,local,token_configured}}；本机 PATCH /api/security/settings 接收 {enabled}，本机 POST /api/security/token 生成一次性配对码。响应和日志不会包含 API 密钥。
- GET/PATCH /api/ai/features 管理 search、memory 和 prompts，使用 revision 做乐观并发控制。搜索 provider 支持 tavily、brave、searxng；本机管理员设置响应包含 search.api_key，让设置卡片在保存和重开后显示密钥。远端配对设备不可访问这些设置；创作快照和 features.public() 不包含密钥。
- POST `/api/ai/translate` 与 `/api/ai/explain` 接收 `{text,target_language?,context?}`，用于阅读器选区操作；结果只返回当前请求，不写书籍或长期记忆。AI 解释按 Markdown 渲染，复制保留原始 Markdown；翻译仍显示纯文本。翻译服务可通过 GET/PATCH `/api/translation/settings` 选择 `ai`、`bilibili`、`microsoft` 或 `custom`，密钥读取时仅返回 `key_configured`。选择 ai 时复用文本模型池并隐藏独立接口字段；区域字段只用于 Microsoft。
- POST /api/ai/web-search 接收 {query}，返回 {query,results:[{title,url,snippet}]}。结果仅作参考资料并保留来源，搜索关闭或上游失败不会改变原文材料。
- GET/POST/PATCH/DELETE /api/ai/memories 管理全局、书籍和人物范围的 personality、meme、world、note 记忆。列表支持 book_id 和 q 过滤；检索有条数和字符上限，不会自动把网页结果写成永久记忆。
- GET /api/ai/catalog 返回供应商公开元数据和模型能力/价格/任务分配。POST/PATCH/DELETE /api/ai/catalog/models 管理模型库；仍被模型池引用的模型不能删除或改 ID。`GET /vendors/{id}/models` 读取供应商模型列表，`POST /models/{id}/pool` 可幂等加入文本或生图模型池。
- GET/PATCH /api/ai/routing/{kind} 的 models 为有序卡片数组，strategy 支持 ordered、round_robin、random。失败/超时/限流会按配置冷却并自动尝试后备模型；快照包含实际模型与提示词哈希，避免生成结果无法复现。
- GET /api/ai/settings、GET /api/ai/connections、模型探测和生成管理均属于本机管理接口。远端客户端即使已配对，也不能读取或修改连接、密钥、模型池和生成设置。
- 私有 API、媒体和下载统一返回 Cache-Control: private, no-store；服务不信任 X-Forwarded-For，也不接受跨端口、重复 Origin 或 null Origin 的写请求。

## 本地离线与设备同步（v0.4.2）

- `POST /api/sync/device` 接收 `{device_id?,label?}`，返回 `{device:{id,label,created_at,last_seen,last_seq}}`。客户端持久保存设备标识和序号；远端设备绑定当前配对会话，不能冒用其他活跃会话的设备。重新配对可接管已退出或过期会话的原设备；已撤销的标识必须重置后重新注册。
- `GET /api/sync/devices` 和 `DELETE /api/sync/devices/{id}` 仅供本机管理。撤销保留同步日志，并注销该远端设备所属的配对会话；不远程删除设备已经下载的书籍。本机服务标识 `server` 不可撤销。
- `POST /api/sync/push` 接收 `{device_id,operations:[{op_id,seq,entity,entity_id,action,payload,base_revision?}]}`。entity 支持 `progress|bookmark|annotation|memory`，action 为 `upsert|delete`；进度不支持删除。每批 1–200 条，每条 payload 最多 64KB，拒绝未知字段；书籍、章节、段落、偏移和批注引文都按本机原文校验。返回 `{accepted:[{op_id,cursor}],duplicates:[op_id],conflicts:[{op_id,entity,entity_id,reason,revision,...}]}`。相同设备、序号、标识及内容的重试幂等；重用标识或序号提交不同内容返回 409。
- 进度按 `updated_at` 合并，旧进度返回 `server_newer`；percent 始终按已校验位置重算，TXT/EPUB 使用正文字符数，CBZ 使用 `(page+1)/page_count`。批注和长期记忆以 `base_revision` 匹配服务端最后 cursor，旧版本返回 `revision_conflict`。进度、批注冲突另返回 `current_updated_at`；显式选择保留本机时，以 `max(当前时间, current_updated_at+1)` 重提，避免远端时钟超前导致无法解决。书签删除保留 tombstone，旧副本不能复活已删除项；同位置不同标识的书签返回 `duplicate_bookmark` 和 `canonical_id`。冲突不会确认或丢弃本机待同步草稿，需在设置中选择保留版本。
- `GET /api/sync/pull?device_id=...&cursor=0&limit=100` 返回 `{operations,cursor,has_more}`，limit 为 1–200。operation 包含 push 字段及 `cursor,device_id,created_at`；客户端按 cursor 继续拉取直到 `has_more=false`。旧阅读 REST 接口的修改也会投影为 `device_id=server` 的日志。远端 pull 不返回长期记忆，但游标仍跨过过滤的日志。
- `GET /api/sync/snapshot?book_id=...&offset=0&limit=500` 返回 `{books,progress,bookmarks,annotations,cursor,offset,next_offset,has_more}`，book_id 可省略，limit 为 1–1000。各数组按相同 offset 独立分页，阅读实体包含 `sync_revision`。客户端应加载全部分页，记录**第一页 cursor**，随后从该 cursor 再次 pull，避免分页期间发生的新增、修改和删除被跳过；存在待同步操作的实体保留本机版本。
- `GET /api/sync/books/{book_id}/manifest` 返回书籍公开元数据、章节/漫画页/EPUB 图片的受保护资源 URL 和原文件 `source_url`。manifest 与 snapshot 不返回本机文件路径、密钥或模型连接配置。远端 memory push 被拒绝，AI、模型库、搜索、提示词和长期记忆仍仅由本机设置管理。
- 浏览器的 IndexedDB 仅保存书库元数据、显式下载的阅读正文/图片、进度、书签、批注和操作队列。Service Worker 仅缓存应用静态文件，不缓存 `/api/` 或 `/media/` 响应；图片由独立下载流程保存经过类型/大小校验的同源图片字节，不保存请求凭据。Android 从 APK 提供同源静态壳，API 与媒体继续经过配对鉴权；不同服务器 origin 的本机数据相互隔离。
- 首版离线范围为已下载 TXT/EPUB/CBZ 阅读、正文搜索及阅读记录编辑；恢复连接后同步进度、书签和批注。云端生图、联网搜索及未部署在当前设备的文本模型需要相应服务可达，APK 不包含本地大模型。设备撤销控制后续联网访问，不能撤回已下载的离线副本。
