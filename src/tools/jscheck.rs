use crate::config::Config;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 抽出 HTML 里所有内联 <script>（跳过 src= 外链和非 JS 类型），返回 (起始行号, 代码)
pub fn inline_scripts(html: &str) -> Vec<(usize, String)> {
    let bytes = html.as_bytes();
    let lower = html.to_lowercase();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = lower[i..].find("<script") {
        let tag_start = i + rel;
        let Some(rel_gt) = lower[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + rel_gt; // '>' 的位置
        let tag = &lower[tag_start..=tag_end];
        let Some(rel_close) = lower[tag_end + 1..].find("</script") else {
            break;
        };
        let body_start = tag_end + 1;
        let body_end = body_start + rel_close;

        // 外链脚本没有内联代码可查；非 JS 类型（json/template）也跳过
        let is_external = tag.contains(" src=");
        let is_nonjs = tag.contains("type=")
            && !tag.contains("type=\"text/javascript\"")
            && !tag.contains("type=\"module\"")
            && !tag.contains("type='text/javascript'")
            && !tag.contains("type='module'");
        if !is_external && !is_nonjs {
            let code = &html[body_start..body_end];
            if !code.trim().is_empty() {
                let line = bytes[..body_start].iter().filter(|&&b| b == b'\n').count() + 1;
                out.push((line, code.to_string()));
            }
        }
        i = body_end;
    }
    out
}

fn node_bin() -> Option<String> {
    if let Ok(n) = std::env::var("I_AGENT_NODE") {
        if !n.trim().is_empty() {
            return Some(n);
        }
    }
    for c in ["node", "nodejs", "node.exe"] {
        if Command::new(c)
            .arg("-v")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(c.to_string());
        }
    }
    None
}

/// 逐个 <script> 块单独做语法检查。
/// 关键：**绝不把多个块拼起来再检**——拼接时插入的分隔符会把真实的漏分号掩盖掉。
/// 返回 Err(报告) 表示有语法错，Ok(说明) 表示通过或无法检查。
pub fn check_html_syntax(cfg: &Config, html: &str) -> Result<String, String> {
    let scripts = inline_scripts(html);
    if scripts.is_empty() {
        return Ok("（无内联脚本）".into());
    }
    let Some(node) = node_bin() else {
        return Ok("（未找到 node，跳过 JS 语法检查）".into());
    };

    let tmp = if cfg.stateless {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "i-agent-polaris-jscheck-{}-{nonce}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    } else {
        cfg.workspace.join(".i-agent").join("tmp")
    };
    if cfg.stateless {
        std::fs::create_dir(&tmp).map_err(|e| format!("安全创建 JS 检查临时目录失败: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700));
        }
    } else {
        let _ = std::fs::create_dir_all(&tmp);
    }
    let mut errs: Vec<String> = Vec::new();

    for (idx, (line, code)) in scripts.iter().enumerate() {
        let f = tmp.join(format!("script_{idx}.js"));
        if std::fs::write(&f, code).is_err() {
            continue;
        }
        let out = Command::new(&node)
            .arg("--check")
            .arg(&f)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        let Ok(out) = out else { continue };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // node 报的是临时文件内的行号，换算回 HTML 里的真实行号
            let mut detail = String::new();
            for l in stderr.lines().take(6) {
                if let Some(cap) = l.strip_prefix(&format!("{}:", f.display())) {
                    if let Some((n, _)) = cap.split_once(':') {
                        if let Ok(n) = n.parse::<usize>() {
                            detail
                                .push_str(&format!("  （对应 HTML 第 {} 行附近）\n", line + n - 1));
                            continue;
                        }
                    }
                }
                if l.contains("Error")
                    || l.trim_start().starts_with('^')
                    || l.contains("SyntaxError")
                {
                    detail.push_str(&format!("  {}\n", l.trim()));
                }
            }
            errs.push(format!(
                "第 {} 个内联 <script>（HTML 第 {line} 行起）语法错误:\n{detail}",
                idx + 1
            ));
        }
        let _ = std::fs::remove_file(&f);
    }

    if cfg.stateless {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    if errs.is_empty() {
        Ok(format!(
            "JS 语法检查通过（{} 个内联脚本，逐块单独校验）",
            scripts.len()
        ))
    } else {
        Err(format!(
            "JS 语法错误（{} / {} 个脚本块）:\n{}\n\
             注意：这类错误会让脚本整块不执行 → 页面白屏。常见成因是模板/数据拼接的接缝处缺分号、\
             多余逗号，或 JSON 字符串里有未转义的英文双引号。",
            errs.len(),
            scripts.len(),
            errs.join("\n")
        ))
    }
}
