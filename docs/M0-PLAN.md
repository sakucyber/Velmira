# M0 实施方案（思路）

> 2026-11-12 · 目标：把 docs 里的设计落成「能跑起来的最小闭环」。
> 契约文件：docs/API-CONTRACT.md（前后端唯一真相，动手前先读）

---

## 0. 一句话目标

打开网页 → 导入 TXT → 流畅分页阅读 → 有目录/主题/进度恢复 → 能搜索原文 → 能看到本章候选分镜片段。
**不做**：出图、ComfyUI、人物卡、LLM 调用（M2/M3 再接）。

---

## 1. 五个关键取舍（先说为什么）

| # | 决策 | 理由 |
| --- | --- | --- |
| 1 | **M0 不引入 foliate-js**，自研约 200 行分页内核 | foliate-js 自带 iframe + blob URL + 多模块状态，内存与调试成本都高，且其 README 自述 API 不稳定。M0 先把「能读、读得快、读得省」做扎实，M1 接 EPUB/CBZ 时再按 section/view 契约 vendor 进来，适配层只依赖 open/relocate/sections 三点 |
| 2 | **无构建**：原生 ES Module + 原生 CSS，不用 React/Vite | 零 node_modules、零打包体积、首屏 JS < 120KB，启动时间最省 |
| 3 | 后端 **FastAPI + SQLite(WAL)**，段落以 JSON 存 chapters 表 | 15,265 段不建独立表：单章 30 段、4KB，直接 JSON 反序列化最快，省一次 JOIN 也省掉 15k 行表维护 |
| 4 | 上传走 **raw octet-stream**，不用 multipart | 免 python-multipart 依赖，curl/web 都是一行 |
| 5 | 检索用 **FTS5 trigram，按需增量建索引** | 中文两字人名（主角A）能命中子串；不阻塞导入 |

---

## 2. 分页内核（自研，约 200 行）

原理：CSS 多列 + translateX。

~~~text
.reader-viewport  overflow:hidden; height: 100%
.reader-column    columns:1; column-width: 100%; column-fill: auto; height: 100%; width: 100%
                  → 内容按视口高度切成一列一列排到右边，scrollWidth = 页数 × 视口宽
翻页 = transform: translateX(-page * viewportWidth)
~~~

要点：

1. **一次测量**：章加载后读一次 scrollWidth，算出总页数并缓存；resize 时用 Locator 反推当前阅读位置再重排，不能丢位置。
2. **降级**：若检测到列布局不生效（scrollWidth <= 视口宽）或 Safari 下溢出异常，自动切垂直滚动模式，功能不受影响。
3. **大章切块**：单章 < 2 千字是常态，但极端章可能 9 万字 → 前端把章节按段落切成 ≤ 8000 字的 chunk，一次只渲染一个 chunk，翻到边界自动进下一块。这样 DOM 节点数恒定，不随章节长度爆炸。
4. **定位**：Locator = {chapter, para, offset}（chunk 由 para 推出，不单独持久化），与 docs 的定位体系一致。

---

## 3. UI 内存约束（硬性指标，写进验收）

| 项 | 预算 | 机制 |
| --- | --- | --- |
| 前端 JS 总量 | < 120KB（未压缩） | 无框架、无 polyfill、无第三方库 |
| 首屏请求数 | ≤ 8 | 单 index.html + 1 css + 少量 ESM |
| 单章 DOM 节点 | < 1500 | 只渲染当前 chunk；切章销毁旧 DOM（innerHTML 替换，不 retain 引用） |
| 全书文本 | 前端 0 字节常驻 | 按章拉取，读完即弃；localStorage 只存 < 2KB 的 locator/设置 |
| 图片缓存 | 0 张（M0 无图） | 后续分镜图统一 lazy + LRU 24 + objectURL revoke |
| 切 20 章后 JS heap | 回落基线 +5MB 内 | 无全局数组累积、无事件监听泄漏（事件委托 + AbortController） |
| 滚动/翻页 | 不触发强制重排 | 测量值缓存，翻页只改 transform，不读 layout |

自检方式：Chrome DevTools Memory 面板 + performance.memory（控制台打印），B 负责在 README 记录实测数值。

---

## 4. 目录与文件所有权（防冲突）

~~~text
server/
  __init__.py, app/__init__.py            A
  app/main.py                             A  app 工厂、路由挂载、静态托管
  app/db.py  app/schema.sql               A  连接、初始化、迁移
  app/importer.py                         A  编码探测/章节切分/段落索引
  app/api/books.py                        A  books/chapters/toc/progress/bookmarks
  app/api/segments.py                     C  分段 API
  app/api/search.py                       C  检索 API
  app/pipeline/{segmenter,features,scoring,search_index}.py   C
web/
  index.html  app.js                      B
  ui/{reader,paging,toc,theme,settings,segments,search,library}.js  B
  styles/*.css                            B
  util/*.js                               B
tests/                                    D
scripts/run.ps1                           Lead
docs/M0-PLAN.md  docs/API-CONTRACT.md     Lead
~~~

跨层唯一耦合点 = 契约文档。A 的 main.py 用 try/except 可选挂载 C 的 router（C 未就绪时也能跑）。

---

## 5. 交付顺序（3 个写手并行 + 1 个验证）

~~~text
Lead: 契约 + run.ps1
  ├─ A backend-core : 导入 + API + 静态托管      （server/）
  ├─ B web-reader   : 分页阅读器 + 目录/主题/进度 （web/）
  └─ C pipeline     : 分段打分 + FTS 检索        （server/app/pipeline + api/segments|search）
        └─ D verifier（A/B/C 完成后）：tests + 实测数据 + 内存/性能报告
~~~

---

## 6. 验收（可执行）

1. 导入 [book/...txt] 1.48MB / 51.6 万字 / 234 章，接口返回 import_ms < 3000。
2. 打开书首章，翻页无卡顿；刷新后回到原段落（para 完全一致）。
3. 目录 234 项可跳转；主题/字号切换不重载章节。
4. 搜索「主角A」：**温查 < 200ms**（实测 8–11ms）；冷查含一次性建索引实测 237ms（预算 <2s 即可，不算 FAIL）。
5. 本章候选片段列表可点，跳到原文并高亮。
6. 390×844 视口无横向滚动条。
7. 前端 JS < 120KB；切 20 章后 heap 回落基线 +5MB 内（B 自测 + D 复核）。
7.1 段落/标定数字以「干净剥离书头与简介块」后的实测为准，允许 ≤0.1% 偏差并注明原因。
8. 冷启动（无 data/）能自动建库并完成一次导入。

---

## 7. 风险与回退

| 风险 | 回退 |
| --- | --- |
| CSS 多列分页在某浏览器异常 | 自动降级为垂直滚动（同一套 Locator，不影响其它功能） |
| FastAPI 版本差异 | 只用 0.141 已有特性（StaticFiles、APIRouter），不引新依赖 |
| FTS5 trigram 不可用 | 降级 LIKE 查询，并把 index_state 标记为 degraded |
| 并列写文件冲突 | 文件所有权上表已切死；任何人不得改别人目录（Lead 除外） |
| 时间不够 | 砍 bookmarks 与分镜片段面板，保 1–5 验收项 |

---

## 8. 里程碑推进

M0（本次）→ M1 阅读体验完善（书签/注释/EPUB+CBZ/foliate-js 接入）→ M2 分段 LLM + 人物卡 → M3 出图一致性 → M4 拼页与导出。

---

## 9. 实施结果（2026-11-12 完成，Lead 复核）

| 交付 | 状态 | 实测 |
| --- | --- | --- |
| server/（导入 + API + 静态托管） | 完成 | 导入 1.48MB/234 章：**27ms**；15,245 段；encoding=utf-8（无 BOM） |
| web/（无构建分页阅读器） | 完成 | JS **51.5KB**、首屏 8 请求、单章 DOM ≤176、切 20 章 heap +0.6MB、翻页 0.075ms |
| 分段 + FTS5 检索 | 完成 | 475 段 / 选 234 / picked 214,167；搜索温查 8–11ms、冷查 237ms |
| 进度 | 完成 | 末段 percent=1.0；刷新恢复到同一段 |
| 自动化回归 | 完成 | scripts/verify.ps1：41 项 API 断言全 PASS + 37 单测 OK + 6 个 JS 语法检查 |
| 独立验收 | 完成 | tests/REPORT.md：A–H 全 PASS，0 P0 / 0 P1 / 4 P2（已修 2 条） |

截图见 docs/screenshots/；一键回归：powershell -NoProfile -File scripts/verify.ps1。
