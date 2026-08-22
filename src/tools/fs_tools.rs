use super::{arg_str, cap};
use crate::config::Config;
use serde_json::Value;
use std::path::Path;

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(1024).any(|b| *b == 0)
}

fn reject_root_walk(root: &Path) -> Result<(), String> {
    if root.has_root() && root.parent().is_none() {
        return Err("拒绝从文件系统根目录递归搜索；请改查工作目录或一个明确的安装目录。".into());
    }
    Ok(())
}

pub fn read(args: &Value, cfg: &Config) -> Result<String, String> {
    let path = arg_str(args, "path").ok_or("缺少 path")?;
    let p = cfg.resolve(path);
    let bytes = std::fs::read(&p).map_err(|e| format!("读取 {path} 失败: {e}"))?;
    if is_binary(&bytes) {
        return Ok(format!(
            "（二进制文件，{} 字节，类型 {}）",
            bytes.len(),
            p.extension().and_then(|e| e.to_str()).unwrap_or("未知")
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(600)
        .clamp(1, 2000) as usize;
    let total = text.lines().count();
    let mut out = String::new();
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let line: String = line.chars().take(500).collect();
        out.push_str(&format!("{:>5}| {}\n", i + 1, line));
    }
    if offset - 1 + limit < total {
        out.push_str(&format!(
            "…（共 {total} 行，本次到第 {} 行，续读用 offset={}）",
            offset - 1 + limit,
            offset + limit
        ));
    }
    if out.is_empty() {
        out = format!("（空文件或 offset 超界，共 {total} 行）");
    }
    Ok(cap(out, 48000))
}

pub fn write(args: &Value, cfg: &Config) -> Result<String, String> {
    let path = arg_str(args, "path").ok_or("缺少 path")?;
    let content = arg_str(args, "content").ok_or("缺少 content")?;
    let append = args
        .get("append")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let p = cfg.resolve(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if append {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .map_err(|e| format!("打开 {path} 失败: {e}"))?;
        f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
        let total = f.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(format!("已追加到 {path}（现共 {total} 字节）"))
    } else {
        std::fs::write(&p, content).map_err(|e| format!("写入 {path} 失败: {e}"))?;
        Ok(format!("已写入 {path}（{} 字节）", content.len()))
    }
}

pub fn edit(args: &Value, cfg: &Config) -> Result<String, String> {
    let path = arg_str(args, "path").ok_or("缺少 path")?;
    let old = arg_str(args, "old").ok_or("缺少 old")?;
    let new = arg_str(args, "new").ok_or("缺少 new")?;
    let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    if old.is_empty() {
        return Err("old 不能为空".into());
    }
    let p = cfg.resolve(path);
    let text = std::fs::read_to_string(&p).map_err(|e| format!("读取 {path} 失败: {e}"))?;
    let count = text.matches(old).count();
    if count == 0 {
        return Err(
            "未找到匹配文本。请先 read 该文件确认原文（注意空格与换行须逐字一致）。".into(),
        );
    }
    if count > 1 && !all {
        return Err(format!(
            "匹配了 {count} 处。请提供更长的唯一片段，或设 all=true 全部替换。"
        ));
    }
    let replaced = if all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    std::fs::write(&p, &replaced).map_err(|e| e.to_string())?;
    Ok(format!(
        "已替换 {path} 中 {} 处",
        if all { count } else { 1 }
    ))
}

pub fn ls(args: &Value, cfg: &Config) -> Result<String, String> {
    let path = arg_str(args, "path").unwrap_or(".");
    let p = cfg.resolve(path);
    let rd = std::fs::read_dir(&p).map_err(|e| format!("读目录 {path} 失败: {e}"))?;
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<(String, u64)> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        match e.metadata() {
            Ok(md) if md.is_dir() => dirs.push(name),
            Ok(md) => files.push((name, md.len())),
            Err(_) => files.push((name, 0)),
        }
    }
    dirs.sort();
    files.sort();
    let mut out = String::new();
    for d in dirs.iter().take(120) {
        out.push_str(&format!("{d}/\n"));
    }
    for (f, sz) in files.iter().take(200) {
        out.push_str(&format!("{f}  {sz}\n"));
    }
    if out.is_empty() {
        out = "（空目录）".into();
    }
    Ok(cap(out, 8000))
}

pub fn glob(args: &Value, cfg: &Config) -> Result<String, String> {
    let pattern = arg_str(args, "pattern").ok_or("缺少 pattern")?;
    let root = cfg.resolve(arg_str(args, "path").unwrap_or("."));
    if cfg.stateless {
        reject_root_walk(&root)?;
    }
    let matcher = globset::GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .map_err(|e| format!("通配模式无效: {e}"))?
        .compile_matcher();
    let mut hits: Vec<(std::time::SystemTime, String)> = Vec::new();
    for entry in ignore::WalkBuilder::new(&root)
        .hidden(false)
        .build()
        .flatten()
    {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let rel = p.strip_prefix(&root).unwrap_or(p);
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        if matcher.is_match(&rel_s)
            || matcher.is_match(p.file_name().unwrap_or_default().to_string_lossy().as_ref())
        {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            hits.push((mtime, rel_s));
            if hits.len() >= 500 {
                break;
            }
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    if hits.is_empty() {
        return Ok("无匹配文件".into());
    }
    let out: Vec<String> = hits.into_iter().take(200).map(|(_, p)| p).collect();
    Ok(cap(out.join("\n"), 8000))
}

pub fn grep(args: &Value, cfg: &Config) -> Result<String, String> {
    let pattern = arg_str(args, "pattern").ok_or("缺少 pattern")?;
    let re = regex_lite::Regex::new(pattern).map_err(|e| format!("正则无效: {e}"))?;
    let root = cfg.resolve(arg_str(args, "path").unwrap_or("."));
    if cfg.stateless {
        reject_root_walk(&root)?;
    }
    let name_filter = match arg_str(args, "glob") {
        Some(g) => Some(
            globset::GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| format!("glob 无效: {e}"))?
                .compile_matcher(),
        ),
        None => None,
    };

    let mut out = String::new();
    let mut count = 0usize;
    let single_file = root.is_file();
    let mut scan = |p: &Path| -> bool {
        if let Some(nf) = &name_filter {
            let fname = p.file_name().unwrap_or_default().to_string_lossy();
            if !nf.is_match(fname.as_ref()) {
                return true;
            }
        }
        let Ok(bytes) = std::fs::read(p) else {
            return true;
        };
        if bytes.len() > 2_000_000 || is_binary(&bytes) {
            return true;
        }
        let text = String::from_utf8_lossy(&bytes);
        let rel = p
            .strip_prefix(&root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        let label = if rel.is_empty() {
            p.to_string_lossy().to_string()
        } else {
            rel
        };
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                let line: String = line.trim().chars().take(240).collect();
                out.push_str(&format!("{label}:{}: {line}\n", i + 1));
                count += 1;
                if count >= 150 {
                    return false;
                }
            }
        }
        true
    };

    if single_file {
        scan(&root);
    } else {
        for entry in ignore::WalkBuilder::new(&root)
            .hidden(false)
            .build()
            .flatten()
        {
            let p = entry.path();
            if p.is_file() && !scan(p) {
                break;
            }
        }
    }
    if out.is_empty() {
        return Ok("无匹配".into());
    }
    if count >= 150 {
        out.push_str("…[结果过多已截断，请缩小范围]");
    }
    Ok(cap(out, 10000))
}

#[cfg(test)]
mod tests {
    use super::reject_root_walk;
    use std::path::Path;

    #[test]
    fn root_walk_guard_rejects_root_but_allows_scoped_directories() {
        assert!(reject_root_walk(Path::new(std::path::MAIN_SEPARATOR_STR)).is_err());
        assert!(reject_root_walk(Path::new(".")).is_ok());
        assert!(reject_root_walk(Path::new("/opt/polaris")).is_ok());
    }
}
