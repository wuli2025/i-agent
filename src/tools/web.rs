use super::{arg_str, cap};
use crate::config::Config;
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

pub fn fetch(args: &Value, _cfg: &Config) -> Result<String, String> {
    let url = arg_str(args, "url").ok_or("缺少 url")?;
    let max_chars = args
        .get("max_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(8000)
        .clamp(500, 60000) as usize;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("url 必须以 http(s):// 开头".into());
    }
    let mut b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .redirects(5);
    if let Some(p) = crate::llm::proxy_for(url) {
        b = b.proxy(p);
    }
    let agent = b.build();
    let resp = agent
        .get(url)
        .set("User-Agent", "Mozilla/5.0 (compatible; i-agent/0.1)")
        .set("Accept", "text/html,application/json,text/plain,*/*")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            ureq::Error::Transport(t) => format!("网络错误: {t}"),
        })?;
    let ct = resp.content_type().to_string();
    let mut body = String::new();
    resp.into_reader()
        .take(4_000_000)
        .read_to_string(&mut body)
        .map_err(|e| format!("读响应失败: {e}"))?;
    let text = if ct.contains("html") {
        html_to_text(&body)
    } else {
        body
    };
    Ok(cap(text, max_chars))
}

fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    for tag in ["script", "style", "noscript", "svg", "head"] {
        let re = regex_lite::Regex::new(&format!(r"(?is)<{tag}[^>]*>.*?</{tag}>")).unwrap();
        s = re.replace_all(&s, " ").to_string();
    }
    let re_break = regex_lite::Regex::new(r"(?i)<(br|/p|/div|/li|/h[1-6]|/tr)[^>]*>").unwrap();
    s = re_break.replace_all(&s, "\n").to_string();
    let re_tag = regex_lite::Regex::new(r"(?s)<[^>]+>").unwrap();
    s = re_tag.replace_all(&s, "").to_string();
    for (from, to) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ] {
        s = s.replace(from, to);
    }
    // 压缩空行
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(t);
        out.push('\n');
    }
    out
}
