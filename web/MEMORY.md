# web/ MEMORY —— M1/v0.3 前端（web-reader-m1）实测记录

> 2026-11-12 · 归属：web-reader-m1（task-6 M1 / task-10 v0.3）· 唯一契约：docs/API-CONTRACT.md（v0.3 第 8 节）
> 全部为实测值：headless Edge（Chromium 内核）+ 原生 CDP 驱动。
> · M1 记录（1–11 节）：真实后端 127.0.0.1:8783，DATA_DIR=data/_m1_web；3 本书（txt 234 章/15245 段/516256 字、
>   epub 234 章/482081 字、cbz 60 页测试图）。
> · v0.3 记录（13 节）：真实后端 127.0.0.1:8792，DATA_DIR=data/_v3_fe；中性生成的 16 本测试 txt + samples/demo-comic.cbz。
> M0 基线保留在第 12 节；M1 改动以 1–11 节为准，v0.3 增量见第 13 节。

## 1. 文件与职责（全部原生 ESM / 原生 CSS，零依赖、零构建）

| 文件 | 职责 | 行数 | 字节 |
| --- | --- | --- | --- |
| index.html | 应用外壳、Velmira 品牌、书签/笔记/目录工具栏、书库状态点、主题恢复脚本 | 88 | 4,867 |
| styles/main.css | 主题变量、书库（格式徽标/实时状态点/新增高亮）、分页、书签/注释/笔记/抽屉/目录过滤/移动端 | 511 | 24,932 |
| app.js | 路由分发（/ 书库，/?book= 阅读器） | 42 | 1,626 |
| util/core.js | DOM/存储/格式化/网络（+ PATCH/DELETE、fmtTime） | 184 | 6,541 |
| ui/paging.js | **分页内核 + 注释高亮渲染**（拆/包 mark、整章重建） | 473 | 16,865 |
| ui/reader.js | 章节/进度/书签/注释编排、滑动、comic 委托、debug | 734 | 28,126 |
| ui/library.js | 书库列表、格式徽标、TXT/EPUB/CBZ 导入、**v0.3 实时刷新（SSE + 轮询兜底 + 原地 diff）** | 382 | 15,289 |
| ui/panels.js | 目录（+过滤）/搜索/片段/书签/笔记抽屉、主题菜单 | 396 | 15,907 |
| ui/annotations.js | **注释交互层（懒加载）**：选区浮动菜单、高亮小菜单、笔记弹层 | 335 | 12,754 |
| ui/comic.js（web-comic-m1） | CBZ 漫画模式 | 398 | 15,303 |
| styles/comic.css（web-comic-m1） | 漫画模式样式 | 266 | 6,526 |

**JS 总量实测 112,411 B ≈ 109.8 KB（11 个 .js 文件，含 comic.js 15,303 B）< 150KB 硬约束**；
其中本人 7 个文件 97,108 B ≈ 94.8 KB。CSS 31,458 B；HTML 4,867 B。

## 2. 品牌 Velmira（契约 7.1）

- 书库 `<title>` = `Velmira`；brand = `Velmira` + `v0.3` 标签。
- 阅读器 `document.title` = `<书名> · Velmira`（实测格式为「<导入书名> · Velmira」，文档不采集具体书名）。
- 旧产品名已在 web/ 全部代码与样式中清零（grep 无匹配；M0 的 CSS 头注释已同步更新）。
- 视觉沿用 M0 配色/排版，只加徽标与格式徽标；书库 1440×900 无横向溢出。

## 3. 书签（契约 4.9，M0 API + M1 UI）

| 项 | 实测 |
| --- | --- |
| 加书签 | 点 ☆ → POST → API `bookmarks.length 0→1`，图标 `☆→★`，aria-pressed=true |
| 去重 | 同章同段重复 toggle 3 次后 API 仍只有 1 条（按 chapter+para 判重，不重复添加） |
| 取消 | 再点 ★ → DELETE → API 1→0，图标回 ☆ |
| 抽屉 | `.bm-item` 列出「第 1 章 · 第 1 段 + MM-DD HH:mm」，当前段高亮 |
| 跳转 | 从第 3 章打开抽屉点第 1 章书签 → `ch 2→0`、抽屉关闭、URL 同步 |
| 删除 | 抽屉内「删除」→ 列表 2→1，API 1 |
| 刷新 | 加书签后整页重载 → 600ms 后台拉取书签后 ★/aria-pressed=true 恢复 |
| 快捷键 | `B` toggle；书签列表接口失败 → 静默，按钮仍可用（不阻塞阅读） |

## 4. 注释与高亮（契约 7.5 / 7.6，后端 api/annotations.py 真联调）

### 4.1 渲染

- API 预造 2 条（para0 [0,2) 黄；para1 [1,4) 绿 + note）→ 首章渲染 `mark.ann` **2 个**，
  class 分别 `ann ann-yellow`/`ann ann-green`，`data-ann` 与 API id 一致；
  `mark.textContent` = 原文切片；段落 `textContent` 与 `GET /chapters/0` **逐字相同**（只包文本节点，不改内容）。
- 重叠/越界由 paging 的区间合并裁切处理（105 条预造注释 → 97 个 mark，完全覆盖的区间跳过，文本不变）。
- 切章、改字号（19→22→25px）、换主题（day→night）后 `_applyAnnotations()` 整章重建：
  实测 mark 数量与颜色保持（4 条注释在 25px/night 下仍 4 个 mark，night 高亮色变量生效）。
- 切章清空：`__velmira.annotations().length` 在无注释的第 2 章 = 0；切回第 1 章重建 4 个 mark。
- 翻页：同块内翻 3 页（1/8→4/8）mark 数量不变；跨块渲染时 `_renderChunk` 自动按注释重建。

### 4.2 交互

| 场景 | 实测 |
| --- | --- |
| 选中正文 | 浮动菜单 `.ann-float-sel`：4 个颜色圆点 + 笔记 + 复制；取消选区 140ms 后自动隐藏；拖动改选区时菜单跟随（实测 3 次选区菜单 y=104→344→536 且始终在视口内） |
| 建高亮 | 点粉色 → POST 201 → para2 偏移 [2,6)、text=原文对应切片，mark 3 个，API 3 条 |
| 跨段选择 | 从 para3 偏移 5 到 para4 偏移 10 → 收缩为 para3 [5, len=35)，`text`=para3 尾串，API 校验通过 |
| 点击高亮 | `.ann-float-mark`：4 色（当前色高亮圈）+ 笔记 + 删除 |
| 改色 | 点蓝 → PATCH → API color=blue、mark class `ann ann-blue` |
| 改笔记 | 弹层（引用原文 + textarea）→ 保存 PATCH → API note=「示例笔记」，弹层关闭 |
| 删除 | 删除后 mark 4→3、API 4→3 |
| 刷新 | 整页重载后 mark 2 个、颜色/文本/笔记全保留 |
| 复制 | `navigator.clipboard` 无权限时 toast「复制失败（浏览器未授权剪贴板）」，不抛错不阻塞 |
| 落点闪烁 | 笔记抽屉点条目 → 跳章/段并给对应 mark 加 `.ann-flash`（4 次 0.55s 动画） |

### 4.3 笔记抽屉

- 打开才请求 `GET /annotations?limit=100&offset=0`（按 chapter 分组头「第 N 章」）。
- 预造 105 条：首屏 **100 项 + 「加载更多」**；点一次 → 105 项 + 「已加载全部」+ disabled。
- 抽屉内删除：105→104，API 104，`__velmira.annotations()` 104，DOM mark 96（与重叠合并一致）。
- 跨章分组实测：ch0/ch1 各 2 条 → 抽屉 4 项、分组头「第 1 章 / 第 2 章」；
  点第 2 组条目 → 跳 ch1、正文 2 个 mark、命中 mark 闪烁；改字号后 ch1 mark 重建仍 2 个。
- 关闭抽屉即释放列表（`notes=[]`），符合「只驻留当前章注释」的内存约束。

## 5. 触摸滑动翻页（P1，契约 7.6）

- pointer events（touch/pen，鼠标不参与，避免影响文本选择），阈值 **40px** 且 `|dx| > |dy|*1.2`、<900ms。
- CDP `Input.dispatchTouchEvent` 实测：左滑 200px → 页码 0→1；右滑 140px → 1→0；**20px 小滑动不翻页**。
- 滑动后 450ms 内抑制 click，避免「滑动 + 1/3 点击区」双翻页；
- 键盘 ←/→（CDP keyDown/keyUp）与点击右 1/3 区仍分别翻 1 页（回归通过）；长按选区时不翻页。
- `touch-action: pan-y`：横向手势交给翻页，纵向滚动仍归浏览器（垂直降级模式可用）。

## 6. 目录过滤（P1）

- 目录抽屉 234 项一次渲染（打开时才请求 toc），顶部过滤框输入即过滤（40ms debounce）。
- 输入 `100` → 可见 **1 项**「第 100 章（示例章节名）」，meta「匹配 1 / 234 章」；清空 → 234 项恢复。
- 输入「第100章」同样 1 项，点击跳转 `ch=99`、标题/页码/URL 同步、抽屉关闭。

## 7. 书库格式徽标与 EPUB / CBZ 接入

- GET /api/books 每项含 format：书库 3 张卡徽标 **EPUB / TXT / CBZ**；meta 分别
  「234 章 · EPUB 文本」「234 章 · 51.6 万字」「60 页 · 漫画」，侧栏「继续阅读 · 第 33 章 / 第 1 页」。
- 未返回 format 的旧后端按 txt 兼容（`fmtInfo` 兜底），不报错。
- 拖拽/点选接受 .txt/.epub/.cbz/.zip（服务端按字节识别）；提示文案与 accept 同步。
- EPUB：`/?book=b_f244ca&ch=0` 正常阅读 8 页/88 段，`chapters/32` 首段为对应正文（文档不引用原文）；
  document.title 为 Velmira 格式；阅读流程与 txt 完全复用。
- CBZ 委托：reader.js 取到 book 后 `format==='cbz'` → `await import('./comic.js')` → `mountComic({bookId,params,book})`；
  实测 `body.comic-mode`、`#reader-main` 内 **1 个 img**（/media/.../0）、94 DOM 节点、无文本阅读器残留。
- 兜底：CDP `Network.setBlockedURLs(['*comic.js*'])` 模拟模块缺失 → `#reader-main` 显示
  「漫画模块未就绪 / 这本书是 CBZ 漫画，但漫画阅读模块加载失败」+ 返回书架/重试，**不白屏、无 JS 报错**。
- 漫画分支不再静态加载 paging.js（动态 import 仅在文本分支）。CBZ 首屏（web-comic-m1 在 8785 独立复测，5 页 1200×1800 真图）：
  URL 带 `ch=` 时 9 个资源 + index.html = **10 个请求**（/、main.css、app.js、reader.js、core.js、GET book、comic.js、
  comic.css、GET pages、GET media）；URL 不带 ch 时追加 GET /progress = 11 个。**paging.js 已不在 CBZ 首屏**。
  文本分支首屏数量不变（8 资源 + index.html = 9）。
- 注：本轮我数据目录里的 cbz=b_124faa（comic60，60 页）是 backend-m1 分页边界测试产物，页图是 **1px 占位图**，
  所以截图里画面是「小黑块」——不是漫画加载失败；web-comic-m1 已用真实 1200×1800 的 5 页 cbz 复测入口与翻页正常。

## 8. 首屏请求（契约 7.6 ≤ 9）

首章渲染完成瞬间（debug 下 `window.__firstScreen`，不含之后的动态 import）：
```
/styles/main.css, /app.js, /ui/reader.js, /util/core.js, /ui/paging.js,
/api/books/{id}, /api/books/{id}/annotations?chapter=0&limit=200, /api/books/{id}/chapters/0
= 8 个资源请求 + index.html = 9 个首屏请求（达标）
```
首屏之后（setTimeout/idle，非阻塞）：`/ui/annotations.js`（注释交互层）、`/api/books/{id}/bookmarks`、`/api/books/{id}/progress`。
书库页首屏：index.html + main.css + app.js + library.js + core.js + GET /api/books = 6。
painting 前 JS 资源总量：app 1,626 + reader 28,126 + core 6,541 + paging 16,865 ≈ 53.2 KB（未含 12.7KB 懒加载注释层）。

## 9. 内存 / DOM / 存储（?debug=1，CDP HeapProfiler + performance.memory）

| 指标 | 实测 | 结论 |
| --- | --- | --- |
| 切 20 章 heap | 1.4 MB → 1.7 MB（GC 后） | +0.3MB，无累积 |
| DOM 节点 | 普通章 131–159；97 个 mark 时 256 | 远低于 <1500 |
| 注释驻留 | 仅当前章数组（切到无注释章 = 0） | 契约「单章注释才驻留」 |
| 笔记抽屉 | 关闭即清空数组 | 不长期持有全书注释 |
| localStorage | `velmira.r1` 79 B，字段仅 {book,chapter,para,offset,theme,fontSize} | <2KB，M1 未新增持久字段 |

切章 20 次（含 fetch + 渲染 + 测量 + 注释重建）无 JS 错误；每章只有 1 个 h2，无旧章残留。

## 10. E2E 汇总（真实后端 + CDP，0 页面报错）

- 全流程捕获 `window.onerror / unhandledrejection / Runtime.exceptionThrown / Log.entryAdded(error)`：
  书库、阅读、书签、注释全流程 **0 条**（使用 API 不存在的 mock 阶段除外，本 M1 全部真联调）。
- 覆盖：书库徽标、txt/epub 阅读、cbz 委托与兜底、书签增删/抽屉/跳转/刷新、注释 4 色渲染/改色/笔记/删除/
  重建/笔记抽屉分页/跨段收缩、目录过滤、滑动/键盘/点击区、字号/主题、20 章内存。
- 截图（临时目录，测完清理）：`%TEMP%\velmira_m1_web\shots\01..09`（书库/EPUB/漫画/兜底/夜间高亮/选区菜单/
  高亮菜单/笔记抽屉/移动端 390×844）。
- 移动端 390×844：`scrollWidth=clientWidth=390`（无横向溢出），工具栏 8 个按钮宽 278px 全部可点，
  mark 正常渲染，页数随字号变化（19px→16 页）。

## 11. 已知缺口 / 后续建议

1. **P2 双页模式未实现**（契约 7.6 明确可选）：时间优先保质量，单页模式无回归；后续可在 paging.js 加
   `column-count:2` 分支 + 页宽 stride×2，默认关闭。
2. **reader.js 734 行**超出契约「每模块 < 400 行」软约束（M0 已超：487 行）。功能与体积正常，
   建议 M2 把书签、debug、comic 委托再拆为懒加载模块。
3. **复制依赖剪贴板权限**：headless + 未授权时走 toast 降级（不抛错）；真实浏览器用户手势下 Chromium 授权正常。
4. **垂直滚动降级未本机实测**（Chromium 列布局正常路径）；mark 渲染在 `.mode-scroll` 下同样走 _applyAnnotations，逻辑一致。
5. **重叠注释**按「先到先得 + 裁切」合并渲染，DOM mark 数可能少于 API 条数（105→97 实测），文本与坐标不丢；
   如需逐条独立显示需改区间切分算法（M2 视需求）。
6. **CBZ 首屏 10–11 个请求**（带 ch 时 9 资源 + html = 10；不带 ch 加 GET progress = 11，含 comic.js/comic.css/media），
   漫画模式本身无法压到 ≤9（契约该条按文本阅读口径）；已通过「paging.js 仅文本分支动态加载」减 1 个，paging.js 不再进 CBZ 首屏。
   漫画侧由 web-comic-m1 验收（单 img/naturalWidth=1200/94 DOM/0 报错，8785 真实入口复测通过）。
7. 书签 API 新增时 201 返回体为单个 bookmark；UI 已兼容。若后端将来返回 409 重复书签，前端会 toast 失败，
   可改为幂等复用（当前 toggle 已避免同段重复）。
8. 进度条百分比在「URL 直接带 ch= 且本地无记录」时首帧 0%，等 1.2s 防抖 PUT 回填（M0 遗留，未变）。

## 12. M0 基线（原 web-reader 记录，保留备查）

- 分页内核：CSS 多列 + translateX，只改 transform；第 1 章 88 段 = 8 页，翻页 20 次 avg 0.075ms；
  切章 20 次 avg 32.7ms；大章按 ≤8000 字切块；列布局失效降级垂直滚动。
- M0 E2E：书库/首章/翻页/目录 234 项/刷新恢复/搜索示例关键词/片段/主题/字号/移动端 390×844，0 报错。
- M0 JS 总量 52,159 B（6 文件）< 120KB；localStorage 80 B；首屏 8 请求。
- M1 回归验证（本轮）：搜索 20 命中 + snippet、目录过滤后跳 ch99、字号 4 档/主题 3 档、键盘/点击区翻页、
  移动端无横向溢出，均通过；reader.js/panels.js 重构未引入回归。

## 13. v0.3 书库实时刷新 + 内容脱敏（task-10）

### 13.1 实现（契约 8.2 / 8.3）

- `ui/library.js`：挂载时 `new EventSource('/api/events')`；收到 `books_changed` → 100ms 去抖后
  `GET /api/books`，**原地 diff 更新**（按 `data-id` 复用 `<a.book-card>` 节点，只改变化字段；
  首次渲染后的新增项加 `.is-new` 淡入高亮 1.7s 一次；更新前后保存/恢复 `#library-view.scrollTop`）。
- 兜底：EventSource 不可用（`typeof EventSource === 'undefined'`）/ 构造失败 / 404 / 断线 →
  **5s 轮询** `GET /api/books` 比指纹（id/title/format/章/页/字数/进度）后更新；轮询期间每周期尝试一次 SSE 重连，
  成功 `open` 后停轮询。全程 **1 个 EventSource + 1 个轮询定时器**（重连复用轮询周期，不另开定时器）。
- 生命周期：`pagehide` / `beforeunload` → `close()` + `clearInterval`；bfcache `pageshow(persisted)` 时重新订阅并补拉。
- 状态点 `#lib-live`（书架标题旁）：连接中 / 实时 / 轮询 / 离线 四态（颜色 + 文案 + title）。
- `index.html` 空书库文案中性：「书架还是空的 · 把 TXT / EPUB / CBZ 拖到上方导入区即可开始」，不预置任何书/片段。
- `styles/main.css`：状态点四态配色（含夜间主题）、`.book-card.is-new` 新增淡入高亮动画。

### 13.2 实测（Edge headless + CDP，8792 / DATA_DIR=data/_v3_fe，中性生成 txt ×16 + samples/demo-comic.cbz）

| 场景 | 实测 |
| --- | --- |
| 空书库首屏 | 0 张卡、只显示中性引导文案；SSE 连接后状态「实时」，服务端 `/api/events/status.subscribers=1` |
| 另一路 POST 导入（首次） | 新卡 **200ms** 出现（<1s），页面未 reload（window 标记保持），scrollTop 0，`.book-card.is-new=1` |
| 批量导入 11 本后滚动 | 列表 12→13 张；先滚到 scrollTop=320，导入后 scrollTop **仍为 320**，更新 200ms；DOM 节点复用（同一卡片元素引用相等，reused=true） |
| 同文件再导入（dedupe） | POST 200 `deduped:true`，卡片数不变、id 唯一（无重复），不广播 |
| force 重建 | POST 201 触发广播，卡片数不变（同 id 复用），无闪烁 |
| 进入阅读器 | 新页面无 `/api/events` 资源；服务端 subscribers **1→0**（EventSource 已释放）；标题 `demo-text-13 · Velmira` |
| 返回书库 | 重新订阅，状态「实时」，subscribers 1，列表恢复 |
| 禁用 EventSource | 状态「轮询」，另一路导入后 **2800ms** 自动出现（<5s），subscribers 0（未建立 SSE） |
| 阻断 events+books | **4.7s** 进入「离线」（红点 + 文案），列表为空且提示加载失败（无预置内容） |
| 恢复网络 | **4.6s** 回到「实时」（轮询拉回 16 本 + SSE 重连成功，subscribers=1） |
| 出错率 | 全流程 `window.onerror/unhandledrejection/Runtime.exceptionThrown` **0 条** |

- 首屏（书库空/正常）：index.html + main.css + app.js + library.js + core.js + GET /api/books = **6 个请求**；
  `/api/events` 是 EventSource 长连接，不出现在 `performance.getEntriesByType('resource')`，
  验收以服务端 `/api/events/status.subscribers` 计数为准。
- 内存：任意时刻 1 EventSource + 1 轮询定时器（+SSE 事件 100ms 去抖定时器，自动清除）。

### 13.3 内容脱敏（契约 8.1）

- `grep` 全 web/：书名/角色名/原文句子 **0 匹配**（含此前已知的角色名词条）。
- `ui/panels.js` 搜索占位改为中性「输入关键词，如「示例」」。
- `web/MEMORY.md` 内 M1 实测引用全部改为通用描述（`<导入书名>`、示例笔记、示例章节名、示例关键词），
  数字/结论保留；不再记录具体小说标题、角色名或原文句子。
- 书库/阅读器不预置任何书、注释或书签；演示数据只来自用户导入或仓库 `samples/`（中性占位）。

### 13.4 M1 前端回归（同一次 8792 环境）

| 项 | 结果 |
| --- | --- |
| 阅读器加载 + 标题 | `demo-text-17 · Velmira`、1/1 页、3 段、debug 钩子正常 |
| 注释高亮 | API 预造 1 条 → `mark.ann`=1；刷新/切字号不变 |
| 书签 | 点 ☆ → API 1 条 + ★；再点 → 0（去重/切换正常） |
| 目录过滤 | 8 章 → 过滤「第8章」→ 可见 1 项，过滤框存在 |
| 触摸滑动 | CDP touch 左滑：ch0→ch1（跨章翻页生效） |
| CBZ 漫画入口 | body.comic-mode、1 个 img、97 DOM、无 fallback |
| 页面报错 | 0 条 |

### 13.5 体积 / 行数（v0.3 实测）

- JS 总量 **112,411 B ≈ 109.8 KB**（11 个 .js；本人 97,108 B，comic.js 15,303 B）< 150KB。
- CSS 31,458 B；HTML 4,867 B；`ui/library.js` 382 行；`node --check` 全部 .js 通过（8/8）。

### 13.6 已知缺口 / 备注

1. 契约/后端当前**没有「删除书」接口**，因此「删除后再验证」无法真跑；`applyBooks` 的节点移除分支已实现，
   但未用真实删除事件验证（等后端提供 DELETE /api/books/{id} 后可补测）。
2. SSE 断开由浏览器原生重连 + 轮询周期内重建双保险；离线→恢复实测 4.6s。
3. 状态点在 SSE 短暂抖动期间会显示「轮询」（兜底语义，不是错误）。
4. `samples/demo-text.txt` 不在 web/ 所有权内，本次未创建；前端对其无硬依赖（用 %TEMP% 中性文本测）。
5. bfcache 恢复逻辑已加（pageshow persisted），headless 未实测 bfcache 场景。
6. 本次测试服务 8792、Edge 9434、DATA_DIR=data/_v3_fe、%TEMP%\velmira_v3 已全部清理。
