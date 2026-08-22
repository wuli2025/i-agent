---
name: canvas
description: 通过统一命令层控制 Polaris AI 无限画布、卡片和影视分镜
keywords: 画布, 分镜, 卡片, 九宫格, 25宫格, 工作流, storyboard, canvas, 镜头
---

# AI 画布控制技能

使用内置 `canvas` 工具，不要用 shell/curl 绕过命令层。工具、网页 UI 和 MCP Server 共用同一个 CommandBus，操作会实时出现在画布并自动保存。

## 标准顺序

1. `canvas {"action":"health"}` 确认服务可用。
2. `canvas {"action":"query"}` 获取当前节点 ID、类型、坐标和分镜序号。
3. 先说明要新增/修改/排列什么，再调用工具；已有节点一律使用查询返回的稳定 ID，禁止猜 ID。
4. 多卡片优先一次 `storyboard` 创建，或完成编辑后一次 `arrange`，不要逐张手算位置。
5. 需要生成产物时先 `run`，拿到 `promptId` 后必须 `wait`；不要把“已入队”误报成“已完成”。
6. 最后再次 `query`，核对 nodeCount / edgeCount / groupCount 和卡片顺序。

## 常用调用

### 9/25 宫格分镜

`action=storyboard`，字段：

- `title`：整套分镜名
- `columns`：9 宫格用 3，25 宫格用 5
- `shots`：镜头数组，每项至少 `title`；建议给 `prompt`、`duration`、`shotSize`、`cameraMovement`，可选 `notes`、`dialogue`、`character`、`imageUrl`
- `origin:{x,y}`：世界坐标起点
- `connectSequentially:true`：按镜头顺序连线

这会原子地创建分镜卡、顺序连线、Group 和可替换素材插槽，不要再手工重复打组。

### 自动排列已有卡片

`action=arrange`，传 `nodeIds`（省略表示全画布）、`layout`：

- `storyboard`：按 `shotNumber` 排序后网格排列
- `grid`：普通网格
- `horizontal` / `vertical`：横排 / 竖排

可传 `columns`、`gapX`、`gapY`、`origin`。

### 单节点与连接

- `add`：`type/title/x/y/width/height/params`
- `update`：`nodeId` + 要改的字段；`params` 按 key 合并，不会抹掉其余参数
- `remove`：`nodeIds`
- `connect`：`source/sourceHandle/target/targetHandle/dataType`
- `group`：`nodeIds/title/color`

连接必须是 DAG；重复连接、自环、环路会被服务端拒绝。端口名来自 `query detail=full` 返回的节点定义或画布节点语义；常用端口：`text`、`image`、`video`、`prompt`、`sequence`、`shot`。

### 执行

- `run`：不传 `targetIds` 执行全图；传目标 ID 时只执行它们的祖先闭包。
- `wait`：传 `promptId`，最多等 300000ms。返回缓存命中、逐节点进度、产物和成本。

节点运行态是瞬态，不写入 Yjs 文档；节点、连线、分组和参数会持久化。只有本地 UI 命令进入当前用户的撤销栈，Agent 操作不会污染该撤销栈。

## 配置

默认 API：`http://127.0.0.1:8787`，默认画布：`main`。可用环境变量覆盖：

- `I_AGENT_CANVAS_URL` 或 `AI_CANVAS_API_URL`
- `I_AGENT_CANVAS_ID` 或 `AI_CANVAS_ID`

服务未启动时，在 `D:\polaris\AIGC\ai-canvas` 运行 `npm run dev`。
