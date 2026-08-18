use super::arg_str;
use super::browser;
use super::jscheck;
use crate::config::Config;
use serde_json::Value;

/// 确定性校验器：不靠模型自觉，靠工具兜底。
/// kind=json: 严格 JSON 语法校验，报行列号；顺带做游戏数据的引用完整性检查（若形似 GAME 数据）。
/// kind=html: 逐块 JS 语法校验 + 真浏览器执行冒烟（白屏/运行时异常/外链依赖）。
/// kind 省略时按扩展名自动判断。
pub fn run(args: &Value, cfg: &Config) -> Result<String, String> {
    let path = arg_str(args, "path").ok_or("缺少 path")?;
    let lower = path.to_lowercase();
    let auto = if lower.ends_with(".html") || lower.ends_with(".htm") { "html" } else { "json" };
    let kind = arg_str(args, "kind").unwrap_or(auto);
    let p = cfg.resolve(path);
    let text = std::fs::read_to_string(&p).map_err(|e| format!("读取 {path} 失败: {e}"))?;
    match kind {
        "json" => check_json(path, &text),
        "html" => check_html(path, &text, cfg),
        _ => Err(format!("不支持的 kind: {kind}（支持 json / html）")),
    }
}

/// HTML 全套校验：先静态查语法（快、能精确定位行号），过了再真浏览器跑（慢、但能抓白屏）。
fn check_html(path: &str, text: &str, cfg: &Config) -> Result<String, String> {
    let syntax = match jscheck::check_html_syntax(cfg, text) {
        Ok(msg) => msg,
        Err(e) => {
            // 语法就挂了，不必再开浏览器——先修语法
            return Ok(format!("{e}\n\n修好语法后重新 check。"));
        }
    };
    match browser::smoke(cfg, path) {
        Ok((_, report)) => Ok(format!("{syntax}\n\n{report}")),
        Err(e) => Ok(format!(
            "{syntax}\n\n浏览器冒烟未能执行: {e}\n（静态语法已通过，但没有真浏览器验证就不能确定不白屏）"
        )),
    }
}

fn check_json(path: &str, text: &str) -> Result<String, String> {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            let line = e.line();
            let col = e.column();
            let ctx = text
                .lines()
                .nth(line.saturating_sub(1))
                .unwrap_or("")
                .chars()
                .take(160)
                .collect::<String>();
            return Ok(format!(
                "JSON 语法错误: {path} 第 {line} 行第 {col} 列: {e}\n该行内容: {ctx}\n常见原因: 字符串内有未转义的英文双引号（对话引用请改用中文引号「」）、多余逗号、缺逗号。请用 edit 修复后重新 check。"
            ));
        }
    };
    let mut report = format!("JSON 语法正确: {path}");
    // 游戏数据附加检查：schema 形状 + 场景引用完整性 + 结局兜底
    if let (Some(scenes), Some(endings)) =
        (v.get("scenes").and_then(|s| s.as_array()), v.get("endings").and_then(|s| s.as_array()))
    {
        let mut shape: Vec<String> = Vec::new();
        for field in ["stats", "affinity", "flags"] {
            if let Some(fv) = v.get(field) {
                match fv.as_array() {
                    None => shape.push(format!("{field} 必须是数组（形如 [{{\"key\":...,\"name\":...}}]），不能是对象")),
                    Some(arr) => {
                        if arr.iter().any(|e| e.get("key").and_then(|k| k.as_str()).is_none()) {
                            shape.push(format!("{field} 数组每项必须有字符串 key 字段"));
                        }
                        if field == "stats"
                            && arr.iter().any(|e| e.get("init").is_none() || e.get("max").is_none())
                        {
                            shape.push("stats 每项必须有 init 与 max 字段（不是 initial/min）".into());
                        }
                    }
                }
            }
        }
        if let Some(start) = v.get("start").and_then(|s| s.as_str()) {
            if !scenes.iter().any(|s| s.get("id").and_then(|i| i.as_str()) == Some(start)) {
                shape.push(format!("start 指向的场景 {start} 不存在"));
            }
        } else {
            shape.push("缺少顶层 start 字段（开局场景 id）".into());
        }
        if !shape.is_empty() {
            report.push_str("\nGAME schema 问题（对照 SKILL.md 顶层结构修正）:\n");
            for s in &shape {
                report.push_str(&format!("- {s}\n"));
            }
        }
        let ids: std::collections::HashSet<&str> =
            scenes.iter().filter_map(|s| s.get("id").and_then(|i| i.as_str())).collect();
        let mut bad: Vec<String> = Vec::new();
        let mut refs = |scene_id: &str, key: &str, val: Option<&Value>| {
            if let Some(t) = val.and_then(|v| v.as_str()) {
                if t != "END" && !ids.contains(t) {
                    bad.push(format!("场景 {scene_id} 的 {key} 指向不存在的场景 {t}"));
                }
            }
        };
        for s in scenes {
            let sid = s.get("id").and_then(|i| i.as_str()).unwrap_or("?");
            refs(sid, "next", s.get("next"));
            if let Some(cs) = s.get("choices").and_then(|c| c.as_array()) {
                for c in cs {
                    refs(sid, "goto", c.get("goto"));
                    if let Some(ck) = c.get("check") {
                        refs(sid, "check.success", ck.get("success"));
                        refs(sid, "check.fail", ck.get("fail"));
                    }
                }
            }
        }
        let has_fallback = endings.iter().any(|e| {
            e.get("cond").and_then(|c| c.as_str()).map(|c| c.trim() == "true").unwrap_or(false)
        });
        if !has_fallback {
            bad.push("endings 缺少 cond 为 \"true\" 的兜底结局".into());
        }
        if bad.is_empty() {
            report.push_str(&format!(
                "\n游戏数据检查通过: {} 个场景引用完整，{} 个结局含兜底。",
                scenes.len(),
                endings.len()
            ));
        } else {
            report.push_str("\n游戏数据问题:\n");
            for b in &bad {
                report.push_str(&format!("- {b}\n"));
            }
        }
    }
    Ok(report)
}
