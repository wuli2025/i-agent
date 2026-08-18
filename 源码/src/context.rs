use crate::config::Config;
use crate::llm::{self, Msg};

/// 粗略 token 估算：中英文混排按 3 字节 1 token 保守计
pub fn est_tokens(s: &str) -> usize {
    s.len() / 3 + 1
}

fn msg_tokens(m: &Msg) -> usize {
    let mut t = 8;
    if let Some(c) = &m.content {
        t += est_tokens(c);
    }
    if let Some(tcs) = &m.tool_calls {
        for tc in tcs {
            t += est_tokens(&tc.function.arguments) + est_tokens(&tc.function.name) + 8;
        }
    }
    t
}

pub fn total_tokens(msgs: &[Msg]) -> usize {
    msgs.iter().map(msg_tokens).sum()
}

/// 两级压缩：先裁剪旧工具输出，仍超则 LLM 摘要旧历史。
/// 面向短上下文模型，这里必须激进且无感。
pub fn ensure_budget(cfg: &Config, msgs: &mut Vec<Msg>) {
    let budget = cfg.context_window.saturating_sub(cfg.max_output + 1500);
    if budget == 0 || msgs.len() < 6 {
        return;
    }
    let threshold = budget * 3 / 4;
    if total_tokens(msgs) <= threshold {
        return;
    }

    // 第一级：裁剪旧工具输出（保护最近 6 条）
    let keep_from = msgs.len().saturating_sub(6);
    for m in msgs[1..keep_from].iter_mut() {
        if m.role == "tool" {
            if let Some(c) = &m.content {
                if c.len() > 400 {
                    let head: String = c.chars().take(120).collect();
                    m.content = Some(format!("{head}\n…[旧工具输出已裁剪]"));
                }
            }
        }
    }
    if total_tokens(msgs) <= threshold {
        return;
    }

    // 第二级：把旧历史摘要成一条备忘
    // 切点：保留最近 6 条，且不能切断 assistant(tool_calls) 与 tool 结果对
    let mut cut = msgs.len().saturating_sub(6).max(2);
    while cut < msgs.len() && msgs[cut].role == "tool" {
        cut += 1;
    }
    if cut <= 2 || cut >= msgs.len() {
        return;
    }

    let mut hist = String::new();
    for m in &msgs[1..cut] {
        let role = &m.role;
        let mut c = m.content.clone().unwrap_or_default();
        if let Some(tcs) = &m.tool_calls {
            for tc in tcs {
                let args: String = tc.function.arguments.chars().take(160).collect();
                c.push_str(&format!(" [调用 {}({})]", tc.function.name, args));
            }
        }
        let c: String = c.chars().take(if role == "tool" { 200 } else { 800 }).collect();
        hist.push_str(&format!("{role}: {c}\n"));
    }
    let prompt = format!(
        "把以下 agent 工作历史压缩成简洁备忘（600字以内），必须保留：任务总目标、已完成的步骤、已创建或修改的文件路径、关键决定、尚未完成的事项。直接输出备忘正文。\n\n{hist}"
    );
    let summary = match llm::chat_simple(cfg, &prompt, 1200) {
        Ok(s) => s,
        Err(_) => "（历史摘要失败，旧对话已省略）".to_string(),
    };
    let tail: Vec<Msg> = msgs[cut..].to_vec();
    let sys = msgs[0].clone();
    msgs.clear();
    msgs.push(sys);
    msgs.push(Msg::text(
        "user",
        &format!("[前文工作摘要，供你继续任务]\n{summary}"),
    ));
    msgs.extend(tail);
}
