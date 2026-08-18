use crate::config::Config;
use crate::context;
use crate::llm::{self, Msg, ToolCall};
use crate::prompt;
use crate::session;
use crate::tools;
use serde_json::Value;
use std::sync::Arc;

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
    /// Last shell/python call that exited non-zero, if it has not been made good yet.
    failed_exec: Option<String>,
    /// 其中已经在真浏览器里冒烟通过的
    verified: Vec<String>,
    /// 被当作 bundle 输入的片段——它们是零件不是交付物，不进门禁
    bundle_inputs: Vec<String>,
    pub usage_in: u64,
    pub usage_out: u64,
    pub usage_cached: u64,
    pub llm_calls: u64,
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
        self.turn_loop(on_text)
    }

    fn turn_loop(&mut self, on_text: &dyn Fn(&str)) -> Result<String, String> {
        let max_turns = if self.depth == 0 {
            self.cfg.max_turns
        } else {
            self.cfg.max_turns / 2
        };
        let specs = tools::specs(&self.cfg, self.depth);
        let mut last_text = String::new();

        let mut empty_nudges = 0;
        let mut gate_nudges = 0;

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
                // 交付门禁之一：最后一次 shell/python 是失败的，不算完成。
                // 一次失败的自检 == 没有自检。放它过去，产物就是「没验证过」发出去的。
                if let Some(fail) = self.failed_exec.clone() {
                    if gate_nudges < 2 {
                        gate_nudges += 1;
                        self.log("▸ 交付门禁：上一次命令执行失败，未通过自检");
                        self.messages.push(Msg::text(
                            "user",
                            &format!(
                                "[系统] 交付被拦下：你最后一次执行是失败的——\n{fail}\n\n\
                                 一次失败的自检等于没有自检。请先把它跑通：\n\
                                 · 跑 Python 一律用 python 工具（直接给源码，不经 shell，没有引号问题）；\n\
                                 · shell 命令必须用本机 shell 的语法。\n\
                                 命令成功退出后才能给出交付说明；严禁凭记忆写「自证结果」。"
                            ),
                        ));
                        continue;
                    }
                }
                // 交付门禁之二：HTML 产物没在真浏览器里验证过，不算完成。
                // 「我检查过代码了」不作数——文本检查看不出白屏。
                let pending = self.unverified();
                if !pending.is_empty() && gate_nudges < 2 {
                    gate_nudges += 1;
                    self.log(&format!(
                        "▸ 交付门禁：{} 尚未通过浏览器验证",
                        pending.join(", ")
                    ));
                    self.messages.push(Msg::text(
                        "user",
                        &format!(
                            "[系统] 交付被拦下：以下 HTML 产物还没有在真浏览器里验证过——{}。\n\
                             请对每个文件调用一次 browser 工具（或 check kind=html）。\
                             若报白屏或运行时异常，先修好再验证；全部通过后才能给出交付说明。",
                            pending.join("、")
                        ),
                    ));
                    continue;
                }
                return Ok(last_text);
            }

            if !last_text.is_empty() {
                on_text("\n");
                last_text.clear();
            }

            let results = self.exec_tools(&tool_calls);
            for (id, content) in results {
                let tm = Msg::tool_result(&id, content);
                self.persist(&tm);
                self.messages.push(tm);
            }
            if turn == max_turns - 1 {
                self.messages.push(Msg::text(
                    "user",
                    "[系统] 已达最大轮数，请立即总结目前完成情况与产物路径，不要再调用工具。",
                ));
                let fin = llm::chat(&self.cfg, &self.messages, &Value::Array(vec![]), on_text)?;
                return Ok(fin.msg.content.unwrap_or_default());
            }
        }
        Ok(last_text)
    }

    fn exec_tools(&mut self, tool_calls: &[ToolCall]) -> Vec<(String, String)> {
        // 多个 task 并行；其余顺序执行
        let tasks: Vec<&ToolCall> = tool_calls
            .iter()
            .filter(|t| t.function.name == "task")
            .collect();
        let mut results: Vec<(String, String)> = Vec::new();

        if tasks.len() > 1 && tasks.len() == tool_calls.len() && self.depth == 0 {
            self.log(&format!("▸ 并行派出 {} 个子任务", tasks.len()));
            let handles: Vec<(String, std::thread::JoinHandle<String>)> = tasks
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
                let r = h.join().unwrap_or_else(|_| "子任务线程崩溃".into());
                self.log("▸ 子任务完成");
                results.push((id, r));
            }
            return results;
        }

        for tc in tool_calls {
            let name = &tc.function.name;
            let args: Value = match serde_json::from_str(&tc.function.arguments) {
                Ok(v) => v,
                Err(e) => {
                    results.push((
                        tc.id.clone(),
                        format!("错误: 工具参数不是合法 JSON ({e})。请修正后重试。"),
                    ));
                    continue;
                }
            };
            if args.get("__truncated__").is_some() {
                results.push((
                    tc.id.clone(),
                    "错误: 本次工具参数超出单次输出上限被截断，未执行。请把内容拆块：先 write 写文件开头，再用 write append:true 续写，每块 500-800 行。".into(),
                ));
                continue;
            }

            // doom loop 熔断：同工具同参数连调 3 次
            let sig = format!("{name}:{}", tc.function.arguments);
            self.recent_sigs.push(sig.clone());
            if self.recent_sigs.len() > 6 {
                self.recent_sigs.remove(0);
            }
            if self.recent_sigs.iter().filter(|s| **s == sig).count() >= 3 {
                results.push((
                    tc.id.clone(),
                    "错误: 检测到同一调用重复 3 次，已熔断。请换一种方法或修改参数。".into(),
                ));
                continue;
            }

            let brief: String = tc.function.arguments.chars().take(120).collect();
            self.log(&format!("▸ {name} {brief}"));

            let result = if name == "task" {
                if self.depth >= 1 {
                    Err("子任务里不能再派子任务，请直接完成。".into())
                } else {
                    Ok(run_subtask(self.cfg.clone(), &args))
                }
            } else {
                tools::dispatch(name, &args, &self.cfg)
            };
            let content = match result {
                Ok(s) => s,
                Err(e) => format!("错误: {e}"),
            };
            self.track_artifacts(name, &args, &content);
            results.push((tc.id.clone(), content));
        }
        results
    }

    /// 盯住 HTML 交付物的验证状态：谁被写出来了、谁真的在浏览器里跑通了。
    fn track_artifacts(&mut self, name: &str, args: &Value, result: &str) {
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
                    if is_html(p) && !skip && !result.starts_with("错误:") {
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
                    if is_html(p) && result.contains("浏览器冒烟通过") {
                        if !self.verified.iter().any(|x| x == p) {
                            self.verified.push(p.to_string());
                        }
                    }
                }
            }
            _ => {}
        }

        // A shell/python call that exited non-zero is not a fact the model gets to
        // narrate past. On Windows this is exactly how an unverified artifact used to
        // ship: the self-check command died on a quoting error, and — told to self-check
        // at most once — the model shrugged and wrote up a verification table it had
        // never actually run. Remember the failure; the delivery gate below refuses to
        // let the run end while one is outstanding.
        if matches!(name, "shell" | "python") {
            if result.starts_with("退出码 ")
                || result.contains("超时（")
                || result.starts_with("找不到可用的 python")
            {
                let head: String = result.lines().take(6).collect::<Vec<_>>().join("\n");
                self.failed_exec = Some(format!("{name}: {head}"));
            } else {
                self.failed_exec = None;
            }
        }
    }
}

fn run_subtask(cfg: Arc<Config>, args: &Value) -> String {
    let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    if prompt.trim().is_empty() {
        return "错误: task 需要 prompt 参数".into();
    }
    let mut sub = Agent::new(cfg, 1);
    match sub.run(prompt, &|_| {}) {
        Ok(s) => {
            let capped: String = s.chars().take(6000).collect();
            if capped.is_empty() {
                "子任务完成（无文字回复，产物见文件）".into()
            } else {
                capped
            }
        }
        Err(e) => format!("子任务失败: {e}"),
    }
}
