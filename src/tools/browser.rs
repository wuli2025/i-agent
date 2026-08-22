use super::arg_str;
use crate::config::Config;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

static CLOAK_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SmokeBackend {
    CloakBrowser,
    LegacyPlaywright,
}

fn smoke_backend(stateless: bool) -> SmokeBackend {
    if stateless {
        SmokeBackend::CloakBrowser
    } else {
        SmokeBackend::LegacyPlaywright
    }
}

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

/// npm 的全局 node_modules 根；失败时静默交给其他候选路径。
fn npm_global_root() -> Option<String> {
    for cand in ["npm", "npm.cmd", "npm.exe"] {
        let Ok(out) = Command::new(cand)
            .args(["root", "-g"])
            .stdin(Stdio::null())
            .output()
        else {
            continue;
        };
        if out.status.success() {
            let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !root.is_empty() {
                return Some(root);
            }
        }
    }
    None
}

/// playwright 的 node_modules 候选根（与 smoke.mjs 的 loadPlaywright 保持一致），
/// 供 O5 依赖自检探测用。
fn playwright_candidates(cfg: &Config) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Ok(n) = std::env::var("I_AGENT_PLAYWRIGHT") {
        if !n.trim().is_empty() {
            v.push(n);
        }
    }
    if let Some(root) = npm_global_root() {
        v.push(root);
    }
    if let Ok(a) = std::env::var("APPDATA") {
        v.push(format!("{a}/npm/node_modules"));
    }
    if let Ok(prefix) = std::env::var("NPM_CONFIG_PREFIX") {
        v.push(format!("{prefix}/node_modules"));
        v.push(format!("{prefix}/lib/node_modules"));
    }
    if let Some(h) = crate::config::home_dir().to_str() {
        v.push(format!("{h}/.npm-global/lib/node_modules"));
        v.push(format!("{h}/.i-agent/node_modules"));
    }
    v.push(
        cfg.workspace
            .join("node_modules")
            .to_string_lossy()
            .into_owned(),
    );
    v.push(
        cfg.assets_dir
            .join("node_modules")
            .to_string_lossy()
            .into_owned(),
    );
    v.push("/usr/lib/node_modules".to_string());
    v.push("/usr/local/lib/node_modules".to_string());
    v.sort();
    v.dedup();
    v
}

/// 跑冒烟脚本，拿回它打印的那行 JSON
fn run_legacy_smoke(cfg: &Config, args: Vec<String>, timeout_s: u64) -> Result<Value, String> {
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
        let cands = playwright_candidates(cfg);
        if !cands
            .iter()
            .any(|c| std::path::Path::new(c).join("playwright").exists())
        {
            let hint = match npm_global_root() {
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
        return Err(format!(
            "浏览器冒烟超时（{timeout_s}s）。页面可能死循环或卡在加载。"
        ));
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

struct CloakTempSource(Option<PathBuf>);

impl Drop for CloakTempSource {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

// Polaris fast mode must not silently fall back to a separately installed Node
// Playwright. CloakBrowser owns both the browser build and its launch hardening, so
// run the acceptance probe through that exact route. The script deliberately imports
// only cloakbrowser; a missing package becomes a structured fatal result.
const CLOAK_SMOKE_PY: &str = r##"
import argparse, csv, json, pathlib, sys

def emit(value):
    print(json.dumps(value, ensure_ascii=False))

p = argparse.ArgumentParser(add_help=False)
p.add_argument('--file')
p.add_argument('--url')
p.add_argument('--clicks', type=int, default=3)
p.add_argument('--wait', type=int, default=1200)
p.add_argument('--shot')
p.add_argument('--out')
a = p.parse_args()

try:
    from cloakbrowser import launch
except Exception as exc:
    emit({'ok': False, 'fatal': 'CloakBrowser 不可用: ' + str(exc).splitlines()[0],
          'hint': '安装 cloakbrowser 后重试；Polaris 模式不会回退到裸 Playwright。'})
    raise SystemExit(0)

target = a.url
if not target and a.file:
    path = pathlib.Path(a.file).resolve()
    if not path.exists():
        emit({'ok': False, 'fatal': '文件不存在: ' + str(path)})
        raise SystemExit(0)
    target = path.as_uri()
if not target:
    emit({'ok': False, 'fatal': '缺少 --file 或 --url'})
    raise SystemExit(0)

browser = None
errors, external = [], []
try:
    browser = launch(humanize=True)
    page = browser.new_page(viewport={'width': 1280, 'height': 800})

    def on_console(msg):
        try:
            if msg.type == 'error' and len(errors) < 20:
                errors.append('console.error: ' + msg.text[:200])
        except Exception:
            pass

    def on_page_error(exc):
        if len(errors) < 20:
            errors.append('运行时异常: ' + str(exc).splitlines()[0][:240])

    def on_request(req):
        try:
            url = req.url
            if url.lower().startswith(('http://', 'https://')) and len(external) < 10:
                external.append(url[:160])
        except Exception:
            pass

    def on_request_failed(req):
        try:
            url = req.url
            if not url.startswith(('data:', 'blob:')) and len(errors) < 20:
                errors.append('资源加载失败: ' + url[:140])
        except Exception:
            pass

    page.on('console', on_console)
    page.on('pageerror', on_page_error)
    page.on('request', on_request)
    page.on('requestfailed', on_request_failed)
    page.add_init_script("""
      window.__audio = false; window.__raf = 0;
      const AC = window.AudioContext || window.webkitAudioContext;
      if (AC) { const P = new Proxy(AC, {construct(t,a){window.__audio=true;return new t(...a)}});
        window.AudioContext=P; window.webkitAudioContext=P; }
      const r = window.requestAnimationFrame;
      window.requestAnimationFrame = function(cb){window.__raf++;return r.call(window,cb)};
    """)

    loaded = True
    try:
        page.goto(target, wait_until='load', timeout=20000)
    except Exception as exc:
        loaded = False
        errors.append('页面加载失败: ' + str(exc).splitlines()[0][:240])
    page.wait_for_timeout(max(200, min(a.wait, 15000)))

    snap = page.evaluate(r"""() => {
      const clipped=[];
      for (const e of document.querySelectorAll('body *')) {
        const cs=getComputedStyle(e), box=e.getBoundingClientRect();
        if ((cs.overflow==='visible' && cs.overflowY==='visible') || box.width<100 || box.height===0 || box.height>=60) continue;
        let bottom=0; for (const c of e.children) bottom=Math.max(bottom,c.getBoundingClientRect().bottom-box.top);
        if (bottom-box.height>120) clipped.push({sel:String(e.className||e.tagName).slice(0,30),h:Math.round(box.height),contentH:Math.round(bottom)});
      }
      const tables=[...document.querySelectorAll('table')].map(t =>
        [...t.rows].map(r => [...r.cells].map(c => (c.innerText||'').trim()).join('\t')).join('\n'));
      return {
        bodyText:(document.body?.innerText||'').trim().length,
        domNodes:document.querySelectorAll('body *').length,
        canvas:document.querySelectorAll('canvas').length,
        svg:document.querySelectorAll('svg').length,
        svgDrawn:[...document.querySelectorAll('svg')].filter(s => s.querySelectorAll('rect,path,circle,line,polyline,polygon,text').length>=2).length,
        images:document.querySelectorAll('img').length,
        audioContext:!!window.__audio, rafCalls:window.__raf||0,
        clipped:clipped.slice(0,5), domText:(document.body?.innerText||'').trim().slice(0,6000),
        tables, html:(document.body?.innerHTML||'').length
      };
    }""")

    interaction = {'clicked': 0, 'domChanged': False, 'errorsAfterClick': 0}
    before_errors = len(errors)
    previous = page.locator('body').inner_html()
    selector = ('button,[onclick],[role="button"],[role="tab"],a[href="#"],select,'
                '.choice,.option,.btn,.start,li[data-goto],[class*="tab"],'
                '[class*="range"],[class*="filter"],[class*="toggle"],th[data-sort]')
    loc = page.locator(selector)
    count = min(loc.count(), max(0, min(a.clicks, 10)))
    for i in range(count):
        el = loc.nth(i)
        try:
            if not el.is_visible():
                continue
            el.click(timeout=3000)
            interaction['clicked'] += 1
            page.wait_for_timeout(120)
            now = page.locator('body').inner_html()
            interaction['domChanged'] = interaction['domChanged'] or now != previous
            previous = now
        except Exception:
            continue
    interaction['errorsAfterClick'] = max(0, len(errors) - before_errors)

    reasons = []
    if not loaded: reasons.append('页面没有成功加载')
    if snap['bodyText'] < 20 or snap['domNodes'] < 1: reasons.append('页面正文过少，疑似白屏')
    if snap['clipped']: reasons.append('检测到内容被容器裁剪')
    if errors: reasons.append('存在控制台、运行时或资源加载错误')
    if a.file and external: reasons.append('本地单文件存在外链依赖')

    result = dict(snap)
    result.update({'ok': not reasons, 'reasons': reasons, 'errors': errors,
                   'externalRequests': external, 'interaction': interaction, 'url': page.url})
    if a.shot:
        pathlib.Path(a.shot).parent.mkdir(parents=True, exist_ok=True)
        page.screenshot(path=a.shot, full_page=True)
        result['screenshot'] = a.shot
    if a.out:
        out = pathlib.Path(a.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        try:
            if out.suffix.lower() == '.json':
                out.write_text(json.dumps({'url': page.url, 'text': snap['domText'], 'tables': snap['tables']}, ensure_ascii=False, indent=2), encoding='utf-8')
                kind, rows = 'JSON', len(snap['tables'])
            elif out.suffix.lower() == '.csv':
                out.write_text((snap['tables'][0] if snap['tables'] else ''), encoding='utf-8')
                kind, rows = 'CSV', (snap['tables'][0].count('\n') + 1 if snap['tables'] else 0)
            else:
                out.write_text(snap['domText'], encoding='utf-8')
                kind, rows = '正文', len(snap['domText'].splitlines())
            result['outInfo'] = {'path': str(out), 'kind': kind, 'rows': rows}
        except Exception as exc:
            result['outInfo'] = {'error': str(exc)}
    emit(result)
except Exception as exc:
    emit({'ok': False, 'fatal': 'CloakBrowser 冒烟失败: ' + str(exc).splitlines()[0]})
finally:
    if browser is not None:
        try: browser.close()
        except Exception: pass
"##;

fn run_cloak_smoke(cfg: &Config, args: &[String], timeout_s: u64) -> Result<Value, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let source = std::env::temp_dir().join(format!(
        "i-agent-cloak-smoke-{}-{nonce}-{}.py",
        std::process::id(),
        CLOAK_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut source_file = options
        .open(&source)
        .map_err(|e| format!("安全创建 CloakBrowser 冒烟脚本失败: {e}"))?;
    source_file
        .write_all(CLOAK_SMOKE_PY.as_bytes())
        .map_err(|e| format!("写 CloakBrowser 冒烟脚本失败: {e}"))?;
    drop(source_file);
    let _source_guard = CloakTempSource(Some(source.clone()));
    let exes: &[&str] = if cfg!(windows) {
        &["python", "python3", "py"]
    } else {
        &["python3", "python"]
    };
    let mut last_spawn_err = String::new();

    for exe in exes {
        let mut command = Command::new(exe);
        command
            .arg(&source)
            .args(args)
            .current_dir(&cfg.workspace)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                last_spawn_err = format!("{exe}: {e}");
                continue;
            }
        };

        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let stdout_thread = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            bytes
        });
        let stderr_thread = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(timeout_s);
        let timed_out = loop {
            match child.try_wait() {
                Ok(Some(_)) => break false,
                Ok(None) if Instant::now() >= deadline => {
                    #[cfg(unix)]
                    unsafe {
                        libc::killpg(child.id() as i32, libc::SIGKILL);
                    }
                    #[cfg(windows)]
                    {
                        let _ = Command::new("taskkill")
                            .args(["/PID", &child.id().to_string(), "/T", "/F"])
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(80)),
                Err(e) => return Err(format!("等待 CloakBrowser 冒烟失败: {e}")),
            }
        };
        let out = String::from_utf8_lossy(&stdout_thread.join().unwrap_or_default()).to_string();
        let err = String::from_utf8_lossy(&stderr_thread.join().unwrap_or_default()).to_string();
        if timed_out {
            return Err(format!("CloakBrowser 冒烟超时（{timeout_s}s）"));
        }
        let Some(line) = out
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
        else {
            let detail: String = err.chars().take(600).collect();
            last_spawn_err = format!("{exe} 没有返回协议 JSON。stderr: {detail}");
            continue;
        };
        return serde_json::from_str(line)
            .map_err(|e| format!("CloakBrowser 冒烟结果不是合法 JSON: {e}"));
    }

    Err(format!(
        "找不到可用的 python 解释器，无法启动 CloakBrowser（{last_spawn_err}）"
    ))
}

fn run_smoke(cfg: &Config, args: Vec<String>, timeout_s: u64) -> Result<Value, String> {
    match smoke_backend(cfg.stateless) {
        SmokeBackend::CloakBrowser => run_cloak_smoke(cfg, &args, timeout_s),
        SmokeBackend::LegacyPlaywright => run_legacy_smoke(cfg, args, timeout_s),
    }
}

fn requested_clicks(args: &Value, url_mode: bool) -> u64 {
    args.get("clicks")
        .and_then(|value| value.as_u64())
        .unwrap_or(if url_mode { 0 } else { 3 })
        .min(10)
}

fn smoke_succeeded(value: &Value) -> bool {
    value.get("ok").and_then(|item| item.as_bool()) == Some(true)
        && value.get("fatal").and_then(|item| item.as_str()).is_none()
}

fn url_smoke_args(url: &str, clicks: u64, wait: u64, out: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--url".into(),
        url.into(),
        "--clicks".into(),
        clicks.to_string(),
        "--wait".into(),
        wait.to_string(),
    ];
    if let Some(path) = out {
        args.push("--out".into());
        args.push(path.into());
    }
    args
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

    let audio = v
        .get("audioContext")
        .and_then(|a| a.as_bool())
        .unwrap_or(false);
    s.push_str(&format!(
        "运行时: AudioContext {} / requestAnimationFrame 调用 {} 次\n",
        if audio { "已实例化" } else { "未使用" },
        g("rafCalls")
    ));

    if let Some(it) = v.get("interaction") {
        let c = it.get("clicked").and_then(|x| x.as_i64()).unwrap_or(0);
        let d = it
            .get("domChanged")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let e = it
            .get("errorsAfterClick")
            .and_then(|x| x.as_i64())
            .unwrap_or(0);
        s.push_str(&format!(
            "交互: 点击 {c} 次，界面{}，点击后新增报错 {e} 处\n",
            if d {
                "有响应"
            } else {
                "无变化（可能没接上事件，或选项点了没反应）"
            }
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
    let wait = args
        .get("wait_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(1200)
        .clamp(200, 15000);

    // URL 模式：导航到线上页面，抓渲染后的 DOM/正文，并返回页面里的表格数据。
    if let Some(url) = arg_str(args, "url") {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("url 必须以 http(s):// 开头".into());
        }
        // --out：把渲染后的表格/正文直接落盘，模型不必再自写 playwright 核对
        let out_rel = arg_str(args, "out");
        let resolved_out = out_rel.map(|path| cfg.resolve(path));
        let out_string = resolved_out
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let a = url_smoke_args(
            url,
            requested_clicks(args, true),
            wait,
            out_string.as_deref(),
        );
        let v = run_smoke(cfg, a, 120)?;
        let mut s = render_mode(&v, url, true);
        let mut ok = smoke_succeeded(&v);
        if let Some(oi) = v.get("outInfo") {
            if let Some(err) = oi.get("error").and_then(|e| e.as_str()) {
                ok = false;
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
                s.push_str(&format!(
                    "\n【页面内 <table>（共 {} 张，渲染后的实际内容）】\n",
                    t.len()
                ));
                for (i, tab) in t.iter().take(5).enumerate() {
                    s.push_str(&format!(
                        "── 表 {} ──\n{}\n",
                        i + 1,
                        tab.as_str().unwrap_or_default()
                    ));
                }
            }
        }
        return if ok { Ok(s) } else { Err(s) };
    }

    let path = arg_str(args, "path").ok_or("缺少 path（本地 HTML）或 url（线上页面）")?;
    let clicks = requested_clicks(args, false);
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
    let shot = args
        .get("screenshot")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if shot {
        let name = full
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "page".into());
        let dir = if cfg.stateless {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "i-agent-polaris-shots-{}-{nonce}-{}",
                std::process::id(),
                CLOAK_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).map_err(|e| format!("安全创建截图临时目录失败: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            dir
        } else {
            cfg.workspace.join(".i-agent").join("shots")
        };
        if !cfg.stateless {
            let _ = std::fs::create_dir_all(&dir);
        }
        a.push("--shot".into());
        a.push(
            dir.join(format!("{name}.png"))
                .to_string_lossy()
                .to_string(),
        );
    }

    let v = run_smoke(cfg, a, 90)?;
    let report = render(&v, path);
    if smoke_succeeded(&v) {
        Ok(report)
    } else {
        Err(report)
    }
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

#[cfg(test)]
mod tests {
    use super::{
        requested_clicks, run, smoke_backend, smoke_succeeded, url_smoke_args, SmokeBackend,
    };
    use crate::config::{detect_protocol, Config, Provider};

    #[test]
    fn polaris_stateless_mode_never_uses_bare_playwright_smoke() {
        assert_eq!(smoke_backend(true), SmokeBackend::CloakBrowser);
        assert_eq!(smoke_backend(false), SmokeBackend::LegacyPlaywright);
    }

    #[test]
    fn live_url_mode_is_non_interactive_by_default_and_has_no_legacy_dump_flag() {
        let empty = serde_json::json!({});
        assert_eq!(requested_clicks(&empty, true), 0);
        assert_eq!(requested_clicks(&empty, false), 3);
        assert_eq!(requested_clicks(&serde_json::json!({"clicks": 2}), true), 2);

        let args = url_smoke_args("https://example.test", 0, 500, None);
        assert!(!args.iter().any(|arg| arg == "--dump"));
        assert_eq!(
            args,
            vec![
                "--url",
                "https://example.test",
                "--clicks",
                "0",
                "--wait",
                "500"
            ]
        );
    }

    #[test]
    fn failed_or_fatal_smoke_is_not_successful_tool_evidence() {
        assert!(smoke_succeeded(&serde_json::json!({"ok": true})));
        assert!(!smoke_succeeded(&serde_json::json!({"ok": false})));
        assert!(!smoke_succeeded(
            &serde_json::json!({"ok": true, "fatal": "browser crashed"})
        ));
    }

    #[test]
    #[ignore = "requires an installed CloakBrowser and Chromium"]
    fn real_cloakbrowser_smoke_accepts_a_local_html_file() {
        let workspace = std::env::temp_dir().join(format!(
            "i-agent-real-cloak-{}-{}",
            std::process::id(),
            super::CLOAK_TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("probe.html"),
            "<!doctype html><html><body><main><h1>CLOAK-SMOKE-OK</h1><p>真实浏览器校验内容已经加载完成。</p></main></body></html>",
        )
        .unwrap();
        let cfg = Config {
            workspace: workspace.clone(),
            provider: Provider {
                name: "test".into(),
                base: String::new(),
                model: String::new(),
                key: String::new(),
                protocol: detect_protocol("", ""),
            },
            fallbacks: vec![],
            image_providers: vec![],
            context_window: 32_768,
            max_output: 4_096,
            max_turns: 8,
            assets_dir: workspace.clone(),
            quiet: true,
            stateless: true,
            canvas_url: "http://127.0.0.1:8787".into(),
            canvas_id: "main".into(),
        };

        let result = run(
            &serde_json::json!({"path": "probe.html", "screenshot": false}),
            &cfg,
        )
        .unwrap();
        assert!(result.contains("浏览器冒烟通过"), "{result}");

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = "<!doctype html><html><head><link rel=\"icon\" href=\"data:,\"></head><body><main><h1>CLOAK-URL-OK</h1><p>URL 模式已通过真实 Chromium 加载。</p></main></body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        });
        let url_result = run(
            &serde_json::json!({"url": format!("http://{address}"), "wait_ms": 200}),
            &cfg,
        )
        .unwrap();
        server.join().unwrap();
        assert!(url_result.contains("浏览器冒烟通过"), "{url_result}");
        assert!(url_result.contains("CLOAK-URL-OK"), "{url_result}");
        assert!(!workspace.join(".i-agent").exists());
        let _ = std::fs::remove_dir_all(workspace);
    }
}
