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

pub fn run(args: &Value, cfg: &Config) -> Result<String, String> {
    let cmd = arg_str(args, "cmd").ok_or("缺少 cmd")?;
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
