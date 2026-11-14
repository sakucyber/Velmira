# 一次生成整页无字漫画

Velmira 0.5.3 源码新增“无字整页”：文本模型先规划 2–6 个连续分格，图像模型**一次请求**生成包含全部分格、黑色边框和白色间隔的整页图片。程序不叠加对白、旁白、气泡或任何文字，适合作为上字底稿。

## 使用

阅读页打开「AI → 漫画生成 → 无字整页」：

1. 选择章节。
2. 选择分格数（默认 4 格，可 2–6 格）。
3. 选择页面尺寸：
   - `portrait`：1024×1536，默认竖版；
   - `tall`：832×1216，更长的竖版；
   - `square`：1024×1024。
4. 点击「一次生成无字漫画页」。

生成在后台进行，面板会显示排队、生成中、已完成或失败；完成后可打开原图保存，并在下方记录中重新查看。

## 分格布局

程序按分格数固定描述版面，并要求图像模型把全部分格画在同一张图里：

| 分格数 | 版面 |
|---|---|
| 2 | 上下两格 |
| 3 | 上通栏 + 下方左右两格 |
| 4 | 上方左右两格 + 中间通栏 + 底部通栏（默认，接近示例） |
| 5 | 上 2 格 + 中 2 格 + 底部通栏 |
| 6 | 三行 × 两列 |

这是给图像模型的版面约束，不保证像素级对齐；不同模型的边框和分格比例会有差异。

## 无文字约束

最终图像提示词明确包含：

> absolutely no text: no speech bubbles, no captions, no narration, no letters, no numbers, no sound effects, no signs, no logos, no watermarks, no signatures and no UI

文本模型的分格规划只包含镜头、动作、环境、光线和表情，不允许对白、旁白、拟声词或文字描述。图像生成后也不会调用拼页排版，因此不会出现后加文字。

## API

`POST /api/books/{book_id}/manga/single-page`：

~~~json
{"chapter": 0, "panels": 4, "size": "portrait", "scene_id": "optional_scene_id"}
~~~

返回 202 和 `{"page": {...}}`。`scene_id` 可选；提供时使用该场景的原文片段和人物，否则使用本章开头最多 5000 字符和正文明示的人物。

其他接口：

- `GET /api/books/{book_id}/manga/single-page?limit=20`：最近记录。
- `GET /api/books/{book_id}/manga/single-page/{id}`：状态与分格规划。
- `GET /api/books/{book_id}/manga/single-page/{id}/image`：生成的 PNG。

`page.spec.panels` 含每格 `shot/characters/action/environment/lighting/emotion`，可用于审核或后续改图。

## 人物参考图

如果本章/场景中的人物存在已确认且版本一致的参考图，生成请求会使用其中一张作为参考图，并在提示词中附上人物身份描述，帮助多个分格保持同一人物外观。没有参考图时仍可按文字生成，但一致性取决于模型。

## 限制

- 一次图像请求生成整页，分格边界和表情连续性由图像模型理解，可能出现格线不齐、人物比例差异；建议生成后人工检查。
- 不含文字是提示词约束和输出设计目标，极端模型仍可能画出类似字母的装饰；请以“无字”版本作为底稿，必要时局部重绘。
- Windows 包为便携版；Android 参考图/模型能力取决于已配置的图像服务。
