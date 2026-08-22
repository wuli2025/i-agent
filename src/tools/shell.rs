use super::{arg_str, cap};
use crate::config::Config;
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
fn probe(exe: &str, args: &[&str]) -> bool {
    Command::new(exe)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Which shell to hand the model's command to, on Windows.
///
/// The model writes POSIX — `python3 -c "…"`, `A && B`, `ls -la`, `grep`, heredocs —
/// because that is what it was trained on, and saying "系统: Windows" in the prompt
/// does not stop it. Handing that to PowerShell fails on most of those forms (5 of 7
/// in measurement, even on pwsh 7: `&&` parses only on 7, and `python3`/`ls -la`/
/// `rm -f`/`grep` fail on both). Almost every Windows dev box has a bash from Git or
/// WSL, so look for one first and only fall back to PowerShell if there is none.
#[cfg(windows)]
fn git_bash() -> Option<String> {
    // Git for Windows ships an MSYS bash that shares the Windows filesystem, PATH and
    // Python — exactly the environment the rest of the run assumes. Git can be installed
    // outside Program Files, so locate it from git.exe rather than guessing a fixed path:
    // git.exe lives in <root>\cmd or <root>\bin, and bash is at <root>\bin\bash.exe.
    let out = Command::new("where").arg("git.exe").output().ok()?;
    let paths = String::from_utf8_lossy(&out.stdout).to_string();
    for line in paths.lines() {
        let git = std::path::Path::new(line.trim());
        if let Some(root) = git.parent().and_then(|p| p.parent()) {
            let bash = root.join("bin").join("bash.exe");
            if bash.is_file() {
                return Some(bash.to_string_lossy().into_owned());
            }
        }
    }
    for p in [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ] {
        if std::path::Path::new(p).is_file() {
            return Some(p.to_string());
        }
    }
    None
}

#[cfg(windows)]
fn windows_shell() -> (String, Vec<String>) {
    use std::sync::OnceLock;
    static SHELL: OnceLock<(String, Vec<String>)> = OnceLock::new();
    SHELL
        .get_or_init(|| {
            // Git bash first: it runs the model's POSIX verbatim against the same
            // Windows Python and files everything else in this run uses.
            //
            // NOT C:\Windows\System32\bash.exe — that is the WSL launcher. It would
            // drop the command into a different operating system, where `python` is
            // Linux's and openpyxl/python-pptx may not even be installed. Silently
            // running the build script against the wrong interpreter is a worse bug
            // than the quoting errors this is meant to fix.
            if let Some(b) = git_bash() {
                if probe(&b, &["-c", "exit 0"]) {
                    return (b, vec!["-c".to_string()]);
                }
            }
            let ps = if probe("pwsh", &["-NoProfile", "-Command", "1"]) {
                "pwsh"
            } else {
                "powershell"
            };
            (
                ps.to_string(),
                [
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            )
        })
        .clone()
}

/// Name of the shell the model is actually talking to — the prompt states this so the
/// model writes the right dialect instead of guessing from the OS.
pub fn shell_name() -> &'static str {
    #[cfg(windows)]
    {
        let (exe, _) = windows_shell();
        if exe.contains("bash") {
            return "bash";
        }
        if exe.contains("pwsh") {
            return "pwsh";
        }
        return "powershell";
    }
    #[cfg(not(windows))]
    {
        "sh"
    }
}

fn is_filesystem_root(token: &str) -> bool {
    let token = token.trim_matches(|c: char| matches!(c, '\'' | '"' | ',' | ';'));
    let unix_root = token.chars().all(|c| c == '/') || matches!(token, "/." | "/./");
    let windows = token.replace('/', "\\");
    let unc_root = windows.starts_with("\\\\")
        && windows
            .trim_start_matches('\\')
            .trim_end_matches('\\')
            .split('\\')
            .filter(|part| !part.is_empty())
            .count()
            == 2;
    unix_root
        || token == r"\"
        || (token.len() == 3
            && token.as_bytes()[1] == b':'
            && matches!(token.as_bytes()[2], b'\\' | b'/'))
        || unc_root
}

/// 拦截模型最容易误触发的无界全盘扫描。它不仅拖住当前任务，还会遍历凭据、挂载盘和
/// 其它用户目录；需要找工具时应检查 PATH、工作目录或一个已知安装目录。
fn reject_unbounded_root_scan(cmd: &str) -> Result<(), String> {
    for segment in cmd.split([';', '\n', '|', '&']) {
        let words: Vec<_> = segment.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let executable = words[0]
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(words[0])
            .to_ascii_lowercase();
        let recursive = matches!(
            executable.as_str(),
            "find" | "rg" | "grep" | "du" | "fd" | "fdfind" | "tree"
        ) || (executable == "ls"
            && words.iter().any(|word| {
                *word == "--recursive"
                    || (word.starts_with('-') && !word.starts_with("--") && word.contains('R'))
            }))
            || (executable == "dir" && words.iter().any(|w| w.eq_ignore_ascii_case("/s")))
            || (matches!(executable.as_str(), "get-childitem" | "gci")
                && words.iter().any(|w| w.eq_ignore_ascii_case("-recurse")));
        if recursive && words.iter().skip(1).any(|word| is_filesystem_root(word)) {
            return Err(
                "拒绝无界扫描文件系统根目录；请改查工作目录、PATH 或明确的安装目录。".into(),
            );
        }
    }
    Ok(())
}

pub fn run(args: &Value, cfg: &Config) -> Result<String, String> {
    let cmd = arg_str(args, "cmd").ok_or("缺少 cmd")?;
    if cfg.stateless {
        reject_unbounded_root_scan(cmd)?;
    }
    let timeout = args
        .get("timeout_s")
        .and_then(|v| v.as_u64())
        .unwrap_or(120)
        .clamp(1, 600);
    let cwd = match arg_str(args, "cwd") {
        Some(c) => cfg.resolve(c),
        None => cfg.workspace.clone(),
    };

    #[cfg(windows)]
    let mut command = {
        let (exe, args) = windows_shell();
        let mut c = Command::new(exe);
        c.args(args);
        c.arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = Command::new("sh");
        c.args(["-c", cmd]);
        c
    };

    // Chinese output through a Windows console gets mangled unless Python is told to
    // emit UTF-8; the model then wastes turns "fixing" it with cmd.exe syntax that
    // PowerShell rejects. Set it up front so that whole failure mode never starts.
    command.env("PYTHONIOENCODING", "utf-8");
    command.env("PYTHONUTF8", "1");

    let mut child = command
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动命令失败: {e}"))?;

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let ho = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let he = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
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
            Err(e) => return Err(format!("等待命令失败: {e}")),
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
    match status {
        Some(st) => {
            let code = st.code().unwrap_or(-1);
            if code == 0 {
                Ok(if text.is_empty() {
                    "（命令成功，无输出）".into()
                } else {
                    text
                })
            } else {
                Ok(format!("退出码 {code}\n{text}"))
            }
        }
        None => Ok(format!("命令超时（{timeout}s）已终止\n{text}")),
    }
}

#[cfg(test)]
mod tests {
    use super::reject_unbounded_root_scan;

    #[test]
    fn rejects_unbounded_unix_and_windows_root_scans() {
        for command in [
            r#"find / -maxdepth 6 -name "polaris-forge""#,
            "rg --files /",
            "ls -R /",
            "tree /",
            "fd cloakbrowser /",
            r#"dir C:\ /s"#,
            r#"Get-ChildItem C:\ -Recurse"#,
            r#"Get-ChildItem \\server\share\ -Recurse"#,
        ] {
            assert!(
                reject_unbounded_root_scan(command).is_err(),
                "should reject {command}"
            );
        }
    }

    #[test]
    fn allows_scoped_searches_and_non_search_uses_of_slash() {
        for command in [
            "find . -maxdepth 3 -name polaris-forge",
            "rg --files /opt/polaris",
            "printf '/'",
        ] {
            assert!(
                reject_unbounded_root_scan(command).is_ok(),
                "should allow {command}"
            );
        }
    }
}
