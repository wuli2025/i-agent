# i-agent

轻量办公主力 agent。纯 Rust 单二进制（约 4-6MB，为 Claude Code 原生构建的约 6%），面向国产模型设计，短上下文（8k-32k）也能稳定干长活。无权限门控——设计为每实例跑在独立 Docker 容器中。

## 能做什么

| 领域 | 能力 | 交付形态 |
|---|---|---|
| 文字互动游戏 | 橙光式加点/好感/旗标/检定/加权多结局，内置生产级 VN 引擎模板 | 单文件 HTML，双击即玩 |
| PPT/演示 | 投影式一屏一意幻灯，AI 生图配插画，可打印成 PDF | 单文件 HTML |
| 办公表格 | CSV 读写编辑、统计聚合、零依赖导出真 .xlsx、数据图表报告 | .xlsx / 报告 HTML |
| 长文本 | 大纲先行 + 子 agent 分章并行扇出 + Bible 一致性锚定，十万字不触顶 | 分章 md + 合并 |
| 知识体系 | 资料整理成可 grep 检索的结构化知识库 | kb/ 文件夹 |
| 网页/动效 | 单文件响应式明暗主题页面，镜头/转场/粒子动效指令集 | 单文件 HTML |
| 生图 | MiniMax image-01 / 智谱 CogView-4 / 硅基流动 Kolors，自动容错切换 | png |

## 快速开始

```
# 任选一个国产模型密钥（内置直连，按下表自动选用）
set KIMI_API_KEY=...        # 或 DEEPSEEK_API_KEY / ZHIPU_API_KEY / MINIMAX_API_KEY / MOONSHOT_API_KEY / DASHSCOPE_API_KEY / SILICONFLOW_API_KEY
i-agent                     # 交互模式
i-agent -p "做一个修仙题材的加点游戏"   # 无头模式
```

内置供应商（`--provider` 可点名）：

| 名字 | 密钥变量 | 默认模型 | 端点 / 协议 |
|---|---|---|---|
| kimi | `KIMI_API_KEY` | k3 | api.kimi.com/coding · Anthropic |
| deepseek | `DEEPSEEK_API_KEY` | deepseek-chat | api.deepseek.com/anthropic · Anthropic |
| glm | `ZHIPU_API_KEY` / `GLM_API_KEY` | glm-4.6 | open.bigmodel.cn/api/anthropic · Anthropic |
| minimax | `MINIMAX_API_KEY` | MiniMax-M3 | api.minimaxi.com/anthropic · Anthropic |
| moonshot | `MOONSHOT_API_KEY` | kimi-k2-0905-preview | api.moonshot.cn/v1 · OpenAI 兼容 |
| qwen | `DASHSCOPE_API_KEY` / `QWEN_API_KEY` | qwen-plus | dashscope compatible-mode · OpenAI 兼容 |
| siliconflow | `SILICONFLOW_API_KEY` | DeepSeek-V3 | api.siliconflow.cn/v1 · OpenAI 兼容 |

能走 Anthropic 协议的一律走 Anthropic 端点：思考内容由服务端剥离成独立 block，不会混进正文，长任务更稳。要换模型用 `-m`，要用别的端点走配置文件或 `I_AGENT_*`。

任意 OpenAI 兼容端点：`I_AGENT_BASE_URL` + `I_AGENT_API_KEY` (+ `I_AGENT_MODEL`)。
Anthropic 协议端点：`ANTHROPIC_AUTH_TOKEN`（或 `ANTHROPIC_API_KEY`）+ `ANTHROPIC_BASE_URL` (+ `ANTHROPIC_MODEL`)，与 Claude Code 同一套变量。

**供应商选择规则**：显式配置（`I_AGENT_*` / `ANTHROPIC_*` / 配置文件 providers）优先且**排他**——只要有任何显式配置，靠密钥环境变量扫出来的内置供应商就不会进入自动候选和回退链（避免给生图准备的 `MINIMAX_API_KEY` 劫持对话模型）。`--provider` 点名不受此限。供应商回退发生时会向 stderr 打警告，绝不静默换模型。

**代理**：认 `HTTP(S)_PROXY` / `ALL_PROXY`，并按目标逐个匹配 `no_proxy`/`NO_PROXY`（支持 `127.*` 这类 glob 和域名后缀写法）；发往 localhost/127.x/::1 的请求永远直连。

## Docker（推荐的隔离运行方式）

```
docker build -t i-agent .
docker run --rm -e MINIMAX_API_KEY=xxx -v $PWD:/work i-agent -p "把 data.csv 做成月度报告"
```

## 命令行

```
i-agent [选项] [任务]
  -p, --print        无头模式，执行完退出
  -C, --dir <路径>   工作目录
  --provider <名>    deepseek/kimi/glm/qwen/siliconflow/minimax/custom
  -m, --model <名>   覆盖模型
  -c, --continue     继续上次会话（会话存 <工作目录>/.i-agent/session.jsonl）
  -q, --quiet        隐藏工具调用过程
  init-assets        释放内置技能包到 ~/.i-agent/assets
                     （该目录随二进制升级自动刷新；要自定义技能包请复制到别处并用 I_AGENT_ASSETS 指过去）
```

配置文件（可选）：`~/.i-agent/config.json` 或 `<工作目录>/.i-agent/config.json`
```json
{ "context_window": 32768, "max_turns": 48,
  "providers": [{"name":"my","base":"https://.../v1","model":"...","key_env":"MY_KEY"}] }
```
`I_AGENT_CTX=8192` 可为短窗口模型收紧上下文预算（触发更激进的两级压缩）。

## 为什么短上下文也能干长活

1. 两级上下文压缩：先裁旧工具输出，再 LLM 摘要旧历史，切点永不撕裂工具调用对；
2. task 子任务扇出：长内容分章并行写，每章上下文独立且轻，主线程只持大纲；
3. 技能包渐进披露：系统提示只注入一行索引，做任务时才读全文；
4. 工具输出全部有界截断，read 默认分段。

## 架构

```
src/
  main.rs      CLI（交互 REPL + 无头）
  agent.rs     双层循环内核：工具耗尽 + 多轮；doom-loop 熔断；task 并行扇出
  llm.rs       OpenAI 兼容流式客户端 + 供应商自动容错 + <think> 过滤（国产推理模型适配）
  context.rs   token 记账 + 两级压缩
  tools/       read write edit ls glob grep shell fetch image xlsx task
  skills.rs    技能包发现/内嵌释放/索引注入
assets/skills/ 六大能力包（game/motionfx/slides/office/webpage/longform/knowledge）
```

xlsx 导出为零依赖手写实现（CSV 解析 + ZIP + SpreadsheetML），无需装任何 Office 库。
