# Velmira Native

Velmira Native 是 Rust 重构中的桌面预览。界面使用 egui/Glow，中文正文由 cosmic-text 和 Swash 排版。SQLite、章节压缩与阅读资源由应用进程直接处理。

```sh
cargo build --release --locked -p velmira-native
cargo test --workspace --locked
cargo run --release --locked -p velmira-native -- --import samples/demo-text.txt
```

Windows 可运行 `target/release/velmira-native.exe`。Linux 桌面需要 X11 或 Wayland、OpenGL 驱动及用于文件对话框的 XDG Desktop Portal。可以用拖放或 `--import` 直接打开本地文件。内置简体中文字体覆盖基本汉字和扩展 A。正文需要更多语言字形时，可用环境变量 `VELMIRA_FONT` 指定涵盖所需字符的字体。

- 导入 TXT、EPUB 正文或 CBZ。书架用封面卡片显示阅读进度，支持书名搜索、排序、按格式筛选、改名和移出后恢复。
- 支持目录、方向键／空格翻页、字号调节、进度恢复和 CBZ 适页／适宽。小说正文默认跟随应用的日间或夜间模式，也可单独选用纸色、浅色或深色。
- 在文字阅读页搜索本书、记录当前位置书签。在正文内拖动选择同一段落的文字，再点「添加批注」选择颜色和写笔记。
- 数据保存到操作系统应用数据目录，亦可通过 `--data-dir PATH` 指定。当前数据库文件名为 `velmira.db`。
- 如需迁移旧版正文和阅读记录，可传入 `--migrate-legacy PATH`，参数可指向旧版数据目录或该目录内的 `app.db`。它会读取旧版 TXT／EPUB 章节、进度、书签和批注。旧版漫画、插图、封面、AI 配置和密钥尚未迁移。
- `--bench-startup PATH.jsonl` 记录应用入口到首帧、书架加载、排版和页面渲染各阶段的时间。保持空书架窗口打开五分钟可额外记录空闲内存。Windows 记录私有工作集，Linux 记录 RSS。应用内计时不含系统装载可执行文件的时间。

EPUB 和 CBZ 支持从光栅图片提取书架封面。EPUB 阅读界面目前显示正文，插图锚点已有解析，尚未接入阅读视图。设备同步、AI 和漫画生成、原生安卓入口、安装器以及 v1.1 的扩展格式仍在后续阶段。当前构建用于验证原生桌面阅读链路。
