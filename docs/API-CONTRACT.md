# API 契约 v0.4（Velmira · 前后端唯一真相）

> 所有 Agent 动手前必须先读本文件。字段名、路径、状态码以本文为准，任何一方不得单方面改动。
> **v0.2 = v0.1 + 第 7 节 M1 增量**（EPUB/CBZ 导入、漫画分页媒体、注释高亮、品牌/版本）。
> **v0.4 = v0.3 + 第 9 节**（批注预览与统一版本）。v0.3 = v0.2 + 第 8 节；冲突时以较新章节为准。
> 端口：127.0.0.1:8765（测试用 8799 或 --port 随机，数据目录互相隔离）
> 索引约定：章节 idx **0-based**（前端展示 +1）；段落下标 0-based。

---

## 1. 通用约定

- 全部 JSON 使用 UTF-8，Content-Type: application/json; charset=utf-8
- 成功：2xx + 直接返回数据对象；创建：201
- 失败：{"error": {"code": "snake_case", "message": "人类可读"}}，配 400/404/409/500
- 无鉴权、无 CORS（同源静态托管）
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
- `server/app/config.py: VERSION = "0.4.0"`（当前版本，见第 9 节）
- GET /api/health 变为：
~~~json
{"ok": true, "name": "Velmira", "version": "0.4.0", "data_dir": "..."}
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
- **epub**：解析 container.xml → OPF → spine 顺序 → 每个 XHTML 抽正文为段落（剥标签、去脚本样式、合并连续空行），生成 chapters；`encoding="utf-8"`；title/author 取 OPF 的 dc:title/dc:creator（缺失回退文件名）；不支持内嵌图片（仅文本，报告中注明）
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
  - 前端 JS 总量 < 150KB（M1 预算，含 comic.js）
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

- 当前 `GET /api/health` 返回 `name="Velmira"`、`version="0.4.0"`；书库标识为 v0.4，APK 版本为 0.4.0。
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
- 自动抽取幂等键为 `(book_id, segment_id, prompt_version, sha256(segment_text + prompt_version))`；provider 输出非法时只将该片段标为 `needs_review`，不会中断整批。
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

- `GET/PATCH /config`：图像 provider 配置；`provider=comfyui|mock`，包含 `base_url/unet/clip/vae/edit_lora/width/height/steps/cfg/sampler/scheduler/seed/style/negative/face_threshold/max_attempts/timeout/face_python/face_models`。宽高须为 16 的倍数；最多 3 次尝试；默认人脸阈值 0.55。校验器路径为空时使用环境变量或本机默认值。
- `POST /probe`：检查节点和模型，返回 `{ok,provider,missing,models?,reference_mode}`。Mock 只验证流程，不代表人物一致性通过。
- `GET /forms`：本书人物形态，含版本、冻结身份及 `refs[]`。基础形态从 M2 人物卡同步；修改基础身份沿用 M2 编辑/解冻/冻结接口。
- `POST /forms`：`{character_id,form_key,label,card,start_ch?,start_para?,reversible?}` 新增形态；生效锚点为从 0 开始的章节、段落，重复锚点拒绝。`PATCH /forms/{id}` 编辑未冻结的派生形态，`POST /forms/{id}/actions/freeze|unfreeze` 冻结/解冻（解冻须 `confirm=true`，升版本并标旧结果过期）。
- `POST /forms/{id}/refs`：原始图片字节，最大 16 MB，转换为本地 PNG，默认未确认。`PATCH /refs/{id}` `{accepted:boolean}` 人工确认/撤销；卡版本不一致时拒绝确认，撤销使使用该参考图的任务过期。
- `POST /jobs`：`{kind?:panel|reference,form_id?,character_id?,scene_id?,shot?,action?,seed?,flashback?,para?,request_id?}`，201 返回 `{job}`。单人格必须属于所选 M2 场景，身份须冻结，参考图须已确认且版本一致。多人场景拒绝并要求拆格。显式旧形态须标记闪回，未生效形态始终拒绝。`request_id` 为书内幂等键，重复键搭配不同快照返回 409。
- `GET /jobs?limit=30` 和 `GET /jobs/{id}`：返回配置快照、prompt、config hash、每次尝试的 seed/workflow/prompt id/score/图片 URL。
- `POST /jobs/{id}/cancel|retry|reroll|approve|reference`：取消、续查失败任务、换种子新建任务、人工确认分镜、存为参考图候选。`reroll` 可传 `{seed}`；`reference` 创建的候选仍须确认。
- `GET /assets/{id}`：本书内图片。拒绝跨书访问，不接受任意文件路径。

任务状态：`queued → running → done|needs_review|error|stale|cancelled`。仅单人脸测量分数达到阈值才标 `done`；无人脸、多人脸或校验器不可用都进入 `needs_review`，人工确认后为独立状态 `approved`，不虚构分数。参考图候选始终需要人工确认。进程重启将 `running` 恢复入队，有 prompt id 时续查原任务；ComfyUI 提交前保存 UUID，避免超时后无记录地重复提交。部署使用一个后端进程和一个串行生成队列。
