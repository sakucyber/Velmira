# Velmira

Velmira 是一个本地优先的小说与漫画阅读器。当前公开版本是使用 Rust、egui 和 SQLite 构建的原生桌面预览版，支持导入 TXT、EPUB 和 CBZ，在本机保存书库、阅读进度、书签与批注。

AI 小说转漫画通过可配置的模型池调用文本与图像服务。设备配对使用一次性配对码和端到端加密信道，不依赖账号或云端业务服务器。

## 功能

### 本地书库

- 导入 TXT、EPUB 和 CBZ，自动加入书架。
- TXT 编码识别、书头清理、章节切分和段落索引。
- EPUB 目录、正文、插图及安全资源引用。
- CBZ 图片自然排序和按页阅读。
- 封面、书名搜索、格式筛选、阅读状态筛选和排序。
- 重命名、移出书架、恢复，以及重新导入时恢复原有记录。

### 小说阅读

- 分页阅读、目录跳转、全文检索和精确进度恢复。
- 纸色、浅色和深色阅读主题，字号连续调整。
- 键盘翻页、点击翻页和触摸滑动。
- 书签管理、四色高亮、文字笔记、编辑和删除。
- EPUB 正文插图与章节锚点。

### 漫画阅读

- 适页和适宽两种视图。
- 页面切换、缩放滚动和独立阅读进度。
- 支持打开由 Velmira 导出的 CBZ 项目文件。

### AI 小说转漫画

- 从当前正文生成 2–6 格分镜。
- 每格包含画面描述和原文引用，生成前可检查与修改。
- 文本模型生成分镜，图像模型生成漫画页。
- 支持多个模型连接、顺序降级、轮询和随机调度。
- 支持 RPM、并发限制、重试、冷却和故障转移。
- 导出 CBZ 并重新加入本地书库。

模型池配置位于应用数据目录的 `ai-model-pool.json`。API Key 在“小说转漫画”窗口内填写，只用于当前运行，不写入配置文件。

```json
{
  "strategy": "ordered",
  "connections": [
    {
      "endpoint": "https://api.openai.com/v1",
      "text_model": "gpt-4.1-mini",
      "image_model": "gpt-image-1",
      "rpm": 60,
      "concurrency": 2
    }
  ]
}
```

`strategy` 可选：

- `ordered`：按连接顺序故障转移。
- `round_robin`：在可用连接之间轮询。
- `random`：随机选择可用连接。

服务地址必须使用 HTTPS，或指向本机的 HTTP 地址。

### 设备配对

公开版本提供命令行设备配对入口：

```sh
# 接收端
velmira-native --pair-listen 0.0.0.0:7878

# 连接端
velmira-native --pair-connect 192.168.1.10:7878

# 查看或撤销已配对设备
velmira-native --paired-devices
velmira-native --unpair-device DEVICE_ID
```

配对使用 6 位一次性配对码。配对后的数据传输使用 Noise 协议加密，信任信息保存在本机。

### 更新检查

应用可检查 GitHub Release，并验证目标平台安装包的发布信息与下载完整性。更新不会修改书库或阅读记录。

## 构建

要求 Rust 1.88 或更高版本。

```sh
cargo build --release --locked -p velmira-native
cargo test --workspace --locked
```

Windows 产物：

```text
target/release/velmira-native.exe
```

Linux 和 macOS 产物：

```text
target/release/velmira-native
```

Linux 构建需要 GTK 3、WebKitGTK 4.1 和相应的 X11/Wayland 开发库；文件对话框依赖 xdg-desktop-portal。

## 运行

```sh
# 使用系统应用数据目录
cargo run --release --locked -p velmira-native

# 指定独立数据目录
cargo run --release --locked -p velmira-native -- --data-dir ./data

# 启动时导入一本书
cargo run --release --locked -p velmira-native -- --import ./book.epub
```

把 `portable.txt` 放在可执行文件旁边，或在可执行文件目录创建 `data/` 后，应用会自动使用该目录作为便携数据目录。

主要命令行参数：

```text
--data-dir PATH
--import BOOK
--migrate-legacy OLD_DATA_DIR
--bench-startup LOG.jsonl
--pair-listen IP:PORT
--pair-connect IP:PORT
--paired-devices
--unpair-device ID
```

## 数据与隐私

- 书籍、阅读记录、模型配置和配对信息默认只保存在本机。
- 生成分镜时会向配置的文本服务发送选定正文。
- 生成漫画时只发送分镜画面描述和模型请求。
- 应用不内置书籍内容，也不会上传整座书库。
- 清除应用数据或卸载应用会删除本地书库，请自行备份重要文件。

## 项目结构

```text
apps/desktop/          原生桌面应用、阅读器、漫画生成与更新界面
crates/vm-ai/          文本/图像请求、模型池和故障转移
crates/vm-core/        书籍、章节、段落和定位等核心模型
crates/vm-format/      TXT、EPUB、CBZ 解析
crates/vm-media/       封面生成与图片解码
crates/vm-store/       SQLite 书库、进度、书签、批注和漫画归档
crates/vm-sync/        配对、加密信道和同步协议
crates/vm-text/        分页、排版和阅读位置计算
crates/vm-update/      Release 检查和安装包下载验证
scripts/               原生打包脚本
```

## 当前状态

这是原生桌面预览版。当前公开树不包含 Android 客户端；全书分析、人物卡冻结、逐格原文定位、语音朗读和跨设备书籍下载仍属于后续工作。

## 许可证

MIT，见 [LICENSE](LICENSE)。
