use super::{arg_str, cap};
use crate::config::Config;
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run Python source directly, with no shell in between.
///
/// The single most common way a run died in measurement was the model trying to push a
/// generator script through the shell — `python3 -c "…"` with nested quotes, or a
/// heredoc — and losing to the quoting rules. It is the same failure whichever shell
/// wins: bash mangles the escapes, PowerShell rejects the heredoc outright.
///
/// So take the source as a plain argument, write it to a temp file, and execute it.
/// Nothing to escape, nothing to parse.
pub fn run(args: &Value, cfg: &Config) -> Result<String, String> {
    let code = arg_str(args, "code").ok_or("缺少 code")?;
    let timeout = args
        .get("timeout_s")
        .and_then(|v| v.as_u64())
        .unwrap_or(180)
        .clamp(1, 600);
    let cwd = match arg_str(args, "cwd") {
        Some(c) => cfg.resolve(c),
        None => cfg.workspace.clone(),
    };

    let dir = cwd.join(".i-agent");
    let _ = std::fs::create_dir_all(&dir);
    let src = dir.join("_run.py");
    std::fs::write(&src, code).map_err(|e| format!("写临时脚本失败: {e}"))?;

    // python3 on unix, python on Windows — try both rather than making the model guess.
    let exes: &[&str] = if cfg!(windows) {
        &["python", "python3", "py"]
    } else {
        &["python3", "python"]
    };

    let mut last_spawn_err = String::new();
    for exe in exes {
        let mut c = Command::new(exe);
        c.arg(&src)
            .current_dir(&cwd)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                last_spawn_err = format!("{exe}: {e}");
                continue;
            }
        };

        let mut so = child.stdout.take().unwrap();
        let mut se = child.stderr.take().unwrap();
        let ho = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = std::io::Read::read_to_end(&mut so, &mut b);
            b
        });
        let he = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = std::io::Read::read_to_end(&mut se, &mut b);
            b
        });

        let deadline = Instant::now() + Duration::from_secs(timeout);
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break Some(st),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
                Err(e) => return Err(format!("等待 python 失败: {e}")),
            }
        };

        let out = String::from_utf8_lossy(&ho.join().unwrap_or_default()).to_string();
        let err = String::from_utf8_lossy(&he.join().unwrap_or_default()).to_string();
        let mut text = out;
        if !err.trim().is_empty() {
            text.push_str("\n[stderr]\n");
            text.push_str(&err);
        }
        let text = cap(text.trim().to_string(), 12000);

        return Ok(match status {
            None => format!("python 超时（{timeout}s）已终止\n{text}"),
            Some(st) => match st.code().unwrap_or(-1) {
                0 if text.is_empty() => "（python 执行成功，无输出）".into(),
                0 => text,
                code => format!("退出码 {code}\n{text}"),
            },
        });
    }

    let _ = std::io::stderr().flush();
    Err(format!("找不到可用的 python 解释器（{last_spawn_err}）"))
}
