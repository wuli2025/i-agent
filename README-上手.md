# i-agent（验证型外壳版）

一个轻量办公 agent。和别的外壳最大的区别：**它交付前会在真浏览器里跑一遍自己做的东西**，跑不通不许收工。

## 快速开始

### 1. 配模型（认 Claude Code 那套环境变量）

```bash
export ANTHROPIC_AUTH_TOKEN="你的key"
export ANTHROPIC_BASE_URL="https://api.minimaxi.com/anthropic"
export ANTHROPIC_DEFAULT_HAIKU_MODEL="MiniMax-M3"
```

同一份 env 可以同时驱动 claude / codex / opencode / i-agent。

也支持 OpenAI 兼容端点：

```bash
export I_AGENT_API_KEY="..."
export I_AGENT_BASE_URL="https://api.deepseek.com/v1"
export I_AGENT_MODEL="deepseek-chat"
```

或者内置的：`DEEPSEEK_API_KEY` / `MOONSHOT_API_KEY` / `ZHIPU_API_KEY` / `DASHSCOPE_API_KEY` / `SILICONFLOW_API_KEY` / `MINIMAX_API_KEY`，设了哪个用哪个。

### 2. 装浏览器验证的依赖（**重要**）

`browser` 工具是这个版本的核心，它需要 Node + Playwright + Chromium：

```bash
npm i -g playwright
npx playwright install chromium
```

装完就能用。如果 Chromium 装在非常规位置，用 `I_AGENT_CHROME` 指过去：

```bash
export I_AGENT_CHROME="/path/to/chrome"     # 或 Windows 上的 chrome.exe / msedge.exe
export I_AGENT_PLAYWRIGHT="/path/to/node_modules"   # 含 playwright 的目录
```

没装也能跑，只是 `browser` 工具会明确报错告诉你缺什么——**它不会假装验证过**。

### 3. 跑

```bash
i-agent -p "做一个单文件 HTML 数据看板，保存为 dashboard.html"    # 无头模式
i-agent                                                        # 交互模式
i-agent -C /some/dir -p "..."                                  # 指定工作目录
```

## 它和普通 agent 外壳有什么不一样

### 交付门禁：不许"我觉得没问题"就收工

任何 HTML 产物，交付前**必须**在真 Chromium 里跑通。模型想直接给结论？系统会把它拦下来，逼它去跑 `browser`。改过文件（`edit`）之后，之前的验证作废，必须重验。

### `browser` 工具查的是那些"读代码看不出来"的问题

| 检测 | 为什么文本检查抓不到 |
|---|---|
| **白屏** | 脚本在解析阶段整块失效时，连 `pageerror` 都不会有 |
| **运行时异常** | `pageerror` + `console.error` |
| **内容被裁剪** | 不报错、不白屏、DOM 齐全，但主内容整块看不见（子元素 `position:absolute` 撑不开父容器，父容器又 `overflow:hidden` → 塌成几 px 全裁掉） |
| **空图表** | 源码里数 `<svg` 标签毫无意义——标签在、里面空空如也，图表照样不存在 |
| **外链依赖** | 拦截所有 http(s) 请求，零依赖单文件要求为 0 |
| **点了没反应** | 真的去点按钮/选下拉，看 DOM 变不变 |

### `bundle` 工具：把"接缝"从模型手里拿走

拼接 HTML 一律用 `bundle`，**禁止**模型手写 `cat head + data + tail`。接缝漏一个分号 → 整个 `<script>` 块不执行 → 白屏，而这种错文本检查几乎发现不了。

`bundle` 按 JS 的 ASI 规则**确定性地**判断接缝要不要补 `;`，拼完立刻逐块做真语法检查。

### `jscheck`：每个 `<script>` 块**单独**校验

绝不把多块拼起来检——拼接时插入的分隔符会把真实的漏分号**掩盖掉**。

## Windows 上编译

没有预编译的 exe（本机没有交叉编译链接器）。在 Windows 上装好 [Rust](https://rustup.rs/) 后：

```powershell
.\build-windows.ps1
```

产物在 `target\release\i-agent.exe`。

## 其他环境变量

| 变量 | 用途 |
|---|---|
| `I_AGENT_CTX` | 上下文窗口大小（默认 65536） |
| `I_AGENT_NODE` | node 可执行文件路径 |
| `I_AGENT_CHROME` | Chromium/Chrome/Edge 可执行文件路径 |
| `I_AGENT_PLAYWRIGHT` | 含 playwright 的 node_modules 目录 |
| `I_AGENT_ASSETS` | 自定义技能包目录（避免被内置资产覆盖刷新） |
| `HTTPS_PROXY` / `HTTP_PROXY` | **会被读取**（很多 HTTP 库不读，导致在需要代理出网的机器上静默挂死） |

配置文件：`~/.i-agent/config.json` 与 `<工作目录>/.i-agent/config.json`（后者优先）。

## 已知问题

- 白屏判定对**极小的页面**会误报（正文 < 20 字且无 canvas/svg 且 DOM < 5 节点）。正常交付物不受影响。
- token 记账依赖上游返回的 usage 字段；MiniMax 流式在 `message_start` 里把 `input_tokens` 报成 0（已从 `message_delta` 兜底补读）。
