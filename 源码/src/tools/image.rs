use super::arg_str;
use crate::config::{Config, ImageProvider};
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

pub fn generate(args: &Value, cfg: &Config) -> Result<String, String> {
    let prompt = arg_str(args, "prompt").ok_or("缺少 prompt")?;
    let path = arg_str(args, "path").ok_or("缺少 path")?;
    let size = arg_str(args, "size").unwrap_or("1024x1024");
    if cfg.image_providers.is_empty() {
        return Err("未配置生图供应商（需 ZHIPU_API_KEY 或 SILICONFLOW_API_KEY）".into());
    }
    let mut errors = Vec::new();
    for p in &cfg.image_providers {
        match call_provider(p, prompt, size) {
            Ok(bytes) => {
                // 按真实字节魔数纠正扩展名，避免 JPEG 顶着 .png 落盘
                let actual_ext = sniff_ext(&bytes);
                let mut out = cfg.resolve(path);
                let mut final_path = path.to_string();
                let claimed = out.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                if !actual_ext.is_empty() && claimed != actual_ext {
                    out.set_extension(actual_ext);
                    final_path = out
                        .strip_prefix(&cfg.workspace)
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|_| out.to_string_lossy().to_string());
                }
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&out, &bytes).map_err(|e| e.to_string())?;
                let note = if final_path != path {
                    format!("（供应商实际返回 {actual_ext} 格式，已按真实格式存为 {final_path}，引用处请用此路径）")
                } else {
                    String::new()
                };
                return Ok(format!(
                    "已生成图片 {final_path}（{} 字节，{}/{}）{note}",
                    bytes.len(),
                    p.name,
                    p.model
                ));
            }
            Err(e) => errors.push(format!("[{}] {e}", p.name)),
        }
    }
    Err(format!("生图失败:\n{}", errors.join("\n")))
}

/// 按目标 URL 构造客户端：代理走不走由 no_proxy 逐个判定
/// （API 端点和返回的图片 CDN 域名可能一个要代理一个不要）
fn img_agent(url: &str) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(180));
    if let Some(p) = crate::llm::proxy_for(url) {
        b = b.proxy(p);
    }
    b.build()
}

fn call_provider(p: &ImageProvider, prompt: &str, size: &str) -> Result<Vec<u8>, String> {
    let mut body = json!({"model": p.model, "prompt": prompt});
    let endpoint;
    if p.name.contains("minimax") {
        endpoint = "image_generation";
        body["aspect_ratio"] = json!(size_to_ratio(size));
        body["response_format"] = json!("url");
        body["n"] = json!(1);
    } else if p.name.contains("siliconflow") {
        endpoint = "images/generations";
        body["image_size"] = json!(size);
        body["batch_size"] = json!(1);
    } else {
        endpoint = "images/generations";
        body["size"] = json!(size);
    }
    let url = format!("{}/{endpoint}", p.base.trim_end_matches('/'));
    let resp = img_agent(&url)
        .post(&url)
        .set("Authorization", &format!("Bearer {}", p.key))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                let b: String = r.into_string().unwrap_or_default().chars().take(300).collect();
                format!("HTTP {code}: {b}")
            }
            ureq::Error::Transport(t) => format!("网络错误: {t}"),
        })?;
    let v: Value = serde_json::from_str(&resp.into_string().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    // minimax: data.image_urls[0]
    if let Some(u) = v
        .get("data")
        .and_then(|d| d.get("image_urls"))
        .and_then(|a| a.get(0))
        .and_then(|u| u.as_str())
    {
        return download(u);
    }
    // 兼容 data[0].url / data[0].b64_json / images[0].url
    let item = v
        .get("data")
        .or_else(|| v.get("images"))
        .and_then(|d| d.get(0))
        .ok_or("响应中无图片数据")?;
    if let Some(url) = item.get("url").and_then(|u| u.as_str()) {
        return download(url);
    }
    if let Some(b64) = item.get("b64_json").and_then(|u| u.as_str()) {
        return base64_decode(b64).ok_or_else(|| "base64 解码失败".into());
    }
    Err("响应中无 url 或 b64_json".into())
}

fn sniff_ext(bytes: &[u8]) -> &'static str {
    if bytes.len() < 12 {
        return "";
    }
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else {
        ""
    }
}

fn size_to_ratio(size: &str) -> &'static str {
    let (w, h) = match size.split_once('x') {
        Some((w, h)) => (w.parse::<f32>().unwrap_or(1.0), h.parse::<f32>().unwrap_or(1.0)),
        None => return "1:1",
    };
    let r = w / h;
    if r > 2.0 {
        "21:9"
    } else if r > 1.55 {
        "16:9"
    } else if r > 1.2 {
        "4:3"
    } else if r > 0.85 {
        "1:1"
    } else if r > 0.65 {
        "3:4"
    } else {
        "9:16"
    }
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let resp = img_agent(url).get(url).call().map_err(|e| format!("下载失败: {e}"))?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(30_000_000)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = val(c)?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}
