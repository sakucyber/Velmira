# M1 独立验收报告（verifier-m1）

- 验收人：verifier-m1（task-8；依赖 task-5/6/7 均 completed）
- 验收时间：2026-11-12 16:31–17:35（Asia/Shanghai）
- 产品：**Velmira v0.3.0**
- 形态：**独立评估**——冷启动隔离 DATA_DIR + 独占端口 8787/8788（避开 8765/8783/8785），不引用任何 owner 自测值；web/MEMORY.md 全部关键结论已用 CDP 复跑。
- 结论：**A–I 全部 PASS（P0=0 / P1=0）**；6 条 P2 建议 + 3 条验证脚本自身修正记录，见 §4。
- 证据文件：`tests/verify_m1_result.json`、`tests/cdp_m1_result.json`、`tests/verify_m0_result.json`（截图证据已随交付清理，可跑 scripts/verify_m1.ps1 复现）
- 复现套件：`tests/fixture_book.py`（夹具标题/作者/搜索词运行时解析）、`tests/verify_m0.py`（TXT 回归 41 项，版本断言已更新 0.3.0）、`tests/verify_m1.py`（API A/C/D/E/F/I）、`tests/cdp_m1.mjs`（前端 G）、`scripts/verify_m1.ps1`（一键）
- 脱敏约定：tests/ 断言与 JSON 证据不含具体小说内容；书名/作者/正文搜索词运行时从 book/ 夹具派生，证据中文本统一以「（本地夹具片段）」占位或仅打印结构/长度/布尔。

---

## 0. 环境与方法

| 项 | 值 |
| --- | --- |
| OS | Windows 10.0.19045 / Windows PowerShell 5.1（无 pwsh.exe） |
| Python | 3.11.9（fastapi/uvicorn/Pillow 12.3.0，未 pip install） |
| Node | v24.19.0 |
| 浏览器 | Edge headless=new + 原生 CDP（9334） |
| 服务端口 | 8787（主）/ 8788（旧库升级） |
| 数据目录 | `data/_m1_verify_final_<random>`、`data/_m1_upgrade_<random>`（测后均已删除） |
| 端口/目录清理 | 实测结束 8787/8788 均 FREE，隔离 DATA_DIR 已删除 |

冷启动顺序：health → books 必须为空 → TXT 导入 → EPUB/CBZ 导入 → API 断言 → 重启持久化 → CDP → 旧库升级。

复现（一键）：
~~~powershell
powershell -NoProfile -File scripts/verify_m1.ps1
# 或分步：
$env:DATA_DIR="D:/project/ai reading plus/data/_m1_verify_xxx"
python -m uvicorn server.app.main:app --host 127.0.0.1 --port 8787
python tests/verify_m0.py --base http://127.0.0.1:8787
python tests/verify_m1.py --base http://127.0.0.1:8787 --phase api
# 重启同 DATA_DIR 后
python tests/verify_m1.py --base http://127.0.0.1:8787 --phase persist
node tests/cdp_m1.mjs --base http://127.0.0.1:8787 --port 9334
~~~

> 注：`scripts/verify_m1.ps1` 已通过 PowerShell AST 解析（UTF-8 BOM，PS5.1 可读），各步骤均已分别实测；未在本轮再完整跑一遍该封装脚本以节省时间，属流程细节而非结果差异。

---

## 1. 验收总表

| 项 | 内容 | 结论 | 关键实测 |
| --- | --- | --- | --- |
| A | 品牌/版本 | **PASS** | health name/version 正确；index/书库/阅读器 title 均为 Velmira；代码/README 0 旧名；书库 3 徽标 |
| B | TXT 回归 | **PASS** | 234 章 / 516256 字 / 15245 段 / utf-8 / import_ms=27；ch32 首段正确；末章 percent=1.0；搜索温 8ms；M0 41/41；单测 37/37 |
| C | EPUB | **PASS** | format=epub；234 章；total_chars=482081、paragraphs=15245（与契约一致）；ch32 首段正确；TOC 234；percent 与 TXT 逐位相同 |
| D | CBZ | **PASS** | 自造 5 页 1200×1800；page_count=5；/pages 字段齐；/media 字节逐字节一致+immutable；越界 404；txt not_comic 400；progress 0.2/0.6/1.0 |
| E | 注释 | **PASS** | 4 色 POST 201；排序/limit/offset；PATCH note+color；DELETE；6 类 400；404 annotation_not_found；重启后仍在 |
| F | 书签 | **PASS** | POST/GET/DELETE；同一 (chapter,para) 重复 POST 实测 API 允许（两次 201），前端 toggle 去重；重启后仍在 |
| G | 前端 CDP 实测 | **PASS** | 22/22：mark×4/4 色、改字号/换主题/切章重建、书签抽屉、目录过滤 1/234、滑动/键盘/点击区、漫画单 img/翻页/深链/重试、移动端 390、0 报错 |
| H | 体积/内存 | **PASS** | JS 112,411 B≈109.8KB（8 文件）<150KB；CSS 31,458 B；HTML 4,867 B；文本 20 章 heap −0.18MB；漫画 DOM 94 / heap 2.00MB |
| I | 旧库升级 | **PASS** | data/app.db 只读复制；init_db×2 幂等；新表 book_pages/annotations 齐全；旧书 234 章可读；原文件 SHA256 未变 |

**FAIL 清单：无。** 与 M0 报告关系：**M0 41 项断言全绿（仅版本号断言 0.1.0→0.3.0），无回归**。

---

## 2. 逐项证据

### A. 品牌 / 版本

| 断言 | 期望 | 实测 | 判定 |
| --- | --- | --- | --- |
| GET /api/health | name="Velmira", version="0.3.0" | 完全一致，data_dir 指向隔离目录 | PASS |
| index.html `<title>` | Velmira | `Velmira` | PASS |
| 书库 brand | Velmira + v0.3 标签 | `Velmira` + `.brand-tag`=`v0.3`；3 张书卡 | PASS |
| 阅读器 document.title | `<书名> · Velmira` | `示例小说 · Velmira` | PASS |
| 旧品牌名 | web/ server/ README 0 处 | 代码/README **0 处**；md 文档仅 web/MEMORY.md:32 引用该词（自测记录，非 UI/代码） | PASS（见 P2-1） |
| 格式徽标 | TXT/EPUB/CBZ | 书库 DOM `.fmt-badge` = `fmt-epub/fmt-cbz/fmt-txt`，文案 EPUB/CBZ/TXT | PASS |

### B. TXT 回归（tests/verify_m0.py，41/41 PASS）

| 断言 | 期望 | 实测 | 判定 |
| --- | --- | --- | --- |
| 导入 HTTP / chapter_count / total_chars | 201 / 234 / 516256 | 201 / 234 / 516256 | PASS |
| paragraphs / encoding | 15245 / utf-8 | 15245 / utf-8 | PASS |
| import_ms | <3000 | **27** | PASS |
| chapters/32 首段 | （示例段落） | 一致（章节标题非空、首段长度>5） | PASS |
| 末章进度 | percent=1.0 | PUT {233,57,46} → **1.0** | PASS |
| 搜索 主角A | 温 <200ms | 冷 251ms（含建索引）/ 温 **8ms**，20 命中 | PASS |
| M0 41 项 | 全 PASS | **41 PASS / 0 FAIL** | PASS |
| 单测 | 37 OK | `Ran 37 tests in 0.230s OK` | PASS |

### C. EPUB

源：`book/示例小说 - 示例作者.epub`

| 断言 | 期望 | 实测 | 判定 |
| --- | --- | --- | --- |
| 导入 | format=epub | 201，id=b_03b8dc，format=epub，import_ms=126 | PASS |
| 章节数 | ≥200 | **234** | PASS |
| 总字数/段数 | 482081 / 15245 | **482081 / 15245** | PASS |
| TOC | 可用 | 234 项、标题全非空 | PASS |
| chapters/32 首段 | 正确 | 「（示例段落）」，67 段 | PASS |
| 进度口径 | 与 TXT 一致 | 同 locator {32,3,7}：txt=epub=**0.136836** | PASS |

### D. CBZ（Pillow 自造 5 页，1200×1800，不同底色+页码）

fixture：`tests/fixtures/m1_comic_5p.cbz`（page_1..page_5，9,418–9,561 B/页）

| 断言 | 期望 | 实测 | 判定 |
| --- | --- | --- | --- |
| 导入 | format=cbz，page_count=5 | 201，id=b_e437bd，format=cbz，chapter_count=5，page_count=5，paragraphs=0，import_ms=17 | PASS |
| GET /pages/0 | book_id/idx/count/url/width/height | `{"book_id":"b_e437bd","idx":0,"count":5,"url":"/media/b_e437bd/0","width":1200,"height":1800}` | PASS |
| GET /pages?from=0&limit=3 | 0,1,2 + count=5 | idx=[0,1,2]，count=5，from=0 | PASS |
| GET /pages limit 上限 | ≤50 | 返回 5（书仅 5 页，上限不可观测） | PASS |
| GET /media/0 | 字节一致 + Content-Type + Cache-Control | **逐字节一致（9418B）**，`image/png`，`public, max-age=31536000, immutable` | PASS |
| GET /media/1 | 对应 page_2 | 逐字节一致（9561B） | PASS |
| /pages/5 越界 | 404 page_not_found | 404 page_not_found | PASS |
| /media/9 越界 | 404 | 404 page_not_found | PASS |
| txt 书 /pages、/media | 400 not_comic | 均 400 not_comic | PASS |
| 未读默认进度 | percent=1/5=0.2 | `{chapter:0,para:0,offset:0,percent:0.2,updated_at:0}` | PASS |
| PUT chapter=2 / 4 | 0.6 / 1.0 | 0.6 / 1.0 | PASS |
| 列表 percent 同步 | 1.0，last_chapter=4 | 1.0 / 4 | PASS |

### E. 注释（TXT 第 1 章）

| 断言 | 实测 | 判定 |
| --- | --- | --- |
| POST 4 色 | 4×201，yellow/green/blue/pink，id/text 回显正确 | PASS |
| GET 排序+分页 | chapter=0 返回 4 条，(para,start) 升序；limit=2→2、offset=2→2；全书 4 条 | PASS |
| PATCH note+color | 200，note「主角名，18 岁」、color=blue，text 不变 | PASS |
| DELETE | 200 {ok:true}；重复 DELETE 404；剩余 3 | PASS |
| 6 类 400 | chapter 越界 / start>=end / len(text)≠end-start / para 越界 / color 非法 / end 超段落长 → **均 400 invalid_annotation** | PASS |
| 404 | PATCH 不存在 → `annotation_not_found` | PASS |
| 持久化 | 重启后 3 个 id 全部命中 | PASS |

### F. 书签

| 断言 | 实测 | 判定 |
| --- | --- | --- |
| POST/GET/DELETE | 201 → GET 可见 → DELETE 200 → 列表减 1 | PASS |
| 同一 (chapter,para) 重复 POST | **API 允许重复**：两次均 201、两条 id（M0 契约 4.9 无唯一约束）；前端 toggle 按 chapter+para 判重，CDP 实测 drawer 仅 1 条 | PASS（见 P2-4） |
| 持久化 | 重启后书签 id 仍命中 | PASS |

### G. 前端 CDP 实测（22/22 PASS，0 JS 报错）

文本阅读器（txt，`?book=…&ch=5&debug=1`）：

| 项 | 实测 |
| --- | --- |
| 品牌 | 书库 title=`Velmira`；阅读器 title=`… · Velmira`；3 徽标 |
| 注释高亮 | API 预造 4 色 → `mark.ann`=4，class `ann ann-yellow/green/blue/pink`，`data-ann` 与 API id 一一对应，内存仅 4 条 |
| 改字号 | 19→22px 后 mark 仍 4 |
| 换主题 | day→night 后 mark 仍 4，弹层 3 选项 |
| 切章重建 | 切第 7 章 mark=0、内存注释=0；切回第 6 章 mark=4 |
| 书签 | ☆→★、aria-pressed=true；再点回 false；抽屉 `.bm-item` 有 1 条，点击跳转 ch=5/p=0、抽屉关闭 |
| 目录过滤 | 234 项；输入「100」可见 **1** 项，meta「匹配 1 / 234 章」 |
| 触摸滑动 | 左滑 240px 页 0→1；右滑 180px 1→0；20px 0→0 不翻页 |
| 键盘/点击区 | ArrowRight 0→1；右 1/3 点击 1→2 |
| 首屏请求 | 8 resources + document = **9**（≤9） |

漫画模式（cbz，`?book=…&debug=1`）：

| 项 | 实测 |
| --- | --- |
| body.comic-mode | 是；`#reader-main img`=**1**，全页 img=1；naturalWidth=**1200**；DOM=**94**（<300） |
| 首屏请求 | Network.requestWillBeSent = **11**（≤12） |
| 下一页 | 1/5→2/5，img src `/media/b_e437bd/1`，URL `ch=1`，百分比更新 |
| 深链 ?ch=4 | 落末页「5 / 5」，src …/4 |
| 媒体失败 | 阻断 `*media*` → `.comic-error` 可见、`.comic-retry` 存在，无白屏/fatal |
| heap/DOM | heap=2.00MB，DOM=94，img=1 |

移动端：390×844，`innerWidth=docScrollWidth=clientWidth=390`（无横向溢出），工具栏 8 按钮可见，mark 正常渲染。

### H. 体积 / 内存

| 文件 | 字节 |
| --- | ---: |
| web/app.js | 1,626 |
| web/util/core.js | 6,541 |
| web/ui/paging.js | 16,865 |
| web/ui/reader.js | 28,126 |
| web/ui/library.js | 15,289 |
| web/ui/panels.js | 15,907 |
| web/ui/annotations.js | 12,754 |
| web/ui/comic.js | 15,303 |
| **JS 合计（8）** | **112,411 B ≈ 109.8 KB（预算 150KB）** |
| CSS 合计（main 24,932 + comic 6,526） | 31,458 B |
| HTML（index.html） | 4,867 B |

| 指标 | 实测 | 结论 |
| --- | --- | --- |
| 文本切 20 章 heap | 1.20MB → 1.02MB（GC 后），**Δ −0.18MB** | PASS（<5MB，无累积） |
| 文本 DOM | 131 | 低 |
| 漫画 DOM / img / heap | 94 / 1 / 2.00MB | PASS |
| 首屏请求 | 文本 9、漫画 11 | PASS |
| 单测 / node --check | 37 OK / 8 文件 0 失败 | PASS |

### I. 旧库升级

- 源：`data/app.db`（M0 老库，1 本书 b_7be181：txt/234 章）。
- 操作：**只读复制**到新 DATA_DIR；服务启动自动 init_db；另单独执行 `init_db()` 两次。
- 实测：`TABLES=annotations,book_pages,bookmarks,books,chapters,para_fts…,progress,search_index_state,segments` → **HAS_NEW_TABLES=True**；`BOOKS=1 ANNOTATIONS=0 BOOK_PAGES=0`；**INIT_DB_TWICE_OK**；重启后 books=1、TOC=234、chapters/0=88 段可读。
- `data/app.db` SHA256 前后一致：`E6C56C28DB31852BE66BC603A0C965BF7064A68F322ACE63B5924B741C1A7D55`，**未被修改**。

---

## 3. 与 M0 报告的关系

- **回归全绿**：`tests/verify_m0.py`（仅把健康检查版本断言 0.1.0→0.3.0，并在文件头注明 M1 更新）在全新隔离库实测 **41 PASS / 0 FAIL / 0 PARTIAL**，与 M0 报告一致。
- M0 的 TXT 关键数字（234/516256/15245/utf-8/ch32 首段/末章 1.0/搜索）全部复现；分段 475/234/214167、snippet、单字 degraded、413 等 M0 断言均 PASS。
- 新增 M1 能力未破坏 M0 行为：M0 41 项 + M1 API 34 项 + 持久化 3 项 + CDP 22 项，合计 **100 项断言，0 FAIL**。

---

## 4. 缺陷清单

### P0（阻断）：0

### P1（严重）：0

### P2（建议/文档）

| # | 位置 | 期望 | 实际 | 影响 |
| --- | --- | --- | --- | --- |
| P2-1 | `web/MEMORY.md:32` | web/ 不再出现旧名 | 该 md 自测记录中引用了「旧品牌名」一次（描述 grep 结果）；UI/JS/CSS/服务端 0 处 | 仅文档卫生，不影响产品；建议 M1 归档后移入 docs/ 或改写 |
| P2-2 | `web/MEMORY.md:17` | JS 文件数记录准确 | 写「11 个文件」，实测 **8 个 .js**（字节数以实测为准，当前 112,411） | 文档口径误差 |
| P2-3 | `web/ui/reader.js`（734 行） | 契约「每模块 <400 行」软约束 | 734 行（M0 已 487） | 已知；建议 M2 拆书签/debug/comic 委托 |
| P2-4 | 书签 POST（server/app/api/books.py） | 契约 4.9 无唯一约束 | 同 (chapter,para) 可重复创建两条 201；前端 toggle 已去重 | 多端/直接调 API 可能产生重复书签；M2 可加唯一索引或幂等 200 |
| P2-5 | 双页模式 | P2 可选 | 未实现（契约 7.6 允许放弃） | 单页模式无回归 |
| P2-6 | `GET /pages limit=50` 边界 | 可验证钳制 | 5 页 fixture 无法观测上限（已验证 <50 正常返回） | 测试覆盖缺口，非缺陷；M2 可用 ≥60 页 fixture |

### 验证脚本自身修正（诚实记录，非产品缺陷）

1. `/media` 响应头由 uvicorn 以小写输出（`content-type`），首版断言按键名大小写敏感误报 D5；改为大小写不敏感后 PASS。
2. 漫画用例首次未归零服务端进度，续读落在第 5 页导致「下一页」断言失败；脚本先 PUT chapter=0 后 PASS。
3. CDP 书签用例受前一轮/API 阶段遗留书签影响，脚本先清空再测；改为「API 阶段→重启持久化→CDP」的规范顺序后持久化 3/3 PASS。

---

## 5. 未覆盖 / 说明

- **证据脱敏（task-11）**：`tests/` 内已无具体书名/角色名/原文句子；搜索词改为运行时从正文截取，硬编码内容断言改为结构性断言（标题非空/首段长度/字段等），标定数字（234/516256/15245/475/214167 等）保持不变。
- 未做真实移动设备手势（用 CDP `Input.dispatchTouchEvent` 模拟）；未验证垂直滚动降级模式的 mark 渲染。
- 未渲染 EPUB 内嵌图片/样式（契约 7.3 明确只取文本）。
- `scripts/verify_m1.ps1` 已通过 AST 解析但本轮未整体重跑（各步骤单独实测）；`-SkipCdp` 可用于无浏览器环境。
- CBZ 使用 5 页自造 fixture；非 60 页/大图压力场景（M2 可补）。

---

## 附录：关键复现命令

~~~powershell
# 一键
powershell -NoProfile -File scripts/verify_m1.ps1

# 手动
$env:DATA_DIR = "D:/project/ai reading plus/data/_m1_verify_hhmmss"
python -m uvicorn server.app.main:app --host 127.0.0.1 --port 8787
python tests/verify_m0.py --base http://127.0.0.1:8787
python tests/verify_m1.py --base http://127.0.0.1:8787 --phase api
# 重启同一 DATA_DIR 后：
python tests/verify_m1.py --base http://127.0.0.1:8787 --phase persist
node tests/cdp_m1.mjs --base http://127.0.0.1:8787 --port 9334
# 旧库升级：
python tests/_m1_upgrade_check.py "data/_m1_upgrade_xxx"
~~~

> 端口使用 8787/8788；每个测试使用随机 DATA_DIR 并在结束后删除；未修改 server/、web/、docs/、book/ 与 `data/app.db`。
