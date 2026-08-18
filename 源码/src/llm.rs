use crate::config::{Config, Protocol, Provider};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::time::Duration;

/// 「这一轮模型什么都没吐出来」的标记。
/// 不能当致命错误：推理模型偶尔整轮只有思考内容，过滤后就是空的。
/// 遇到它要重试／推一把，而不是让整个任务当场中止。
pub const EMPTY_MARK: &str = "__EMPTY_RESPONSE__";

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct FnCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub typ: String,
    pub function: FnCall,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Msg {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Msg {
    pub fn text(role: &str, content: &str) -> Msg {
        Msg {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn tool_result(id: &str, content: String) -> Msg {
        Msg {
            role: "tool".into(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: Some(id.into()),
        }
    }
}

pub struct ChatOut {
    pub msg: Msg,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
}

/// 从 URL 里抠出主机名（去掉 scheme/userinfo/端口/IPv6 方括号）
fn url_host(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let rest = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let rest = rest.rsplit('@').next().unwrap_or(rest);
    if let Some(r) = rest.strip_prefix('[') {
        return r.split(']').next().unwrap_or(r).to_ascii_lowercase();
    }
    match rest.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            h.to_ascii_lowercase()
        }
        _ => rest.to_ascii_lowercase(),
    }
}

/// 极简 glob：`*` 匹配任意串（no_proxy 里常见 `127.*`、`*.internal` 这类写法）
fn glob_match(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut pos = 0usize;
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !s.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == last {
            return s.len() >= pos + part.len() && s[pos..].ends_with(part);
        } else {
            match s[pos..].find(part) {
                Some(p) => pos += p + part.len(),
                None => return false,
            }
        }
    }
    true
}

/// host 是否命中 no_proxy 列表（逗号分隔）。
/// 认三种写法：`*` 全匹配；带 `*` 的 glob（如 `127.*`）；
/// 普通域名按 curl 语义做后缀匹配（`example.com` 同时命中自身与子域）。
fn host_in_no_proxy(host: &str, list: &str) -> bool {
    for tok in list.split(',') {
        let mut t = tok.trim().to_ascii_lowercase();
        if t.is_empty() {
            continue;
        }
        if t == "*" {
            return true;
        }
        // 去掉可选的 :端口 后缀
        if let Some((h, p)) = t.rsplit_once(':') {
            if !h.is_empty() && !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) {
                t = h.to_string();
            }
        }
        let t = t.trim_start_matches('.');
        if t.contains('*') {
            if glob_match(t, host) {
                return true;
            }
        } else if host == t || host.ends_with(&format!(".{t}")) {
            return true;
        }
    }
    false
}

/// 这个目标是否应该绕过代理直连。
/// loopback（localhost/127.x/::1）永远直连——把发往本机的请求塞给外部代理
/// 没有任何正确的场景，代理无法回环路由，只会答 502。
fn bypass_proxy(host: &str) -> bool {
    if host == "localhost" || host == "::1" || host.starts_with("127.") {
        return true;
    }
    for k in ["NO_PROXY", "no_proxy"] {
        if let Ok(v) = std::env::var(k) {
            if host_in_no_proxy(host, &v) {
                return true;
            }
        }
    }
    false
}

/// ureq 不像 curl 那样自动读代理环境变量，得手动接上——
/// 否则在需要走代理才能出网的机器上，请求会一直挂到超时，且毫无提示。
/// 同时必须按目标 URL 认 no_proxy：不认的话，发往 127.0.0.1 本地服务的
/// 请求也会被塞进系统代理，直接 502（评测 harness 实测踩过）。
pub fn proxy_for(url: &str) -> Option<ureq::Proxy> {
    if bypass_proxy(&url_host(url)) {
        return None;
    }
    for k in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            if let Ok(p) = ureq::Proxy::new(v) {
                return Some(p);
            }
        }
    }
    None
}

pub fn agent_http_for(url: &str) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(300))
        .timeout_write(Duration::from_secs(60));
    if let Some(p) = proxy_for(url) {
        b = b.proxy(p);
    }
    b.build()
}

fn err_body(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let body: String = body.chars().take(400).collect();
            format!("HTTP {code}: {body}")
        }
        ureq::Error::Transport(t) => format!("网络错误: {t}"),
    }
}

fn is_retryable(e: &str) -> bool {
    e.contains("HTTP 429") || e.contains("HTTP 5") || e.contains("网络错误")
}

/// 流式过滤 <think>...</think>（国产推理模型把思考混在 content 里）：
/// 思考内容不显示、不进历史，省短上下文。
struct ThinkFilter {
    in_think: bool,
    pending: String,
}

impl ThinkFilter {
    fn new() -> ThinkFilter {
        ThinkFilter { in_think: false, pending: String::new() }
    }
    /// 输入增量文本，返回可安全显示/保留的文本
    fn feed(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for c in chunk.chars() {
            self.pending.push(c);
            loop {
                if self.in_think {
                    if let Some(pos) = self.pending.find("</think>") {
                        self.pending.drain(..pos + 8);
                        self.in_think = false;
                        continue;
                    }
                    // 只保留可能构成 </think> 的尾巴（注意 UTF-8 字符边界）
                    if self.pending.len() > 24 {
                        let mut cut = self.pending.len() - 8;
                        while !self.pending.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        let keep = self.pending.split_off(cut);
                        self.pending = keep;
                    }
                } else if let Some(pos) = self.pending.find("<think>") {
                    out.push_str(&self.pending[..pos]);
                    self.pending.drain(..pos + 7);
                    self.in_think = true;
                    continue;
                } else if !"<think>".starts_with(suffix_partial(&self.pending)) {
                    // pending 尾部不可能是 <think> 前缀，全部放行
                    out.push_str(&self.pending);
                    self.pending.clear();
                }
                break;
            }
        }
        out
    }
    fn finish(&mut self) -> String {
        if self.in_think {
            self.pending.clear();
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}

/// 返回 pending 尾部可能构成标签开头的部分（从最后一个 '<' 起）
fn suffix_partial(s: &str) -> &str {
    match s.rfind('<') {
        Some(i) => &s[i..],
        None => "",
    }
}

/// 流式对话。tools 为空数组时不传 tools 字段。
pub fn chat(
    cfg: &Config,
    messages: &[Msg],
    tools: &Value,
    on_text: &dyn Fn(&str),
) -> Result<ChatOut, String> {
    let mut providers: Vec<&Provider> = vec![&cfg.provider];
    providers.extend(cfg.fallbacks.iter());
    let mut errors: Vec<String> = Vec::new();

    for (pi, p) in providers.iter().enumerate() {
        // 供应商回退意味着换了模型（可能连协议/计费方都换了），绝不能静默进行 ——
        // 静默切换会让配置错误看起来「一切正常」，跑出来的结果却全不是那回事。
        if pi > 0 {
            eprintln!(
                "[警告] 供应商 {} 调用失败，回退到 {}（模型 {} → {}）",
                providers[pi - 1].name,
                p.name,
                providers[pi - 1].model,
                p.model
            );
        }
        let mut attempt = 0;
        let mut empty_retries = 0;
        let mut with_stream_options = true;
        loop {
            attempt += 1;
            let r = match p.protocol {
                Protocol::Anthropic => anthropic_once(cfg, p, messages, tools, on_text),
                Protocol::OpenAI => {
                    stream_once(cfg, p, messages, tools, on_text, with_stream_options)
                }
            };
            match r {
                Ok(out) => return Ok(out),
                Err(e) => {
                    if e.contains("stream_options") && with_stream_options {
                        with_stream_options = false;
                        continue;
                    }
                    // 整轮空输出：重采样一次通常就好了，别把任务判死
                    if e == EMPTY_MARK && empty_retries < 2 {
                        empty_retries += 1;
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    let retry = is_retryable(&e) && attempt < 3;
                    if retry {
                        std::thread::sleep(Duration::from_millis(1200 * attempt as u64));
                        continue;
                    }
                    errors.push(format!("[{}] {}", p.name, e));
                    break;
                }
            }
        }
    }
    // 所有供应商都只给出空输出 → 交给上层去「推一把」，而不是直接失败
    if errors.iter().all(|e| e.contains(EMPTY_MARK)) {
        return Err(EMPTY_MARK.into());
    }
    Err(errors.join("\n"))
}

/* ==================== Anthropic Messages 协议 ==================== */

/// 把内部的 OpenAI 形状消息转成 Anthropic 的 system + messages。
/// 要点：tool 结果必须以 user 消息里的 tool_result block 出现，且连续的 tool 结果要合并成一条。
fn to_anthropic_messages(messages: &[Msg]) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut out: Vec<Value> = Vec::new();

    for m in messages {
        match m.role.as_str() {
            "system" => {
                if let Some(c) = &m.content {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(c);
                }
            }
            "tool" => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content.clone().unwrap_or_default(),
                });
                // 紧跟在上一条 user 的 tool_result 后面就合并进去
                let merged = out
                    .last_mut()
                    .filter(|last| last.get("role").and_then(|r| r.as_str()) == Some("user"))
                    .and_then(|last| last.get_mut("content"))
                    .and_then(|c| c.as_array_mut())
                    .filter(|arr| {
                        arr.first().and_then(|b| b.get("type")).and_then(|t| t.as_str())
                            == Some("tool_result")
                    })
                    .map(|arr| arr.push(block.clone()))
                    .is_some();
                if !merged {
                    out.push(json!({"role": "user", "content": [block]}));
                }
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(c) = &m.content {
                    if !c.trim().is_empty() {
                        blocks.push(json!({"type": "text", "text": c}));
                    }
                }
                for tc in m.tool_calls.iter().flatten() {
                    let input: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or_else(|_| json!({}));
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.function.name,
                        "input": input,
                    }));
                }
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            _ => {
                // user
                if let Some(c) = &m.content {
                    out.push(json!({"role": "user", "content": [{"type": "text", "text": c}]}));
                }
            }
        }
    }
    (system, out)
}

/// OpenAI 的 tools schema → Anthropic 的 input_schema 形状
fn to_anthropic_tools(tools: &Value) -> Vec<Value> {
    tools
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let f = t.get("function")?;
                    Some(json!({
                        "name": f.get("name")?,
                        "description": f.get("description").cloned().unwrap_or(json!("")),
                        "input_schema": f.get("parameters").cloned()
                            .unwrap_or(json!({"type":"object","properties":{}})),
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn anthropic_once(
    cfg: &Config,
    p: &Provider,
    messages: &[Msg],
    tools: &Value,
    on_text: &dyn Fn(&str),
) -> Result<ChatOut, String> {
    let (system, msgs) = to_anthropic_messages(messages);
    let mut body = json!({
        "model": p.model,
        "messages": msgs,
        "max_tokens": cfg.max_output,
        "temperature": 0.6,
        "stream": true,
    });
    // F-02 显式缓存断点：此前 system 以纯字符串发送、全仓零 cache_control，
    // 前缀缓存 100% 依赖端点自动缓存——一旦端点不自动缓存命中率直接归零。
    // 现在 system 改 block 数组，并在三处打 ephemeral 断点：
    // ① system 末尾（最大、最稳的一段前缀）② tools 末项 ③ 最近一条消息（滚动前缀）。
    // 端点不支持 cache_control 时会忽略该字段，不报错——所以这是纯增益。
    if !system.is_empty() {
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": {"type": "ephemeral"},
        }]);
    }
    let at = to_anthropic_tools(tools);
    if !at.is_empty() {
        let mut at = at;
        if let Some(last) = at.last_mut() {
            last["cache_control"] = json!({"type": "ephemeral"});
        }
        body["tools"] = json!(at);
    }
    // 在最近一条消息的最后一个 content block 上打断点，滚动缓存对话前缀
    if let Some(arr) = body["messages"].as_array_mut() {
        if let Some(last_msg) = arr.last_mut() {
            if let Some(blocks) = last_msg["content"].as_array_mut() {
                if let Some(last_block) = blocks.last_mut() {
                    last_block["cache_control"] = json!({"type": "ephemeral"});
                }
            }
        }
    }

    let base = p.base.trim_end_matches('/');
    // base 可能已经带 /v1，也可能没有
    let url = if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    };

    let resp = agent_http_for(&url)
        .post(&url)
        .set("x-api-key", &p.key)
        .set("Authorization", &format!("Bearer {}", p.key))
        .set("anthropic-version", "2023-06-01")
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(err_body)?;

    let mut reader = BufReader::new(resp.into_reader());
    let mut content = String::new();
    let (mut u_in, mut u_out, mut u_cache) = (0u64, 0u64, 0u64);
    // index -> (id, name, partial_json)
    let mut blocks: Vec<(String, String, String)> = Vec::new();
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| format!("读流失败: {e}"))?;
        if n == 0 {
            break;
        }
        let l = line.trim();
        let Some(data) = l.strip_prefix("data:") else { continue };
        let Ok(ev) = serde_json::from_str::<Value>(data.trim()) else { continue };
        let typ = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match typ {
            "message_start" => {
                if let Some(u) = ev.get("message").and_then(|m| m.get("usage")) {
                    u_in = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    u_cache =
                        u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                }
            }
            "content_block_start" => {
                let idx = ev.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                while blocks.len() <= idx {
                    blocks.push((String::new(), String::new(), String::new()));
                }
                if let Some(cb) = ev.get("content_block") {
                    if cb.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        blocks[idx].0 =
                            cb.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        blocks[idx].1 =
                            cb.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    }
                }
            }
            "content_block_delta" => {
                let idx = ev.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                while blocks.len() <= idx {
                    blocks.push((String::new(), String::new(), String::new()));
                }
                let Some(d) = ev.get("delta") else { continue };
                match d.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "text_delta" => {
                        if let Some(t) = d.get("text").and_then(|v| v.as_str()) {
                            content.push_str(t);
                            on_text(t);
                        }
                    }
                    "input_json_delta" => {
                        if let Some(t) = d.get("partial_json").and_then(|v| v.as_str()) {
                            blocks[idx].2.push_str(t);
                        }
                    }
                    // thinking_delta / signature_delta：推理内容，不显示也不入历史
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(u) = ev.get("usage") {
                    u_out = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(u_out);
                    // MiniMax 在 message_start 里把 input_tokens 报成 0，真实值可能补在
                    // message_delta 里——取到非零就认，否则输入用量会一直显示 0。
                    if let Some(i) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                        if i > 0 {
                            u_in = i;
                        }
                    }
                    if let Some(c) = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()) {
                        if c > 0 {
                            u_cache = c;
                        }
                    }
                }
            }
            // Anthropic 的流没有 [DONE] 哨兵，必须在 message_stop 上主动收流；
            // 只等 EOF 的话，keep-alive 连接会让读取永久阻塞。
            "message_stop" => break,
            "error" => {
                let m = ev
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知错误");
                return Err(format!("服务端错误: {m}"));
            }
            _ => {}
        }
    }

    let tool_calls: Vec<ToolCall> = blocks
        .into_iter()
        .enumerate()
        .filter(|(_, b)| !b.1.is_empty())
        .map(|(i, (id, name, args))| {
            // 输出被截断时 partial_json 是残缺的，必须消毒，否则下轮请求会被 400
            let arguments = if args.trim().is_empty() {
                "{}".to_string()
            } else if serde_json::from_str::<Value>(&args).is_ok() {
                args
            } else {
                "{\"__truncated__\":true}".to_string()
            };
            ToolCall {
                id: if id.is_empty() { format!("call_{i}") } else { id },
                typ: "function".into(),
                function: FnCall { name, arguments },
            }
        })
        .collect();

    let content = content.trim().to_string();
    if content.is_empty() && tool_calls.is_empty() {
        return Err(EMPTY_MARK.into());
    }

    Ok(ChatOut {
        msg: Msg {
            role: "assistant".into(),
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            tool_call_id: None,
        },
        prompt_tokens: u_in,
        completion_tokens: u_out,
        cached_tokens: u_cache,
    })
}

fn stream_once(
    cfg: &Config,
    p: &Provider,
    messages: &[Msg],
    tools: &Value,
    on_text: &dyn Fn(&str),
    with_stream_options: bool,
) -> Result<ChatOut, String> {
    let mut body = json!({
        "model": p.model,
        "messages": messages,
        "stream": true,
        "max_tokens": cfg.max_output,
        "temperature": 0.6,
    });
    if tools.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        body["tools"] = tools.clone();
    }
    if with_stream_options {
        body["stream_options"] = json!({"include_usage": true});
    }

    let url = format!("{}/chat/completions", p.base.trim_end_matches('/'));
    let resp = agent_http_for(&url)
        .post(&url)
        .set("Authorization", &format!("Bearer {}", p.key))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(err_body)?;

    let mut reader = BufReader::new(resp.into_reader());
    let mut content = String::new();
    let mut filter = ThinkFilter::new();
    let (mut u_in, mut u_out, mut u_cache) = (0u64, 0u64, 0u64);
    // (id, name, arguments) 按 index 聚合
    let mut tcs: Vec<(String, String, String)> = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| format!("读流失败: {e}"))?;
        if n == 0 {
            break;
        }
        let l = line.trim();
        let Some(data) = l.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            u_in = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(u_in);
            u_out = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(u_out);
            u_cache = u
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(u_cache);
        }
        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else { continue };
        let Some(delta) = choice.get("delta") else { continue };
        if let Some(t) = delta.get("content").and_then(|v| v.as_str()) {
            if !t.is_empty() {
                let visible = filter.feed(t);
                if !visible.is_empty() {
                    content.push_str(&visible);
                    on_text(&visible);
                }
            }
        }
        if let Some(arr) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in arr {
                let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                while tcs.len() <= idx {
                    tcs.push((String::new(), String::new(), String::new()));
                }
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    tcs[idx].0.push_str(id);
                }
                if let Some(f) = tc.get("function") {
                    if let Some(nm) = f.get("name").and_then(|v| v.as_str()) {
                        tcs[idx].1.push_str(nm);
                    }
                    if let Some(a) = f.get("arguments").and_then(|v| v.as_str()) {
                        tcs[idx].2.push_str(a);
                    }
                }
            }
        }
    }

    let rest = filter.finish();
    if !rest.is_empty() {
        content.push_str(&rest);
        on_text(&rest);
    }
    let content = content.trim().to_string();

    let tool_calls: Vec<ToolCall> = tcs
        .into_iter()
        .enumerate()
        .filter(|(_, t)| !t.1.is_empty())
        .map(|(i, (id, name, args))| {
            // 输出超长被截断时参数会是残缺 JSON：必须消毒后再入历史，
            // 否则下一轮请求会被服务端 400 拒绝
            let arguments = if serde_json::from_str::<Value>(&args).is_ok() {
                args
            } else {
                "{\"__truncated__\":true}".to_string()
            };
            ToolCall {
                id: if id.is_empty() { format!("call_{i}") } else { id },
                typ: "function".into(),
                function: FnCall { name, arguments },
            }
        })
        .collect();

    // 整轮只有 <think>：过滤后 content 为空、也没有工具调用。
    // 这不是致命错误，交给上层重试／推一把。
    if content.is_empty() && tool_calls.is_empty() {
        return Err(EMPTY_MARK.into());
    }

    Ok(ChatOut {
        msg: Msg {
            role: "assistant".into(),
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            tool_call_id: None,
        },
        prompt_tokens: u_in,
        completion_tokens: u_out,
        cached_tokens: u_cache,
    })
}

/// 非流式简单调用（用于历史摘要等内部用途），不带工具。
pub fn chat_simple(cfg: &Config, prompt: &str, max_tokens: usize) -> Result<String, String> {
    let p = &cfg.provider;
    let base = p.base.trim_end_matches('/');

    let (url, body, anthropic) = match p.protocol {
        Protocol::Anthropic => {
            let url = if base.ends_with("/v1") {
                format!("{base}/messages")
            } else {
                format!("{base}/v1/messages")
            };
            let body = json!({
                "model": p.model,
                "messages": [{"role": "user", "content": [{"type":"text","text": prompt}]}],
                "max_tokens": max_tokens,
                "temperature": 0.3,
            });
            (url, body, true)
        }
        Protocol::OpenAI => {
            let body = json!({
                "model": p.model,
                "messages": [{"role": "user", "content": prompt}],
                "stream": false,
                "max_tokens": max_tokens,
                "temperature": 0.3,
            });
            (format!("{base}/chat/completions"), body, false)
        }
    };

    let resp = agent_http_for(&url)
        .post(&url)
        .set("Authorization", &format!("Bearer {}", p.key))
        .set("x-api-key", &p.key)
        .set("anthropic-version", "2023-06-01")
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(err_body)?;
    let v: Value = serde_json::from_str(&resp.into_string().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    if anthropic {
        // content 是 block 数组，取所有 text block 拼起来（thinking block 自动跳过）
        let text: String = v
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if text.is_empty() {
            return Err("摘要调用返回为空".into());
        }
        return Ok(text);
    }

    v.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "摘要调用返回为空".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_host() {
        assert_eq!(url_host("http://127.0.0.1:8804/v1/messages"), "127.0.0.1");
        assert_eq!(url_host("https://api.anthropic.com"), "api.anthropic.com");
        assert_eq!(url_host("https://API.Kimi.com/coding?x=1"), "api.kimi.com");
        assert_eq!(url_host("http://user:pw@host.com:81/p"), "host.com");
        assert_eq!(url_host("http://[::1]:8080/x"), "::1");
        assert_eq!(url_host("http://localhost/x"), "localhost");
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("127.*", "127.0.0.1"));
        assert!(!glob_match("127.*", "128.0.0.1"));
        assert!(glob_match("*.internal", "svc.internal"));
        assert!(!glob_match("*.internal", "svc.internal.com"));
        assert!(glob_match("10.*.1", "10.0.0.1"));
        assert!(glob_match("exact.com", "exact.com"));
        assert!(!glob_match("exact.com", "notexact.com"));
    }

    #[test]
    fn test_host_in_no_proxy() {
        // 精确 + 子域后缀（curl 语义）
        assert!(host_in_no_proxy("example.com", "example.com"));
        assert!(host_in_no_proxy("api.example.com", "example.com"));
        assert!(host_in_no_proxy("api.example.com", ".example.com"));
        assert!(!host_in_no_proxy("badexample.com", "example.com"));
        // glob（clash 一类代理常见的 no_proxy 写法）
        assert!(host_in_no_proxy("127.0.0.1", "127.*"));
        assert!(host_in_no_proxy("anything.com", "*"));
        // 带端口的写法
        assert!(host_in_no_proxy("localhost", "localhost:8080"));
        // 逗号列表 + 空白
        assert!(host_in_no_proxy("127.0.0.1", "localhost, 127.0.0.1"));
        assert!(!host_in_no_proxy("example.com", "localhost,127.0.0.1"));
    }

    #[test]
    fn test_loopback_always_bypasses() {
        assert!(bypass_proxy("127.0.0.1"));
        assert!(bypass_proxy("127.1.2.3"));
        assert!(bypass_proxy("localhost"));
        assert!(bypass_proxy("::1"));
        assert!(!bypass_proxy("api.kimi.com"));
    }
}
