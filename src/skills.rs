use crate::config::home_dir;
use crate::embedded;
use std::path::{Path, PathBuf};

/// 资产目录发现：env > exe 同级 assets > 释放内置资产到 ~/.i-agent/assets
pub fn assets_dir() -> PathBuf {
    if let Ok(d) = std::env::var("I_AGENT_ASSETS") {
        let p = PathBuf::from(d);
        if p.exists() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let adj = dir.join("assets");
            if adj.join("skills").exists() {
                return adj;
            }
        }
    }
    materialize_assets(false)
}

/// 内嵌资产指纹（FNV-1a）：内容一变指纹就变，用于升级后自动刷新已释放资产
fn assets_fingerprint() -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for (rel, content) in embedded::ASSETS {
        feed(rel.as_bytes());
        feed(content.as_bytes());
    }
    h
}

/// 把编译期内嵌的技能包写到 ~/.i-agent/assets。
/// 指纹不匹配（二进制升级过）或 force 时整体覆盖刷新；
/// 需要自定义技能包时用 I_AGENT_ASSETS 指向独立目录，避免被刷新覆盖。
pub fn materialize_assets(force: bool) -> PathBuf {
    let root = home_dir().join(".i-agent").join("assets");
    let marker = root.join(".fingerprint");
    let fp = format!("{:016x}", assets_fingerprint());
    let stale = std::fs::read_to_string(&marker)
        .map(|m| m.trim() != fp)
        .unwrap_or(true);
    if !force && !stale {
        return root;
    }
    for (rel, content) in embedded::ASSETS {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, content);
    }
    let _ = std::fs::write(&marker, fp);
    root
}

/// 关键词命中：任务文本命中技能包时，把 SKILL.md 正文直接注入用户消息
/// （省掉模型分段 read 学习的 2-4 轮请求），最多注入 2 个包。
pub fn match_hint(assets: &Path, input: &str) -> Option<String> {
    let skills_dir = assets.join("skills");
    let mut hits: Vec<(String, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&skills_dir) {
        for e in rd.flatten() {
            let dir = e.path();
            let md = dir.join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&md) else {
                continue;
            };
            let mut name = String::new();
            let mut keywords = String::new();
            for line in text.lines().take(12) {
                if let Some(v) = line.strip_prefix("name:") {
                    name = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("keywords:") {
                    keywords = v.trim().to_string();
                }
            }
            let hit = keywords
                .split([',', '，'])
                .map(|k| k.trim())
                .any(|k| !k.is_empty() && input.contains(k));
            if hit && hits.len() < 2 {
                // 去掉 frontmatter，正文截到 6000 字符
                let body = match text.splitn(3, "---").nth(2) {
                    Some(b) => b.trim(),
                    None => text.as_str(),
                };
                let body: String = body.chars().take(6000).collect();
                hits.push((name, body));
            }
        }
    }
    if hits.is_empty() {
        None
    } else {
        let mut s = String::from(
            "\n\n[系统提示] 本任务命中以下技能包，正文已附上（无需再 read SKILL.md，模板文件按需自行读取）：\n",
        );
        for (name, body) in hits {
            s.push_str(&format!("\n===== 技能包 {name} =====\n{body}\n"));
        }
        Some(s)
    }
}

/// 解析 SKILL.md 的 frontmatter，产出系统提示里的索引行。
pub fn index_lines(assets: &Path) -> String {
    let skills_dir = assets.join("skills");
    let mut lines: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&skills_dir) {
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let md = dir.join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&md) else {
                continue;
            };
            let mut name = String::new();
            let mut desc = String::new();
            for line in text.lines().take(12) {
                if let Some(v) = line.strip_prefix("name:") {
                    name = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("description:") {
                    desc = v.trim().to_string();
                }
            }
            if name.is_empty() {
                name = dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
            }
            lines.push(format!("- {}: {} => {}", name, desc, md.display()));
        }
    }
    if lines.is_empty() {
        "（未发现技能包，可运行 i-agent init-assets 释放内置技能包）".into()
    } else {
        lines.join("\n")
    }
}
