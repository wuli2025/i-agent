---
name: game
description: 橙光式文字互动游戏/视觉小说制作：数值加点、好感度、旗标、属性检定、分支剧情、加权多结局
keywords: 游戏,橙光,视觉小说,VN,galgame,加点,好感度,属性,结局,分支,剧情,互动,文字游戏
---

# 文字互动游戏制作指南

产出形态：单文件 HTML。引擎模板已切成 templates/engine_head.html 与 templates/engine_tail.html 两段，你只需写中间的 `const GAME = {...};` 数据段再拼接，引擎代码一律不读不改（templates/vn_engine.html 是含示例数据的完整可玩参考，仅在需要看数据示例时分段读其 300-420 行附近）。

## 制作流程（顺序不可颠倒）

1. 设计数值系统：先定 schema——属性表 stats、好感表 affinity、旗标表 flags。数值是骨架，剧情围绕数值展开。
2. 写大纲与结局表：一句话主线 + 每章一句话 + 结局表（每个结局：id、标题、达成条件表达式、权重）。先在纸面验证每个结局条件数值可达，再写正文。
3. 分块写数据文件 game_data.json：内容是**严格 JSON**（键带双引号、无注释、无尾逗号、布尔小写）。正文对话里的引用一律用中文引号「」，严禁在 JSON 字符串里出现未转义的英文双引号。场景多时分多次 write（首次正常写，后续 append:true 续写），每次不超过约 200 行，严禁一次输出整个大对象。
4. 用 check 工具校验 game_data.json：语法错会报行列号，用 edit 修到通过为止；它同时自动检查场景引用完整性和兜底结局。
5. 用 **bundle 工具**拼成品（**严禁用 shell 的 cat / Get-Content 手拼**），game.html 必须落在工作目录：
   `bundle(out="game.html", parts=["<模板目录>/engine_head.html", "game_data.json", "<模板目录>/engine_tail.html"])`
   为什么必须用 bundle：引擎头以 `const GAME =` 结尾、引擎尾以 `;` 开头，接缝处只要少一个分号，整个 `<script>` 块就不执行 → 打开是纯白屏；而这种错用文本检查几乎看不出来（把多个脚本块拼起来检查时，拼接用的分隔符反而会把它掩盖掉）。bundle 会按 JS 规则确定性地补分号，并逐块单独做真语法检查。
6. **浏览器冒烟（不可跳过）**：`browser(path="game.html", clicks=4)`。必须看到：不白屏、无运行时异常、点击选项后界面有响应。任何一条不满足就去修，修完重跑，直到通过。**没跑过 browser 的游戏不算做完**——白屏的成品和没做是一回事。
7. 终检：对照下方死路清单逐条过一遍结局可达性。

## 数值系统规范

- 属性 stats：0-100 整数，3-5 个为宜（如 武力/智谋/声望）。初始 15-25，单次加点 5-15，引擎自动钳位到 [0, max]。
- 好感度 affinity：角色 -> 数值，单次 +5~+15，结局阈值设 30-60。
- 旗标 flags：布尔，记剧情事件（关键道具、知晓秘密），命名用名词。
- 检定 check：属性值 + 随机骰(1..rand) >= difficulty 即成功，成功/失败各跳一个场景。难度设为「检定时点属性期望值上下 10 以内」，保证两种结果都可能出现。
- 加权结局裁定：走到 goto:"END" 时逐条求值 endings 的 cond（表达式里可用 stats.X / affinity.X / flags.X），在所有满足者中取 weight 最高者。必须有一条 cond:"true"、weight:1 的兜底结局。

## GAME 顶层结构（严格 JSON，字段名与形状不得自创）

```json
{
  "meta": { "title": "游戏名", "author": "i-agent", "cover": "linear-gradient(...)" },
  "config": { "typeSpeed": 30, "condMode": "hide", "transition": "fade" },
  "start": "s1",
  "stats": [ { "key": "才学", "name": "才学", "init": 20, "max": 100 } ],
  "affinity": [ { "key": "沈青梅", "name": "沈青梅", "init": 0 } ],
  "flags": [ { "key": "线索", "name": "关键线索" } ],
  "scenes": [ ... ],
  "endings": [ { "id": "e1", "title": "结局名", "text": "结局正文", "cond": "stats.才学>=60", "weight": 5 } ]
}
```

注意：stats/affinity/flags 是**数组**（不是对象），字段名是 key/name/init/max（不是 label/initial/min）。

## 场景 JSON 格式（严格 JSON，与模板引擎严格一致）

```json
{ "id": "s1",
  "bg": "linear-gradient(...) 或图片路径",
  "transition": "fade 或 slide 或 ink",
  "speaker": "沈青芜",
  "text": "正文，可含 [shake] [flash] 强调标记与 \n 换行，引用用「中文引号」",
  "effects": {"剑心": 10},
  "sprites": {"left": "a.png", "center": null, "right": null},
  "choices": [
    { "text": "应战", "cond": "stats.剑心>=30", "condMode": "hide",
      "effects": {"勇气": 10, "线索": true}, "goto": "s2",
      "check": {"stat": "剑心", "difficulty": 45, "rand": 20, "success": "s6", "fail": "s7"} }
  ],
  "next": "s2"
}
```

说明：bg/transition/speaker/effects/sprites 均可省略；bg 省略沿用上一场景，speaker 省略即旁白；next 与 choices 二选一，写 "END" 触发结局裁定；condMode 取 "hide" 或 "gray"。

effects 的键先匹配 stats、再匹配 affinity，都不是则按旗标赋布尔值。

## 死路检测清单（发布前逐条过）

- **browser 冒烟已通过**：不白屏、无运行时异常、点击有响应、零外链依赖。这条不过，下面几条都没意义。
- 每个场景至少一条出路：有 choices 时，任意数值状态下至少一个选项可见可点——全部带 cond 的选项必须配一个无 cond 选项。
- 每个结局可达：为每个结局手推一条具体选择路径，验证数值累加确实满足条件。
- 检定不锁死：失败分支也必须能走到某个结局，不能失败即断头。
- 数值不溢出锁死：cond 阈值不得高于该点位理论最大值；负向 effects 不要把关键属性扣到条件永不可达。
- goto / next / success / fail 引用的场景 id 全部存在，所有支线在 "END" 前都能汇流。

## 常见类型套路

- 养成加点：开局与中期各设一次「三选一加点」场景，结局按最高属性分流。
- 恋爱好感：每角色 3-4 个专属事件，好感阈值做结局门槛；共通线之后按好感分流。
- 悬疑收集：线索做旗标，真相选项的 cond 要求集齐 N 个旗标（用 && 连接），缺线索走坏结局。
- 宫斗对抗：己方属性对抗难度递增的检定链，失败扣声望，`stats.声望<=0` 的失败结局给最高权重优先裁定。
