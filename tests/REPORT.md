# M0 独立验收报告（verifier）

- 验收人：verifier（task-4，依赖 task-1/2/3 均 completed）
- 验收时间：2026-11-12 16:00–16:55 (Asia/Shanghai)
- 形态：**冷启动隔离实测**（独立 DATA_DIR + 独立端口），非引用 owner 自测值
- 结论：**A–H 全部通过（无 P0 / 无 P1）**；4 条 P2 建议 + 2 条环境说明，见 §3
- 复现套件：tests/verify_m0.py（API 全量断言）、tests/cdp_probe.mjs（浏览器 CDP 探针）、scripts/verify.ps1（一键回归）
- 原始证据：tests/verify_m0_result.json（截图证据已随交付清理，可跑 scripts/verify.ps1 复现）
- 回归自检：scripts/verify.ps1 已用全新隔离 DATA_DIR 端到端跑通（41 项断言 0 FAIL，退出码 0）；本机只有 Windows PowerShell 5.1，运行方式 powershell -NoProfile -File scripts/verify.ps1（脚本已带 UTF-8 BOM，勿存为无 BOM 否则中文串会破坏解析）

## 0. 环境与方法

| 项 | 值 |
| --- | --- |
| OS | Windows 10.0.19045 / Windows PowerShell 5.1 |
| Python | 3.11.9（fastapi 0.141.1、uvicorn 0.54.0，未 pip install） |
| Node | v24.19.0 |
| 浏览器 | Edge（headless=new + CDP 9333） |
| 服务端口 | **8780**（8799 实测空闲，按指示避开；未使用 mock 服务） |
| 数据目录 | data/_test_verify_749204（测试前 data/ 为空 = 冷启动；测试后已删除） |
| 启动命令 | $env:DATA_DIR='D:/project/ai reading plus/data/_test_verify_749204'; python -m uvicorn server.app.main:app --host 127.0.0.1 --port 8780 |
| 书 | book/示例小说 - 示例作者.txt，1,483,773 bytes |

冷启动顺序：GET /api/health（确认 version=0.1.0、data_dir 指向隔离目录）→ GET /api/books 必须为空 → 导入。所有数字来自该次冷启动。

## 1. 验收总表

| 项 | 内容 | 结论 | 关键实测 |
| --- | --- | --- | --- |
| A | 冷启动导入 + TOC/章节 | **PASS** | 201；234 章；516,256 字；15,245 段；encoding=utf-8；import_ms=27；无 BOM；chapters/32 首段与 chapters/0 无污染均符合 |
| B | 契约错误路径（7 例） | **PASS** | 404×2、400×3、413×1、400×1，error.code 结构统一 |
| C | 进度百分比/往返/去重 | **PASS** | 0.999905 → 1.0；单调；GET==PUT；deduped=true |
| D | 分段标定 | **PASS** | 475/234/214167/hash 稳定/55ms；Top1 c33-s0 raw 3.414 score 0.999；cached=true；无 text 字段 |
| E | 检索 | **PASS** | 冷 237ms（含建索引）/温 8ms；234 章已索引；snippet 转义+<b>；单字 degraded；空 q 400 |
| F | 前端静态检查 | **PASS** | 6 文件 node --check 0 失败；JS 52,159 B（50.9KB）<120KB；无第三方 import；正文无 innerHTML；首屏不请求 toc/segments |
| G | 浏览器实测 | **PASS** | 桌面 1440×900 / 移动 390×844 截图非空白；无横向溢出（docScroll=client=390）；翻页/切章/目录 234 项可用；heap 0.77–1.4MB、DOM 122–137 |
| H | 单测 | **PASS** | python -m unittest discover -s tests -v → Ran **37** tests, OK（0.200s）；pytest 未安装（禁止 pip，见 §6） |
| 回归 | scripts/verify.ps1 端到端 | **PASS** | 全新 DATA_DIR + 端口 8810：41 项断言 0 FAIL，node --check 6/6，unittest OK，退出码 0，端口/数据目录自动清理 |

## 2. 逐项证据与实测值

### A. 冷启动导入

命令：python tests/verify_m0.py --base http://127.0.0.1:8780

| 断言 | 期望 | 实测 | 判定 |
| --- | --- | --- | --- |
| 源文件 BOM | 无 | 前 3 字节 = E4 B9 A6（"书"），无 BOM | PASS |
| HTTP | 201 | 201 | PASS |
| chapter_count | 234 | 234 | PASS |
| total_chars | 516,256 | 516,256 | PASS |
| paragraphs | 15,245 | 15,245 | PASS |
| encoding | "utf-8" | "utf-8"（非 utf-8-sig 伪报） | PASS |
| import_ms | <3000 | **27** | PASS |
| title / author | 书名 / 示例作者 | 示例小说 / 示例作者 | PASS |
| deduped | false | false | PASS |
| TOC | 234 项 | 234（idx 0..233，首项标题非空） | PASS |
| chapters/32 首段 | （示例段落） | 「（示例段落）」 | PASS |
| chapters/0 污染词 | 无 书籍信息/书名：/【简介】 | 0 命中；首段非空且非书头元信息 | PASS |
| Σ chapters.chars（契约 4.2 分母） | 482,081 | **482,081**（实测 TOC chars 求和） | PASS |

### B. 契约错误路径

| # | 请求 | 期望 | 实测 | 判定 |
| --- | --- | --- | --- | --- |
| B1 | GET /chapters/234 | 404 chapter_not_found | 404 chapter_not_found | PASS |
| B2 | GET /api/books/b_nonexistent | 404 book_not_found | 404 book_not_found | PASS |
| B3 | POST /api/books（无 filename） | 400 missing_filename | 400 missing_filename | PASS |
| B4 | POST /api/books?filename=empty.txt + 空 body | 400 empty_file | 400 empty_file | PASS |
| B5 | POST 65MB（手写 Content-Length） | 413 file_too_large | **413** {"error":{"code":"file_too_large","message":"文件超过 64MB 上限"}} | PASS |
| B6 | PUT progress chapter=234 / 999 | 400 invalid_locator | 400 invalid_locator（两例） | PASS |
| B7 | PUT progress 非法 JSON | 400 invalid_locator | 400 invalid_locator | PASS |

所有错误响应均满足 {"error":{"code": str, "message": str}} 且 Content-Type=application/json（B5 亦为 JSON）。
注：首轮 verify_m0.py 的 B5 因脚本内函数名 http 遮蔽 http.client 模块误报 1 次 FAIL，**属验脚本 bug，已修复并用独立脚本复测 PASS**（诚实记录，非产品缺陷）。

### C. 进度

| 场景 | 期望 | 实测 | 判定 |
| --- | --- | --- | --- |
| 无记录 GET | chapter=0, percent=0.0（不 404） | {"chapter":0,"para":0,"offset":0,"percent":0.0,"updated_at":0} | PASS |
| PUT {233,57,0} | percent ≥ 0.999 | **0.999905** | PASS |
| PUT {233,57,46} | percent = 1.0 | **1.0**（精确） | PASS |
| 中间章单调 | 递增 | ch0/50/100/150/200/233 → 0.0, 0.213869, 0.427538, 0.642272, 0.855257, 0.995808 | PASS |
| GET 重算一致性 | 与 PUT 相同 | PUT=GET={"chapter":32,"para":3,"offset":7,"percent":0.136836} | PASS |
| 重复导入 | 200 + deduped=true + 同 id | 200 deduped=True id=b_e12b55，import_ms=1 | PASS |

### D. 分段标定（spike 复现）

POST body = 契约 4.10 默认参数（target_chars 900 / overlap 1 / min 400 / max 1500，per_chapter 1，force false）。

| 断言 | spike 标定 | 实测 | 偏差 | 判定 |
| --- | --- | --- | --- | --- |
| segments | 475 | **475** | 0 | PASS |
| selected | 234 | **234** | 0 | PASS |
| picked_chars | 214,184 | **214,167** | −17（−0.0079%，≤0.1% 允许） | PASS |
| raw 范围 | 0.505–3.414 | **0.505–3.414** | 0 | PASS |
| raw p50 | 1.937 | **1.937** | 0 | PASS |
| 段均值 chars | ~913 | **912.7** | ~0 | PASS |
| segment score 范围 | 0..1 | 0.001–0.999 | — | PASS |
| params_hash | 稳定 | 04f79a194cda7e92（两次调用一致） | — | PASS |
| elapsed_ms | <3000 | **55**（force=false 冷算） | — | PASS |
| 二次 POST | cached=true 不重算 | cached=true，同 hash，elapsed_ms=3 | — | PASS |

GET /segments?chapter=32&selected=1&limit=50 首条：

{"id":"c33-s0","chapter":32,"idx":0,"start_para":0,"end_para":29,"chars":919,"raw":3.414,"score":0.999,"selected":1,"title":"第33章 示例章节","head":"（示例段落）"}

| 断言 | 实测 | 判定 |
| --- | --- | --- |
| Top1 = c33-s0 / chapter 32 | 一致 | PASS |
| raw ≈ 3.414 / score ≈ 0.999 | 3.414 / 0.999 | PASS |
| head 前缀「（示例段落）」 | 一致 | PASS |
| 不返回 text 全文 | 返回键 = chapter/chars/end_para/head/id/idx/raw/score/selected/start_para/title，**无 text** | PASS |
| 默认 limit=50 | 返回 50 条 | PASS |
| limit=2000 被钳制、不报错 | 200，返回 475（全书仅 475 段；实现上限 _MAX_LIMIT=1000，边界不可观测） | PASS |
| limit=-5 钳制到 1 | 返回 1 条 | PASS |

### E. 检索

| 断言 | 期望 | 实测 | 判定 |
| --- | --- | --- | --- |
| q=主角A 冷（首次，含建索引） | 记录 | 响应 elapsed_ms=**237**，wall 241ms，hits=20，index={chapters_indexed:234,degraded:false}，首命中 ch0/para0 | PASS（记录项） |
| q=主角A 温 ×3 | <200ms | 响应 **8ms**（wall 10.0/9.9/10.2ms），hits=20 | PASS |
| hits ≤ limit | ≤20 | 20 | PASS |
| snippet 含 <b>主角A</b> | 全部命中 | 20/20 含 <b>主角A</b>，由 html.escape 后包 <b>（server/app/pipeline/search_index.py:157-179） | PASS |
| 注入 q=<script>alert(1)</script> | snippet 不得原样输出 | 200，hits=[]；hits 内无未转义 <script>；响应体仅回显 q 字段含 1 次原始串（JSON 回显；前端用 input.value/textContent，不构成 DOM XSS） | PASS（附注 P2-3） |
| q=单字（动态） | degraded=true 不报错 | 200，degraded=true，hits=20，elapsed 6ms | PASS |
| q 空 | 400 empty_query | 400 empty_query | PASS |
| 2 字走 LIKE 按 chapter/para | 有序 | (0,0),(0,3),(0,4),(0,8),(0,9)… 升序 | PASS |
| ≥3 字走 bm25 | 负分升序 | q=3 字动态词：8 命中 score=-10.681…-5.128，升序 | PASS |

### F. 前端静态检查

| 断言 | 命令/方法 | 实测 | 判定 |
| --- | --- | --- | --- |
| 全部 web JS 语法 | 复制为 .mjs 后逐文件 node --check | 6/6 OK，失败 0（app.js / ui/library.js / ui/paging.js / ui/panels.js / ui/reader.js / util/core.js） | PASS |
| JS 总体积 | 递归求和 | **52,159 bytes（50.9KB）** < 120KB | PASS |
| 无第三方 import | grep 全部 import/from | 5 条静态 import + 2 条动态 import，**全部相对路径**（reader.js:9,10；panels.js:6；paging.js:12；library.js:7；app.js:19,43；reader.js:83） | PASS |
| 无 innerHTML 拼接正文 | grep innerHTML/outerHTML/insertAdjacentHTML/document.write | 唯一命中 web/ui/panels.js:128 的 snip.innerHTML = hit.snippet 赋值（服务端已转义 snippet，任务允许）；章节正文走 paging.js:285-295 的 textContent + appendChild | PASS |
| 首屏不请求 toc/segments | CDP 记录 performance.getEntriesByType('resource') | 首屏资源：/styles/main.css, /app.js, /ui/reader.js, /util/core.js, /ui/paging.js, /api/books/{id}, /api/books/{id}/progress, /api/books/{id}/chapters/32 —— **无 /toc、无 /segments**；点击目录按钮后才出现 /api/books/{id}/toc + /ui/panels.js | PASS |
| 首屏请求数（M0-PLAN §3 预算 ≤8） | 同上 | 首屏渲染关键路径恰 **8**；随后 1.2s 防抖内另有 1 次重复 GET /progress + 1 次 PUT 进度（见 P2-1，不阻断） | PASS（附注） |
| 单章 DOM | CDP 实测 | 130 DOM 节点，1 块 | PASS |

### G. 浏览器实测（Edge headless + CDP）

| 检查 | 方法 | 实测 | 判定 |
| --- | --- | --- | --- |
| 桌面 1440×900 | --headless=new --screenshot --window-size=1440,900 | 截图非空白、正文可见、1/6 页 | PASS |
| 翻页后 | CDP 点击右 90% 区域 | p1/34 → **p2/34**，para=2，URL ch=32&p=1&para=2 同步；DOM 保持 130，heap 0.83MB | PASS |
| 切章 | ?ch=32 | 目标章节标题/正文正确 | PASS |
| 移动 390×844 | CDP Emulation.setDeviceMetricsOverride(mobile=true) | innerWidth=390，docScroll=client=390，viewport/column scrollWidth=390，**horizontalOverflow=false**，10 页，正文完整无裁切 | PASS |
| 目录 | 点击 #btn-toc | 234 项渲染，DOM 1065，heap 0.98MB，抽屉可见 | PASS |
| 进度恢复 | 切 20 章后 reload | 回到 ch20·第21章·p1/31（URL ch=20&p=0&para=0 一致）；heap 由 1.4MB 回落到 0.8MB | PASS（章级；段落级由 URL para 同步保证） |
| 内存/DOM（CDP） | performance.memory、querySelectorAll('*') | 见 §4 内存表 | PASS |

**环境说明（非产品缺陷）**：--window-size=390,844 --screenshot 产出的图出现右侧裁切（toolbar 与正文被截断），原因是 Edge headless 窗口最小宽度限制导致实际渲染视口约 500px 而截图裁到 390px；用 CDP Emulation.setDeviceMetricsOverride(390×844) 实测布局无溢出、正文完整（对比 tests/screenshots/mobile_390x844.png 与 mobile_cdp_390x844.png）。结论：390 布局合格，首图为工具假象。

### H. 单测

| 命令 | 结果 | 归属 |
| --- | --- | --- |
| python -m unittest discover -s tests -v | **Ran 37 tests, OK**，37 ok / 0 fail / 0 error，0.200s | 全部为 pipeline 的 tests/test_pipeline.py + test_search.py（task-3 owner） |
| python -m pytest tests -q | **未安装**：No module named pytest（禁止 pip install，无法补） | 环境限制 |

## 3. 缺陷清单

### P0 阻断：0 条
### P1 影响质量：0 条

### P2 建议 / 观察项（4 条）

| # | 级别 | 位置 | 现象 | 期望 vs 实际 | 复现 |
| --- | --- | --- | --- | --- | --- |
| P2-1 | 建议 | web/ui/reader.js:121 与 :135 | 无 ?ch 参数进入阅读器时，GET /api/books/{id}/progress 被请求 **2 次**（:135 无条件发请求，仅用返回值补 UI），随后 1.2s 防抖又发 1 次 PUT。首屏总请求 10（关键路径 8 = 预算内，故不阻断） | 期望 1 次 GET；实际 2 次 GET + 1 次 PUT | 冷启动打开 /?book=b_e12b55，看 Network/CDP resource 列表 |
| P2-2 | 建议 | server/app/api/segments.py:183（_MAX_LIMIT=1000） | GET /segments?limit=2000 返回 200 且被钳到 1000，但整书仅 475 段，钳制边界不可观测；契约 4.11 未定义 limit 上限 | 期望契约写明上限（如 ≤1000）；实际未写 | GET /api/books/{id}/segments?limit=2000 |
| P2-3 | 安全备注 | server/app/api/search.py:48-54 | 响应体回显 q 原样（含 <script>），命中字段均已转义。当前前端用 input.value/textContent，无 DOM XSS；若未来用 innerHTML 渲染 q 会引入风险 | 现状安全；建议契约注明「q 仅可回显、前端不得 innerHTML 渲染」 | GET /api/books/{id}/search?q=%3Cscript%3Ealert(1)%3C/script%3E |
| P2-4 | 观察项 | server/app/api/search.py / pipeline/search_index.py | 冷启动首次搜索 237ms（含 234 章建索引），超过 M0-PLAN §6-4「搜索<200ms」字面值，但契约把「建索引 <2s」「查询 <200ms」分开，温查 8ms 达标 | 验收字面未区分冷/温；建议验收措辞改为「温查<200ms，冷启动含建索引<2s」 | 冷启动后第一次 GET /search?q=主角A |

## 4. 内存与体积实测表

| 指标 | 预算 | 实测 | 判定 |
| --- | --- | --- | --- |
| 前端 JS 总量 | <120KB | **52,159 B / 50.9KB**（app.js 1,626；ui/library.js 4,821；ui/paging.js 13,599；ui/panels.js 8,868；ui/reader.js 17,318；util/core.js 5,927） | PASS |
| 首屏 JS heap | — | **0.77–0.95MB**（performance.memory.usedJSHeapSize） | PASS |
| 单章 DOM 节点 | <1500 | **130**（切 20 章区间 122–137） | PASS |
| 切 20 章后 heap | 基线+5MB 内 | 基线 0.8MB → 峰 1.4MB，**+0.6MB**；随后 reload 回落 **0.8MB** | PASS |
| 目录抽屉展开 DOM | <1500 | 1,065（234 个 TOC 项，按需 DOM 生成） | PASS |
| 全书正文常驻 | 0 字节 | 单章 60–67 段 / 2,046–2,099 字符在 DOM；切章先 clear 旧 DOM（reader.js:159） | PASS |
| 首屏请求数 | ≤8 | 关键路径 **8**（1 css + 4 ESM + books + progress + chapters）；此后 2 次后台进度读/写 | PASS（附注 P2-1） |
| import 耗时 | <3000ms | **27ms** | PASS |
| 分段耗时 | <3000ms | **55ms**（缓存命中 3ms） | PASS |
| 温检索 | <200ms | **8ms** | PASS |

## 5. 与 docs 标定的差异说明

1. **picked_chars 214,167 vs docs/spike_report.json 的 214,184（−17 字，−0.0079%）**：已由 Lead 裁定为「干净剥离书头/简介后的正常偏差」，契约 4.10 明确允许 ≤0.1% 且示例即 214184→214167。实测 segments=475、selected=234、raw 0.505–3.414、p50 1.937、均值 912.7 全部与 spike 一致，**未为对齐数字保留污染片段**（chapters/0 无 书籍信息/书名：/【简介】，已断言）。
2. **total_chars 516,256 与 Σ chapters.chars 482,081 的差异**：符合契约 4.2——前者是解码后全文长度（含换行与书头），后者是正文总字数 = percent 分母；实测两者分别为 516,256 / 482,081，未混用。
3. **encoding="utf-8"**：源文件无 BOM（前 3 字节 E4 B9 A6），服务端未伪报 utf-8-sig，符合契约 4.2 近期更新。
4. **冷/温检索口径**：契约 4.12 只对「建索引 <2s、查询 <200ms」分别约束，M0-PLAN §6-4 写「搜索<200ms」未区分冷温，实测冷 237ms / 温 8ms，按契约判定 PASS（见 P2-4 建议修订措辞）。

## 6. 未实测 / 限制

1. pytest 未安装且禁止 pip install → 只跑了 unittest（37 项全绿）。
2. 段落级「刷新回到同一段（para 完全一致）」只验证到章级 + URL ch/p/para 同步：CDP reload 前后 URL 均为 ch=20&p=0&para=0（第 21 章 1/31 页）。未构造「翻到第 N 页不切章再硬刷新」的独立用例；按契约 Locator 含 para 且 URL 带 para，风险低。
3. 真实移动设备触摸翻页未实测（headless 仅验证布局与点击翻页逻辑）。
4. 内存为 performance.memory（Chromium 估算值），非 DevTools 精确快照；趋势（0.8 → 1.4 → 0.8MB）与 DOM 恒定足以支撑「无累积泄漏」判断。

---

## 7. Lead 复核补记（2026-11-12 17:10）

在 verifier 报告之外，Lead 用 CDP 复跑并补了 3 个 verifier 未覆盖的用例，均已修复并回归：

| # | 发现 | 级别 | 处理 |
| --- | --- | --- | --- |
| L1 | 桌面端「主题 / 字号」纯图标按钮是空的（.btn .ico 桌面 display:none，只有移动端媒体查询才显示） | P1 视觉 | web/styles/main.css 增加 .btn-icon .ico { display:inline }，截图复核通过（docs/screenshots/10-toolbar.png） |
| L2 | URL 只带 p 不带 para 时，init 把 p 当段号，深链落在第 1 页 | P2 | web-reader 修复：p=页码，缺 para 时首章后 pager.goToPage(qP)；已实测 ch=99&p=2 → 3/10 页 |
| L3 | 首屏重复 GET /progress（2 次 GET + 1 次 PUT） | P2 | web-reader 用 progressFetched 标记修复，冷启仅 1 次 GET |

Lead 最终回归：scripts/verify.ps1 → 41 项 API 断言 PASS、37 单测 OK、6 个 JS 语法检查通过；活服务 8765 实测桌面/移动首屏 window.onerror 与 unhandledrejection 均为 0，heap 0.96MB、DOM 151（桌面）/116（移动），390 视口 docScrollWidth=390 无横向溢出。
