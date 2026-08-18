use super::arg_str;
use super::jscheck;
use crate::config::Config;
use serde_json::Value;

/// 判断在两段代码之间是否必须补一个 `;`。
/// 规则来自 JS 的 ASI（自动分号插入）实际会咬人的那几种情况：
///   `const GAME =` + `{...}`   → 不能补（左边在等一个值）
///   `{...}`       + `const x`  → 必须补（否则上一句没结束）
///   `}` / `]` / `)` / 字面量   + `(` / `[` / 反引号 → 必须补（否则被当成调用/下标/模板串）
fn needs_semicolon(prev: &str, next: &str) -> bool {
    let a = prev.trim_end();
    let b = next.trim_start();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    // 下一段已经自带分号了
    if b.starts_with(';') {
        return false;
    }
    let last = a.chars().last().unwrap();
    // 左边在等一个值：赋值、逗号、开括号、运算符之后，绝不能插分号
    if matches!(
        last,
        '=' | ',' | '(' | '[' | '{' | '+' | '-' | '*' | '/' | '&' | '|' | ':' | '?' | ';'
    ) {
        return false;
    }
    // 左边是一个完整的值/语句结尾（} ] ) 数字 引号 标识符），右边又要开始新东西 → 必须断句
    matches!(last, '}' | ']' | ')' | '"' | '\'' | '`') || last.is_alphanumeric() || last == '_'
}

/// bundle：把多段文件确定性地拼成一个产物。
/// 存在的意义：模型手写 `cat a b c > out.html` 时，接缝处漏一个分号就整页白屏，
/// 而这种错文本检查很难发现（把脚本块拼起来检查反而会把它掩盖掉）。
/// 这里把拼接变成工具的确定性行为：按 ASI 规则补分号 → 逐块单独做真语法检查 → 报告。
pub fn run(args: &Value, cfg: &Config) -> Result<String, String> {
    let out = arg_str(args, "out").ok_or("缺少 out")?;
    let parts = args
        .get("parts")
        .and_then(|p| p.as_array())
        .ok_or("缺少 parts（文件路径数组，按拼接顺序）")?;
    if parts.len() < 2 {
        return Err("parts 至少要两个文件".into());
    }

    let mut texts: Vec<String> = Vec::new();
    for p in parts {
        let path = p.as_str().ok_or("parts 每项必须是文件路径字符串")?;
        let full = cfg.resolve(path);
        let t = std::fs::read_to_string(&full).map_err(|e| format!("读取 {path} 失败: {e}"))?;
        texts.push(t);
    }

    let mut content = String::new();
    let mut repaired: Vec<String> = Vec::new();
    for (i, t) in texts.iter().enumerate() {
        if i > 0 {
            let prev = content.as_str();
            if needs_semicolon(prev, t) {
                content.push_str(";\n");
                let name = parts[i - 1].as_str().unwrap_or("?");
                let next = parts[i].as_str().unwrap_or("?");
                repaired.push(format!("{name} → {next} 之间补了一个 ;"));
            } else if !content.ends_with('\n') && !t.starts_with('\n') {
                content.push('\n');
            }
        }
        content.push_str(t);
    }

    let full_out = cfg.resolve(out);
    if let Some(parent) = full_out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&full_out, &content).map_err(|e| format!("写入 {out} 失败: {e}"))?;

    let lines = content.lines().count();
    let kb = content.len() as f64 / 1024.0;
    let mut report = format!(
        "已拼接 {} 段 → {out}（{lines} 行, {kb:.1}KB）\n",
        parts.len()
    );
    if repaired.is_empty() {
        report.push_str("接缝检查: 无需补分号\n");
    } else {
        report.push_str("接缝修复:\n");
        for r in &repaired {
            report.push_str(&format!("- {r}\n"));
        }
    }

    // 只有 HTML 产物才做 JS 语法检查
    if out.to_lowercase().ends_with(".html") || out.to_lowercase().ends_with(".htm") {
        match jscheck::check_html_syntax(cfg, &content) {
            Ok(msg) => {
                report.push_str(&format!("{msg}\n"));
                report.push_str("下一步：必须用 browser 工具在真浏览器里跑一次，确认不白屏、无运行时异常，才算完成。");
            }
            Err(e) => {
                report.push_str(&e);
                report.push_str("\n拼接产物有语法错误，请修复源文件后重新 bundle。");
            }
        }
    }
    Ok(report)
}
