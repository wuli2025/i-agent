mod agent;
mod branch;
mod config;
mod context;
mod embedded;
mod llm;
mod prompt;
mod session;
mod skills;
mod tools;

use serde_json::json;
use std::io::{BufRead, Read, Write};
use std::sync::Arc;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_help() {
    println!(
        "i-agent {VERSION} — 轻量办公主力 agent（游戏/PPT/办公/长文本/生图）

用法:
  i-agent                     交互模式
  i-agent -p \"任务描述\"       无头模式，执行完退出
  i-agent init-assets         把内置技能包释放到 ~/.i-agent/assets

选项:
  -p, --print        无头模式（后接任务文本）
  -C, --dir <路径>   指定工作目录（默认当前目录）
      --provider <名> 指定供应商 (anthropic/custom/kimi/moonshot/deepseek/glm/qwen/siliconflow/minimax)
  -m, --model <名>   覆盖模型名
  -c, --continue     继续本工作目录上次会话（当前分支）
  -q, --quiet        不打印工具调用过程
      --polaris-stdio Polaris 机器协议：stdin 读任务，stdout 输出 NDJSON，不写会话
  -V, --version      版本

批量派生（一次任务出多个版本；只有批量任务才该走这条路）:
  --variants \"A|B|C\"  同一前缀派生多个变体分支并行跑，共享前缀缓存
                      每项可写成 \"标签=指令\"，如 \"小红书=写成种草笔记|公众号=写成长文\"
  --prepare \"任务\"    共享准备：先跑一轮通读资料/出大纲，其产出即所有变体的派生点
                      （省钱省的就是这份「每支都要重做一遍」的活，强烈建议配合使用）
  --branch <名>       切到指定分支继续（切换时自动把上一条分支的工作摘要注入）
  --branches          打印分支树后退出
  --from <entry-id>   指定派生/续接的起点（默认取分支末端）
  --no-parallel       变体串行跑（默认：先冷跑一支写热缓存，其余并行）
  示例: i-agent -p \"写云间茶馆开业文案，守住红线，带找路指引\" \\
          --prepare \"完整读 material.md，提炼卖点/价值锚/红线到 outline.md\" \\
          --variants \"小红书=种草笔记体|公众号=长文体|抖音=30秒口播稿\"

供应商配置（显式配置优先且排他；都没有时才按内置密钥自动选用）:
  显式: I_AGENT_API_KEY + I_AGENT_BASE_URL (+ I_AGENT_MODEL / I_AGENT_PROTOCOL)
        ANTHROPIC_AUTH_TOKEN 或 ANTHROPIC_API_KEY (+ ANTHROPIC_BASE_URL / ANTHROPIC_MODEL)
        配置文件 providers
  内置: KIMI_API_KEY (Kimi k3，api.kimi.com/coding，Anthropic 协议)
        DEEPSEEK_API_KEY / ZHIPU_API_KEY (GLM) / MINIMAX_API_KEY (M3) —— 均走各家 Anthropic 端点
        MOONSHOT_API_KEY (开放平台 k2) / DASHSCOPE_API_KEY (通义) / SILICONFLOW_API_KEY —— OpenAI 兼容
生图密钥: ZHIPU_API_KEY (cogview-4) / SILICONFLOW_API_KEY (Kolors) / MINIMAX_API_KEY (image-01)
  （生图密钥不参与对话模型选择）
代理: 认 HTTP(S)_PROXY / ALL_PROXY，且按目标逐个匹配 no_proxy；loopback 永远直连
配置文件: ~/.i-agent/config.json 与 <工作目录>/.i-agent/config.json（后者优先）"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let polaris_stdio_requested = args.iter().any(|arg| arg == "--polaris-stdio");
    let mut headless = false;
    let mut quiet = false;
    let mut cont = false;
    let mut workspace: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut model: Option<String> = None;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut variants_spec: Option<String> = None;
    let mut prepare_task: Option<String> = None;
    let mut branch_sel: Option<String> = None;
    let mut list_branches = false;
    let mut from_id: Option<u64> = None;
    let mut parallel = true;
    let mut polaris_stdio = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" | "--print" => headless = true,
            "-q" | "--quiet" => quiet = true,
            "--polaris-stdio" => {
                polaris_stdio = true;
                headless = true;
                quiet = true;
            }
            "-c" | "--continue" => cont = true,
            "--branches" => list_branches = true,
            "--no-parallel" => parallel = false,
            "--variants" => {
                variants_spec = Some(required_option_value(
                    &args,
                    &mut i,
                    "--variants",
                    polaris_stdio_requested,
                ));
            }
            "--prepare" => {
                prepare_task = Some(required_option_value(
                    &args,
                    &mut i,
                    "--prepare",
                    polaris_stdio_requested,
                ));
            }
            "--branch" => {
                branch_sel = Some(required_option_value(
                    &args,
                    &mut i,
                    "--branch",
                    polaris_stdio_requested,
                ));
            }
            "--from" => {
                let value = required_option_value(&args, &mut i, "--from", polaris_stdio_requested);
                match value.trim_start_matches('#').parse::<u64>() {
                    Ok(v) => from_id = Some(v),
                    Err(_) => {
                        exit_cli_error(
                            &format!("--from 需要一个 entry id（整数），收到: {value}"),
                            polaris_stdio_requested,
                            2,
                        );
                    }
                }
            }
            "-C" | "--dir" => {
                let flag = args[i].clone();
                workspace = Some(required_option_value(
                    &args,
                    &mut i,
                    &flag,
                    polaris_stdio_requested,
                ));
            }
            "--provider" => {
                provider = Some(required_option_value(
                    &args,
                    &mut i,
                    "--provider",
                    polaris_stdio_requested,
                ));
            }
            "-m" | "--model" => {
                let flag = args[i].clone();
                model = Some(required_option_value(
                    &args,
                    &mut i,
                    &flag,
                    polaris_stdio_requested,
                ));
            }
            "-V" | "--version" => {
                if polaris_stdio_requested {
                    exit_cli_error("--polaris-stdio 不能与 --version 组合", true, 2);
                }
                println!("i-agent {VERSION}");
                return;
            }
            "-h" | "--help" => {
                if polaris_stdio_requested {
                    exit_cli_error("--polaris-stdio 不能与 --help 组合", true, 2);
                }
                print_help();
                return;
            }
            "init-assets" => {
                if polaris_stdio_requested {
                    exit_cli_error("--polaris-stdio 不能运行 init-assets", true, 2);
                }
                let dir = skills::materialize_assets(true);
                println!("技能包已释放到: {}", dir.display());
                return;
            }
            // 拼错的选项绝不能被当成任务文本悄悄咽下去。
            // 一次 --prepare 没被识别，那段话就被并进了任务描述，
            // 三个变体齐刷刷交回大纲，而命令行看上去一切正常 —— 这种失败最难查。
            s if s.starts_with('-') && s.len() > 1 => {
                exit_cli_error(
                    &format!("未知选项: {s}。用 -h 查看可用选项；如果它本该是任务文本，请加引号。"),
                    polaris_stdio_requested,
                    2,
                );
            }
            s => prompt_parts.push(s.to_string()),
        }
        i += 1;
    }

    if polaris_stdio
        && (cont
            || list_branches
            || branch_sel.is_some()
            || from_id.is_some()
            || variants_spec.is_some()
            || prepare_task.is_some()
            || !parallel)
    {
        exit_cli_error(
            "--polaris-stdio 是单轮无状态协议，不能与会话、分支、prepare 或 variants 选项组合",
            true,
            2,
        );
    }

    let polaris_prompt = if polaris_stdio {
        if !prompt_parts.is_empty() {
            emit_polaris_event(json!({
                "type": "result",
                "ok": false,
                "error": "--polaris-stdio 只从 stdin 接收任务，不能再带位置参数"
            }));
            std::process::exit(2);
        }
        let mut prompt = String::new();
        if let Err(error) = std::io::stdin().read_to_string(&mut prompt) {
            emit_polaris_event(json!({
                "type": "result",
                "ok": false,
                "error": format!("读取 stdin 失败: {error}")
            }));
            std::process::exit(1);
        }
        Some(prompt)
    } else {
        None
    };

    let cfg = match config::Config::load(workspace, provider, model, quiet, polaris_stdio) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            if polaris_stdio {
                emit_polaris_event(json!({"type": "result", "ok": false, "error": e}));
                std::process::exit(1);
            }
            eprintln!("配置错误: {e}");
            std::process::exit(1);
        }
    };

    // --branches 是只读操作，打完分支树就走
    if list_branches {
        let log = session::Log::load(&cfg.workspace);
        branch::print_branches(&log, &session::read_head(&cfg.workspace));
        return;
    }

    let mut ag = agent::Agent::new(cfg.clone(), 0);
    let mut active_branch = session::MAIN.to_string();
    let mut sink_head: Option<u64> = None;

    if polaris_stdio {
        // Polaris 自己持久化对话历史；这里必须完全无状态，不能清写用户项目里的
        // .i-agent/session.jsonl，也不能让不同 Polaris 对话共用一条 i-agent 分支。
    } else if cont || branch_sel.is_some() || from_id.is_some() {
        let log = session::Log::load(&cfg.workspace);
        if !log.is_empty() {
            let prev = session::read_head(&cfg.workspace);
            let target = branch_sel.clone().unwrap_or_else(|| prev.clone());
            let head = from_id
                .or_else(|| log.head(&target))
                .or_else(|| log.head(session::MAIN));
            match head {
                Some(h) => {
                    ag.messages.extend(log.chain(h));
                    active_branch = target.clone();
                    sink_head = Some(h);
                    eprintln!(
                        "[已加载分支 {target}，接到 entry #{h}，共 {} 条消息]",
                        ag.messages.len() - 1
                    );
                    // 换了分支就得把上一条分支干过的事交接过来，
                    // 否则新分支对刚才那一大段工作完全失忆。
                    if target != prev {
                        if let Some(old_leaf) = log.head(&prev) {
                            if old_leaf != h {
                                match branch::branch_summary(&cfg, &log, old_leaf, h) {
                                    Some(sum) => {
                                        let mut sk = session::Sink::new(&target, sink_head, None);
                                        sk.append_summary(&cfg.workspace, &sum);
                                        sink_head = sk.head;
                                        ag.messages.push(sum);
                                        eprintln!("[已注入分支 {prev} 的工作摘要]");
                                    }
                                    None => eprintln!(
                                        "[分支 {prev} 无可摘要的新工作，或摘要调用失败，已跳过]"
                                    ),
                                }
                            }
                        }
                    }
                }
                None => eprintln!("[分支 {target} 不存在，从空会话开始]"),
            }
        }
    } else {
        session::clear(&cfg.workspace);
    }
    if !polaris_stdio {
        session::write_head(&cfg.workspace, &active_branch);
        ag.sink = Some(session::Sink::new(&active_branch, sink_head, None));
    }

    // ===== F-14 批量派生：一次任务派生多个版本 =====
    if let Some(spec) = variants_spec {
        let vs = branch::parse_variants(&spec);
        if vs.len() < 2 {
            eprintln!(
                "--variants 至少要两个变体（用 | 分隔）。只要一个版本，直接跑普通任务就行——\n\
                 批量派生的全部意义是让多个版本共用同一份前缀缓存，一个版本没有可共享的对象。"
            );
            std::process::exit(1);
        }
        let task = prompt_parts.join(" ");
        if task.trim().is_empty() && prepare_task.is_none() && ag.messages.len() <= 1 {
            eprintln!(
                "批量派生需要共享上文，三选一：给出任务文本、用 --prepare 先做共享准备、\n\
                 或用 -c 复用已有会话作为共享前缀。"
            );
            std::process::exit(1);
        }
        if !quiet {
            eprintln!(
                "[i-agent] 供应商 {} | 模型 {} | 端点 {}",
                cfg.provider.name, cfg.provider.model, cfg.provider.base
            );
        }
        // 共享准备先跑一轮。派生要省钱，省的必须是「每个变体都得做一遍的那件事」——
        // 通读资料、提炼大纲、定风格卡。不先把它做掉就直接派生，
        // 每个变体都会自己重做一遍，共享的只剩一句任务描述，等于没派生。
        let mut prep_out: Option<branch::Outcome> = None;
        if let Some(prep) = &prepare_task {
            if !quiet {
                eprintln!("\x1b[2m[F-14] 共享准备（只做一次，结果被所有变体继承）…\x1b[0m");
            }
            let t0 = std::time::Instant::now();
            let echo = |s: &str| {
                print!("{s}");
                let _ = std::io::stdout().flush();
            };
            if let Err(e) = ag.run(prep, &echo) {
                eprintln!("\n共享准备失败: {e}");
                std::process::exit(1);
            }
            println!();
            prep_out = Some(branch::Outcome {
                label: "共享准备".into(),
                branch: active_branch.clone(),
                cold: false,
                calls: ag.llm_calls,
                fresh_in: ag.usage_in,
                cached_in: ag.usage_cached,
                out: ag.usage_out,
                wall_s: t0.elapsed().as_secs_f64(),
                text: String::new(),
                err: None,
            });
        }

        // 共享任务写进当前分支，它就是所有变体的派生点。
        // 技能包正文在这里注入一次且仅此一次：注进共享前缀，各变体都吃得到，
        // 而前缀仍然只有一份 —— 要是留给每个变体自己注入，前缀当场分叉，缓存全废。
        if !task.trim().is_empty() {
            let enriched = match skills::match_hint(&cfg.assets_dir, &task) {
                Some(h) => format!("{task}{h}"),
                None => task.clone(),
            };
            let m = llm::Msg::text("user", &enriched);
            let ws = cfg.workspace.clone();
            if let Some(s) = &mut ag.sink {
                s.append(&ws, &m);
            }
            ag.messages.push(m);
        }
        let fork_from = ag.sink.as_ref().and_then(|s| s.head);
        let prefix = ag.messages.clone();
        let outs = branch::run_batch(cfg.clone(), prefix, fork_from, vs, parallel);

        for o in &outs {
            if o.err.is_none() && !o.text.trim().is_empty() {
                println!(
                    "\n───── 变体「{}」（分支 {}）─────\n{}",
                    o.label,
                    o.branch,
                    o.text.trim()
                );
            }
        }
        branch::report(&outs, prep_out.as_ref(), branch::Price::from_env());
        if outs.iter().any(|o| o.err.is_some()) {
            std::process::exit(1);
        }
        return;
    }

    if headless {
        let task = polaris_prompt.unwrap_or_else(|| prompt_parts.join(" "));
        if task.trim().is_empty() {
            if polaris_stdio {
                emit_polaris_event(json!({
                    "type": "result",
                    "ok": false,
                    "error": "stdin 任务不能为空"
                }));
                std::process::exit(1);
            }
            eprintln!("无头模式需要任务文本: i-agent -p \"...\"");
            std::process::exit(1);
        }
        if polaris_stdio {
            ag.set_event_sink(Arc::new(|event| match event {
                agent::AgentEvent::Tool { name, detail } => {
                    emit_polaris_event(json!({
                        "type": "tool",
                        "name": name,
                        "detail": detail,
                    }));
                }
            }));
            let on_text = |text: &str| {
                emit_polaris_event(json!({"type": "delta", "text": text}));
            };
            match ag.run(&task, &on_text) {
                Ok(result) => {
                    emit_polaris_usage(&ag);
                    emit_polaris_event(json!({
                        "type": "result",
                        "ok": true,
                        "result": result,
                        "usage": {
                            "requests": ag.llm_calls,
                            "input_tokens": ag.usage_in,
                            "cached_input_tokens": ag.usage_cached,
                            "output_tokens": ag.usage_out,
                        }
                    }));
                }
                Err(error) => {
                    emit_polaris_usage(&ag);
                    emit_polaris_event(json!({
                        "type": "result",
                        "ok": false,
                        "error": error,
                        "usage": {
                            "requests": ag.llm_calls,
                            "input_tokens": ag.usage_in,
                            "cached_input_tokens": ag.usage_cached,
                            "output_tokens": ag.usage_out,
                        }
                    }));
                    std::process::exit(1);
                }
            }
            return;
        }
        let on_text = |s: &str| {
            print!("{s}");
            let _ = std::io::stdout().flush();
        };
        // 把实际选中的供应商/模型/端点亮出来：选错了模型静默跑完
        // 比当场报错糟糕得多（评测里就是这样露的馅）
        if !quiet {
            eprintln!(
                "[i-agent] 供应商 {} | 模型 {} | 端点 {}",
                cfg.provider.name, cfg.provider.model, cfg.provider.base
            );
        }
        match ag.run(&task, &on_text) {
            Ok(_) => {
                println!();
                // Anthropic 语义里 input_tokens 与 cache_read_input_tokens 是**互斥相加**的，
                // 不是包含关系——原来写「其中缓存命中 X」是错的，会出现缓存数比输入还大的怪象。
                let total_in = ag.usage_in + ag.usage_cached;
                eprintln!(
                    "[用量] 请求 {} 次 | 输入 {} tok（新算 {} + 缓存命中 {}）| 输出 {} tok | 合计 {} tok",
                    ag.llm_calls,
                    total_in,
                    ag.usage_in,
                    ag.usage_cached,
                    ag.usage_out,
                    total_in + ag.usage_out
                );
            }
            Err(e) => {
                eprintln!("\n执行失败: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // 交互模式
    let on_text = |s: &str| {
        print!("{s}");
        let _ = std::io::stdout().flush();
    };
    println!(
        "i-agent {VERSION} | 模型 {} ({}) | 工作目录 {}",
        cfg.provider.model,
        cfg.provider.name,
        cfg.workspace.display()
    );
    println!("输入任务开始；/new 清空会话  /branches 看分支树  /exit 退出\n");
    if !prompt_parts.is_empty() {
        let task = prompt_parts.join(" ");
        println!("› {task}");
        if let Err(e) = ag.run(&task, &on_text) {
            eprintln!("\n执行失败: {e}");
        }
        println!();
    }
    let stdin = std::io::stdin();
    loop {
        print!("› ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        match input {
            "/exit" | "/quit" => break,
            "/new" => {
                session::clear(&cfg.workspace);
                ag = agent::Agent::new(cfg.clone(), 0);
                ag.sink = Some(session::Sink::main(None));
                session::write_head(&cfg.workspace, session::MAIN);
                println!("[会话已清空]");
                continue;
            }
            "/branches" => {
                let log = session::Log::load(&cfg.workspace);
                branch::print_branches(&log, &session::read_head(&cfg.workspace));
                continue;
            }
            _ => {}
        }
        if let Err(e) = ag.run(input, &on_text) {
            eprintln!("\n执行失败: {e}");
        }
        println!("\n");
    }
}

fn emit_polaris_event(event: serde_json::Value) {
    println!("{event}");
    let _ = std::io::stdout().flush();
}

fn required_option_value(
    args: &[String],
    index: &mut usize,
    flag: &str,
    polaris_stdio: bool,
) -> String {
    *index += 1;
    let Some(value) = args.get(*index) else {
        exit_cli_error(&format!("{flag} 缺少参数值"), polaris_stdio, 2);
    };
    if value.starts_with('-') {
        exit_cli_error(
            &format!("{flag} 缺少参数值（下一项是选项 {value}）"),
            polaris_stdio,
            2,
        );
    }
    value.clone()
}

fn exit_cli_error(message: &str, polaris_stdio: bool, code: i32) -> ! {
    if polaris_stdio {
        emit_polaris_event(json!({"type": "result", "ok": false, "error": message}));
    } else {
        eprintln!("{message}");
    }
    std::process::exit(code);
}

fn emit_polaris_usage(agent: &agent::Agent) {
    emit_polaris_event(json!({
        "type": "usage",
        "requests": agent.llm_calls,
        "input_tokens": agent.usage_in,
        "cached_input_tokens": agent.usage_cached,
        "output_tokens": agent.usage_out,
    }));
}
