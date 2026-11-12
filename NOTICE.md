# 第三方归属与许可 / Attribution & Licenses

## foliate-js

Velmira 的阅读器方案基于 **foliate-js**：

- 项目：https://github.com/johnfactotum/foliate-js
- 作者：John Factotum
- 许可：MIT License

本仓库（M0–v0.3）是独立实现，未直接复制 foliate-js 源码；阅读内核（合成 Book 对象、
按 section 加载、基于 Locator 的定位与重排）沿用其公开契约与设计思路，后续接入 EPUB/CBZ
时将继续参考其实现。foliate-js 的 MIT 许可要求其版权与许可声明随衍生作品保留，故在此声明。

## MyGo

Velmira 的原生桌面端使用 **MyGo** 构建：

- 项目：https://github.com/egoist/mygo
- 许可：MIT License（版权归 MyGo contributors）

桌面壳（desktop/）通过 Go 模块依赖 `github.com/egoist/mygo`，用它创建系统 WebView2 窗口、菜单与窗口生命周期，见 [desktop/README.md](desktop/README.md)。

## 本仓库

- 许可：MIT（见 [LICENSE](LICENSE)）

## 字体 / 素材

- 示例漫画（samples/demo-comic.cbz）由 Pillow 程序绘制，不含第三方素材。
- 界面使用系统字体，无第三方字体文件。
