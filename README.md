# i-agent

> 一个基于 p-agent 思路开发的更强轻量 agent，主要面向办公型任务；能力目标接近 Claude Code 类 agent，但更轻、更节省上下文。

`i-agent` 是纯 Rust 单二进制办公 agent，面向国产模型与 8k–32k 短上下文优化。当前 release 构建在 Windows 约 **2.5 MiB**、Linux 约 **2.8 MiB**（不同工具链会有差异）。它不做宿主机权限门控，推荐让每个实例运行在独立 Docker 容器或其他受控工作区中。

## 能做什么

| 领域 | 能力 | 交付形态 |
|---|---|---|
| 文字互动游戏 | 加点、好感、旗标、检定、加权多结局，内置 VN 引擎模板 | 单文件 HTML |
| PPT / 演示 | 一屏一意幻灯、插画、打印布局 | 单文件 HTML / PDF |
| 办公表格 | CSV 读写、统计聚合、零依赖导出 `.xlsx`、数据报告 | `.xlsx` / HTML |
| 长文本 | 大纲先行、子 agent 分章并行、Bible 一致性锚定 | 分章 Markdown |
| 知识体系 | 资料整理成可检索的结构化知识库 | `kb/` 目录 |
| 网页 / 动效 | 单文件响应式页面、镜头、转场和粒子动效 | 单文件 HTML |
| 生图 | MiniMax image-01、智谱 CogView-4、硅基流动 Kolors | PNG |

## 快速开始

### 使用 Anthropic 兼容端点

```powershell
$env:ANTHROPIC_AUTH_TOKEN = "你的密钥"
$env:ANTHROPIC_BASE_URL = "https://api.minimaxi.com/anthropic"
$env:ANTHROPIC_DEFAULT_HAIKU_MODEL = "MiniMax-M3"

i-agent -p "把 data.csv 做成月度报告"
```

也可以设置更明确的 `ANTHROPIC_MODEL`；它的优先级高于 `ANTHROPIC_DEFAULT_HAIKU_MODEL`。

### 使用内置供应商

```powershell
$env:MINIMAX_API_KEY = "你的密钥" # 也可用 KIMI_API_KEY / DEEPSEEK_API_KEY / ZHIPU_API_KEY 等
i-agent
```

内置供应商（可用 `--provider` 点名）：

| 名字 | 密钥变量 | 默认模型 | 端点 / 协议 |
|---|---|---|---|
| `kimi` | `KIMI_API_KEY` | `k3` | api.kimi.com/coding · Anthropic |
| `deepseek` | `DEEPSEEK_API_KEY` | `deepseek-chat` | api.deepseek.com/anthropic · Anthropic |
| `glm` | `ZHIPU_API_KEY` / `GLM_API_KEY` | `glm-4.6` | bigmodel Anthropic 兼容 |
| `minimax` | `MINIMAX_API_KEY` | `MiniMax-M3` | api.minimaxi.com/anthropic · Anthropic |
| `moonshot` | `MOONSHOT_API_KEY` | `kimi-k2-0905-preview` | OpenAI 兼容 |
| `qwen` | `DASHSCOPE_API_KEY` / `QWEN_API_KEY` | `qwen-plus` | OpenAI 兼容 |
| `siliconflow` | `SILICONFLOW_API_KEY` | `DeepSeek-V3` | OpenAI 兼容 |

任意 OpenAI 兼容端点使用 `I_AGENT_API_KEY` + `I_AGENT_BASE_URL`（可选 `I_AGENT_MODEL` / `I_AGENT_PROTOCOL`）。Anthropic 兼容端点使用 `ANTHROPIC_AUTH_TOKEN` 或 `ANTHROPIC_API_KEY` + `ANTHROPIC_BASE_URL`。

**供应商选择规则**：显式配置（`I_AGENT_*`、`ANTHROPIC_*`、配置文件 `providers`）优先且排他；只有完全没有显式配置时，才扫描内置供应商的密钥。发生回退时会向 stderr 明确告警，不会静默换模型。

> 不要把真实密钥写进配置示例、脚本、日志或 Git。优先使用环境变量或系统凭据存储；已经公开或粘贴到共享记录中的密钥应及时轮换。

## 构建

需要 Rust 1.85+；重新生成内嵌资产还需要 Node.js。

```powershell
node scripts/gen_embedded.mjs
cargo test
cargo build --release
.\target\release\i-agent.exe -V
```

Windows 也可运行 `./build-windows.ps1`；制作可分发 zip 可运行 `./scripts/package.ps1`。

## 浏览器验收（可选）

HTML 交付门禁会调用 Playwright + Chromium 做真实渲染检查：

```powershell
npm i -g playwright
npx playwright install chromium
```

程序会依次检查 `I_AGENT_PLAYWRIGHT`、`npm root -g`、常见用户/项目目录。特殊安装可显式设置：

```powershell
$env:I_AGENT_PLAYWRIGHT = "包含 playwright 的 node_modules 路径"
$env:I_AGENT_CHROME = "chrome.exe 或 msedge.exe 路径"
```

未安装浏览器依赖时，普通文件、Shell、Python、表格等能力仍可使用；`browser` 工具会明确返回缺失依赖，不会假装验证成功。

## Docker（推荐隔离方式）

```bash
docker build -t i-agent .
docker run --rm -e MINIMAX_API_KEY -v "$PWD:/work" i-agent -p "把 data.csv 做成月度报告"
```

## 命令行

```text
i-agent [选项] [任务]
  -p, --print          无头模式，执行完退出
  -C, --dir <路径>     工作目录
      --provider <名>  指定供应商
  -m, --model <名>     覆盖模型
  -c, --continue       继续当前工作目录的上次会话
  -q, --quiet          隐藏工具调用过程
      --variants "A|B" 从共享前缀派生多个版本
      --prepare "任务" 先做一次共享准备再派生
      --branches       查看分支树
      init-assets      释放内嵌技能包到 ~/.i-agent/assets
```

会话保存在 `<工作目录>/.i-agent/session.jsonl`。配置文件位置为 `~/.i-agent/config.json` 和 `<工作目录>/.i-agent/config.json`（工作目录配置优先）。示例见 [`config.example.json`](config.example.json)。

## 为什么短上下文也能干长活

1. 两级上下文压缩：先裁旧工具输出，再由 LLM 摘要旧历史，切点不撕裂工具调用对；
2. `task` 子任务扇出：长内容分章并行，每章持有独立小上下文；
3. 技能包渐进披露：系统提示只注入索引，命中任务时再读完整技能；
4. 工具输出有界截断，`read` 默认分段；
5. Anthropic 协议显式设置 prompt-cache 断点，并单独处理 thinking block。

## 架构

```text
src/
  main.rs      CLI、无头模式、会话分支和批量派生
  agent.rs     双层 agent 循环、doom-loop 熔断、task 并行扇出
  llm.rs       Anthropic / OpenAI 兼容流式客户端与供应商回退
  context.rs   token 记账与两级压缩
  tools/       read/write/edit/glob/grep/shell/fetch/browser/image/xlsx/task
  skills.rs    技能发现、内嵌释放与索引注入
assets/
  skills/      game、motionfx、slides、office、webpage、longform、knowledge
  tools/       浏览器验收脚本
```

`xlsx` 导出为零依赖手写 ZIP + SpreadsheetML 实现，不要求安装 Office 库。

## 小规模横评

2026-08-18 在同一 `MiniMax-M3` 上进行了两题 × 四 CLI 的探索性 smoke test：i-agent、Claude Code CLI、Codex CLI 与 OpenCode 均为 **26/26**；i-agent 总耗时 **132.7s**，其余三者为 **237.9–361.8s**，同时 i-agent 的总输入 token 最少。该结果只有单次小样本，证明的是本轮工程效率，不是通用能力排行榜。

可复现台架、逐项数据、协议差异和限制见 [`benchmarks/minimax-m3-smoke/REPORT.md`](benchmarks/minimax-m3-smoke/REPORT.md)；8 个成功 run 的真实交付物可在单文件离线浏览器 [`ARTIFACTS.html`](benchmarks/minimax-m3-smoke/ARTIFACTS.html) 中预览和下载。

## 许可证

仓库目前未声明开源许可证；公开可读不等于自动授予复制、修改或分发权。如需开放使用，请由仓库所有者选择并添加明确许可证。
