# 桌面端与界面润色（v0.4）

> 2026-11-12 · Velmira v0.4：原生桌面壳（MyGo）+ 存量界面润色（oil-ui 流程）

## 1. 原生桌面端

- 框架：[MyGo](https://github.com/egoist/mygo)（Go；MIT），窗口使用系统 WebView2。
- 结构：Go 壳（desktop/）负责窗口、菜单、生命周期；启动时在空闲端口拉起 Python 后端，健康检查通过后导航到界面；退出时结束后端进程树；已有开发服务时直接复用。
- 构建：`scripts/build-desktop.ps1`（Go 1.27+，无需 Bun）；产物 `desktop/Velmira.exe`（约 9 MB）。
- 细节：desktop/README.md；窗口证据：docs/screenshots/desktop-window.png；内容证据：desktop-app.png。
- 限制：Python 边车未打包（目标机需 Python 3.11+）；仅测 Windows/WebView2。

## 2. 界面润色（存量优化，不换品牌）

流程：产品事实与基线 → 单一方向（静、纸感、排印驱动）→ 灰度结构 → 实现 → 真实截图自查 → 独立视觉评审 → 修正 → 复验。
过程材料：.oil-ui/velmira-polish/（DESIGN-NOTES.md、baseline/、after/、review-1.md、ux-review-1.md）。

### 结构
- 桌面（≥1024px）：左侧 232px 侧栏（Velmira wordmark / 书架·继续阅读 / 底部导入主按钮 + 书库状态 + 版本），右侧主区（书架标题 + 书名过滤 + 刷新、紧凑导入条、书架封面网格）。
- 书架：3:4 纸感封面（按格式配色 + 标题首字 + ≥11px 格式徽标 + 3px 进度）、14px 标题 / 12px 元信息、「开始/继续」入口；空态 / 骨架 / 错误条。
- 阅读器：顶栏分组（主组 inline SVG 图标 + 单标签；从组主题/字号 icon-only），正文 measure 760px（19px 下约 37 字/行），章题 22px，底部状态条。
- 抽屉：420px，52px 头部，条目 14/13/12 层级，滑入 260ms。
- 漫画：深色舞台 + 页面阴影、圆形侧箭头、顶/底栏重排、底部进度。

### 动效（三处基本）
1. 主操作反馈：按钮/卡片按下 scale，新增书卡入场。
2. 状态切换：抽屉滑入/滑出、翻页 180ms（仅按钮/键盘）、章节淡入、浮动菜单展开、漫画页切换 160ms。
3. 首次进入：书库 header + 卡片 stagger 40ms（prefers-reduced-motion 全部直达）。

### 验收
- 视觉：独立评审（存量润色标准，不打分）无阻断；4 项必须修 + 2 项次要已全部落实并复核截图。
- 回归：`scripts/verify.ps1` 41/41；`scripts/verify_m1.ps1` 41 + 34 + 3 + CDP 22 全 PASS；37 单测 OK。
- 体积：JS 116.3KB（含漫画）/ CSS 49.3KB / HTML 10.4KB；1280×860 与 390×844 无横向溢出。
- 证据：docs/screenshots/library.png、reader-desktop.png、notes-drawer.png、toc.png、search.png、comic.png、comic-mobile.png、mobile-reader.png、desktop-window.png、desktop-app.png。
