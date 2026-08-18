# MiniMax-M3 × 4 CLI 小规模 smoke test

这个目录比较同一个 `MiniMax-M3` 模型在四种 agent 外壳中的表现：

- i-agent
- Claude Code CLI
- Codex CLI
- OpenCode

它是**每题每工具只跑一次**的探索性测试，用来发现协议兼容、工具循环、交付纪律和明显的 token/耗时差异，不是统计显著的排行榜。

## 公平性边界

- 四条泳道使用相同模型、相同 key、相同提示词和逐次复制的干净 fixture。
- 每次串行执行，避免并发限流；工作区、会话目录和用户插件相互隔离。
- i-agent、Claude Code 和 OpenCode 请求 MiniMax 的 Anthropic Messages 端点。
- Codex 当前只支持 OpenAI Responses wire API，因此请求同一个 MiniMax-M3 的 `/v1/responses`。这条泳道是**同模型、不同协议**，报告会始终保留该限制。
- 外部 grader 位于工作区之外，只看真实文件和行为，不看 agent 的完成自述。
- token 数来自各 CLI 自己返回的 usage；上游/CLI 口径不一致或拿不到时显示为空，不推算假精度。

## 两道任务

1. `bugfix`：修复一个 Decimal、汇总和输入验证都有陷阱的 Python 函数，并通过公开与外部用例。
2. `data-report`：清洗带重复和坏行的 CSV，生成规范化 CSV、拒绝清单、精确 JSON 和 Markdown 办公报告。

满分分别为 12 和 14。Fixture 在 [`fixtures/`](fixtures/)，任务定义在 [`tasks.json`](tasks.json)，外部判分在 [`grade.py`](grade.py)。

## 运行

要求 Windows PowerShell 7、Rust、Python，以及已经安装在 PATH 中的 `claude`、`codex`、`opencode`。

```powershell
$env:ANTHROPIC_AUTH_TOKEN = "你的 MiniMax key"
$env:ANTHROPIC_BASE_URL = "https://api.minimaxi.com/anthropic"
$env:ANTHROPIC_DEFAULT_HAIKU_MODEL = "MiniMax-M3"

.\benchmarks\minimax-m3-smoke\run.ps1
```

可只跑部分泳道或任务：

```powershell
.\benchmarks\minimax-m3-smoke\run.ps1 `
  -Agents iagent,claude `
  -Tasks bugfix `
  -TimeoutSeconds 600
```

Runner 只从进程环境读取密钥。OpenCode 配置使用 `{env:ANTHROPIC_AUTH_TOKEN}`，Codex 配置只记录环境变量名 `MINIMAX_BENCH_API_KEY`；明文 key 不会写入配置、meta 或汇总。

> 四个 agent 都会在各自的临时工作区中自动执行命令和修改文件。请在受控机器上运行，不要把未知 fixture 放进这个 bypass-permissions 台架。

## 输出

- `.runs/`：各次产物与原始 stdout/stderr（ignored）
- `.homes/`：隔离的 CLI 状态（ignored）
- `results.local.json`：本机脱敏汇总（ignored，便于复核）
- [`results.json`](results.json)：复核后提交的规范化结果
- [`REPORT.md`](REPORT.md)：面向人的结论、优势和限制
- [`ARTIFACTS.html`](ARTIFACTS.html)：8 个成功 run 的真实交付物离线浏览器，可预览、筛选并下载原文件
- [`ARTIFACTS.html.sha256`](ARTIFACTS.html.sha256)：HTML 完整性哈希

`.runs` 仍在本机时，可重新构建证据包：

```powershell
python .\benchmarks\minimax-m3-smoke\build_artifacts_html.py `
  --copy-to "$HOME\Desktop\i-agent-MiniMax-M3-压测产物-2026-08-18.html"
```

Builder 会重新判分 8 个 workspace、核对登记的文件大小并在嵌入前扫描密钥和个人绝对路径。HTML 只收录白名单内的最终 Python/CSV/JSON/Markdown 产物与共享 fixture；不会收录 `.homes`、`.i-agent`、session、pycache 或原始 stdout/stderr。
