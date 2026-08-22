use crate::config::Config;
use crate::context;
use crate::llm::{self, Msg, ToolCall};
use crate::prompt;
use crate::session;
use crate::tools;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    Tool { name: String, detail: String },
}

type EventSink = Arc<dyn Fn(AgentEvent) + Send + Sync>;

struct ExecutedTool {
    id: String,
    content: String,
    succeeded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FailedExecution {
    tool: String,
    target: Option<String>,
    validation_kind: Option<String>,
    detail: String,
}

pub struct Agent {
    pub cfg: Arc<Config>,
    pub messages: Vec<Msg>,
    /// 会话落盘游标。None = 不落盘（子任务）。
    /// 分支派生时每个变体拿到自己的游标，写进各自的分支链。
    pub sink: Option<session::Sink>,
    depth: u8,
    recent_sigs: Vec<String>,
    /// 本次运行产出的 HTML 交付物
    html_artifacts: Vec<String>,
    /// Last failed execution/validation, including enough identity to prevent an unrelated
    /// successful tool from laundering it into a completed delivery.
    failed_exec: Option<FailedExecution>,
    /// 其中已经在真浏览器里冒烟通过的
    verified: Vec<String>,
    /// 被当作 bundle 输入的片段——它们是零件不是交付物，不进门禁
    bundle_inputs: Vec<String>,
    pub usage_in: u64,
    pub usage_out: u64,
    pub usage_cached: u64,
    pub llm_calls: u64,
    event_sink: Option<EventSink>,
}

impl Agent {
    pub fn new(cfg: Arc<Config>, depth: u8) -> Agent {
        let sys = prompt::system_prompt(&cfg);
        Agent {
            cfg,
            messages: vec![Msg::text("system", &sys)],
            sink: None,
            depth,
            recent_sigs: Vec::new(),
            html_artifacts: Vec::new(),
            failed_exec: None,
            verified: Vec::new(),
            bundle_inputs: Vec::new(),
            usage_in: 0,
            usage_out: 0,
            usage_cached: 0,
            llm_calls: 0,
            event_sink: None,
        }
    }

    pub fn set_event_sink(&mut self, sink: EventSink) {
        self.event_sink = Some(sink);
    }

    fn emit_event(&self, event: AgentEvent) {
        if let Some(sink) = &self.event_sink {
            sink(event);
        }
    }

    /// 还没经过真浏览器验证的 HTML 产物
    fn unverified(&self) -> Vec<String> {
        self.html_artifacts
            .iter()
            .filter(|p| !self.verified.contains(p))
            .cloned()
            .collect()
    }

    fn log(&self, s: &str) {
        if !self.cfg.quiet {
            eprintln!("\x1b[2m{s}\x1b[0m");
        }
    }

    /// 从既有消息前缀派生一个分支 agent。
    ///
    /// 前缀是 `clone` 出来的，逐字节与源分支一致 —— 这正是前缀缓存能命中的前提，
    /// 也是 F-14「共享前缀」的全部技术含量：谁都别去重建 system prompt、别重算技能注入。
    /// depth 固定 0，保证工具集与源分支相同（工具 schema 变了同样会击穿缓存）。
    pub fn fork(cfg: Arc<Config>, prefix: &[Msg], sink: Option<session::Sink>) -> Agent {
        let mut a = Agent::new(cfg, 0);
        a.messages = prefix.to_vec();
        a.sink = sink;
        a
    }

    /// 把一条消息写进当前分支（若配置了落盘游标）
    fn persist(&mut self, msg: &Msg) {
        let ws = self.cfg.workspace.clone();
        if let Some(s) = &mut self.sink {
            s.append(&ws, msg);
        }
    }

    pub fn run(&mut self, input: &str, on_text: &dyn Fn(&str)) -> Result<String, String> {
        let enriched = match crate::skills::match_hint(&self.cfg.assets_dir, input) {
            Some(hint) => format!("{input}{hint}"),
            None => input.to_string(),
        };
        self.run_raw(&enriched, on_text)
    }

    /// 与 `run` 相同，但**不做技能包注入** —— 供分支变体使用：
    /// 共享前缀里已经带着技能正文了，再注入一次既浪费 token 又会让各分支前缀分叉。
    pub fn run_raw(&mut self, input: &str, on_text: &dyn Fn(&str)) -> Result<String, String> {
        let user = Msg::text("user", input);
        self.persist(&user);
        self.messages.push(user);
        self.turn_loop(
            on_text,
            requires_tool_evidence(input),
            requires_delivery_audit(input),
        )
    }

    fn turn_loop(
        &mut self,
        on_text: &dyn Fn(&str),
        requires_tool_evidence: bool,
        requires_delivery_audit: bool,
    ) -> Result<String, String> {
        let max_turns = if self.depth == 0 {
            self.cfg.max_turns
        } else {
            self.cfg.max_turns / 2
        };
        let specs = tools::specs(&self.cfg, self.depth);
        let mut last_text = String::new();

        let mut empty_nudges = 0;
        let mut gate_nudges = 0;
        let mut successful_tools = 0usize;
        let mut execution_evidence_tools = 0usize;
        let mut audit_requested = false;
        let mut audit_tool_count = 0usize;
        let mut audit_nudges = 0;

        for turn in 0..max_turns {
            context::ensure_budget(&self.cfg, &mut self.messages);
            let out = match llm::chat(&self.cfg, &self.messages, &specs, on_text) {
                Ok(o) => o,
                // 整轮只有思考内容 → 过滤后为空。推一把继续，而不是让任务中止。
                Err(e) if e == llm::EMPTY_MARK => {
                    empty_nudges += 1;
                    if empty_nudges > 3 {
                        return Err("模型连续多轮没有产生任何输出，已放弃".into());
                    }
                    self.log("▸ 本轮模型无输出，推一把继续");
                    self.messages.push(Msg::text(
                        "user",
                        "[系统] 你上一轮没有产生任何输出。不要只在心里想——请立即调用工具推进任务；\
                         若任务确已完成，就用一句话说明做了什么、产物在哪。",
                    ));
                    continue;
                }
                Err(e) => return Err(e),
            };
            self.usage_in += out.prompt_tokens;
            self.usage_out += out.completion_tokens;
            self.usage_cached += out.cached_tokens;
            self.llm_calls += 1;
            let msg = out.msg;
            self.persist(&msg);
            if let Some(c) = &msg.content {
                last_text = c.clone();
            }
            let tool_calls = msg.tool_calls.clone().unwrap_or_default();
            self.messages.push(msg);

            if tool_calls.is_empty() {
                // 用户明确要求改代码/跑命令/做文件/操纵浏览器时，零工具调用却声称完成是
                // 可判定的假交付。先给模型一次纠偏机会；仍不执行则诚实失败，不能把幻觉
                // 当成功 result 交给 Polaris。
                if requires_tool_evidence && execution_evidence_tools == 0 {
                    if gate_nudges < 2 {
                        gate_nudges += 1;
                        self.log("▸ 交付门禁：执行型任务没有任何工具证据");
                        self.messages.push(Msg::text(
                            "user",
                            "[系统] 交付被拦下：这是执行型任务，但本轮没有任何工具调用。\
                             不得声称已修改、已运行或已验证；请立即读取真实文件并调用所需工具推进，\
                             最后用真实命令输出或产物路径交付。",
                        ));
                        continue;
                    }
                    return Err("执行型任务连续两轮没有工具调用，已拒绝假完成".into());
                }
                // 交付门禁之一：最后一次 shell/python 是失败的，不算完成。
                // 一次失败的自检 == 没有自检。放它过去，产物就是「没验证过」发出去的。
                if let Some(fail) = self.failed_exec.clone() {
                    if gate_nudges < 2 {
                        gate_nudges += 1;
                        self.log("▸ 交付门禁：上一次命令执行失败，未通过自检");
                        self.messages.push(Msg::text(
                            "user",
                            &format!(
                                "[系统] 交付被拦下：你最后一次执行是失败的——\n{}\n\n\
                                 一次失败的自检等于没有自检。请先把它跑通：\n\
                                 · 跑 Python 一律用 python 工具（直接给源码，不经 shell，没有引号问题）；\n\
                                 · shell 命令必须用本机 shell 的语法。\n\
                                 命令成功退出后才能给出交付说明；严禁凭记忆写「自证结果」。",
                                fail.detail
                            ),
                        ));
                        continue;
                    }
                    return Err(format!(
                        "执行/验证工具连续失败，已拒绝未经验证的交付: {}",
                        fail.detail
                    ));
                }
                // 交付门禁之二：HTML 产物没在真浏览器里验证过，不算完成。
                // 「我检查过代码了」不作数——文本检查看不出白屏。
                let pending = self.unverified();
                if !pending.is_empty() {
                    if gate_nudges < 2 {
                        gate_nudges += 1;
                        self.log(&format!(
                            "▸ 交付门禁：{} 尚未通过浏览器验证",
                            pending.join(", ")
                        ));
                        self.messages.push(Msg::text(
                            "user",
                            &format!(
                                "[系统] 交付被拦下：以下 HTML 产物还没有在真浏览器里验证过——{}。\n\
                                 请对每个文件调用一次 browser 工具（或 check kind=html）。\n\
                                 若报白屏或运行时异常，先修好再验证；全部通过后才能给出交付说明。",
                                pending.join("、")
                            ),
                        ));
                        continue;
                    }
                    return Err(format!(
                        "HTML 产物连续未通过真浏览器验证，已拒绝交付: {}",
                        pending.join("、")
                    ));
                }
                // 文件型交付不能在「工具都跑过」后直接相信模型的完成陈述。要求它在
                // 所有常规门禁通过后，重新对照原始需求读取产物，尤其核对精确标题、
                // 文件名/格式/数量。审计轮本身也必须产生新的工具证据。
                if requires_delivery_audit {
                    if !audit_requested {
                        audit_requested = true;
                        audit_tool_count = successful_tools;
                        self.log("▸ 最终交付审计：逐项复核原始要求与真实产物");
                        self.messages.push(Msg::text(
                            "user",
                            "[系统] 进入最终交付审计。请重新读取原始要求和真实产物，逐项核对：\
                             精确标题/必需文字、文件名与格式、数量、可解析性，以及所有明确要求。\
                             必须实际调用工具检查；发现缺失就修复、重新导出并复验。\
                             不得只凭上下文总结，也不得在未核完前结束。",
                        ));
                        continue;
                    }
                    if successful_tools == audit_tool_count {
                        if audit_nudges < 1 {
                            audit_nudges += 1;
                            self.log("▸ 最终交付审计没有新的工具证据");
                            self.messages.push(Msg::text(
                                "user",
                                "[系统] 审计尚未执行：你没有新增任何工具调用。请立即读取/解析产物，\
                                 对照原始要求逐项检查；有缺项就修复并重新验证。",
                            ));
                            continue;
                        }
                        return Err("最终交付审计连续两轮没有工具调用，已拒绝未经审计的交付".into());
                    }
                }
                return Ok(last_text);
            }

            if !last_text.is_empty() {
                on_text("\n");
                last_text.clear();
            }

            let results = self.exec_tools(&tool_calls);
            for (tool_call, result) in tool_calls.iter().zip(results.iter()) {
                let name = tool_call.function.name.as_str();
                if result.succeeded {
                    successful_tools += 1;
                    if is_execution_evidence_tool(name) {
                        execution_evidence_tools += 1;
                    }
                }
            }
            for result in results {
                let tm = Msg::tool_result(&result.id, result.content);
                self.persist(&tm);
                self.messages.push(tm);
            }
            if turn == max_turns - 1 {
                let pending = self.unverified();
                if let Some(blocker) = terminal_blocker(
                    requires_tool_evidence,
                    execution_evidence_tools,
                    self.failed_exec
                        .as_ref()
                        .map(|failure| failure.detail.as_str()),
                    &pending,
                    requires_delivery_audit,
                    audit_requested,
                    successful_tools,
                    audit_tool_count,
                ) {
                    return Err(format!("达到最大轮数，但交付门禁仍未通过: {blocker}"));
                }
                self.messages.push(Msg::text(
                    "user",
                    "[系统] 已达最大轮数，请立即总结目前完成情况与产物路径，不要再调用工具。",
                ));
                let fin = llm::chat(&self.cfg, &self.messages, &Value::Array(vec![]), on_text)?;
                return Ok(fin.msg.content.unwrap_or_default());
            }
        }
        let pending = self.unverified();
        if let Some(blocker) = terminal_blocker(
            requires_tool_evidence,
            execution_evidence_tools,
            self.failed_exec
                .as_ref()
                .map(|failure| failure.detail.as_str()),
            &pending,
            requires_delivery_audit,
            audit_requested,
            successful_tools,
            audit_tool_count,
        ) {
            Err(format!("交付门禁未通过: {blocker}"))
        } else {
            Ok(last_text)
        }
    }

    fn exec_tools(&mut self, tool_calls: &[ToolCall]) -> Vec<ExecutedTool> {
        for tool_call in tool_calls {
            self.emit_event(AgentEvent::Tool {
                name: tool_call.function.name.clone(),
                detail: tool_call.function.arguments.chars().take(120).collect(),
            });
        }
        // 多个 task 并行；其余顺序执行
        let tasks: Vec<&ToolCall> = tool_calls
            .iter()
            .filter(|t| t.function.name == "task")
            .collect();
        let mut results: Vec<ExecutedTool> = Vec::new();

        if tasks.len() > 1 && tasks.len() == tool_calls.len() && self.depth == 0 {
            self.log(&format!("▸ 并行派出 {} 个子任务", tasks.len()));
            let handles: Vec<(String, std::thread::JoinHandle<Result<String, String>>)> = tasks
                .iter()
                .map(|tc| {
                    let cfg = self.cfg.clone();
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    let id = tc.id.clone();
                    let h = std::thread::spawn(move || run_subtask(cfg, &args));
                    (id, h)
                })
                .collect();
            for (id, h) in handles {
                let result = h.join().unwrap_or_else(|_| Err("子任务线程崩溃".into()));
                self.log("▸ 子任务完成");
                let (content, succeeded) = match result {
                    Ok(content) => (content, true),
                    Err(error) => (format!("错误: {error}"), false),
                };
                results.push(ExecutedTool {
                    id,
                    content,
                    succeeded,
                });
            }
            return results;
        }

        for tc in tool_calls {
            let name = &tc.function.name;
            let args: Value = match serde_json::from_str(&tc.function.arguments) {
                Ok(v) => v,
                Err(e) => {
                    results.push(ExecutedTool {
                        id: tc.id.clone(),
                        content: format!("错误: 工具参数不是合法 JSON ({e})。请修正后重试。"),
                        succeeded: false,
                    });
                    continue;
                }
            };
            if args.get("__truncated__").is_some() {
                results.push(ExecutedTool {
                    id: tc.id.clone(),
                    content: "错误: 本次工具参数超出单次输出上限被截断，未执行。请把内容拆块：先 write 写文件开头，再用 write append:true 续写，每块 500-800 行。".into(),
                    succeeded: false,
                });
                continue;
            }

            // doom loop 熔断：同工具同参数连调 3 次
            let sig = format!("{name}:{}", tc.function.arguments);
            self.recent_sigs.push(sig.clone());
            if self.recent_sigs.len() > 6 {
                self.recent_sigs.remove(0);
            }
            if self.recent_sigs.iter().filter(|s| **s == sig).count() >= 3 {
                results.push(ExecutedTool {
                    id: tc.id.clone(),
                    content: "错误: 检测到同一调用重复 3 次，已熔断。请换一种方法或修改参数。"
                        .into(),
                    succeeded: false,
                });
                continue;
            }

            let brief: String = tc.function.arguments.chars().take(120).collect();
            self.log(&format!("▸ {name} {brief}"));

            let result = if name == "task" {
                if self.depth >= 1 {
                    Err("子任务里不能再派子任务，请直接完成。".into())
                } else {
                    run_subtask(self.cfg.clone(), &args)
                }
            } else {
                tools::dispatch(name, &args, &self.cfg)
            };
            let (content, succeeded) = match result {
                Ok(s) => (s, true),
                Err(e) => (format!("错误: {e}"), false),
            };
            self.track_artifacts(name, &args, &content, succeeded);
            results.push(ExecutedTool {
                id: tc.id.clone(),
                content,
                succeeded,
            });
        }
        results
    }

    /// 盯住 HTML 交付物的验证状态：谁被写出来了、谁真的在浏览器里跑通了。
    fn track_artifacts(&mut self, name: &str, args: &Value, result: &str, succeeded: bool) {
        let is_html = |p: &str| {
            let l = p.to_lowercase();
            l.ends_with(".html") || l.ends_with(".htm")
        };
        // 中间零件不是交付物：bundle 的输入片段（引擎头/尾、拆分的 body 段）
        // 单独打开本来就渲染不出东西，拿浏览器去验它们纯属误伤。
        // 约定：路径里任一段以 _ 开头（如 _parts/_head.html）即视为中间产物。
        let is_fragment = |p: &str| p.split(['/', '\\']).any(|seg| seg.starts_with('_'));
        match name {
            // 产出 HTML → 登记为待验证
            "write" | "bundle" => {
                // bundle 的 parts 是输入，不是交付物——从待验证清单里摘掉
                if name == "bundle" {
                    if let Some(parts) = args.get("parts").and_then(|v| v.as_array()) {
                        for p in parts.iter().filter_map(|v| v.as_str()) {
                            self.html_artifacts.retain(|x| x != p);
                            self.bundle_inputs.push(p.to_string());
                        }
                    }
                }
                let key = if name == "bundle" { "out" } else { "path" };
                if let Some(p) = args.get(key).and_then(|v| v.as_str()) {
                    let skip = is_fragment(p) || self.bundle_inputs.iter().any(|x| x == p);
                    if is_html(p) && !skip && succeeded {
                        if !self.html_artifacts.iter().any(|x| x == p) {
                            self.html_artifacts.push(p.to_string());
                        }
                        // 内容改了就得重新验证
                        self.verified.retain(|x| x != p);
                    }
                }
            }
            // 改了 HTML → 之前的验证作废
            "edit" => {
                if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                    if is_html(p) {
                        self.verified.retain(|x| x != p);
                    }
                }
            }
            // 真浏览器跑通了才算数
            "browser" | "check" => {
                if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                    if is_html(p) && succeeded && result.contains("浏览器冒烟通过") {
                        if !self.verified.iter().any(|x| x == p) {
                            self.verified.push(p.to_string());
                        }
                    }
                }
            }
            _ => {}
        }

        // 失败的执行/验证结果不能被模型用自然语言总结“冲掉”。这里依赖 dispatch 的
        // 结构化 Result，而不是解析各工具的人类可读文本；后续同类成功执行才清除失败。
        if is_execution_evidence_tool(name) {
            if !succeeded {
                let head: String = result.lines().take(6).collect::<Vec<_>>().join("\n");
                self.failed_exec = Some(FailedExecution {
                    tool: name.to_string(),
                    target: tool_verification_target(name, args),
                    validation_kind: tool_validation_kind(name, args),
                    detail: format!("{name}: {head}"),
                });
            } else if self
                .failed_exec
                .as_ref()
                .is_some_and(|failure| success_covers_failure(failure, name, args))
            {
                self.failed_exec = None;
            }
        }
    }
}

fn requires_tool_evidence(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    let explanatory = ["解释", "为什么", "什么是", "explain ", "what is "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix));
    if explanatory
        && !["必须运行", "真实运行", "写入文件", "run `"]
            .iter()
            .any(|marker| lower.contains(marker))
    {
        return false;
    }
    [
        "修复",
        "实现",
        "更新",
        "真实运行",
        "必须运行",
        "只改",
        "必须生成",
        "全部放在",
        "保存到",
        "写入文件",
        "打开 http",
        "访问 http",
        "cloakbrowser",
        "演示工坊",
        "fix ",
        "run `",
        "write file",
        "create file",
        "implement ",
        "update ",
        "build ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn requires_delivery_audit(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    if ["解释", "为什么", "什么是", "explain ", "what is "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return false;
    }
    let hard_markers = [
        "必须生成",
        "全部放在",
        "交付",
        "polaris.slides.json",
        "create the requested files",
        "deliver a",
        "deliverable",
    ];
    if hard_markers.iter().any(|marker| lower.contains(marker)) {
        return true;
    }
    let action = ["制作", "生成", "创建", "create ", "build "]
        .iter()
        .any(|marker| lower.contains(marker));
    let artifact = ["文件", "源稿", "pptx", "html", "演示", "deck"]
        .iter()
        .any(|marker| lower.contains(marker));
    action && artifact
}

#[cfg(test)]
fn tool_result_succeeded(name: &str, content: &str) -> bool {
    if content.starts_with("错误:") || content.starts_with("子任务失败:") {
        return false;
    }
    if matches!(name, "shell" | "python")
        && (content.starts_with("退出码 ")
            || content.contains("超时（")
            || content.starts_with("找不到可用的 python"))
    {
        return false;
    }
    if matches!(name, "browser" | "check")
        && (content.contains("冒烟未通过")
            || content.contains("冒烟无法执行")
            || content.contains("语法错误"))
    {
        return false;
    }
    true
}

fn tool_verification_target(name: &str, args: &Value) -> Option<String> {
    let keys: &[&str] = match name {
        "browser" | "check" => &["path", "url"],
        "write" | "edit" => &["path"],
        "bundle" => &["out"],
        _ => &[],
    };
    keys.iter()
        .find_map(|key| args.get(*key).and_then(|value| value.as_str()))
        .map(|target| target.replace('\\', "/"))
}

fn tool_validation_kind(name: &str, args: &Value) -> Option<String> {
    match name {
        "browser" => Some("html".into()),
        "check" => {
            if let Some(kind) = args.get("kind").and_then(|value| value.as_str()) {
                return Some(kind.to_ascii_lowercase());
            }
            let path = args
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            Some(if path.ends_with(".html") || path.ends_with(".htm") {
                "html".into()
            } else {
                "json".into()
            })
        }
        _ => None,
    }
}

fn success_covers_failure(
    failure: &FailedExecution,
    successful_tool: &str,
    successful_args: &Value,
) -> bool {
    let same_validation_family = matches!(
        (failure.tool.as_str(), successful_tool),
        ("browser", "check") | ("check", "browser")
    );
    if failure.tool != successful_tool && !same_validation_family {
        return false;
    }
    let successful_kind = tool_validation_kind(successful_tool, successful_args);
    if same_validation_family
        && (failure.validation_kind.as_deref() != Some("html")
            || successful_kind.as_deref() != Some("html"))
    {
        return false;
    }
    if failure.tool == successful_tool
        && failure.validation_kind.is_some()
        && failure.validation_kind != successful_kind
    {
        return false;
    }
    match &failure.target {
        Some(target) => {
            tool_verification_target(successful_tool, successful_args).as_ref() == Some(target)
        }
        None => failure.tool == successful_tool,
    }
}

fn terminal_blocker(
    requires_tool_evidence: bool,
    execution_evidence_tools: usize,
    failed_exec: Option<&str>,
    unverified: &[String],
    requires_delivery_audit: bool,
    audit_requested: bool,
    successful_tools: usize,
    audit_tool_count: usize,
) -> Option<String> {
    if requires_tool_evidence && execution_evidence_tools == 0 {
        return Some("执行型任务没有成功的工具证据".into());
    }
    if let Some(failure) = failed_exec {
        return Some(format!("最后一次执行或验证失败: {failure}"));
    }
    if !unverified.is_empty() {
        return Some(format!(
            "HTML 尚未通过真浏览器验证: {}",
            unverified.join("、")
        ));
    }
    if requires_delivery_audit && (!audit_requested || successful_tools <= audit_tool_count) {
        return Some("最终交付审计没有新的成功工具证据".into());
    }
    None
}

fn is_execution_evidence_tool(name: &str) -> bool {
    !matches!(name, "read" | "ls" | "glob" | "grep")
}

#[cfg(test)]
mod delivery_gate_tests {
    use super::{
        is_execution_evidence_tool, requires_delivery_audit, requires_tool_evidence,
        success_covers_failure, terminal_blocker, tool_result_succeeded, FailedExecution,
    };

    #[test]
    fn detects_explicit_execution_tasks_but_not_plain_questions() {
        assert!(requires_tool_evidence(
            "修复订单排序并真实运行 `npm test`，只改必要文件"
        ));
        assert!(requires_tool_evidence(
            "使用 Polaris 的 CloakBrowser 打开 http://127.0.0.1/catalog"
        ));
        assert!(requires_tool_evidence("必须生成 PPTX，全部放在工作目录"));
        assert!(!requires_tool_evidence(
            "解释一下稳定排序和不可变数组的区别"
        ));
        assert!(!requires_tool_evidence("把这段文字润色得更自然"));
        assert!(requires_tool_evidence(
            "Implement the missing validator and run tests"
        ));
        assert!(!requires_tool_evidence(
            "Explain how to implement a validator"
        ));
    }

    #[test]
    fn requires_a_fresh_final_audit_for_artifact_deliveries() {
        assert!(requires_delivery_audit(
            "制作恰好 12 页中文经营简报，标题为《北辰零售经营简报》，必须生成 PPTX、HTML 和 polaris.slides.json"
        ));
        assert!(requires_delivery_audit(
            "Create the requested files and deliver a parseable PPTX"
        ));
        assert!(!requires_delivery_audit("解释为什么数组排序会修改原数组"));
        assert!(!requires_delivery_audit("解释 PPTX 的制作流程"));
    }

    #[test]
    fn only_successful_action_or_validation_tools_count_as_execution_evidence() {
        assert!(!tool_result_succeeded("read", "错误: 文件不存在"));
        assert!(!tool_result_succeeded("shell", "退出码 1\ntests failed"));
        assert!(!tool_result_succeeded(
            "browser",
            "浏览器冒烟未通过: broken.html"
        ));
        assert!(!tool_result_succeeded(
            "check",
            "JSON 语法错误: broken.json"
        ));
        assert!(tool_result_succeeded("read", "真实文件内容"));
        assert!(!is_execution_evidence_tool("read"));
        assert!(is_execution_evidence_tool("edit"));
        assert!(is_execution_evidence_tool("shell"));
        assert!(is_execution_evidence_tool("browser"));
    }

    #[test]
    fn terminal_gate_cannot_be_bypassed_when_nudges_or_turns_are_exhausted() {
        let none: Vec<String> = Vec::new();
        assert!(terminal_blocker(true, 0, None, &none, false, false, 0, 0)
            .expect("missing execution evidence must block")
            .contains("工具证据"));
        assert!(
            terminal_blocker(false, 1, Some("browser failed"), &none, false, false, 1, 0)
                .expect("a failed validation must block")
                .contains("失败")
        );
        assert!(terminal_blocker(
            false,
            1,
            None,
            &["deck.html".to_string()],
            false,
            false,
            1,
            0,
        )
        .expect("unverified HTML must block")
        .contains("deck.html"));
        assert!(terminal_blocker(false, 1, None, &none, true, true, 2, 2)
            .expect("audit needs a fresh successful tool")
            .contains("审计"));
        assert!(terminal_blocker(false, 1, None, &none, true, true, 3, 2).is_none());
    }

    #[test]
    fn unrelated_success_cannot_clear_a_failed_execution_or_validation() {
        let browser_failure = FailedExecution {
            tool: "browser".into(),
            target: Some("broken.html".into()),
            validation_kind: Some("html".into()),
            detail: "browser failed".into(),
        };
        assert!(!success_covers_failure(
            &browser_failure,
            "shell",
            &serde_json::json!({"command": "echo ok"}),
        ));
        assert!(!success_covers_failure(
            &browser_failure,
            "browser",
            &serde_json::json!({"path": "other.html"}),
        ));
        assert!(success_covers_failure(
            &browser_failure,
            "check",
            &serde_json::json!({"path": "broken.html", "kind": "html"}),
        ));

        let python_failure = FailedExecution {
            tool: "python".into(),
            target: None,
            validation_kind: None,
            detail: "tests failed".into(),
        };
        assert!(!success_covers_failure(
            &python_failure,
            "task",
            &serde_json::json!({"prompt": "unrelated"}),
        ));
        assert!(success_covers_failure(
            &python_failure,
            "python",
            &serde_json::json!({"code": "print('fixed')"}),
        ));

        let json_failure = FailedExecution {
            tool: "check".into(),
            target: Some("broken.json".into()),
            validation_kind: Some("json".into()),
            detail: "json syntax failed".into(),
        };
        assert!(!success_covers_failure(
            &json_failure,
            "browser",
            &serde_json::json!({"path": "broken.json"}),
        ));
        assert!(success_covers_failure(
            &json_failure,
            "check",
            &serde_json::json!({"path": "broken.json", "kind": "json"}),
        ));
    }
}

fn run_subtask(cfg: Arc<Config>, args: &Value) -> Result<String, String> {
    let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    if prompt.trim().is_empty() {
        return Err("task 需要 prompt 参数".into());
    }
    let mut sub = Agent::new(cfg, 1);
    match sub.run(prompt, &|_| {}) {
        Ok(s) => {
            let capped: String = s.chars().take(6000).collect();
            if capped.is_empty() {
                Ok("子任务完成（无文字回复，产物见文件）".into())
            } else {
                Ok(capped)
            }
        }
        Err(e) => Err(format!("子任务失败: {e}")),
    }
}
