---
name: motionfx
description: 网页/游戏/PPT 动效：镜头运动、转场、入场动画、粒子氛围、CSS/JS 动效指令集
keywords: 动效,动画,转场,镜头,粒子,特效,过渡
---

# 动效指令集（纯 CSS / 原生 JS，零依赖）

通则：动画只动 transform 与 opacity（GPU 合成、不回流）；时长 200-800ms；缓动默认 `cubic-bezier(.22,.61,.36,1)`；同屏动效不超过两类。

## 镜头四式（对全屏容器施加 transform）

- 推（推近）：`transform: scale(1) -> scale(1.12); transition: transform 6s ease-out`，慢推营造凝视感。
- 拉（拉远）：`scale(1.15) -> scale(1)`，用于开场揭示全景。
- 摇（原地转向）：`transform-origin: 50% 100%; rotate(-1.5deg) -> rotate(1.5deg)`，幅度不超过 2deg。
- 移（平移跟随）：容器宽于视口（width:120%），`translateX(0) -> translateX(-16%)`，8-12s 线性慢移。

组合技：transform 可叠写 `scale(1.1) translateX(-4%)`，一条 transition 同时完成推与移。

## 转场库（两层叠放，新层入场）

- 淡入淡出：新层 `opacity: 0 -> 1`，0.6s。
- 横移：`translateX(6%) -> 0` 叠加 opacity，翻页感。
- 径向水墨扩散：`clip-path: circle(0% at 50% 55%) -> circle(140% at 50% 55%)`，约 1s；圆心可设在点击坐标，转场期间加轻微 `filter: blur(2px)` 更似墨晕。
- 百叶窗：叠 N 条 `.blind{transform:scaleY(0);transform-origin:top}`，逐条 `transition-delay: i*60ms` 展开到 `scaleY(1)`。

## 入场动画

- fadeUp：`@keyframes fadeUp{from{opacity:0;transform:translateY(16px)}to{opacity:1;transform:none}}`，0.5s both。
- stagger 交错：同组元素统一挂 fadeUp，JS 一行赋延迟：`items.forEach((el,i)=>el.style.animationDelay=i*80+"ms")`。列表、卡片、菜单一律交错进场，整屏齐动显得廉价。

## 氛围粒子（canvas，20 行内）

要点：一个数组存粒子 `{x,y,vx,vy,r,alpha}`，requestAnimationFrame 循环里 清屏 -> 更新 -> 画圆点，出界重置到顶部或随机位。

- 雪：vy 取 0.3-1，vx 叠加 `Math.sin(y*0.01)` 做摆动，白色圆点。
- 尘埃：速度 0.05-0.2 全向漂浮，alpha 0.05-0.15，30 个以内。
- 萤火：暖黄色，alpha 随 `Math.sin(t+相位)` 呼吸明灭，`shadowBlur=8` 发光。

粒子总数不超过 80；canvas 尺寸随 resize 重设；`visibilitychange` 页面不可见时暂停循环。

## 打字机

```js
let i=0;(function t(){el.textContent=str.slice(0,++i);if(i<str.length)setTimeout(t,30)})();
```

点击时直接 `el.textContent=str` 完成跳过；速度 20-40ms/字。

## 视差滚动

监听 scroll，按层深移动：`layer.style.transform="translateY("+scrollY*k+"px)"`，k 取 0.1/0.3/0.6 三层，写进 rAF 节流。纯 CSS 替代：`background-attachment:fixed`（移动端慎用）。

## reduced-motion 降级（必做）

```css
@media (prefers-reduced-motion: reduce){
  *{animation-duration:.01ms!important;transition-duration:.01ms!important}
}
```

JS 粒子与视差启动前判断 `matchMedia("(prefers-reduced-motion: reduce)").matches`，命中则不启动，只保留静态首帧与透明度淡入。
