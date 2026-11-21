# Velmira

Velmira 是本地书库和阅读器。原生版本使用 Rust 和 egui，支持导入 TXT、EPUB 正文与 CBZ，在本机保存书籍、阅读进度、书签与批注。

## 桌面预览

从书架导入文件或直接拖入窗口。书架支持封面、标题搜索、排序、格式筛选和移出后恢复。小说阅读提供目录、分页、全文检索、书签与批注，CBZ 提供适页和适宽两种视图。

```sh
cargo build --release --locked -p velmira-native
cargo test --workspace --locked
```

Windows 可运行 `target/release/velmira-native.exe`。Linux 和 macOS 的二进制名为 `velmira-native`。Linux 图形栈和文件对话框依赖、旧书库导入方式见 [桌面说明](apps/desktop/README.md)。

此前基于 WebView 的 v0.7.0 多平台发行包保存在 [Velmira-old](https://github.com/sakucyber/Velmira-old/releases/tag/v0.7.0)。
