# Velmira — AI 漫画小说阅读器架构设计

> 基于 foliate-js 改造，支持 TXT 导入、AI 分段选片、人物形象固定、分镜生成与拼页。
> 版本 v1 / 2026-11-12 · 验证样本：《示例小说》（234 章 / 51.6 万字）

---

## 0. 关键决策摘要

| 决策点 | 选择 | 理由 |
| --- | --- | --- |
| 阅读内核 | foliate-js（git submodule，锁 commit） | 成熟的分页/重排/标注/搜索能力，纯 ESM 无构建 |
| TXT 接入方式 | **运行时合成 Book 对象**，不改 epub.js | 零依赖 zip.js，不需要打包 EPUB，天然支持流式与惰性加载 |
| 定位体系 | **自定义 Locator（chapter/para/offset）**，不用 EPUB CFI | TXT 无 CFI 语义；段落级定位可回链原文、可高亮、可深链 |
| 后端 | Python FastAPI + SQLite + 文件系统 | 文本处理、ComfyUI 客户端、图像合成/人脸校验生态都在 Python |
| 出图后端 | ComfyUI `/prompt`（MOSS-Image 节点图） | 本机已验证：832×1216 20 步 41s，8 步 LoRA 10s |
| 一致性策略 | **结构化身份卡 + 确定性提示词拼装 + 参考图注入 + 人脸校验**四层 | 单靠提示词无法稳定复现人物，必须多层兜底 |
| 分镜中间层 | storyboard JSON，**LLM 不直接写提示词** | 提示词由确定性 Builder 拼装，避免措辞漂移导致形象漂移 |
| 版面 | 分格出图 → 排版层拼页 → 文字/气泡后置 | 模型渲染中文小字不可靠；后置可编辑、可重排 |
| 部署 | 单机 all-in-one，局域网访问 | 无多租户需求，ComfyUI 与 API 同机 |

---

## 1. 目标与边界

### 1.1 目标
1. 导入 TXT（及 EPUB）后**立即流畅阅读**，桌面/移动端自适应，记住进度。
2. 可配置的文本模型 API，把书按可配置策略切分为「可绘制片段」，并自动挑出适合绘图的片段。
3. 可配置的画图后端（远程 API 或本地 MOSS），把片段生成**连续**的漫画分镜页。
4. 自动从原文抽取人物外貌特征并**冻结**，保证跨分镜一致且符合原文。
5. 分段策略、提示词模板、风格参数、模型接口全部可配置。
6. 支持「参考图注入」模式：预绘人物形象图 → 作为参考图 + 身份说明送入画图模型。

### 1.2 非目标（v1 明确不做）
- 不做多用户/权限体系；不做在线发布与社区。
- 不做自动上色以外的图像编辑（局部重绘 v1 仅保留接口）。
- 不做音频/有声书；不做翻译。
- 不做版权内容分发，仅处理用户本地文件。

### 1.3 关键约束（实测得出）
- 单章约 **2 千字**（本书 234 章，均值 2061 字，max 2552）→ 天然是最好的渲染与生成单元。
- 中文网文 **对话段占 29%**，段落中位数 30 字 → 分割粒度应落在段落级，不能按句子。
- MOSS 提示词上限 4096 token，训练分布 1000 token 内最稳 → 一个 panel 的提示词必须压缩到 300–600 token。
- 12GB 显存 → ComfyUI 必须**串行**出图，不能并发提交，否则 OOM。

---

## 2. 整体架构

### 2.1 分层

```
┌──────────────────────────── 前端（Vite + TS，无框架或轻量 store）────────────────────────────┐
│  Reader 视图                    │  Studio 视图（漫改工作台）        │  Settings 视图            │
│  foliate-view + 合成 Book        │  片段列表 / 人物卡 / 分镜板       │  策略·模板·Provider       │
│  分镜轨（右侧条）/ 原文高亮回调    │  单格重 roll / 拼页预览          │  运行快照 / 复现          │
└───────────────────────────────┬──────────────────────────────────────────────────────────────┘
                                │ REST + SSE（进度）
┌───────────────────────────────▼──────────────────────────────────────────────────────────────┐
│                                    BFF / 编排层（FastAPI）                                    │
│  API 路由 │ 任务队列（SQLite 队列 + asyncio worker）│ SSE 事件总线 │ 运行快照与幂等缓存         │
├──────────────────────────────────────────────────────────────────────────────────────────────┤
│  管线层 Pipeline                                                                              │
│  Importer → Segmenter → Selector → CharacterExtractor → Storyboarder → PromptBuilder          │
│           → ImageRenderer → FaceChecker → PageComposer                                        │
├──────────────────────────────────────────────────────────────────────────────────────────────┤
│  适配层 Providers                                                                             │
│  TextProvider(OpenAI 兼容 / Ollama / llama.cpp / 本地 ninfer) │ ImageProvider(ComfyUI / SD-WebUI / 远程 API) │
│  VisionProvider(人脸校验/画面审阅，可选)                                                       │
├──────────────────────────────────────────────────────────────────────────────────────────────┤
│  存储层  SQLite（结构化） + assets/（图片、参考图、拼页） + 配置 YAML + 运行快照 JSON             │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
                                │ HTTP
                    ┌───────────▼────────────┐
                    │ ComfyUI 127.0.0.1:8188 │  MOSS-Image 节点图
                    └────────────────────────┘
```

### 2.2 两条主链路

**阅读链路（必须快，本地无 AI）**
```
上传 TXT → 编码探测 → 章节切分 → 段落索引 → 合成 Book → foliate-view 分页渲染
                                                  ↘ Locator 持久化（断点/书签/高亮）
```

**漫改链路（可慢，异步，可断点续跑）**
```
章节 → 分段（规则）→ 打分预筛 → LLM 结构化抽取（scene/角色/动作）
     → 人物卡抽取与冻结（首次出场触发，全书扫描补全）
     → 分镜规划（beat → panel）→ Prompt 拼装（身份卡 + 镜头 + 风格）
     → ComfyUI 出图（串行队列）→ 人脸校验 → 不合格重 roll
     → 拼页（网格模板）→ 文字/气泡后置 → 阅读器内联展示
```

### 2.3 关键设计原则

1. **阅读与生成彻底解耦**：阅读器不依赖任何 AI 服务可用性；生成是叠加层，失败不影响读书。
2. **一切生成物带「可复现三元组」**：`config_snapshot + seed + prompt_hash`，任意一格可原样重跑。
3. **中间层优先**：LLM 只产出结构化 JSON，不产出最终提示词；提示词由代码拼装，保证字面稳定。
4. **人工闸门**：人物卡、参考图、验收阈值必须有人点确认，否则整本书的形象会一路歪下去。

---

## 3. 数据模型

### 3.1 SQLite 表

```sql
-- 书与章节
CREATE TABLE books (
  id TEXT PRIMARY KEY, title TEXT, author TEXT, format TEXT,
  file_path TEXT, sha256 TEXT UNIQUE, total_chars INTEGER, created_at INTEGER
);
CREATE TABLE chapters (
  id TEXT PRIMARY KEY, book_id TEXT, idx INTEGER, title TEXT,
  chars INTEGER, para_count INTEGER, text TEXT,
  UNIQUE(book_id, idx)
);

-- 分段结果（可多套策略并存，用 params_hash 区分）
CREATE TABLE segments (
  id TEXT PRIMARY KEY, book_id TEXT, chapter_id TEXT, idx INTEGER,
  start_off INTEGER, end_off INTEGER, text TEXT,
  strategy TEXT, params_hash TEXT,
  score REAL, features_json TEXT, selected INTEGER DEFAULT 0,
  UNIQUE(chapter_id, idx, params_hash)
);

-- 人物卡
CREATE TABLE characters (
  id TEXT PRIMARY KEY, book_id TEXT, name TEXT, aliases_json TEXT,
  card_json TEXT, frozen INTEGER DEFAULT 0, version INTEGER DEFAULT 1,
  first_chapter INTEGER, created_at INTEGER, updated_at INTEGER
);
CREATE TABLE character_refs (
  id TEXT PRIMARY KEY, character_id TEXT, kind TEXT,  -- sheet | avatar | extra
  path TEXT, seed INTEGER, provider TEXT, accepted INTEGER,
  embedding BLOB, created_at INTEGER
);

-- 场景与分镜
CREATE TABLE scenes (
  id TEXT PRIMARY KEY, book_id TEXT, chapter_id TEXT, segment_id TEXT,
  plan_json TEXT, status TEXT, created_at INTEGER
);
CREATE TABLE panels (
  id TEXT PRIMARY KEY, scene_id TEXT, idx INTEGER,
  storyboard_json TEXT, prompt TEXT, negative TEXT,
  seed INTEGER, provider TEXT, params_json TEXT,
  image_path TEXT, status TEXT, attempts INTEGER,
  face_score REAL, config_hash TEXT
);

-- 拼页
CREATE TABLE pages (
  id TEXT PRIMARY KEY, book_id TEXT, chapter_id TEXT,
  layout TEXT, panel_ids_json TEXT, image_path TEXT, status TEXT
);

-- 运行与队列
CREATE TABLE runs (
  id TEXT PRIMARY KEY, book_id TEXT, kind TEXT,
  config_snapshot TEXT, status TEXT, started_at INTEGER, finished_at INTEGER,
  stats_json TEXT
);
CREATE TABLE jobs (
  id TEXT PRIMARY KEY, run_id TEXT, type TEXT, payload_json TEXT,
  status TEXT, attempts INTEGER, error TEXT,
  created_at INTEGER, updated_at INTEGER
);
CREATE INDEX idx_jobs_status ON jobs(status, created_at);
CREATE INDEX idx_segments_lookup ON segments(book_id, chapter_id, idx);
```

### 3.2 核心 JSON 结构

**Segment（LLM 抽取后的片段视图）**
```json
{
  "id": "c33-s0",
  "chapter": 33,
  "char_span": [0, 919],
  "kind": "scene",
  "summary": "角色C捏主角A脸颊逗她，主角A躲开并坦白没信心",
  "location": "学院宿舍室内",
  "time_of_day": "night",
  "characters": [
    {"id": "char_a", "role": "主体", "emotion": "委屈", "action": "侧头躲开"},
    {"id": "moyanqiu", "role": "对手", "emotion": "被逗乐", "action": "伸手掐脸颊"}
  ],
  "key_props": ["定制魔杖"],
  "drawable": 0.86,
  "reason": "有明确动作交互与表情反差，单一室内场景",
  "continuity": {"prev": "c32-s1", "note": "延续魔文课压力话题"}
}
```

**CharacterCard（唯一真相，冻结后不可被自动覆盖）**
```json
{
  "id": "char_a",
  "name": "主角A",
  "aliases": ["昵称", "主角A同学"],
  "gender": "male",
  "age": "18",
  "body": {"height": "中等偏瘦", "build": "少年体型"},
  "hair": {"style": "短碎发", "color": "黑色", "length": "齐耳"},
  "eyes": {"shape": "偏圆", "color": "深棕"},
  "face": {"skin": "偏白", "note": "清秀，常带无奈表情"},
  "outfit": {
    "default": "学院深色制服外套，白衬衫，深色长裤",
    "variants": [{"from_ch": 12, "desc": "换上训练用短披肩"}]
  },
  "accessories": ["定制魔杖（第 33 章后）"],
  "distinguishing": ["常被误认为女生"],
  "palette": ["#1b1c22", "#e8e4dc", "#7a4b52"],
  "identity_block": "Xing Xia, an 18-year-old slender boy, short messy black hair reaching the ears, round dark-brown eyes, fair skin, wearing a dark mage-academy uniform jacket over a white shirt and dark trousers",
  "evidence": [
    {"ch": 1, "quote": "（示例段落）", "field": ["gender", "age"]},
    {"ch": 1, "quote": "反倒是角色B长得很漂亮", "field": ["face"]}
  ],
  "frozen": true,
  "version": 3
}
```

**Storyboard（每格一对象，LLM 产出，字段强约束）**
```json
{
  "page_id": "p033",
  "panels": [
    {
      "panel_id": "p033-1",
      "shot": "medium_close",
      "angle": "eye_level",
      "composition": "双人同框，角色C在右，主角A在左",
      "characters": [
        {"id": "moyanqiu", "pos": "right", "pose": "伸手", "expr": "促狭笑"},
        {"id": "char_a", "pos": "left", "pose": "侧头躲", "expr": "委屈"}
      ],
      "bg": "宿舍室内，夜晚，暖黄台灯",
      "light": "warm_key_light",
      "fx": "无",
      "caption": "（示例段落）",
      "dialogue": [{"speaker": "char_c", "text": "（示例对话）"}]
    }
  ]
}
```

### 3.3 资产目录

```
assets/
  books/<book_id>/cover.png
  characters/<book_id>/<char_id>/sheet_01.png, sheet_02.png, ref.json, face.npy
  panels/<book_id>/<chapter_idx>/p033-1.png
  pages/<book_id>/<chapter_idx>/p033.png
  exports/<book_id>/<book_title>.cbz        # 可选：导出成 CBZ 直接喂 foliate-js 阅读
```

> 导出 CBZ 是很有价值的一步：生成完成后，整本漫画可以当作 `comic-book.js` 支持的 CBZ 再进阅读器，做到「小说/漫画」双模式切换，复用同一套阅读内核。

---

## 4. 模块 A：阅读器改造（foliate-js）

### 4.1 为什么可以「不改」内核

foliate-js 的 `view.js` 只要求传入一个实现 **book 接口** 的对象（文档明确：最低限度需要 `.sections` 且每项有 `.load()`）。因此 TXT 支持不需要 fork epub.js，只需写一个 60 行的适配器：

```js
// src/reader/txt-book.js
const XHTML = (ch) => `<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><meta charset="utf-8"/>
<style>${READER_CSS}</style></head>
<body><h2 id="title">${esc(ch.title)}</h2>
${ch.paras.map((p, i) => `<p id="p${i}" data-p="${i}">${esc(p)}</p>`).join('\n')}
</body></html>`;

export function makeTxtBook(parsed) {
  const urlCache = new Map();               // section index -> blob url（LRU，unload 时 revoke）
  return {
    metadata: { title: parsed.title, author: parsed.author, language: 'zh' },
    getCover: async () => null,
    sections: parsed.chapters.map((ch, i) => ({
      id: `ch${i}`,
      cfi: `/6/${i * 2}[ch${i}]!`,          // 仅用于进度条分组，不做真实 CFI 解析
      size: ch.chars,
      linear: 'yes',
      load: async () => {
        if (!urlCache.has(i)) urlCache.set(i, URL.createObjectURL(
          new Blob([XHTML(ch)], { type: 'application/xhtml+xml' })));
        return urlCache.get(i);
      },
      createDocument: async () => new DOMParser().parseFromString(XHTML(ch), 'application/xhtml+xml'),
      unload: () => { const u = urlCache.get(i); if (u) { URL.revokeObjectURL(u); urlCache.delete(i); } },
    })),
    toc: parsed.chapters.map((ch, i) => ({ label: ch.title, href: `ch${i}`, subitems: [] })),
    resolveHref: async (href) => ({ index: Number(String(href).replace(/\D/g, '')), anchor: null }),
    splitTOCHref: async (href) => [href, null],
    getTOCFragment: (doc, id) => doc.getElementById(id),
    isExternal: () => false,
  };
}
```

要点：
- **每章一个 section**，单 section 2 千字，iframe 文档极小，分页与滚动都流畅；234 章按需 `load()`，首屏不卡。
- `URL.createObjectURL` 必须配 LRU + `revokeObjectURL`，否则长书会累积内存。
- `createDocument()` 用于搜索（供 `search.js`），不复用 `load()` 的 URL，避免 iframe 与搜索抢生命周期。

### 4.2 定位符替代 CFI

TXT 没有 CFI 语义，用三元组定位：

```ts
type Locator = { chapter: number; para: number; offset: number };
```

- **写**：`relocate` 事件的 `detail.range.startContainer` 向上找最近的 `[data-p]`，得到段落号；`offset` 取 Range 在该段文本内的偏移。
- **读**：`view.goTo({ index: chapter, anchor: doc => doc.getElementById('p' + para) })`，再用 `range.setStart` 精修到字。
- **进度**：章节号 + 段内比例加权（不用 foliate 的 `fraction`，因为它按 section 内比例算，跨章会跳）。
- **收益**：书签、断点、AI 片段高亮、原文回链全部共用一套坐标，比 CFI 更易调试。

### 4.3 分镜与原文的双向联动

- **原文 → 分镜**：在阅读器中选中文字 → 浮动菜单「为这段生成分镜」→ 以 `{chapter, paraStart, paraEnd}` 创建 scene。
- **分镜 → 原文**：点击右侧分镜轨的卡片 → `goTo(locator)` + 通过 `create-overlayer` 事件拿到 `overlay`，`overlay.add({ range })` 高亮来源段落（`overlayer.js` 已支持，直接复用）。

### 4.4 响应式与阅读体验

| 断点 | 布局 | 说明 |
| --- | --- | --- |
| `< 640px` | 单栏阅读，分镜轨为底部抽屉 | 左右翻页手势，点击左右 1/3 翻页，中间唤出控制条 |
| `640–1024px` | 单栏 + 可折叠右栏 | 分镜轨 320px |
| `> 1024px` | 双栏：正文 + 分镜轨 380–440px | 分镜轨可拖宽，支持「只显示当前章」 |

排版参数化（写入配置而非硬编码）：`font-size`、`line-height`（1.6–2.0）、`max-width`（38–46 em）、`theme`（日/夜/羊皮纸）、`margin`。用 CSS 变量注入到 iframe 的 `READER_CSS`，切换主题不需要重载 section。

### 4.5 改造清单

| 文件 | 处理 |
| --- | --- |
| `view.js` / `paginator.js` / `overlayer.js` / `search.js` | **不修改**，`git submodule` 引入 |
| `reader.js` / `reader.html` / `ui/` | **不用**（README 明确说不属于库），自己写宿主 UI |
| 新增 `txt-book.js` | 合成 Book 适配器 |
| 新增 `epub-book.js` 薄封装 | 复用 `epub.js` + zip.js，走 `makeBook` 同构接口 |
| 新增 `locator.js` | Range ↔ Locator 双向转换 |

---

## 5. 模块 B：TXT 导入与章节切分

### 5.1 处理流水线

```
bytes → BOM 探测 → utf-8 / gb18030 / utf-16 依次尝试 → 归一化换行 → 去零宽字符
      → 剥离书头信息块 → 章节正则切分 → 段落清洗 → 建索引
```

实测本书：`utf-8-sig`，516,256 字，30,982 行，`第N章` 命中 234 次，与实际章节数一致。

### 5.2 章节识别（按优先级）

```python
CHAP_PATTERNS = [
  r'^第\s*[0-9零一二三四五六七八九十百千]+\s*[章节回卷]\s*\S*$',
  r'^Chapter\s+\d+\b.*$',
  r'^(楔子|序章|序|引子|尾声|后记|番外)\s*\S*$',
  r'^\d{1,4}[\.、]\s*\S{1,30}$',        # 兜底：数字编号标题
]
```

约束：命中行长度 ≤ 40 字，且**前 3 章内命中数 ≥ 2** 才认定为章节体；否则退化为「定长兜底切章」（每 3000 字一章，标题自动生成 `第 N 部分`）。

### 5.3 书头清洗（本书真实踩到的坑）

本书开头有 11 行元信息（书名/作者/字数/书籍 ID/阅读量/更新时间）和 `【简介】` 段，以及章首标签行 `【类型标签】`。若不清理，会：
- 被当成第 1 章正文，污染第一个 segment；
- 标签行进入 LLM 分词，影响叙事理解。

处理：`META_HINT` 正则命中即丢弃；章首的 `^【[^】]*】$`、`^[-=*—]{3,}$` 逐条剥离（已在验证脚本中实现，Top 片段里不再出现标签行）。

### 5.4 段落索引

每段记录 `(chapter_idx, para_idx, start_off, end_off)`，全局 offset 单调递增。作用：
- 分段窗口的边界永远落在段落边界，不切断句子；
- LLM 返回的 span 可直接映射回原文字符偏移，用于高亮；
- 反向：原文选区 → 覆盖到的段落集合 → 生成自定义 scene。

---

## 6. 模块 C：文本分段与片段筛选

### 6.1 策略抽象

```python
class Segmenter(Protocol):
    name: str
    def segment(self, chapter: Chapter, params: dict) -> list[Segment]: ...
```

| 策略 | 参数 | 适用 |
| --- | --- | --- |
| `paragraph_window` **（默认）** | `target_chars=900, overlap_paras=1, min=400, max=1500` | 网文，段落短、语义密度均匀 |
| `chapter` | — | 章节短（<1500 字）或用户想「一章一页」 |
| `dialogue_turn` | `turns_per_seg=6` | 对话驱动的日常/群像戏 |
| `fixed` | `size, overlap` | 兜底，章节识别失败时 |
| `semantic` | `break_threshold` | 需要 LLM 断点，成本高，仅对高质量长段落启用 |
| `manual` | — | 用户框选 |

所有策略共享 `params_hash`，同书可并存多套结果，前端可切换对比。

### 6.2 两段式管线（核心）

```
规则层（0 成本，全量）             LLM 层（有成本，仅候选）
段落窗口切片 ──► 特征打分 ──► 百分位排序 ──► 按章配额取 Top-K ──► 结构化抽取(scene/角色/动作)
                                                                    │
                                                                    ▼
                                                        分镜规划(beat→panel)
```

**为什么两段式**：本书 51.6 万字全量过 LLM 约 38 万 token 输入，且大量心理独白/过渡段落毫无绘图价值。规则层先砍掉一半，LLM 只处理高价值窗口，成本与噪声同时下降。

### 6.3 特征与打分（已用真书标定）

| 特征 | 计算 | 权重 | 实测敏感度 |
| --- | --- | --- | --- |
| `dialogue` | 引号内字符占比 / 对话起始段占比 | +0.9 | 高（网文对话即冲突） |
| `action` | 动作动词密度（tanh 压缩） | +0.9 | 高 |
| `visual` | 可视名词密度（服饰/身体/环境/道具） | +0.7 | 中 |
| `scene` | 场景切换标记（忽然/窗外/第二天/抬头…） | +1.1 | 最高（转场最适合当一格） |
| `rhythm` | 平均段长越短越高（动作戏段落碎） | +0.7 | 中 |
| `length_fit` | 对 900 字的高斯贴合 | +0.5 | 低（防超长） |
| `monologue` | 心理连接词密度（所以/因为/毕竟/意识到） | **-1.2** | 高（负向） |

关键实现细节：**不要用 logistic 归一化**。首版用 `sigmoid` 后 p50=0.97，全部顶到 1，排序完全失效；改用**语料内百分位**（`bisect` 定位），阈值不再随书漂移。

### 6.4 实测结果（本书）

| 指标 | 值 |
| --- | --- |
| 总字数 / 章节 | 516,256 / 234 |
| 段落窗口数（target 900） | 475（均值 913 字） |
| 原始分范围 | 0.51 – 3.41（p50 = 1.94，p90 = 2.63） |
| 按「每章 1 个」配额选出 | 234 个 |
| 送往 LLM 的字数 | 214,184（占全书 **49.4%**） |
| 估算 LLM 输入 token | ≈ 16.1 万（0.75 token/字） |
| 选片通过率 | 49.3% |

Top 片段抽样符合直觉：

| 分数 | 章节 | 片段首句 | 判定 |
| --- | --- | --- | --- |
| 3.41 | 33 | 「（示例段落）」 | 掐脸/躲开 + 对话，双人同框 |
| 3.10 | 146 | 「（示例段落）」 | 视线交锋，特写 |
| 3.09 | 83 | 「（示例声音）」 | 敲门，强声音转视觉 |

结论：**规则层已经能选出可直接绘图的高价值片段**，LLM 层的主要作用不是「挑」，而是「结构化」——补出谁在场、什么动作、什么情绪、连续性如何。

### 6.5 配额与叙事连贯

单纯 Top-K 会挑出「全书最炸的 234 个瞬间」，读起来毫无连贯性。因此选片后再做一层**章节内规划**：

```
每章：至少 1 个 scene（保底，保证章节可读）
      若章节 >3000 字或含 2 个以上场景切换 → 2 个
      相邻 scene 必须共享 location 或 character，否则视为跳切，插入「过渡格」
每 scene：2–4 panel，叙事节奏固定为 起（establishing）→ 承（reaction）→ 转（action）→ 合（余韵）
```

分镜轨在 UI 上按章折叠，用户可拖拽增删，形成最终页序。

### 6.6 幂等与增量

- 分段结果以 `(book_id, chapter_id, params_hash)` 唯一约束缓存，改策略不覆盖旧结果。
- LLM 抽取以 `hash(segment_text + prompt_version)` 为键缓存；重跑只补缺。
- 编辑原文（v1 不支持，但接口预留）时按 offset 失效受影响 segment。

---

## 7. 模块 D：人物特征提取与固定

### 7.1 抽取时机

```
T1 首次出场：第 1 章渲染后，异步对前 3 章做一次「人物候选扫描」，产出 name/aliases
T2 单片段：每次 scene 抽取时，把该片段出现的角色名与已有卡片做匹配（别名归一）
T3 全书补全：用户点「补全人物卡」→ 分批扫描全书，只补空缺字段，不覆盖已冻结字段
```

### 7.2 只抽「画得出来的」字段

Schema 强约束（JSON Schema 校验 + 失败重试），且每个字段必须带原文证据：

```json
{"field": "hair.color", "value": "黑色", "quote": "一头黑发被风吹得乱翘", "ch": 3}
```

禁止抽取：性格、心理、身世、能力等级（这些不改变像素，写进提示词只会稀释权重）。姓名/别名的归一单独处理（「昵称」→ `char_a`）。

### 7.3 冲突消解

| 冲突类型 | 处理 |
| --- | --- |
| 同字段多说法（黑发 vs 墨发） | 同义归并表 + 出现频次优先；频次接近取最早出现 |
| 换装/成长（第 12 章换训练服） | 写成 `outfit.variants[{from_ch, desc}]`，按当前章节选版本 |
| 性别反转/变身（本书核心设定） | **版本化**：`card.version++` 并记录 `effective_from_ch`，旧章节仍用旧版 |
| 模型幻觉出新特征 | 无 `quote` 证据的字段直接丢弃 |

本书的「变身」设定是关键测试案例：主角主角A前期男性、后期形态变化，必须按章节区间取卡，不能全书一张卡。

### 7.4 冻结机制

```python
if card.frozen:
    # 自动流程只能写 variants / 新证据，不能改 identity_block
    raise FrozenError("identity_block 已冻结，请人工编辑或升版本")
```

冻结是整条链路的「地基」：一旦 `identity_block` 被后续抽取改动，前面所有已出图的分镜立即不一致。因此 v1 采用**冻结优先**：宁可漏掉后期设定，也不允许静默变更。

### 7.5 identity_block 的确定性渲染（一致性第一支柱）

```python
TEMPLATE = ("{name_en}, {age}-year-old {gender_word}, {hair_style} {hair_color} hair, "
            "{eye_shape} {eye_color} eyes, {skin} skin, wearing {outfit}, {accessories}")

def render_identity(card, chapter):
    outfit = pick_variant(card.outfit, chapter)
    block = TEMPLATE.format(**resolve(card, outfit))
    assert block == card.identity_block if card.frozen else True
    return block
```

规则：
- 拼装顺序固定（年龄→性别→发型→发色→眼→肤→服→配饰），永不因 LLM 输出顺序变化。
- 英文输出（MOSS 的文本编码器对英文属性词更稳），专有名词音译 + 中文原名做负向排除。
- 每张卡 `identity_block` **落库并存哈希**，出图前校验哈希未变，变了就报警而不是静默出图。
- 多角色时按 `character_id` 字典序拼接，而不是按 LLM 返回顺序。

---

## 8. 模块 E：分镜生成与一致性

### 8.1 为什么不让 LLM 直接写提示词

实测经验：同一段文字两次调用，LLM 写出的提示词在「发型」「服装」上措辞会漂移（short black hair / black short hair / dark hair），画图模型对措辞极敏感。因此拆成：

```
LLM 产出 storyboard JSON（结构化） → PromptBuilder（确定性代码拼装） → 最终 prompt
```

PromptBuilder 的拼装顺序（不可变）：

```
[STYLE] 画风 + 色调 + 线条 + 镜头语言 + 分辨率暗示        ← 全局配置，字面固定
[CAST]  按 character_id 排序的 identity_block × N         ← 冻结卡，字面固定
[SCENE] location + time_of_day + bg + light              ← 本格变量
[SHOT]  shot size + angle + composition                   ← 本格变量
[ACT]   每角色 pose + expr，按位置描述（left/right）        ← 本格变量
[EXTRA] key_props / fx
[NEG]   nsfw, worst quality, bad anatomy, watermark, jpeg artifact, extra fingers, text
```

预算控制：MOSS 提示词上限 4096 token，但 **1000 token 内最稳**；单角色 identity_block 约 60–90 token，4 人同框已接近上限 → 硬性限制「单格 ≤ 3 人」，超过则拆格或降级为剪影。

### 8.2 ComfyUI / MOSS 落地

Provider 用模板注入而非硬编码节点图，模板即配置：

```json
{
  "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "moss\\moss_image_sft_bf16.safetensors", "weight_dtype": "default"}},
  "2": {"class_type": "CLIPLoader", "inputs": {"clip_name": "moss\\qwen3.5_9b_int8_convrot.safetensors", "type": "moss_image", "device": "default"}},
  "3": {"class_type": "VAELoader", "inputs": {"vae_name": "moss\\flux2-vae.safetensors"}},
  "4": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{positive}}", "clip": ["2", 0]}},
  "5": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{negative}}", "clip": ["2", 0]}},
  "6": {"class_type": "EmptyFlux2LatentImage", "inputs": {"width": "{{width}}", "height": "{{height}}", "batch_size": 1}},
  "7": {"class_type": "KSampler", "inputs": {"model": ["1", 0], "positive": ["4", 0], "negative": ["5", 0],
        "latent_image": ["6", 0], "seed": "{{seed}}", "steps": "{{steps}}", "cfg": "{{cfg}}",
        "sampler_name": "{{sampler}}", "scheduler": "{{scheduler}}", "denoise": 1.0}},
  "8": {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["3", 0]}},
  "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "{{prefix}}"}}
}
```

推荐参数（来自本机实测文档）：

| 场景 | 尺寸 | steps / cfg | 耗时 |
| --- | --- | --- | --- |
| 分镜格（竖构图，质量优先） | 832×1216 | 20 / 2.5，er_sde + simple + SLG 0.5 | 41 s |
| 分镜格（草稿/试排） | 768×768 | 8 / 1.0，挂 8 步 LoRA | 10 s |
| 人物设定图 | 1024×1024 | 8 / 1.0 + 风格 LoRA | 20 s |

工程约束：ComfyUI 串行排队，Provider 内建**单飞锁 + 队列**；提交后轮询 `/history/{prompt_id}`，失败读 `status_str=error` 的堆栈；超时（默认 180s）先 `POST /interrupt` 再重试一次。

### 8.3 一致性五支柱

| 层 | 手段 | 失败时的表现 |
| --- | --- | --- |
| 1. 文本 | 冻结 identity_block + 固定拼装顺序 + 哈希校验 | 措辞漂移 → 发色/服型乱变 |
| 2. 种子 | `seed = hash(book, char_set, shot_key) % 2^31`，同角色同镜头族共享种子池 | 随机重排导致脸型变化 |
| 3. 参考图 | 人物设定图注入（模块 F） | 转向「像但不像」 |
| 4. 风格锁 | 全局 LoRA + 固定 style block + 固定分辨率 | 画风跳变 |
| 5. 校验 | 人脸 embedding 余弦 ≥ 0.55（可配），不合格换 seed 重试 ≤3 次 | 串脸、糊脸 |

本文档把「参考图注入」单列成模块 F，因为它是唯一能让弱模型也稳定复现人物的手段，也是本项目的核心卖点。

### 8.4 多人物同框的降级链

```
≤2 人  → 直接同框，位置锚点明确（left/right），各自参考图按顺序注入
3 人   → 尝试同框；若人脸校验有任一角色 < 阈值 → 拆成 2 格
>3 人  → 强制拆格；或走「群像模式」：只锁主角，其余按 generic 角色生成（不注入参考图）
```

### 8.5 拼页

- 网格模板：`1x1 / 2x1 / 2x2 / 2x3 / 3x2 / 1+2 / 2+1`，配置化，按 panel 数自动选。
- 后端 Pillow 批量合成（白边 8px、圆角、外框），前端 Canvas 仅做预览。
- **文字后置**：caption 与 dialogue 不进模型，拼页后用排版层绘制（字体、字号、气泡形状可配）。理由：MOSS 中文渲染虽强，但在 200px 宽的气泡里必然糊；后置还让文案可改、可翻译。
- 导出：`pages/p033.png` + 整本 `book.cbz`。

---

## 9. 模块 F：参考图注入

### 9.1 三阶段流程

```
阶段 1  建卡    LLM 抽卡 → 人工确认字段 → frozen
阶段 2  建参考图 每角色生成 3–5 张设定图（正面/侧面/半身/表情）→ 人工挑选 → 存 refs
阶段 3  注入出图 分镜请求 = identity 文本 + 参考图 + 镜头差异描述
```

阶段 2 的产出直接决定上限。建议提示词模板：

```
[STYLE] + [IDENTITY] + "character reference sheet, front view, side view, three-quarter view,
neutral pose, plain grey background, full body, consistent design, no text"
```

负面加 `multiple characters, cropped head, different outfits`。一次出 4 张（`batch_size` 会翻倍显存，12GB 卡建议 2 张 × 2 次），人工点选 1–2 张作为正式参考。

### 9.2 注入实现：能力探测 + 三级降级

```python
def choose_ref_mode(provider, settings):
    info = provider.object_info()          # GET /object_info
    names = info.keys()
    if settings.reference.mode == "off":      return "off"
    if has_any(names, ["ReferenceLatent", "FluxKontextImageScale", "MossImageEdit"]):
        return "edit"        # 原生参考图/编辑节点：保结构最强
    if has_any(names, ["IPAdapterApply", "IPAdapterAdvanced"]):
        return "ipadapter"   # 权重 0.6–0.8，需配 faceid/plus 模型
    if {"LoadImage", "VAEEncode"} <= set(names):
        return "img2img"     # 降级：参考图编码进 latent，denoise 0.55–0.7
    return "off"             # 只靠文本 + seed，并提示用户风险
```

> **已实测（2026-11-12，`GET /object_info` + 实机出图）**：本机存在原生参考图编辑节点 `TextEncodeMOSSImageEdit`（`clip` / `prompt` / `vae` / `edit_format` / 多图 Autogrow / `image_strength`），并已装 `moss_image_edit_lora_bf16`，**无需降级**，直接走 `mode=edit`。
>
> 三点与预期不同，必须按实测修正：
> 1. 实际路线是 MOSS 原生编辑节点，不是 IP-Adapter（`clip_vision` 目录为空，44 个 IPAdapter 节点全部不可用），也不是 Kontext/Qwen-Edit（无权重）。
> 2. API 序列化必须用点号扁平键 `images.image_0`，嵌套字典会报 `required_input_missing`。
> 3. `image_strength` 在 `edit_format=Moss-Image-Edit`（视觉通道关闭）时**完全无效**，不要作为可调参数暴露。
>
> 完整定义、三种 format 的行为差异、多参考图的三次对照实验，见 `docs/TRANSFORM.md` 第 2–3 节。

### 9.3 参考图 + 文本的分工

| 渠道 | 负责 | 不负责 |
| --- | --- | --- |
| identity_block 文本 | 命名特征（发色、服型、年龄、配饰） | 精确脸型相似度 |
| 参考图 | 脸型、五官比例、画风统一 | 姿态与镜头（必须由本格描述覆盖） |
| 镜头描述 | 构图、景别、朝向、表情 | 身份 |

三者冲突时的优先级：**镜头描述 > 参考图的结构 > 文本特征**。所以提示词必须显式写「same character as reference, new pose/shot」，否则模型倾向复刻参考图姿态（img2img 模式下尤其严重）。

### 9.4 缓存「已验证组合」

一致性调试的本质是搜索：同角色 × 同镜头族 × 不同 seed，逐个看脸。系统要缓存：

```
(character_id, shot_family, ref_id, provider, style_hash) -> best_seed, face_score, accepted_at
```

下次遇到同族镜头，直接从 `best_seed` 起步，把「3 次重试」压缩到「1 次就中」。本书 234 章、约 900 格，这个缓存能省掉大量显存时间。

---

## 10. 模块 G：配置管理

### 10.1 分层配置

```
config/
  providers.yaml     # 文本/图像/视觉 provider 列表与凭据引用（凭据只存 env 名）
  prompts/*.j2       # 提示词模板（Jinja2），带 version 字段
  styles/*.yaml      # 画风预设：style_block + negative + 推荐 LoRA + 参数
  segmentation.yaml  # 默认分段/选片参数
  app.yaml           # 端口、并发、路径、日志
```

单一来源：所有配置用 JSON Schema 定义，Python 侧 pydantic、TS 侧由 schema 生成类型，避免前后端字段漂移。

### 10.2 Provider 抽象

```python
class TextProvider(Protocol):
    async def complete(self, *, system: str, user: str,
                       schema: dict | None = None,
                       temperature: float = 0.3) -> dict | str: ...

class ImageProvider(Protocol):
    async def render(self, job: RenderJob) -> RenderResult: ...
    def object_info(self) -> dict: ...          # 能力探测
```

实现：`OpenAICompat`（任意 base_url，覆盖 DeepSeek/Qwen/本地 vLLM）、`Ollama`、`LlamaCpp`、`ComfyUI`、`SDWebUI`、`Mock`（离线跑通全链路，CI 用）。

统一要求：**结构化输出必须走 JSON Schema 校验**，失败自动重试一次（把校验错误回贴给模型），再失败则该片段标记 `needs_review` 而非中断整批。

### 10.3 模板系统

```jinja
{# prompts/segment_extract.j2  version: 3 #}
你是漫画编剧。下面是小说片段，请抽取可绘制的分镜要素。

人物白名单（只能从这里选 id）：{{ characters | map(attribute='id') | join(', ') }}
{% if prev_summary %}前情：{{ prev_summary }}{% endif %}

片段：
{{ segment_text }}

严格输出 JSON，字段：{{ schema_hint }}
```

规则：
- 变量白名单（Jinja2 `StrictUndefined`），模板里出现未传变量立即报错，避免静默出空。
- 模板文件带 `version`，与 `runs.config_snapshot` 一起存，保证「三个月后重跑得到同样结果」。
- 前端提供模板编辑器 + 「用当前片段试跑」按钮，改动即时看效果。

### 10.4 运行快照

```json
{
  "run_id": "r-20261112-001",
  "book_sha256": "…",
  "text_provider": {"name": "openai-compat", "model": "deepseek-v3", "temperature": 0.3},
  "image_provider": {"name": "comfyui", "base": "127.0.0.1:8188", "template": "moss.v1"},
  "templates": {"segment_extract": 3, "storyboard": 2, "panel_prompt": 5},
  "style": {"name": "ink-wash", "lora": ["kzmynon-moss-v1:1.0"], "width": 832, "height": 1216,
            "steps": 20, "cfg": 2.5, "sampler": "er_sde"},
  "segmentation": {"strategy": "paragraph_window", "target_chars": 900, "overlap_paras": 1},
  "reference": {"mode": "auto", "strength": 0.65}
}
```

每个 panel 存 `config_hash = sha256(snapshot + prompt + seed)`。任意一格「重 roll」= 复制快照改 seed；「复现」= 用快照重跑。

---

## 11. 模块 H：任务编排与成本

### 11.1 队列

- 单表 `jobs` + `asyncio` worker，`status IN (queued, running, done, failed, canceled)`。
- 幂等键：`type + payload_hash`，重复入队直接返回已有结果。
- 任务粒度：`segment_chapter` / `extract_characters` / `plan_storyboard` / `render_panel` / `compose_page`。粒度细才能「单格重 roll」和「暂停续跑」。
- ComfyUI 侧并发 = 1（12GB 显存硬约束），文本侧并发 4–8（可配）。
- 进度通过 SSE `/api/events?run_id=` 推送，前端显示「234 格中已出 87 格」并支持取消。

### 11.2 成本估算（基于本书实测）

| 环节 | 数量 | 单价 | 合计 |
| --- | --- | --- | --- |
| 分段（规则） | 234 章 / 475 段 | 0 | 秒级 |
| LLM 片段抽取 | 234 次调用 / 16.1 万输入 token | 视 provider | 本地模型 ~15–30 min |
| 人物卡 | 8 主角 × 3 轮 | — | 分钟级 |
| 人物设定图 | 8 人 × 4 张 = 32 张 | 20 s | ≈ 11 min |
| 分镜格 | 234 scene × 4 格 ≈ 936 张 | 41 s（质量）/ 10 s（草稿） | **10.6 h / 2.6 h** |
| 重 roll 预期 +35% | +328 张 | 41 s | +3.7 h |
| 拼页 | 234 页 | <1 s | 分钟级 |

结论：整本精修约 **14 小时** 单机串行；草稿模式（8 步 + 768 宽）约 **3.5 小时**。因此产品上必须提供「草稿模式 → 人工筛 → 精修选中页」的两级工作流，而不是一键全书精修。

---

## 12. 前端交互

### 12.1 布局

- 顶栏：书名 / 进度 / 模式切换（阅读 · 漫改 · 设置）
- 阅读区：foliate-view 占满，右侧 380–440px 分镜轨
- 分镜轨：按章分组，卡片显示缩略图 + 首句 + 状态徽标（待生成/生成中/待审/已通过）
- 底部：章节滑块 + 生成本章按钮 + 全局进度

### 12.2 关键交互

| 操作 | 行为 |
| --- | --- |
| 选中正文 | 浮动菜单：生成分镜 / 添加人物 / 复制 |
| 点击分镜卡 | 跳原文 + 高亮来源段落 |
| 双击分镜图 | 打开大图 + 重 roll + 调 seed + 改提示词（临时覆盖） |
| 长按分镜图（移动端） | 同双击 |
| 拖拽分镜卡 | 调整页内顺序 / 移到相邻页 |

### 12.3 移动端注意事项

- 分镜轨改底部抽屉（`max-height: 62vh`），手势下滑关闭。
- iframe 内禁用双指缩放冲突：`touch-action: pan-y` 交给 paginator，缩放用外层容器。
- 出图/拼页在服务端完成，移动端只拉缩略图（`?w=480`），避免 936 张原图打爆流量。

---

## 13. 目录结构

```
ai-reading-plus/
├─ foliate-js/                 # git submodule（锁 commit）
├─ web/                        # Vite + TypeScript
│  ├─ src/reader/              # txt-book.js, locator.js, view-host.ts, theme.css
│  ├─ src/studio/              # segment-list, character-card, storyboard, page-preview
│  ├─ src/settings/            # provider / template / style 表单
│  └─ src/shared/              # api client, sse, types(由 schema 生成)
├─ server/
│  ├─ app/api/                 # books, segments, characters, scenes, panels, pages, runs
│  ├─ app/pipeline/            # importer, segmenter, selector, characters, storyboard, prompt, compose
│  ├─ app/providers/           # text_openai.py, image_comfy.py, mock.py
│  ├─ app/db/                  # models.py, migrations/
│  └─ app/core/                # config, queue, events, cache
├─ config/                     # providers.yaml, prompts/*.j2, styles/*.yaml
├─ prototype/spike_segment.py  # 分段策略验证脚本（已跑通）
├─ book/                       # 测试书籍
├─ assets/  out/
└─ docs/
```

---

## 14. 里程碑

| 阶段 | 交付 | 验收标准 |
| --- | --- | --- |
| M0 骨架 | 前端壳 + FastAPI + SQLite + 合成 Book | 本书 51.6 万字导入 < 3s，翻页无卡顿，进度可恢复 |
| M1 阅读完整体验 | 目录、书签、主题、搜索、移动端 | iPhone SE / iPad / 桌面三档布局无溢出；搜索 234 章 < 1s |
| M2 分段与人物 | 规则分段 + LLM 抽取 + 人物卡 | 本书 234 scene 抽取成功，JSON 校验通过率 ≥ 98%；8 主角身份卡人工确认 |
| M3 单格出图 | PromptBuilder + ComfyUI + 参考图注入 | 同一角色 10 格中 ≥ 8 格人脸相似度 ≥ 0.55 |
| M4 成页 | 拼页 + 文字后置 + CBZ 导出 | 前 5 章产出 20 页，人工评分 ≥ 4/5；CBZ 可在阅读器中双模式打开 |

---

## 15. 风险与对策

| 风险 | 影响 | 对策 |
| --- | --- | --- |
| foliate-js 自述「不稳定，API 可能变」 | 升级破坏 | submodule 锁 commit，适配层只依赖 `view.open` / `relocate` / `sections` 三个契约 |
| 模型无参考图节点 | 一致性不达标 | 能力探测 + img2img 降级 + 「种子池 + 严格文本卡」兜底；文档明确告知上限 |
| LLM 输出 JSON 不合规 | 批次中断 | Schema 校验 + 错误回贴重试 1 次 + `needs_review` 隔离，绝不整批 fail |
| 12GB 显存 OOM | 出图中断 | 串行 + bf16/int8 双模板切换 + 分辨率降档；OOM 错误码自动换 int8 模板重试 |
| 全书精修 14 小时 | 用户放弃 | 草稿/精修两级 + 按章增量 + 断点续跑 + 后台跑 |
| 变身/成长设定导致形象漂移 | 前后页人物不符 | 卡片版本化 + `effective_from_ch` + 章节区间取卡 |
| 版权与内容安全 | 合规风险 | 仅处理用户本地文件；出图负面词固定含 `nsfw`；不做公开分享 |

---

## 16. 验证计划（用 book/ 中的书）

已完成（`prototype/spike_segment.py`，仅标准库，可直接跑）：

```bash
python prototype/spike_segment.py     # 输出 docs/spike_report.json
```

1. **导入验证**：`utf-8-sig` 正确识别；234 章全部命中；章首标签行与书头信息块已剥离。
2. **分段验证**：475 个窗口，均值 913 字，与 `target_chars=900` 一致。
3. **打分验证**：原始分 0.51–3.41 分布正常，Top 片段人工抽查 5 例全部可绘图。
4. **成本验证**：按每章 1 scene 配额，LLM 输入降至全书 49.4%，约 16.1 万 token。

待补验证（M2/M3 阶段）：

5. 用 5 个场景试跑 LLM 抽取，统计 JSON 合规率与「角色识别正确率」。
6. 抽取 3 个主角卡（主角A / 角色B / 角色C，含变身版本），人工比对原文。
7. `GET /object_info` 探测参考图节点，确定实际注入模式。
8. 同一角色 10 格横评：纯文本 vs 文本+参考图，人脸相似度与人工评分对比。

---

## 附录 A：关键接口

```
POST   /api/books                     上传并解析
GET    /api/books/{id}/chapters/{idx} 章节正文（阅读器按需拉取，可选）
POST   /api/books/{id}/segment        {strategy, params}
POST   /api/books/{id}/characters/extract  {scope: first_n|full, names?}
PATCH  /api/characters/{id}           人工修正 / 冻结 / 升版本
POST   /api/characters/{id}/refs      生成或上传参考图
POST   /api/scenes/{id}/storyboard    生成分镜
POST   /api/scenes/{id}/render        {quality: draft|final}
POST   /api/panels/{id}/reroll        {seed?, prompt_override?}
POST   /api/pages/compose             {chapter, layout?}
GET    /api/events?run_id=            SSE 进度
GET    /api/runs/{id}                 运行快照与统计
```

## 附录 B：与 MOSS-API 文档的对应关系

| 本文档 | MOSS 文档 |
| --- | --- |
| 8.2 节点图模板 | §3.1 提交 / §4 节点说明 |
| 8.2 参数表 | §5 参数建议 / §8 实测性能 |
| Provider 轮询与取图 | §3.2 /history、§3.3 /view |
| 取消与超时 | §10 取消任务（/interrupt） |
| 能力探测 | §2 /object_info |
| 参考图降级 | §4 可选节点（当前未列参考图节点，需探测） |
| 错误处理 | §9 常见错误对照 |
