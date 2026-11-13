# M1 实施方案（思路）

> 2026-11-12 · 项目正式命名 **Velmira**
> 前置：M0 已完成并通过独立验收（导入/分页阅读/进度/分段/检索）
> 契约：docs/API-CONTRACT.md **v0.2 第 7 节**（M1 增量，唯一真相）

---

## 0. M1 要交付什么

一句话：**从「能读 txt」变成「能读 txt/epub/cbz、能标能记、手机能滑」**。

| 优先级 | 能力 | 说明 |
| --- | --- | --- |
| P0 | 品牌改名 Velmira | UI/标题/健康检查/文档统一；版本 0.3.0 |
| P0 | EPUB 导入 | 解析 spine → 章节正文，复用现有阅读器；纯文本（不抽图） |
| P0 | CBZ 漫画导入 + 漫画阅读模式 | 图片按页阅读、懒加载、进度、键盘/触摸翻页；内存只留 1 张图 |
| P0 | 书签 UI | 加/删/列表/跳转（M0 已有 API，本次只做界面） |
| P0 | 注释与高亮 | 选中文字 → 4 色高亮 / 写笔记 / 复制；笔记抽屉；跳转高亮 |
| P1 | 触摸滑动翻页 | 阈值 40px；不破坏键盘与左右 1/3 点击区 |
| P1 | 目录过滤 | 234 项目录加搜索框，输入即过滤 |
| P2 | 双页模式 | ≥1024px 可切换；做不好可放弃，不得破坏单页模式 |
| P2 | 导入进度/格式徽标 | 书库显示 TXT/EPUB/CBZ 徽标与页数 |

**不做**：出图、LLM、人物卡、注释导出/分享、多用户。

---

## 1. 三个关键取舍

| # | 决策 | 理由 |
| --- | --- | --- |
| 1 | EPUB **只取文本**，不渲染内嵌样式/图片 | 读者体验统一（复用同一套排版与高亮坐标），EPUB 样式是阅读器最大的兼容坑；封面/插图留 M2 |
| 2 | CBZ 用**单 img 元素换 src**，不预加载、不用 canvas/objectURL | 漫画原图 2–4MB/页，12 页预加载就上百 MB；采用单 img + HTTP 缓存（immutable）方案，翻页仍 <100ms（本地） |
| 3 | 注释以 **Locator 坐标（chapter/para/start/end）** 存储，不存 DOM 节点 | 与 M0 定位体系同一套坐标，切字号/双页/重排后仍能重建高亮；文本内容冗余存一份用于校验 |

---

## 2. 分工与文件所有权

~~~text
server/                                   backend-m1
  app/importer.py（扩展为分发器）
  app/importer_epub.py, importer_cbz.py   新增
  app/media.py                            新增（/media 静态图）
  app/api/pages.py, api/annotations.py    新增路由
  app/api/books.py, schema.sql, config.py（版本/健康/格式/进度页口径）
web/                                      web-reader-m1
  index.html（品牌）、styles/main.css（书签/高亮/目录过滤/双页）
  ui/reader.js（书签、注释、滑动、comic 委托入口）
  ui/panels.js（书签/笔记抽屉、目录过滤）、ui/library.js（格式徽标）
  ui/paging.js（仅在必要时扩展双页）
web/ui/comic.js, web/styles/comic.css     web-comic-m1（只这两个文件）
tests/, scripts/verify.ps1                verifier（M1 后新开任务）
docs/, README, scripts/run.ps1            Lead
~~~

跨模块唯一接口：契约 7.6 的 `mountComic({ bookId, params, book })`。

---

## 3. 分页与内存（漫画模式）

~~~text
comic 容器
  └─ <img id="comic-img">        同一元素换 src（1 张图驻留）
  ├─ 顶部：书名/页码/返回
  ├─ 底部：百分比进度条
  └─ 手势：← → / 空格 / 左右点击区 / 触摸滑动（40px）
~~~

- 页面元数据用 `GET /pages?from=&limit=12` 懒拉，只保留当前页 ±1 的元数据
- 高清图走 `/media/{book}/{idx}`（immutable 缓存），失败显示重试按钮
- 适用模式：适宽 / 适页（默认适页），记忆到 localStorage
- 进度：PUT /progress `{chapter: page, para: 0}`，服务端按页算 percent

## 4. 注释与高亮的实现要点

1. 选区 → Range → 最近 `[data-p]` 段落 → 段内字符偏移（跨段落选择一律收缩到单段，降低复杂度）
2. 渲染：遍历段落文本节点，按偏移切分，命中区间用 `<mark class="ann ann-<color>" data-ann="<id>">` 包裹
3. 切章/改字号/换主题后**整章重建**高亮（从注释列表），不做增量 diff
4. 点击高亮 → 弹出小菜单（改色 / 改笔记 / 删除）
5. 单章注释数量上限 200（前端），全书列表按章分页拉取

## 5. 验收（M1）

| # | 项 | 标准 |
| --- | --- | --- |
| 1 | 品牌 | 页面标题、书库、阅读器 meta、健康检查均为 Velmira；无旧名残留 |
| 2 | EPUB | 导入本书 epub：spine 章节数 ≥200、段落非空、chapters/32 首段正确、阅读器可读 |
| 3 | CBZ | 3–5 页测试 cbz：导入成功、/pages 与 /media 正确、漫画模式可翻页、进度保存、单 img |
| 4 | 书签 | 加/删/列表/跳转；刷新后仍在；重复点击同一段不重复添加 |
| 5 | 注释 | 4 色高亮渲染、跨刷新/切章仍在、笔记可编辑、删除生效、越界参数 400 |
| 6 | 交互 | 触摸滑动（CDP 模拟）、键盘、点击区均不回归；目录过滤可用 |
| 7 | 内存 | JS 总量 <230KB（v0.4 当前预算；M1 原始 150KB 预算已随功能扩展调整）；漫画模式 DOM <300 节点、只有 1 个 img；切 20 章 heap 回落 |
| 8 | 回归 | scripts/verify.ps1 M0 41 项断言 + 37 单测全绿（版本号断言更新为 0.3.0） |

## 6. 风险

| 风险 | 回退 |
| --- | --- |
| EPUB 章节结构与网文不一致（10 章合并等） | 按 spine 一项 = 一章；标题缺失时用「第N部分」兜底，验收只要求可读 |
| CBZ 图片超大导致首屏慢 | 首屏显示占位与进度，图片 onload 后再隐藏；不做缩略图（需另建缩略图管线） |
| 双页模式与分页内核冲突 | P2 可放弃，只保留单页 |
| web-reader 与 web-comic 同时改 index.html | comic 不得改 index.html；只动态创建 DOM + comic.css |

---

## 7. 实施结果（2026-11-12 完成，Lead 复核）

| 交付 | 状态 | 实测 |
| --- | --- | --- |
| 品牌 Velmira / v0.3.0 | 完成 | health `{name:"Velmira", version:"0.3.0"}`；代码/README 旧名 0 处 |
| EPUB 导入 | 完成 | 234 章 / 15,245 段 / 482,081 字（与同名 TXT 逐数值一致），导入 ~130ms |
| CBZ 导入 + 漫画模式 | 完成 | page_count 正确、/media 与原图逐字节一致；单 img、DOM 94、heap ~0.7MB、翻页 0–4ms |
| 书签 | 完成 | 加/删/抽屉/跳转/刷新恢复；前端同段去重（API 无唯一约束） |
| 注释高亮 | 完成 | 4 色、笔记、6 类非法参数 400、重启持久化、切章/改字号/换主题后重建 |
| 触摸滑动 / 目录过滤 | 完成 | 40px 阈值（39 不动/40 翻页）、234 项目录过滤「100」→1 项 |
| 回归 | 完成 | `scripts/verify.ps1` 41/41；`scripts/verify_m1.ps1` 41 + 34 + 3 + CDP 22 全 PASS，37 单测 OK |
| 旧库升级 | 完成 | M0 库 init_db×2 幂等、新表齐、旧书可读、原库 SHA256 不变 |

自动化入口：`scripts/verify_m1.ps1`（冷启动隔离 + 品牌/API/CDP/单测/旧库升级 + 清理）。

---

## 8. v0.3 增量：实时书库与内容脱敏（用户追加要求）

| 项 | 实现 | 实测 |
| --- | --- | --- |
| 书库实时更新 | 后端 GET /api/events（SSE，books_changed + 15s 心跳）；前端书库页订阅后原地 diff 更新，保留滚动位置；SSE 不可用退化为 5s 轮询；四态状态点 | 另一路导入后新卡 200ms 出现、页面未 reload、DOM 节点复用；dedupe 不重复；禁用 SSE 时 2.8s 内出现 |
| 内容脱敏 | 移除产品界面/README/文档/截图/演示数据中的具体小说内容；示例数据改为中性占位（samples/demo-text.txt、samples/demo-comic.cbz）；测试夹具运行时解析 book/ 第一本，断言改结构性 | 8-gram 审计（除 book/ 与 data/）仅剩日期误报；产品面 token 0 命中；回归 41+34+3+CDP22 全 PASS |
| 契约 | docs/API-CONTRACT.md 升级 v0.3 第 8 节（SSE + 脱敏硬性规则） | — |
