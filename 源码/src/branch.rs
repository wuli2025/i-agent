use crate::agent::Agent;
use crate::config::Config;
use crate::context;
use crate::llm::Msg;
use crate::session;
use std::sync::Arc;
use std::time::Instant;

/// 一个变体分支跑完之后的账
#[derive(Clone, Debug)]
pub struct Outcome {
    pub label: String,
    pub branch: String,
    pub cold: bool,
    pub calls: u64,
    pub fresh_in: u64,
    pub cached_in: u64,
    pub out: u64,
    pub wall_s: f64,
    pub text: String,
    pub err: Option<String>,
}

/// 相对计价权重。默认值是国产模型的典型形状（缓存命中约为新算的 1/10，
/// 输出约为输入的 3 倍），可用 I_AGENT_PRICE_* 覆盖成自己供应商的真实单价。
#[derive(Clone, Copy, Debug)]
pub struct Price {
    pub input: f64,
    pub cached: f64,
    pub output: f64,
}

impl Price {
    pub fn from_env() -> Price {
        let f = |k: &str, d: f64| {
            std::env::var(k).ok().and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(d)
        };
        Price {
            input: f("I_AGENT_PRICE_IN", 1.0),
            cached: f("I_AGENT_PRICE_CACHE", 0.1),
            output: f("I_AGENT_PRICE_OUT", 3.0),
        }
    }
    pub fn cost(&self, o: &Outcome) -> f64 {
        o.fresh_in as f64 * self.input
            + o.cached_in as f64 * self.cached
            + o.out as f64 * self.output
    }

    /// 「谁也不共享」的反事实成本：同样这一支，如果它自己单独跑、
    /// 前缀没有任何人帮它预热，那么它重放过的每一个输入 token 都得按新算价付。
    ///
    /// 用它做基线，而不是用「第一支冷跑 × N」：那个写法只在第一支真的是冷的时候成立，
    /// 一旦是 `-c` 接着上一次会话跑，第一支的前缀早就被上一次调用写热了，
    /// 基线会被低估两三倍，比值当场失真。这个反事实完全由实测数字算出，
    /// 不依赖任何一支的冷热状态。
    pub fn no_share(&self, o: &Outcome) -> f64 {
        (o.fresh_in + o.cached_in) as f64 * self.input + o.out as f64 * self.output
    }
}

/// 解析 --variants 的规格串。用 `|` 或全角 `｜` 分隔；
/// 每项可以写成 `标签=指令`，省略标签时用指令前 12 个字符当标签。
pub fn parse_variants(spec: &str) -> Vec<(String, String)> {
    spec.split(['|', '｜'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| match s.split_once('=') {
            Some((l, i)) if !l.trim().is_empty() && !i.trim().is_empty() => {
                (l.trim().to_string(), i.trim().to_string())
            }
            _ => {
                let label: String = s.chars().take(12).collect();
                (label, s.to_string())
            }
        })
        .collect()
}

/// 变体分支的尾部指令。共享前缀之外**只有这一段**是不同的。
///
/// 两条规则都是被真实跑测打出来的，不是想当然：
/// ① 阶段声明：共享准备那一轮如果说过「只出大纲、别写正文」，那句话会一直留在前缀里，
///    模型会拿它压住后面的正式任务，三个变体齐刷刷交回大纲。必须在这里明确宣布准备阶段结束。
/// ② 产物隔离：三个分支是并行跑的，都往 `index.html` 写的话，
///    最后活下来的只有一个，而且是随机的哪一个。
fn variant_message(idx: usize, total: usize, label: &str, instruction: &str) -> String {
    format!(
        "[系统] 准备阶段到此结束。上文里一切「先出大纲 / 暂时不要动笔 / 只做计划」之类的\
         阶段性限制**到此全部失效**，从本轮起进入正式产出：必须交出最终成品本身，\
         不许再回交大纲、计划或提纲。\n\n\
         {instruction}\n\n\
         [系统] 本次是同一任务的第 {n}/{total} 个变体（标签「{label}」），\
         与其他 {other} 个变体共享上文、并行进行。\
         产物必须写到**独立文件**，文件名或目录名里带上「{label}」，\
         严禁写到与其他变体相同的路径——并行覆盖会让别的分支产物凭空消失。\
         完成后用一句话说明这一版的取向与产物路径。",
        n = idx + 1,
        other = total.saturating_sub(1),
    )
}

fn run_one(
    cfg: Arc<Config>,
    prefix: Vec<Msg>,
    fork_from: Option<u64>,
    idx: usize,
    total: usize,
    label: String,
    instruction: String,
    branch: String,
    cold: bool,
    stream: bool,
) -> Outcome {
    let t0 = Instant::now();
    let sink = session::Sink::new(&branch, fork_from, Some(label.clone()));
    let mut ag = Agent::fork(cfg, &prefix, Some(sink));
    let msg = variant_message(idx, total, &label, &instruction);

    let printer = |s: &str| {
        print!("{s}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
    };
    let silent = |_: &str| {};
    let r = if stream {
        ag.run_raw(&msg, &printer)
    } else {
        ag.run_raw(&msg, &silent)
    };

    let (text, err) = match r {
        Ok(t) => (t, None),
        Err(e) => (String::new(), Some(e)),
    };
    Outcome {
        label,
        branch,
        cold,
        calls: ag.llm_calls,
        fresh_in: ag.usage_in,
        cached_in: ag.usage_cached,
        out: ag.usage_out,
        wall_s: t0.elapsed().as_secs_f64(),
        text,
        err,
    }
}

/// 找一组还没被占用的分支名
fn free_branches(log: &session::Log, n: usize) -> Vec<String> {
    let used: Vec<String> = log.branches().into_iter().map(|b| b.name).collect();
    let mut out = Vec::new();
    let mut i = 1usize;
    while out.len() < n {
        let name = format!("v{i}");
        if !used.contains(&name) && !out.contains(&name) {
            out.push(name);
        }
        i += 1;
    }
    out
}

/// F-14 批量派生：同一前缀派生 N 个变体分支。
///
/// 执行顺序是刻意的：**第 1 个变体单独串行跑完，其余才并行。**
/// 前缀缓存是「先写后读」的——三个请求同时打进去，三个都是 miss，
/// 派生就退化成了单独跑三次。先用一个冷跑把缓存写热，后面的才吃得到。
/// 顺带地，这个冷跑的账单就是天然的对照基线，不需要另外估算。
pub fn run_batch(
    cfg: Arc<Config>,
    prefix: Vec<Msg>,
    fork_from: Option<u64>,
    variants: Vec<(String, String)>,
    parallel: bool,
) -> Vec<Outcome> {
    let total = variants.len();
    let log = session::Log::load(&cfg.workspace);
    let branches = free_branches(&log, total);
    let quiet = cfg.quiet;

    let prefix_tok = context::total_tokens(&prefix);
    if !quiet {
        eprintln!(
            "\x1b[2m[F-14] 派生点 {} | 共享前缀 {} 条消息 ≈ {} tok | {} 个变体\x1b[0m",
            fork_from.map(|i| format!("entry #{i}")).unwrap_or_else(|| "会话起点".into()),
            prefix.len(),
            prefix_tok,
            total
        );
        if prefix_tok < 1024 {
            eprintln!(
                "\x1b[2m[F-14] 提示：共享前缀不足 1024 tok，多数供应商的前缀缓存不会对这么短的前缀生效，\
                 派生收益会很小。批量派生适合前缀厚（已有资料/大纲/初稿）的任务。\x1b[0m"
            );
        }
    }

    let mut outs: Vec<Outcome> = Vec::with_capacity(total);

    // ① 冷跑：一个人跑，把共享前缀写进供应商的缓存，同时充当对照基线
    if !quiet {
        eprintln!(
            "\x1b[2m[F-14] 变体 1/{total}「{}」冷跑中（写热前缀缓存，兼作基线）…\x1b[0m",
            variants[0].0
        );
    }
    outs.push(run_one(
        cfg.clone(),
        prefix.clone(),
        fork_from,
        0,
        total,
        variants[0].0.clone(),
        variants[0].1.clone(),
        branches[0].clone(),
        true,
        true,
    ));
    println!();

    // ② 其余变体：前缀已热，并行扇出
    if total > 1 {
        let rest: Vec<usize> = (1..total).collect();
        if parallel {
            if !quiet {
                eprintln!(
                    "\x1b[2m[F-14] 前缀已热，并行派出剩余 {} 个变体…\x1b[0m",
                    rest.len()
                );
            }
            let handles: Vec<std::thread::JoinHandle<Outcome>> = rest
                .iter()
                .map(|&i| {
                    let cfg = cfg.clone();
                    let prefix = prefix.clone();
                    let (label, instr) = variants[i].clone();
                    let branch = branches[i].clone();
                    std::thread::spawn(move || {
                        run_one(
                            cfg, prefix, fork_from, i, total, label, instr, branch, false,
                            false,
                        )
                    })
                })
                .collect();
            for h in handles {
                match h.join() {
                    Ok(o) => outs.push(o),
                    Err(_) => outs.push(Outcome {
                        label: "?".into(),
                        branch: "?".into(),
                        cold: false,
                        calls: 0,
                        fresh_in: 0,
                        cached_in: 0,
                        out: 0,
                        wall_s: 0.0,
                        text: String::new(),
                        err: Some("变体线程崩溃".into()),
                    }),
                }
            }
        } else {
            for &i in &rest {
                if !quiet {
                    eprintln!(
                        "\x1b[2m[F-14] 变体 {}/{total}「{}」…\x1b[0m",
                        i + 1,
                        variants[i].0
                    );
                }
                outs.push(run_one(
                    cfg.clone(),
                    prefix.clone(),
                    fork_from,
                    i,
                    total,
                    variants[i].0.clone(),
                    variants[i].1.clone(),
                    branches[i].clone(),
                    false,
                    false,
                ));
            }
        }
    }
    outs
}

/// 把一段被放弃的工作序列化成不会被模型误当成「待续对话」的文本。
/// 角色前缀 + 工具结果截断，这两条是必须的：不加前缀模型会接着往下演，
/// 不截断的话几条 read 的输出就能把摘要请求本身撑爆。
fn serialize(msgs: &[Msg]) -> String {
    let mut s = String::new();
    for m in msgs {
        let body = m.content.clone().unwrap_or_default();
        let body: String = if m.role == "tool" {
            body.chars().take(2000).collect()
        } else {
            body.chars().take(4000).collect()
        };
        let tag = match m.role.as_str() {
            "user" => "[用户]",
            "assistant" => "[助手]",
            "tool" => "[工具结果]",
            _ => "[系统]",
        };
        if !body.trim().is_empty() {
            s.push_str(&format!("{tag}: {body}\n"));
        }
        if let Some(tcs) = &m.tool_calls {
            for tc in tcs {
                let args: String = tc.function.arguments.chars().take(200).collect();
                s.push_str(&format!("[助手调用工具]: {}({})\n", tc.function.name, args));
            }
        }
    }
    s
}

/// 切分支时，把「即将离开的那条分支上做过的事」摘要成一条消息注入新分支。
/// 没有它，切过去的分支对刚才那一大段工作一无所知 —— 树状会话最容易丢的就是这个。
pub fn branch_summary(
    cfg: &Config,
    log: &session::Log,
    old_leaf: u64,
    new_leaf: u64,
) -> Option<Msg> {
    let ancestor = log.common_ancestor(old_leaf, new_leaf);
    let abandoned = log.since(ancestor, old_leaf);
    if abandoned.is_empty() {
        return None;
    }
    let text = serialize(&abandoned);
    if text.trim().is_empty() {
        return None;
    }
    let prompt = format!(
        "把以下 agent 工作片段压缩成简明交接备忘（500 字以内），必须保留：\
         做了什么、已创建或修改的文件路径、关键决定、还没做完的事。\
         直接输出备忘正文，不要寒暄。\n\n{text}"
    );
    // 摘要失败不该阻断切分支：宁可少一条上下文，也不能让用户切不过去
    let summary = crate::llm::chat_simple(cfg, &prompt, 900).ok()?;
    if summary.trim().is_empty() {
        return None;
    }
    Some(Msg::text(
        "user",
        &format!(
            "[另一分支的工作摘要，供你参考；那条分支已被搁置]\n{}",
            summary.trim()
        ),
    ))
}

/// 打印分支树
pub fn print_branches(log: &session::Log, active: &str) {
    let bs = log.branches();
    if bs.is_empty() {
        println!("（本工作目录还没有会话）");
        return;
    }
    println!("分支树（* = 当前）:");
    for b in bs {
        let mark = if b.name == active { "*" } else { " " };
        let from = match b.fork_from {
            Some(f) => format!("派生自 entry #{f}"),
            None => "根".to_string(),
        };
        let label = b.label.map(|l| format!("「{l}」")).unwrap_or_default();
        println!(
            "{mark} {:<8} {:<24} {:>3} 条  末端 entry #{}  {}",
            b.name, label, b.len, b.head, from
        );
    }
    println!("\n继续某分支: i-agent -c --branch <名> -p \"...\"");
}

fn pad(s: &str, w: usize) -> String {
    // 中文按 2 列宽算，让表格在等宽终端里对齐
    let width: usize = s.chars().map(|c| if (c as u32) > 0x2000 { 2 } else { 1 }).sum();
    let mut out = s.to_string();
    for _ in width..w {
        out.push(' ');
    }
    out
}

/// 派生报告 + 验收口径核算。
///
/// 全部走 stdout：报告是这次运行的交付物之一，不是日志。
/// （之前写 stderr，在管道里会和各分支正文交错成一团。）
pub fn report(outs: &[Outcome], prep: Option<&Outcome>, price: Price) {
    if outs.is_empty() {
        return;
    }
    let n = outs.len();
    println!("\n\x1b[1m[F-14 批量派生报告]\x1b[0m");
    println!(
        "  {} {} {} {} {} {} {}",
        pad("变体", 18),
        pad("分支", 6),
        pad("请求", 6),
        pad("新算输入", 10),
        pad("缓存命中", 10),
        pad("输出", 9),
        pad("用时", 8)
    );
    let rows: Vec<&Outcome> = prep.into_iter().chain(outs.iter()).collect();
    for o in rows {
        let name = if o.cold { format!("{}（首发）", o.label) } else { o.label.clone() };
        println!(
            "  {} {} {} {} {} {} {} {}",
            pad(&name, 18),
            pad(&o.branch, 6),
            pad(&o.calls.to_string(), 6),
            pad(&o.fresh_in.to_string(), 10),
            pad(&o.cached_in.to_string(), 10),
            pad(&o.out.to_string(), 9),
            pad(&format!("{:.0}s", o.wall_s), 8),
            match &o.err {
                Some(e) => format!("\x1b[31m失败: {e}\x1b[0m"),
                None => "ok".into(),
            }
        );
    }

    // 共享准备只付一次，但在「各跑各的」世界里每一支都得自己重做一遍 —— 所以基线里要乘 N。
    // 派生真正省下来的就是这 (N-1) 份重复劳动。
    let prep_cost = prep.map(|p| price.cost(p)).unwrap_or(0.0);
    let prep_base = prep.map(|p| price.no_share(p)).unwrap_or(0.0) * n as f64;
    let actual: f64 = outs.iter().map(|o| price.cost(o)).sum::<f64>() + prep_cost;
    let base: f64 = outs.iter().map(|o| price.no_share(o)).sum::<f64>() + prep_base;
    let total_in: u64 = outs.iter().map(|o| o.fresh_in + o.cached_in).sum::<u64>()
        + prep.map(|p| p.fresh_in + p.cached_in).unwrap_or(0);
    let cached: u64 =
        outs.iter().map(|o| o.cached_in).sum::<u64>() + prep.map(|p| p.cached_in).unwrap_or(0);
    let hit = if total_in > 0 { cached as f64 / total_in as f64 * 100.0 } else { 0.0 };

    println!(
        "\n  计价权重 新算输入={} 缓存命中={} 输出={}（可用 I_AGENT_PRICE_IN/CACHE/OUT 换成自己的单价）",
        price.input, price.cached, price.output
    );
    println!("  整体缓存命中率 {hit:.1}%（输入侧 {cached}/{total_in} tok 命中）");

    if base > 0.0 {
        let ratio = actual / base * 100.0;
        let verdict =
            if ratio < 40.0 { "\x1b[32m达标\x1b[0m" } else { "\x1b[33m未达标\x1b[0m" };
        println!("  实际总成本当量 {actual:.0}  |  同样 {n} 支各跑各的（零共享）基线 {base:.0}");
        println!("  \x1b[1m比值 {ratio:.1}%\x1b[0m（F-14 验收线 <40%）  {verdict}");
        println!(
            "  \x1b[2m基线口径：把每一支实际重放过的输入全部按「新算」价重算一遍，\n\
             \x20 共享准备那一轮再乘 {n}（各跑各的时，这份准备工作每支都要自己重做）——\n\
             \x20 即这 {n} 个版本若彼此不共享前缀时的应付成本。\n\
             \x20 该基线由实测 token 数导出，不依赖任何一支的冷热状态。\x1b[0m"
        );
    }
    println!("  各分支产物见上文各自的交付说明；`i-agent --branches` 可查看分支树。");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_labelled_and_bare_variants() {
        let v = parse_variants("小红书=写成种草笔记|公众号=写成长文｜抖音口播稿三十秒版");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].0, "小红书");
        assert_eq!(v[0].1, "写成种草笔记");
        assert_eq!(v[1].0, "公众号");
        assert_eq!(v[2].0, "抖音口播稿三十秒版"); // 无标签时取前 12 字
        assert_eq!(v[2].1, "抖音口播稿三十秒版");
    }

    #[test]
    fn skips_empty_segments() {
        assert_eq!(parse_variants("a||b|  |").len(), 2);
        assert!(parse_variants("").is_empty());
    }

    #[test]
    fn variant_message_carries_isolation_rule() {
        let m = variant_message(0, 3, "小红书", "写成种草笔记");
        assert!(m.contains("写成种草笔记"));
        assert!(m.contains("第 1/3 个变体"));
        assert!(m.contains("小红书"));
        assert!(m.contains("独立文件"));
    }

    /// 准备阶段的「只出大纲」会一路留在共享前缀里压住正式任务 ——
    /// 实跑时三个变体齐刷刷交回大纲，就是这么来的。
    #[test]
    fn variant_message_ends_preparation_phase() {
        let m = variant_message(1, 3, "公众号", "写成长文");
        assert!(m.contains("准备阶段到此结束"));
        assert!(m.contains("最终成品"));
        // 阶段声明必须排在变体指令之前，模型才不会把它当成事后补充
        assert!(m.find("准备阶段到此结束").unwrap() < m.find("写成长文").unwrap());
    }

    #[test]
    fn cost_weights_apply() {
        let p = Price { input: 1.0, cached: 0.1, output: 3.0 };
        let o = Outcome {
            label: "x".into(),
            branch: "v1".into(),
            cold: true,
            calls: 1,
            fresh_in: 1000,
            cached_in: 10000,
            out: 100,
            wall_s: 1.0,
            text: String::new(),
            err: None,
        };
        assert_eq!(p.cost(&o), 1000.0 + 1000.0 + 300.0);
        // 零共享反事实：缓存命中的那 10000 tok 要按新算价重算
        assert_eq!(p.no_share(&o), 11000.0 + 300.0);
        // 共享一定不比不共享贵
        assert!(p.cost(&o) < p.no_share(&o));
    }

    #[test]
    fn no_share_equals_cost_when_nothing_cached() {
        let p = Price { input: 1.0, cached: 0.1, output: 3.0 };
        let o = Outcome {
            label: "x".into(),
            branch: "v1".into(),
            cold: true,
            calls: 1,
            fresh_in: 5000,
            cached_in: 0,
            out: 200,
            wall_s: 1.0,
            text: String::new(),
            err: None,
        };
        assert_eq!(p.cost(&o), p.no_share(&o));
    }
}
