# M3：单格出图与一致性

M3 已接入从 M2 场景和冻结人物卡到 ComfyUI/MOSS 单格出图的完整流程：

- `/api/books/{book_id}/manga/config` 保存 ComfyUI、模型、尺寸、采样、人脸阈值和校验器路径；`/probe` 检查节点与模型。
- 形态、章节/段落生效锚点和版本化参考图在 SQLite 中保存。参考图上传或生成后都必须人工确认，身份卡升版会使旧参考图和任务变为过期。
- 生产模板强制一张参考图、`ImageScale` 和 `Moss-Image-Edit`，ComfyUI 队列串行运行；prompt、工作流、seed、prompt id 和每次尝试都持久化。
- 人脸校验使用可选的 buffalo_l/ArcFace 外部 worker。无人脸、多人脸、校验器缺失或分数不足都标为“待复核”，不会自动通过；人工确认是独立状态。
- AI 面板新增“漫画生成”，提供人物/参考图、分镜、出图设置三部分，支持生成候选、确认/撤销参考图、排队、取消、续查、换种子重画、人工确认和保存为参考图候选。

本地验收：

- `python -m unittest discover -s tests -v`：61/61 PASS（其中 M3 15 项）。
- `node tests/cdp_manga.mjs --base http://127.0.0.1:8883`：桌面与 390px 手机面板、参考图闸门、Mock 待复核、人工确认、轮询释放和零 JS 异常 PASS。
- 自包含 Windows 分发包构建成功，并在隔离数据目录/端口 8885 上通过同一套浏览器回归，另覆盖在面板修改已有人物外貌、升版本和撤销旧参考图确认。
- `tests/verify_manga_real.py` 会把 GPU 生成结果、配置、工作流、种子、ArcFace 分数和验收 JSON 写入 `.tmp/`（均被忽略）。ComfyUI 节点/模型探测已通过，真实 10 格测试结果必须以 `accepted: true` 才算达到架构中 8/10、0.55 的门槛；未达门槛的格子保持待复核。

2026-11-13 真实 ComfyUI 验收结果：**8/10 通过，阈值 0.55，18 次总尝试，每格最多 3 次**。8 格通过分数为 0.568–0.970；其余 2 格检测到多个面孔，维持 `needs_review`。固定的十个镜头包含正面/侧面、特写/中景、站立/坐姿和不同表情。[结构化验收记录](../tests/M3-acceptance.json) 仅含中性测试 ID、种子与分数。

调试记录一并保留在本机 `.tmp/m3-real/`：第一轮写实参考图仅 1/10；第二轮改用漫画风格原图为 6/10（含一次服务超时）；第三轮裁剪参考图为 1/10；最终使用未经裁剪的漫画参考图并加强单人画面约束，为 8/10。没有降低阈值，所有尝试与失败结果均保留。ArcFace 对漫画有画风依赖，此结果只证明本次参考图和配置满足 M3 验收，不保证任意人物/画风同样达标。

真实测试命令（本机已有 ComfyUI 与 buffalo_l 权重）：

```powershell
$env:VELMIRA_FACE_PYTHON='D:\AI\ComfyUI-aki-v3.2\python\python.exe'
$env:VELMIRA_FACE_MODELS='D:\AI\models\insightface\models\buffalo_l'
python tests/verify_manga_real.py                 # 先生成并人工检查参考图
python tests/verify_manga_real.py --render --run-id v4 --reference-file <已确认参考图>
```

测试图片、数据库、权重和生成结果不进入 Git；桌面自包含构建只携带 `face_worker.py`，模型仍由本机配置路径提供。

使用入口：打开一本小说 → 工具栏「AI」→「漫画生成」。设置里保存 ComfyUI 地址和模型，人物页确认参考图后，到分镜页选择 M2 场景并排队。Windows 自包含版的人脸校验需在出图设置指定外部 Python 和 buffalo_l 模型目录；缺少配置时可生成图片，但结果会进入待复核。

本地包：`dist/Velmira-desktop-v0.4.0-m3-final/Velmira.exe`。M3 不包含 M4 的拼页、对白后置和 CBZ 导出。
