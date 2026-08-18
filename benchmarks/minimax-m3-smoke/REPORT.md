# MiniMax-M3：i-agent vs Claude Code / Codex / OpenCode

测试日期：2026-08-18

环境：Windows 11 `10.0.26200`、Python 3.14.4

模型：四条泳道均为 `MiniMax-M3`

> 这是两题、每题每工具一次的探索性 smoke test。它能说明本次任务上的明显工程开销，不能说明一般意义上的模型或 agent 排名。完整逐项数据见 [`results.json`](results.json)，任务与复现方式见 [`README.md`](README.md)。

**真实压测产物**：[`ARTIFACTS.html`](ARTIFACTS.html) 是一个零外链、自包含的离线证据包，内嵌 8 个成功 workspace 的白名单交付物，可逐文件预览、核对 SHA-256 或下载原文件。出于安全考虑，它明确不包含 CLI home、认证配置、会话状态和原始日志。

## 一句话结论

**四个 CLI 的产物质量全部满分；i-agent 的优势没有体现在“别人做不对”，而体现在相同正确率下更快、提示词流量更小、安装体积显著更低。**

## 结果总表

| Agent | 版本 | 得分 | 总耗时 | 总输入 token | 其中 fresh / cache read | 输出 token | 可见请求数 |
|---|---:|---:|---:|---:|---:|---:|---:|
| **i-agent** | 0.1.0 | **26/26** | **132.7s** | **109,941** | **17,111 / 92,830** | **7,068** | 17 |
| OpenCode | 1.17.19 | **26/26** | 237.9s | 589,977 | 48,638 / 541,339 | 12,088 | 29 |
| Codex CLI | 0.147.0 | **26/26** | 278.8s | 594,964 | 38,631 / 556,333 | 16,117 | 上游未提供 |
| Claude Code | 2.1.233 | **26/26** | 361.8s | 216,981 | 31,280 / 185,701 | 18,848 | 27 |

这里的“总输入”是 fresh、cache read（以及存在时 cache write）相加的上游口径。缓存 token 常有折扣，因此该列不能直接换算成费用；报告同时列 fresh，避免把 cache-heavy 外壳夸大成同等成本。

相对本次其他三种外壳，i-agent：

- 比 Claude Code **快 63.3%**，总输入少 **49.3%**，fresh 输入少 **45.3%**；
- 比 Codex **快 52.4%**，总输入少 **81.5%**，fresh 输入少 **55.7%**；
- 比 OpenCode **快 44.2%**，总输入少 **81.4%**，fresh 输入少 **64.8%**。

## 分题结果

| 任务 | i-agent | Claude Code | Codex | OpenCode |
|---|---:|---:|---:|---:|
| Python 缺陷修复（12 分） | **12/12 · 53.2s** | 12/12 · 184.7s | 12/12 · 74.4s | 12/12 · 116.5s |
| CSV 清洗与办公报告（14 分） | **14/14 · 79.5s** | 14/14 · 177.1s | 14/14 · 204.4s | 14/14 · 121.4s |

外部 grader 验证了 Decimal 端到端计算、聚合后 HALF_UP、异常输入、真实 unittest，以及 CSV schema、去重、拒绝行号、精确营收、JSON 形状和 Markdown 一致性。四家均无超时，进程退出码均为 0。

## 各自差别与优势

### i-agent

- **本轮最强项是效率，不是独占正确率。** 两题均最快；总输入、fresh 输入和总输出也都是最低。
- 直接针对短上下文和国产模型裁剪系统提示、工具 schema 与输出；17 次请求完成两题，少于 Claude 的 27 次和 OpenCode 的 29 次。
- 当前本机 release exe 为 **2.50 MiB**，技能和工具已经内嵌；非常适合容器内的一次性办公任务、小机器和批量 worker。
- 内置 Python、CSV/XLSX、浏览器验收和交付门禁，使“生成产物再自验”不必完全依赖模型临时造流程。
- 代价是生态和通用扩展能力仍明显少于成熟 CLI；安全模型也不同——i-agent 本身不做权限门控，必须依靠容器或受控工作区。

### Claude Code

- 两题都满分，修复和报告纪律可靠；在本轮并没有质量劣势。
- 但即使使用 `--bare` 隔离用户插件，仍是四家最慢、输出 token 最多的一条泳道，说明成熟通用 agent 的系统与工具循环开销更高。
- 它的优势在更完整的开发者生态、权限系统、IDE/插件/MCP/会话能力；本测试刻意关闭了这些用户扩展，因此没有衡量生态收益。

### Codex CLI

- 缺陷修复仅 74.4s，是该题第二快；编码任务上的执行路径很紧凑。
- 数据报告任务增至 204.4s，总输入接近 59.5 万 token；缓存命中很高，所以不能只按总输入判断费用，但上下文滚动规模明显大。
- **协议限制最明确**：Codex 不能直接使用用户给出的 Anthropic Messages URL。本轮只能把同一 key / MiniMax-M3 配到 `https://api.minimaxi.com/v1/responses`。因此它是同模型参考，不是完全同 wire protocol 对照。

### OpenCode

- 两题满分，且可通过 `@ai-sdk/anthropic` 直接使用 MiniMax Anthropic 端点；开放 provider 配置很灵活。
- 数据报告 121.4s，为该题第二快。
- 本轮请求数最多（29），总输入约 59.0 万 token；它用大量 cache read 换取通用 agent 能力。在任务更复杂、插件或多 provider 场景中这可能值得，但本轮小任务上开销显著。

## 安装体积旁证

本机同一时点测得：

| 工具 | 本地文件占用 |
|---|---:|
| **i-agent release exe** | **2.50 MiB** |
| Claude Code exe | 305.56 MiB |
| Codex npm package | 353.30 MiB |
| OpenCode npm package | 527.34 MiB |

这不是严格的跨平台制品比较：Claude 是单 exe，Codex/OpenCode 数字是本机 npm 包目录，可能包含辅助文件；但数量级足以说明 i-agent 的部署体积优势。

## 必须保留的限制

1. 只有 2 道小题、1 个模型、1 台 Windows 机器、每条泳道 1 次；没有方差、置信区间或显著性。
2. 任务偏确定性 Python/数据处理，没有覆盖大型仓库导航、复杂重构、MCP、IDE、图像、PPT 或长会话。
3. Codex 使用 Responses；其余三家使用 Anthropic Messages，wire protocol 不完全一致。
4. token 来自各 CLI / 上游返回的 usage，字段语义可能有细微差别；未用统一中间代理重新计量。
5. 执行顺序固定为 i-agent → Claude Code → Codex → OpenCode，未随机化，可能受服务端负载和缓存影响。
6. 首次跑 `data-report/opencode` 时，外层编排进程在总计 10 分钟处被终止，未留下可用 meta；随后删除该工作区并从 fixture 重跑。第二次 run 本身是完整的 121.4s，但上游 prompt cache 可能受第一次尝试预热，所以 OpenCode 的该项时间/fresh token 应谨慎解释。

## 适合怎样使用这份结论

- 如果目标是**国产模型 + 容器化办公流水线 + 低部署/上下文开销**，本轮证据支持优先试 i-agent。
- 如果目标是**成熟 IDE、插件、权限治理和广泛生态**，Claude Code / Codex / OpenCode 的额外开销可能换来了本测试未覆盖的价值。
- 下一轮应至少每题跑 3 次并随机顺序，再加入仓库级改错、单文件交互 HTML 真浏览器验收和长文档任务，才能判断优势是否稳定。
