# 变身类设定的处理方案（基于本机节点实测）

> 环境：ComfyUI 0.44.2 / RTX 5070 12GB / 3207 个节点
> 实测时间：2026-11-12，全部通过 `GET /object_info` 读取真实定义 + 实机出图验证
> 相关：`docs/ARCHITECTURE.md` 模块 D / E / F

---

## 1. 结论先行

**推荐方案：形态时间轴 + 单形态单参考图 + 面板级形态解析 + 过渡态离线合成。**

不做"双参考图运行时融合"。原因是实机跑出来的：把"前身图 + 后身图"同时塞给编辑节点，要么后图被完全忽略（首图主导），要么两张图的属性互相渗透（蓝短发与橙长发同框、书本与鲸鱼同时抱在手里）——**没有一种 format 能让模型理解"这两个是同一个人先后两种形态"**。

所以变身在工程上不是一个"图像融合"问题，而是一个**状态机 + 资产切换**问题：任意时刻只有一种形态生效，切换靠换参考图与换身份文本，过渡瞬间靠分格或离线合成的中间态来表现。

---

## 2. 你现有的编辑类节点（真实结构）

### 2.1 `TextEncodeMOSSImageEdit` — 唯一的原生编辑入口

| 输入 | 类型 | 说明 |
| --- | --- | --- |
| `clip` | CLIP | **必须配 `type=moss_image`** |
| `prompt` | STRING | 编辑指令 |
| `vae` | VAE（可选） | 接上才把参考图编码为 `reference_latents`；不接则参考图只进文本编码器 |
| `edit_format` | COMBO | `ai-toolkit` / `ai-toolkit (kv_cache off)` / `Moss-Image-Edit` |
| `images` | Autogrow `image_` | 动态多图槽，`min=1`，默认最多 10 个槽 |
| `image_strength` | FLOAT 0–100，默认 1.0 | 只作用于**文本编码器看到的图**，见 2.2 |

源码里两条通道是**互相独立**的（`comfy_extras/nodes_moss_image.py`）：

```
通道 A｜视觉通道：每张参考图缩到 ≤384×384，在 prompt 前面自动拼
        "Picture 1: <|vision_start|><|image_pad|><|vision_end|>…"
        image_strength 以 log(strength) 写进 context_attention_bias，只调视觉 token 权重
通道 B｜潜空间通道：每张图 VAE 编码成 reference_latents，method 由 edit_format 决定
```

`EDIT_FORMATS` 的真实映射：

| 选项 | reference_latents_method | 视觉通道 | 后果 |
| --- | --- | --- | --- |
| `ai-toolkit` | `index_timestep_zero_isolated` | 开 | 文本能"看到"图，能用 Picture N 指代；参考图隔离，不参与每步重算 |
| `ai-toolkit (kv_cache off)` | `index_timestep_zero` | 开 | 同上，但参考图每步重算，更贴图、更慢 |
| `Moss-Image-Edit` | `index_timestep_zero_text_start` | **关** | 只有 latent 约束，文本无法区分哪张是谁；最稳、最不串 |

三个必须知道的细节：

1. **`image_strength` 在 `Moss-Image-Edit` 格式下完全无效**。源码条件 `if (vision or vae is None) and image_strength > 0` —— vision 为 False 且接了 vae 时，这段根本不执行。想用它调"形态相似度"是徒劳的。
2. **参考图按 `sorted(images)` 字符串排序**。`image_10` 会排在 `image_2` 前面。超过 10 张（或改名不规范）顺序就乱了，而顺序在这里就是语义。
3. **API 序列化的键是点号扁平形式**。写成嵌套 `{"images": {"image_0": …}}` 会报 `required_input_missing: image_0`；必须写成 `{"images.image_0": ["6",0], "images.image_1": ["7",0]}`（实测通过）。

### 2.2 构图与控图节点

| 节点 | 输入要点 | 对变身的作用 |
| --- | --- | --- |
| `MOSSImageLayoutPrompt` | `element`(ARRAY) + `width/height` + `character_boxes="1, 3"` | 把指定序号的 box 标成 `character`，bbox 用 0–1000 归一化 `[y0,x0,y1,x1]`，输出以**画布中心为原点**的像素坐标 JSON。**管构图不管身份** |
| `MossImagePromptBuilderKJ` | `elements_data` / `style_palette_data` / `canvas` | KJ 版版式构建器，带 bbox 预览图 |
| `TE_image_pro_moss_layout` | `快捷功能`：白底三视图 / 白底四视图 / 九宫格[导演]分镜 / 四宫格[剧情]分镜 / 四宫格[视角]分镜；`增强方式`：CLIP / 本地 llama.cpp / API | **已内置"白底三视图"和分镜版式**，正是形态设定图与分镜页要的模板 |
| `MOSSImageControl` | `color_map` + `depth_map` + `strength` + `start/end_percent` | 需要配 color/depth control adapter LoRA。**当前 loras 目录里没有这个 adapter，节点在但不可用** |
| `ImageStitch` | `image1/image2` + 方向 + 间距 | 可把两形态并排拼成一张参考图（绕开多图槽的顺序问题） |

### 2.3 能力盘点：装了节点 ≠ 能用

`/object_info` 有 3207 个节点，但实际能跑的只有 MOSS 一条线：

| 路线 | 节点 | 模型 | 结论 |
| --- | --- | --- | --- |
| MOSS 文生图 | 齐全 | `moss_image_sft_bf16 / int8` + `qwen3.5_9b_int8` + `flux2-vae` | **可用**，41s/张（832×1216, 20 步） |
| MOSS 编辑 | `TextEncodeMOSSImageEdit` | `moss_image_edit_lora_bf16` 已装 | **可用**，实测 20s（640×832, 20 步） |
| IPAdapter / FaceID | 44 个节点全在 | `clip_vision` 目录**为空**，无 ipadapter 模型 | **不可用**。这是最容易踩的坑：节点列表看起来支持参考图，实际跑必报错 |
| Qwen-Image-Edit / Flux Kontext | `TextEncodeQwenImageEditPlus`、`FluxKontextImageScale` 等 | `diffusion_models` 里只有 moss 与 minimax | **不可用**，缺权重 |
| MOSS color/depth 控制 | `MOSSImageControl` | 缺 adapter LoRA | **不可用** |

另外：`MossCLIPLoader` 支持 `adapter`（`qwen3.5-{0.8,2,4}b_w4a8_linear`），说明小编码器必须走它；直接用 `CLIPLoader` 挂 2b 会在 KSampler 报 `mat1 and mat2 shapes cannot be multiplied (43x2048 and 4096x3072)` —— 这是 3072 维小编码器对 4096 维 MOSS 的维度错配，我实测踩到了。

---

## 3. 三次实测：多参考图到底能不能表达"变身"

统一设置：MOSS int8 底模 + 9B int8 编码器 + edit LoRA + `TextEncodeMOSSImageEdit` + KSampler 20 步 / cfg 2.5 / er_sde / simple，640×832，单次 **18–20 s**。

参考图：A = 橙长发白裙持书少女，B = 蓝发女仆装少女（两个**不同角色**，用来模拟"前后两形态"）。

| # | edit_format | prompt 意图 | 结果 |
| --- | --- | --- | --- |
| 1 | `Moss-Image-Edit` | 只描述形态 A | 输出≈A，**B 完全无痕迹** |
| 2 | `ai-toolkit` | "画 A 的脸，换成 B 的发色和女仆装" | 发色/服装听话了，但**整体形象变成 B**，脸也偏 B |
| 3 | `ai-toolkit` | "保持 A 的脸不变，只把头发改成蓝色短发" | **属性混叠**：蓝色短发顶上、橙色长发拖在身后；白裙配深蓝袖；书本与鲸鱼玩偶同时抱在怀里 |

三条结论，直接决定方案取向：

1. **`Moss-Image-Edit`（vision 关）下，第一张参考图主导，后续图基本不参与身份**。好处是绝不串味，坏处是无法用文本指定"哪个特征来自哪张"。
2. **开视觉通道后，文本可控性上升，但模型把多张图当作"同一个体的多视角"处理**，于是发生属性渗透（发色混、道具叠、服装拼接）。
3. **多图注入不是可靠的"特征路由"手段**。想"取 A 的脸 + 取 B 的衣服"，靠提示词和开图顺序都只是概率性碰运气。

> 这也解释了为什么业界做角色一致性都走"单角色单参考 + IP-Adapter/Reference 权重"或"每角色训 LoRA"，而不是"多图拼接靠嘴说"。

---

## 4. 变身的数据与状态衔接

### 4.1 把「形态」提为一等公民

一个角色有 N 个形态，形态是**离散枚举**，不是连续参数。表结构：

```sql
CREATE TABLE character_forms (
  id TEXT PRIMARY KEY,
  character_id TEXT NOT NULL,
  form_key TEXT NOT NULL,              -- base | awakened | partial_1 | adult ...
  label TEXT NOT NULL,                 -- "主角A·男身" / "主角A·魔女形态"
  kind TEXT NOT NULL,                  -- base | derived | transition
  effective_from_ch INTEGER NOT NULL,
  effective_to_ch INTEGER,             -- NULL = 至今
  from_form_id TEXT,                   -- kind=transition 时指向起点形态
  to_form_id TEXT,                     -- kind=transition 时指向终点形态
  identity_block TEXT NOT NULL,        -- 冻结，字面固定
  identity_hash TEXT NOT NULL,
  card_json TEXT NOT NULL,             -- hair/eyes/body/outfit 等结构化字段
  reversible INTEGER DEFAULT 1,        -- 0 = 不可逆（真·变身）
  locked INTEGER DEFAULT 0,
  version INTEGER DEFAULT 1,
  UNIQUE(character_id, form_key)
);

CREATE TABLE form_refs (
  id TEXT PRIMARY KEY,
  form_id TEXT NOT NULL,
  kind TEXT,                           -- sheet | closeup | full_body
  path TEXT NOT NULL,
  seed INTEGER,
  accepted INTEGER DEFAULT 0,
  face_embedding BLOB,
  form_version INTEGER NOT NULL        -- 绑定的卡版本，防图文不同版
);

CREATE TABLE panel_forms (
  panel_id TEXT NOT NULL,
  character_id TEXT NOT NULL,
  form_id TEXT NOT NULL,
  source TEXT NOT NULL,                -- timeline | explicit | carry_over | llm_infer
  confidence REAL,
  PRIMARY KEY (panel_id, character_id)
);
```

**关键点**：`identity_block` 与 `form_refs` 必须绑定同一个 `form_version`。图文不同版是最隐蔽的 bug —— 图是旧发型、文本是新发型，出图后谁看都觉得"有点不对但说不上哪不对"。

### 4.2 时间轴要能表达"章内变身"

只用 `effective_from_ch` 不够。变身往往发生在某一章的中段，同一章前半是男身、后半是女身。所以额外存**事件锚点**：

```json
{
  "form_id": "char_a_awakened",
  "anchor": {
    "chapter": 33,
    "para_from": 0,
    "para_to": 12,
    "keyword": "觉醒",
    "mode": "inclusive_start"
  }
}
```

解析顺序（先到先得，命中即停）：

```
1. panel_forms 显式记录（人工或 LLM 标注）      source=explicit
2. 场景/段落锚点命中                            source=timeline
3. 上一格已确认形态 carry-over                  source=carry_over
4. 章节区间兜底（effective_from_ch ≤ ch < to）   source=timeline
5. 回落到 base
```

`carry_over` 这一条很重要：变身文里连续几十格都是同一形态，不该每格都重新判断。只有检测到"形态变化信号"才打断 carry-over。

### 4.3 面板级而非场景级

一个 scene 里"前 2 格还是男身、第 3 格觉醒、第 4 格已是女身"是常态。所以 `form_resolution` 必须存在 **panel 粒度**，scene 只存"本场涉及的形态集合"用于预加载与校验。

出图时传给 Provider 的载荷：

```json
{
  "panel_id": "p033-3",
  "cast": [
    {"char": "char_a", "form": "char_a_awakened", "form_version": 2,
     "identity_block": "Xing Xia, ... (frozen v2)",
     "ref": "assets/characters/.../awakened_sheet_01.png",
     "ref_scale": [640, 832]}
  ],
  "shot": {"size": "medium", "angle": "low", "composition": "单人居中"},
  "bg": "宿舍室内，夜晚",
  "edit_format": "Moss-Image-Edit",
  "image_strength_note": "vision=off，此参数无效，不要暴露给用户调"
}
```

### 4.4 过渡态：离线合成，不要运行时融合

变身瞬间如果想画"半变身"，做法是**预先生成一张中间形态的设定图**，把它当成第三张 ref（`kind=transition`，`from_form`/`to_form` 指向两端）。

```
前身设定图 ─┐
            ├─→ (离线, 人工审) ─→ 中间态设定图 ─→ 冻结为 form
后身设定图 ─┘
```

这张中间图怎么出？用实测结论：**单图 + 文本**，而不是双图混合。即用后身图作 ref，prompt 描述"部分变化的中间状态"（发色已变、校服未换），出 4–6 张挑 1 张，人工确认后冻结。一次投入，全书复用。

如果确实要"渐变"的多帧效果，再把 N 个中间态排成序列，用已装的 WanVideo / LTXVideo 做首尾帧插值生成过程动画 —— 但这是 v1.1 的可选项，成本高、时序一致性差。

### 4.5 状态机

```
        ┌──────────┐  觉醒事件(anchor)   ┌──────────────┐  稳定      ┌────────────┐
        │ base 男身 │ ─────────────────→ │ transition   │ ────────→ │ awakened   │
        └──────────┘                    │ 半变身(1~3 格)│           │ 魔女形态    │
              ▲                         └──────────────┘           └────────────┘
              │                                                          │
              │  仅当 reversible=1 且显式 override（回忆/闪回）              │ 默认不可逆
              └──────────────────────────────────────────────────────────┘
```

规则：
- `reversible=0` 的形态，正向切换只允许一次；检测到回退请求时**报冲突**（`FormRegressionError`），由人工确认是"回忆"还是"设定矛盾"。
- 回忆/闪回不修改全局时间轴，只在 scene 上打 `form_override`，并标记 `flashback=true`（前端用灰边框区分）。

---

## 5. 编辑节点怎么适配切换

### 5.1 请求构造（推荐路径）

```
UNETLoader(int8) ─┐
                  ├→ LoraLoaderModelOnly(moss_image_edit_lora_bf16, 1.0) → KSampler.model
CLIPLoader(9b, moss_image) ─┬→ TextEncodeMOSSImageEdit(pos)
VAELoader(flux2-vae) ───────┘
LoadImage(form_ref) → ImageScale(目标画幅) → "images.image_0"
EmptyFlux2LatentImage(W,H) → KSampler.latent_image
```

硬性规则：

| 规则 | 原因 |
| --- | --- |
| 单请求只挂 **1 张**形态参考图 | 实测多图会主导/混叠 |
| 参考图必须先 `ImageScale` 到目标画幅 | 节点文档明确"references are used at the size they come in" |
| `edit_format` 固定 `Moss-Image-Edit` | vision 关 = 最不串；且身份只由 latent 决定，行为可预测 |
| prompt 里**不再写**"Picture 1" | vision 关时这些标记不会被注入，写了也没用 |
| 编码器必须 9B（或 `MossCLIPLoader` + adapter） | 2b 直连必 shape mismatch |
| `image_strength` 不外露 | 该格式下无效，暴露给用户只会制造"调了没用"的困惑 |

### 5.2 切换协议（换形态 = 一次原子替换）

```python
def switch_form(panel, char_id, new_form):
    assert new_form.locked, "形态未冻结，禁止出图"
    ref = pick_accepted_ref(new_form)          # 必须有 accepted=1 的参考图
    assert ref.form_version == new_form.version, "图与卡版本不一致"
    if not form_active_at(new_form, panel.chapter, panel.para):
        raise FormRegressionError(...)
    return RenderJob(
        ref=ref.path,
        prompt=build_prompt(panel, identity=new_form.identity_block),
        edit_format="Moss-Image-Edit",
        seed=seed_for(new_form.id, panel.shot_family),
    )
```

三个"必须"：必须已冻结、必须有已验收参考图、必须版本一致。缺一个就拒绝出图，而不是"先出着看看"——否则错误会静默扩散到整章。

### 5.3 双形态同框（变身瞬间）的三条路

| 方案 | 做法 | 成本 | 适用 |
| --- | --- | --- | --- |
| **A. 拆格（推荐）** | 前一格 from_form、后一格 to_form | 2 张图 | 默认。漫画语法本来就该用分格表现变化 |
| B. 单格双人 + layout | 两张 ref + `MOSSImageLayoutPrompt` 的 `character_boxes` 标两个 character | 1 张图 + 调参 | 需要"同框对峙/面对面"的镜头；实测会混叠，必须人工审核 |
| C. 分层合成 | 两形态各出透明底 → `RMBG` 抠图 → `ImageStitch` 拼 | 2 张图 + 抠图 | 要求严格不串、且是封面级画面时 |

方案 B 的实测风险要写进产品文案：同一格出现两个形态时，系统默认标注"待人工确认"，不自动通过。

---

## 6. 边界情况与冲突点

| # | 情况 | 现象/风险 | 处理 |
| --- | --- | --- | --- |
| 1 | 参考图 ≥10 张 | `sorted()` 字符串排序，`image_10` 插到 `image_2` 前 | 单请求 ≤3 张；命名 `image_0..`；代码层断言 |
| 2 | 用 `image_strength` 调形态强度 | `Moss-Image-Edit` 下无效 | UI 不暴露；仅 `ai-toolkit` 格式显示 |
| 3 | 参考图未缩放 | 尺寸不一致，构图错位/显存暴涨 | 强制 `ImageScale` 到画幅，写入 Provider 模板 |
| 4 | 用 2b/4b 小编码器 | KSampler 维度错配报错 | 必须 `MossCLIPLoader` + adapter，或直接 9B |
| 5 | 同 scene 内形态混合 | scene 级只存一个 form 会画错 | `panel_forms` 面板级解析 |
| 6 | 代词误判（他/她混用） | 变身文代词不可靠 | 代词只作弱信号；以时间轴为准；LLM 推断需带 quote 证据 |
| 7 | 形态内换装 vs 形态切换 | 两层覆盖互相打架 | 结构上 `form.outfits[]`，variant 属于 form，不跨层 |
| 8 | 回忆/闪回 | 误判为设定回退 | scene 级 `form_override` + `flashback` 标记，不改时间轴 |
| 9 | 不可逆形态被回退 | 前后矛盾 | `reversible=0` 时抛 `FormRegressionError`，人工裁决 |
| 10 | 卡已冻结但参考图是旧版 | 图文不同版，隐蔽错位 | `form_refs.form_version` 强校验 + 出图前断言 |
| 11 | 中间态随手用双图混合生成 | 属性渗透，前后都对不上 | 中间态必须用"单图 + 文本"离线生成并冻结 |
| 12 | 双形态同框自动通过 | 混叠图进入成页 | 该格 `status=needs_review`，默认不自动放行 |
| 13 | 形态改了但要重跑旧章 | 前面已出的图不一致 | 形态升版 → 受影响 panel 标 `stale`，按 `config_hash` 列出重跑清单 |

---

## 7. 推荐方案与理由

**形态时间轴 + 单形态单参考 + 面板级解析 + 过渡离线合成**

理由，按权重排序：

1. **可预测**。单图编辑 20s/张，质量方差小；多图融合的结果方差大，无法预估返工率。全书 900 格，方差就是成本。
2. **可校验**。形态是离散事实，能被规则静态检查（"第 100 章出现男身"直接报矛盾），而"50% 变身"这种连续状态无法校验。
3. **可解释**。用户看到某一格画错，能立刻定位是"形态归属错了"还是"参考图选错了"还是"文本描述不符"，三段可分开修。
4. **成本最低**。切换只换资产，不重训、不重跑历史。
5. **贴合漫画语法**。变身本来就是"分格 + 特写 + 光效"表现的，不是靠一张图里两个人渐变。

---

## 8. 备选方案对比

| 方案 | 做法 | 适用场景 | 代价 |
| --- | --- | --- | --- |
| **B1 双 ref 运行时融合** | 前身图 + 后身图同时注入，靠 prompt 路由特征 | 仅当模型具备"多主体绑定"能力（如 Qwen-Image-Edit 的 image1/2/3 明确指代）。**当前 MOSS 不具备** | 实测会混叠；需人工逐格审核 |
| **B2 每形态一个 LoRA** | 每形态 20–50 张素材训练专属 LoRA | 形态差异极大（性别/种族/年龄跨度）、长期连载、追求最高一致性 | 训练成本 + 维护成本；换底模要重训 |
| **B3 单卡纯文本切换** | 不换参考图，只改 identity_block 描述 | 形态差异小（换发色、换装、换配饰），或参考图质量差时兜底 | 脸型会随措辞漂移，需要频繁重 roll |
| **B4 分层合成** | 两形态各自出图 → 抠图 → 拼接 | 封面、跨页大图、要求严格不串的高价值画面 | 2 倍出图 + 抠图环节，流程变长 |
| **B5 首尾帧视频插值** | WanVideo / LTXVideo 做变身过程短片 | 宣传物料、动态漫 | 时序一致性难控，不适合静态分镜批量生产 |

组合建议：**B3 作为兜底（无参考图时）+ 推荐方案作为主路径 + B4 用于封面级画面 + B2 仅在长期连载且一致性要求极高时启用**。

---

## 9. 落地清单

- [ ] `character_forms` / `form_refs` / `panel_forms` 三张表 + 迁移脚本
- [ ] `resolve_form(panel, char)` 解析器（5 级优先级）+ 单元测试（含章内变身、闪回、不可逆回退）
- [ ] Provider 模板固化 `Moss-Image-Edit` + 单图槽 + 强制 ImageScale，并在代码层断言"图片数 == 1"
- [ ] 形态设定图生成器：调 `TE_image_pro_moss_layout` 的「白底三视图」或原生 layout prompt，一次出 4 张人工挑 1
- [ ] 中间态离线合成脚本（单图 + 文本，出 6 挑 1）
- [ ] `FormRegressionError` + `stale` 标记 + 重跑清单
- [ ] 双形态同框默认 `needs_review`，前端给"通过/重 roll/改拆格"三个动作
- [ ] 用测试书的形态转折章做端到端验证：同一章内前男身、后女身，检查 4 格是否都归属正确
