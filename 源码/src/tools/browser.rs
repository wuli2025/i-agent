use super::arg_str;
use crate::config::Config;
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// node 可执行文件：env > PATH 上的常见名字
fn node_bin() -> Option<&'static str> {
    static NODE: OnceLock<Option<String>> = OnceLock::new();
    NODE.get_or_init(|| {
        if let Ok(n) = std::env::var("I_AGENT_NODE") {
            if !n.trim().is_empty() {
                return Some(n);
            }
        }
        for cand in ["node", "nodejs", "node.exe"] {
            let ok = Command::new(cand)
                .arg("-v")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Some(cand.to_string());
            }
        }
        None
    })
    .as_deref()
}

/// playwright 的 node_modules 候选根（与 smoke.mjs 的 loadPlaywright 保持一致），
/// 供 O5 依赖自检探测用。
fn playwright_candidates() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Ok(n) = std::env::var("I_AGENT_PLAYWRIGHT") {
        if !n.trim().is_empty() {
            v.push(n);
        }
    }
    v.push("D:/Users/mi/npm/node_modules".to_string());
    if let Ok(a) = std::env::var("APPDATA") {
        v.push(format!("{a}/npm/node_modules"));
    }
    if let Ok(u) = std::env::var("USERPROFILE") {
        v.push(format!("{u}/AppData/Roaming/npm/node_modules"));
    }
    if let Some(h) = crate::config::home_dir().to_str() {
        v.push(format!("{h}/.npm-global/lib/node_modules"));
        v.push(format!("{h}/.i-agent/node_modules"));
        v.push(format!("{h}/.i-agent/assets/node_modules"));
    }
    v.push("/mnt/d/Users/mi/npm/node_modules".to_string());
    v.push("/usr/lib/node_modules".to_string());
    v.push("/usr/local/lib/node_modules".to_string());
    v
}

/// 跑冒烟脚本，拿回它打印的那行 JSON
fn run_smoke(cfg: &Config, args: Vec<String>, timeout_s: u64) -> Result<Value, String> {
    let node = node_bin().ok_or(
        "未找到 node，无法做浏览器冒烟。请安装 Node.js（或设 I_AGENT_NODE 指向可执行文件）。",
    )?;
    let script = cfg.assets_dir.join("tools").join("smoke.mjs");
    if !script.exists() {
        return Err(format!(
            "冒烟脚本缺失: {}（运行 i-agent init-assets 重新释放资产）",
            script.display()
        ));
    }

    // O5 依赖自检：playwright 缺了不该让模型盲试装包。
    // 在 spawn 之前先探测常见位置，缺则把「怎么修」直接给出来。
    if std::env::var("I_AGENT_PLAYWRIGHT").is_err() {
        let cands = playwright_candidates();
        if !cands.iter().any(|c| std::path::Path::new(c).join("playwright").exists()) {
            let npm_root = Command::new(&node)
                .args(["root", "-g"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty());
            let hint = match npm_root {
                Some(root) => format!(
                    "检测到本机 npm 全局根 {root}，但里面没有 playwright。\n\
                     修复（任选其一，装好后重试即可，不必重写脚本）：\n\
                     ① 设 I_AGENT_PLAYWRIGHT={root} 后先在该目录 npm i playwright；\n\
                     ② 在 {} 下执行 npm i playwright；\n\
                     ③ npm i -g playwright && npx playwright install chromium",
                    cfg.assets_dir.display()
                ),
                None => "修复：npm i -g playwright && npx playwright install chromium，\
                         或设 I_AGENT_PLAYWRIGHT 指向含 playwright 的 node_modules 目录"
                    .to_string(),
            };
            eprintln!("\x1b[2m[i-agent] 浏览器冒烟依赖 playwright 未就绪：{hint}\x1b[0m");
        }
    }

    let mut child = Command::new(node)
        .arg(&script)
        .args(&args)
        .current_dir(&cfg.workspace)
        .stdin(Stdio::null()) // 无头场景必须关 stdin，否则子进程可能阻塞读输入
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 node 失败: {e}"))?;

    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let ho = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let he = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });

    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
                }
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(e) => return Err(format!("等待 node 失败: {e}")),
        }
    };

    let out = String::from_utf8_lossy(&ho.join().unwrap_or_default()).to_string();
    let err = String::from_utf8_lossy(&he.join().unwrap_or_default()).to_string();
    if timed_out {
        return Err(format!("浏览器冒烟超时（{timeout_s}s）。页面可能死循环或卡在加载。"));
    }
    // smoke.mjs 只在最后打印一行 JSON；容错地取最后一个 { 开头的行
    let line = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| {
            let e: String = err.chars().take(600).collect();
            format!("冒烟脚本没有返回结果。stderr:\n{e}")
        })?;
    serde_json::from_str(line).map_err(|e| format!("冒烟结果不是合法 JSON: {e}"))
}

/// 把冒烟结果渲染成模型看得懂、能照着改的报告
fn render(v: &Value, path: &str) -> String {
    render_mode(v, path, false)
}

/// url 操纵模式下，「未通过/外链依赖/修复提示」这类面向本地交付物的话术是误导——
/// 线上页面本来就有外链，也不存在「修好再交付」。只保留客观探测结果。
fn render_mode(v: &Value, path: &str, url_mode: bool) -> String {
    if let Some(fatal) = v.get("fatal").and_then(|f| f.as_str()) {
        let hint = v.get("hint").and_then(|h| h.as_str()).unwrap_or("");
        return format!("浏览器冒烟无法执行: {fatal}\n{hint}");
    }
    let ok = v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
    let g = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let mut s = String::new();

    if ok {
        s.push_str(&format!("浏览器冒烟通过: {path}\n"));
    } else {
        s.push_str(&format!("浏览器冒烟未通过: {path}\n"));
        if let Some(rs) = v.get("reasons").and_then(|r| r.as_array()) {
            for r in rs {
                if let Some(t) = r.as_str() {
                    s.push_str(&format!("- {t}\n"));
                }
            }
        }
    }

    s.push_str(&format!(
        "渲染: 正文 {} 字 / DOM {} 节点 / canvas {} / svg {}（真画出内容的 {}）/ img {}\n",
        g("bodyText"),
        g("domNodes"),
        g("canvas"),
        g("svg"),
        g("svgDrawn"),
        g("images")
    ));

    let audio = v.get("audioContext").and_then(|a| a.as_bool()).unwrap_or(false);
    s.push_str(&format!(
        "运行时: AudioContext {} / requestAnimationFrame 调用 {} 次\n",
        if audio { "已实例化" } else { "未使用" },
        g("rafCalls")
    ));

    if let Some(it) = v.get("interaction") {
        let c = it.get("clicked").and_then(|x| x.as_i64()).unwrap_or(0);
        let d = it.get("domChanged").and_then(|x| x.as_bool()).unwrap_or(false);
        let e = it.get("errorsAfterClick").and_then(|x| x.as_i64()).unwrap_or(0);
        s.push_str(&format!(
            "交互: 点击 {c} 次，界面{}，点击后新增报错 {e} 处\n",
            if d { "有响应" } else { "无变化（可能没接上事件，或选项点了没反应）" }
        ));
    }

    if let Some(ext) = v.get("externalRequests").and_then(|e| e.as_array()) {
        if !ext.is_empty() && !url_mode {
            s.push_str(&format!(
                "外链依赖 {} 个（零依赖单文件交付要求为 0，必须内联）:\n",
                ext.len()
            ));
            for u in ext.iter().take(5) {
                if let Some(t) = u.as_str() {
                    s.push_str(&format!("- {t}\n"));
                }
            }
        }
    }

    if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
        if !errs.is_empty() {
            s.push_str("控制台/运行时报错:\n");
            for e in errs.iter().take(8) {
                if let Some(t) = e.as_str() {
                    s.push_str(&format!("- {t}\n"));
                }
            }
        }
    }

    if let Some(sh) = v.get("screenshot").and_then(|s| s.as_str()) {
        s.push_str(&format!("截图: {sh}\n"));
    }

    if !ok && !url_mode {
        s.push_str(
            "\n请定位并修复上述问题后重新 browser 验证；未通过不得交付。\n\
             · 白屏几乎总是脚本初始化时抛异常（模板/数据拼接处缺分号、多余逗号、\
             引用了未定义的变量、JSON 里有未转义引号）——先看第一条运行时异常。\n\
             · 「内容被裁剪」是最阴险的一类：不报错、不白屏，但整块主内容看不见。\
             十有八九是子元素用了 position:absolute（撑不开父容器高度），父容器又 overflow:hidden，\
             于是塌成几 px 把内容全裁掉了 —— 给父容器显式设一个能容下内容的高度（如按行数算出的 height）。\n\
             · 空的 <svg> 标签等于图表没画出来：标签在不代表图在，检查绘图代码到底跑没跑。",
        );
    }
    s
}

/// browser 工具：在真 Chromium 里执行页面。
/// - `path`：对本地 HTML 产物做交付前验收（白屏/异常/可交互）。
/// - `url`：打开任意网址，把渲染后的正文与 DOM 摘要抓回来——这是「操纵浏览器」的入口：
///   JS 异步注入的数据（直接 GET 拿不到的）也要走这条路取，而不是凭源码猜或编。
pub fn run(args: &Value, cfg: &Config) -> Result<String, String> {
    let clicks = args.get("clicks").and_then(|v| v.as_u64()).unwrap_or(3).min(10);
    let wait = args.get("wait_ms").and_then(|v| v.as_u64()).unwrap_or(1200).clamp(200, 15000);

    // URL 模式：导航到线上页面，抓渲染后的 DOM/正文，并返回页面里的表格数据。
    if let Some(url) = arg_str(args, "url") {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("url 必须以 http(s):// 开头".into());
        }
        let mut a: Vec<String> = vec![
            "--url".into(), url.to_string(),
            "--clicks".into(), clicks.to_string(),
            "--wait".into(), wait.to_string(),
            "--dump".into(), "1".into(),
        ];
        // --out：把渲染后的表格/正文直接落盘，模型不必再自写 playwright 核对
        let out_rel = arg_str(args, "out");
        if let Some(o) = out_rel {
            let full = cfg.resolve(o);
            a.push("--out".into());
            a.push(full.to_string_lossy().to_string());
        }
        let v = run_smoke(cfg, a, 120)?;
        let mut s = render_mode(&v, url, true);
        if let Some(oi) = v.get("outInfo") {
            if let Some(err) = oi.get("error").and_then(|e| e.as_str()) {
                s.push_str(&format!("\n落盘失败: {err}\n"));
            } else if let Some(p) = oi.get("path").and_then(|p| p.as_str()) {
                let kind = oi.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                let rows = oi.get("rows").and_then(|r| r.as_i64()).unwrap_or(0);
                s.push_str(&format!(
                    "\n已把渲染后的{kind}数据落盘到 {p}（{rows} 行）。\
                     这是从渲染后 DOM 直接导出的真实数据，可直接引用，无需再用 fetch/curl/playwright 二次核对。\n"
                ));
            }
        }
        if out_rel.is_none() {
            if let Some(d) = v.get("domText").and_then(|d| d.as_str()) {
                s.push_str(&format!("\n【渲染后正文（前 6000 字）】\n{d}\n"));
            }
            if let Some(t) = v.get("tables").and_then(|t| t.as_array()) {
                s.push_str(&format!("\n【页面内 <table>（共 {} 张，渲染后的实际内容）】\n", t.len()));
                for (i, tab) in t.iter().take(5).enumerate() {
                    s.push_str(&format!("── 表 {} ──\n{}\n", i + 1,
                        tab.as_str().unwrap_or_default()));
                }
            }
        }
        return Ok(s);
    }

    let path = arg_str(args, "path").ok_or("缺少 path（本地 HTML）或 url（线上页面）")?;
    let full = cfg.resolve(path);
    if !full.exists() {
        return Err(format!("文件不存在: {path}"));
    }

    let mut a: Vec<String> = vec![
        "--file".into(),
        full.to_string_lossy().to_string(),
        "--clicks".into(),
        clicks.to_string(),
        "--wait".into(),
        wait.to_string(),
    ];

    // 截图默认开：放到 .i-agent/shots 下，不污染交付目录
    let shot = args.get("screenshot").and_then(|v| v.as_bool()).unwrap_or(true);
    if shot {
        let name = full
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "page".into());
        let dir = cfg.workspace.join(".i-agent").join("shots");
        let _ = std::fs::create_dir_all(&dir);
        a.push("--shot".into());
        a.push(dir.join(format!("{name}.png")).to_string_lossy().to_string());
    }

    let v = run_smoke(cfg, a, 90)?;
    Ok(render(&v, path))
}

/// 供 check 工具复用：只返回是否通过 + 报告
pub fn smoke(cfg: &Config, path: &str) -> Result<(bool, String), String> {
    let full = cfg.resolve(path);
    let a: Vec<String> = vec![
        "--file".into(),
        full.to_string_lossy().to_string(),
        "--clicks".into(),
        "3".into(),
        "--wait".into(),
        "1200".into(),
    ];
    let v = run_smoke(cfg, a, 90)?;
    let ok = v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
    Ok((ok, render(&v, path)))
}
