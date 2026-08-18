---
name: webpage
description: 单文件 HTML 网页制作：自包含、响应式、明暗主题、可内联资源的落地页/文档页/展示页
keywords: 网页,HTML,落地页,官网,页面,交互页,文档页
---

# 单文件网页制作

产出物：一个 .html 文件，双击即开，无需服务器和网络。

## 自包含规约（硬性）

- 零 CDN、零外链：禁止引用任何 http(s) 的 JS/CSS/字体/图标库。样式写 `<style>`，脚本写 `<script>`，全部内联。
- 图片：小图标用内联 SVG 或 data:URI；生成的大图放同目录 assets/img/ 用相对路径引用，绝不用绝对路径或盘符路径。
- 字体只用系统字体栈：`system-ui, "PingFang SC", "Microsoft YaHei", sans-serif`。
- 图表、装饰一律手写内联 SVG，不引入任何库。

## 响应式要点

- 移动优先：默认按窄屏写，`@media (min-width: 768px)` 增强宽屏。
- 字号用 clamp：正文 `clamp(15px, 2.5vw, 17px)`，大标题 `clamp(28px, 6vw, 56px)`。
- 布局用 grid：卡片区 `grid-template-columns: repeat(auto-fit, minmax(260px, 1fr))`。
- 图片 `max-width:100%; height:auto`；宽表格外包一层 `overflow-x:auto` 容器，页面本身禁止横向滚动。
- head 必须有 `<meta name="viewport" content="width=device-width, initial-scale=1">`。

## 明暗主题双通道

用 CSS 变量定义色板，两个通道都要写：

```css
:root { --bg:#fafaf8; --fg:#1d2733; --accent:#2c4661; --line:#e2e2de; }
@media (prefers-color-scheme: dark) {
  :root { --bg:#14181e; --fg:#dde3ea; --accent:#7fa3c4; --line:#2a323c; }
}
:root[data-theme="light"] { --bg:#fafaf8; --fg:#1d2733; --accent:#2c4661; --line:#e2e2de; }
:root[data-theme="dark"] { --bg:#14181e; --fg:#dde3ea; --accent:#7fa3c4; --line:#2a323c; }
```

data-theme 覆盖必须在媒体查询之后声明，保证手动切换能赢过系统偏好。全文只用变量取色，不写死颜色。

## 良构自检清单（交付前逐条过）

1. `<html lang="zh-CN">`、viewport、`<title>` 齐全。
2. 标签全部闭合、属性引号成对；JS 无语法错误（浏览器控制台无红）。
3. 明暗两态下文字对比度都够（正文对背景不低于 4.5:1），暗色下检查一遍写死的颜色。
4. 窄屏 375px 宽无横向滚动、无文字溢出。
5. 所有交互（按钮、锚点、折叠）可用，无死链接。

## 常用页型骨架

- 落地页：顶部导航(锚点) + 首屏大标题与行动按钮 + 特性三卡 + 数据/证言区 + 底部 CTA + 页脚。
- 长文档页：左侧固定目录（由 JS 扫描 h2/h3 自动生成，窄屏折叠为顶部下拉），右侧正文限宽 72ch，标题带锚点。
- 卡片画廊：顶部筛选标签 + auto-fit 网格卡片 + 点击弹出详情浮层（Esc 关闭）。

主色默认墨蓝 #2c4661，风格克制，禁用 emoji。
