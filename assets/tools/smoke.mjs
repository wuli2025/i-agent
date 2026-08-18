#!/usr/bin/env node
// i-agent 浏览器冒烟：真 Chromium 里加载页面，抓运行时报错、白屏、可交互性，并截图。
// 用法: node smoke.mjs --file <html> [--clicks 3] [--shot out.png] [--wait 1200] [--url http://...]
// 输出: 单行 JSON 到 stdout（Rust 侧解析）。

import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { existsSync, readdirSync } from 'node:fs';
import { homedir, platform } from 'node:os';
import { dirname, join } from 'node:path';
import { execFileSync } from 'node:child_process';

const argv = process.argv.slice(2);
const arg = (k, d = null) => {
  const i = argv.indexOf(k);
  return i >= 0 && i + 1 < argv.length ? argv[i + 1] : d;
};

const file = arg('--file');
const url = arg('--url');
const clicks = parseInt(arg('--clicks', '3'), 10);
const shot = arg('--shot');
const wait = parseInt(arg('--wait', '1200'), 10);

const die = (msg, hint) => {
  console.log(JSON.stringify({ ok: false, fatal: msg, hint: hint || null }));
  process.exit(0); // 0：让 Rust 侧读到结构化结果而不是进程错误
};

/* ---------- 定位 playwright 与 chromium（不强制用户预装到固定位置） ---------- */
function npmGlobalRoot() {
  const commands = platform() === 'win32' ? ['npm.cmd', 'npm'] : ['npm'];
  for (const command of commands) {
    try {
      const root = execFileSync(command, ['root', '-g'], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
        windowsHide: true,
      }).trim();
      if (root) return root;
    } catch { /* 继续找下一个 npm 启动名 */ }
  }
  return null;
}

function loadPlaywright() {
  const prefix = process.env.NPM_CONFIG_PREFIX;
  const appData = process.env.APPDATA;
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  const cands = [...new Set([
    process.env.I_AGENT_PLAYWRIGHT,
    npmGlobalRoot(),
    appData && join(appData, 'npm/node_modules'),
    prefix && join(prefix, 'node_modules'),
    prefix && join(prefix, 'lib/node_modules'),
    join(process.cwd(), 'node_modules'),
    join(scriptDir, 'node_modules'),
    join(homedir(), '.npm-global/lib/node_modules'),
    join(homedir(), '.i-agent/node_modules'),
    '/usr/lib/node_modules/',
    '/usr/local/lib/node_modules/',
  ].filter(Boolean))];
  for (const base of cands) {
    try {
      const req = createRequire(join(base, '__i_agent_resolver.cjs'));
      return req('playwright');
    } catch { /* 继续找下一个 */ }
  }
  try {
    return createRequire(import.meta.url)('playwright');
  } catch { /* 找不到 */ }
  return null;
}

function findChrome() {
  if (process.env.I_AGENT_CHROME && existsSync(process.env.I_AGENT_CHROME)) {
    return process.env.I_AGENT_CHROME;
  }
  // playwright 自带浏览器缓存：挑版本号最大的一个
  const cacheRoots = [
    join(homedir(), '.cache/ms-playwright'),
    join(homedir(), 'AppData/Local/ms-playwright'),
    join(homedir(), 'Library/Caches/ms-playwright'),
  ];
  const rel = [
    'chrome-linux64/chrome',
    'chrome-linux/chrome',
    'chrome-win/chrome.exe',
    'chrome-mac/Chromium.app/Contents/MacOS/Chromium',
    'chrome-headless-shell-linux64/chrome-headless-shell',
  ];
  for (const root of cacheRoots) {
    if (!existsSync(root)) continue;
    const dirs = readdirSync(root)
      .filter((d) => d.startsWith('chromium'))
      .sort((a, b) => (parseInt(b.split('-').pop(), 10) || 0) - (parseInt(a.split('-').pop(), 10) || 0));
    for (const d of dirs) {
      for (const r of rel) {
        const p = join(root, d, r);
        if (existsSync(p)) return p;
      }
    }
  }
  // 系统浏览器兜底
  const sys = platform() === 'win32'
    ? ['C:/Program Files/Google/Chrome/Application/chrome.exe',
       'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe']
    : platform() === 'darwin'
    ? ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome']
    : ['/usr/bin/chromium', '/usr/bin/chromium-browser', '/usr/bin/google-chrome',
       '/mnt/c/Program Files/Google/Chrome/Application/chrome.exe'];
  return sys.find(existsSync) || null;
}

const pw = loadPlaywright();
if (!pw) die('未找到 playwright 模块', '安装: npm i -g playwright && npx playwright install chromium，或设 I_AGENT_PLAYWRIGHT 指向含 playwright 的 node_modules 目录');
const exe = findChrome();

/* ---------- 探针：Web Audio / requestAnimationFrame 是否真被用起来 ---------- */
const initScript = `
  window.__audio = false; window.__raf = 0; window.__timers = 0;
  const AC = window.AudioContext || window.webkitAudioContext;
  if (AC) {
    const P = new Proxy(AC, { construct(t, a) { window.__audio = true; return new t(...a); } });
    window.AudioContext = P; window.webkitAudioContext = P;
  }
  const _raf = window.requestAnimationFrame;
  window.requestAnimationFrame = function (cb) { window.__raf++; return _raf.call(window, cb); };
  const _st = window.setTimeout;
  window.setTimeout = function (...a) { window.__timers++; return _st.apply(window, a); };
`;

const target = url || (file ? pathToFileURL(file).href : null);
if (!target) die('缺少 --file 或 --url');
if (file && !existsSync(file)) die(`文件不存在: ${file}`);

let browser;
try {
  browser = await pw.chromium.launch({
    executablePath: exe || undefined,
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--autoplay-policy=no-user-gesture-required'],
  });
} catch (e) {
  die('Chromium 启动失败: ' + String(e).split('\n')[0], '可设 I_AGENT_CHROME 指向 chrome/msedge 可执行文件');
}

const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
await ctx.addInitScript(initScript);
const page = await ctx.newPage();

const errors = [];
const push = (s) => { if (errors.length < 20) errors.push(s); };
page.on('pageerror', (e) => push('运行时异常: ' + String(e).split('\n')[0]));
page.on('console', (m) => { if (m.type() === 'error') push('console.error: ' + m.text().slice(0, 200)); });
page.on('requestfailed', (r) => {
  const u = r.url();
  if (!u.startsWith('data:') && !u.startsWith('blob:')) {
    push(`资源加载失败: ${u.slice(0, 120)} (${r.failure()?.errorText || '?'})`);
  }
});

// 外链依赖检测：单文件交付要求零外链
const external = [];
page.on('request', (r) => {
  const u = r.url();
  if (/^https?:/i.test(u) && external.length < 10) external.push(u.slice(0, 120));
});

let loaded = true;
try {
  await page.goto(target, { waitUntil: 'load', timeout: 20000 });
} catch (e) {
  loaded = false;
  push('页面加载失败: ' + String(e).split('\n')[0]);
}
await page.waitForTimeout(wait);

const snap = async () => page.evaluate(() => {
  // 「内容被裁剪」检测：没报错、没白屏，但整块主内容根本没显示出来。
  // 典型成因：子元素 position:absolute 撑不开父容器高度，父容器又 overflow:hidden → 全被裁掉。
  // 注意区分：高度恰好为 0 的通常是「故意折叠」（手风琴、待展开的确认卡），不算 bug；
  // 而 2px 这种非零小值 + 里面藏着几百 px 内容，才是意外塌陷。
  const clipped = [];
  for (const e of document.querySelectorAll('body *')) {
    const cs = getComputedStyle(e);
    if (cs.overflow === 'visible' && cs.overflowY === 'visible') continue;
    const box = e.getBoundingClientRect();
    if (box.width < 100) continue;
    if (box.height === 0 || box.height >= 60) continue; // 0 = 故意折叠，>=60 = 正常展开
    let bottom = 0;
    for (const c of e.children) {
      bottom = Math.max(bottom, c.getBoundingClientRect().bottom - box.top);
    }
    if (bottom - box.height > 120) {
      clipped.push({
        sel: (e.className || '').toString().slice(0, 30) || e.tagName,
        h: Math.round(box.height),
        contentH: Math.round(bottom),
      });
    }
  }
  return {
    text: (document.body?.innerText || '').trim().length,
    nodes: document.querySelectorAll('body *').length,
    canvas: document.querySelectorAll('canvas').length,
    svg: document.querySelectorAll('svg').length,
    // 只算「真的画出了东西」的 svg——空的 <svg> 标签跟没有是一回事
    svgDrawn: [...document.querySelectorAll('svg')].filter(
      (s) => s.querySelectorAll('rect,path,circle,line,polyline,polygon,text').length >= 2
    ).length,
    imgs: document.querySelectorAll('img').length,
    audio: !!window.__audio,
    raf: window.__raf || 0,
    timers: window.__timers || 0,
    clipped: clipped.slice(0, 5),
    html: (document.body?.innerHTML || '').length,
  };
}).catch(() => null);

const before = await snap();

/* ---------- 交互：点可点的东西，看 DOM 会不会变、会不会炸 ---------- */
const interaction = { clicked: 0, domChanged: false, errorsAfterClick: 0 };
if (before && clicks > 0) {
  const errBefore = errors.length;
  let prevHtml = await page.evaluate(() => document.body.innerHTML).catch(() => '');
  // The old selector only knew about game UI (.choice/.option/li[data-goto]/.start).
  // Business pages bind their tabs, range switches and filters with addEventListener on
  // plain divs, so it clicked nothing, reported clicked:0 / domChanged:false, and passed
  // the page anyway — i.e. the gate was blind to exactly the defect it exists to catch:
  // a control that does nothing when you click it. Fall back to anything the page itself
  // presents as clickable (cursor:pointer), which is what a user would try.
  const SELECTOR =
    'button, [onclick], [role="button"], [role="tab"], a[href="#"], select, ' +
    '.choice, .option, .btn, .start, li[data-goto], ' +
    '[class*="tab"], [class*="range"], [class*="filter"], [class*="toggle"], ' +
    '[class*="seg"], [data-range], [data-period], [data-days], th[data-sort], th';
  for (let i = 0; i < clicks; i++) {
    let els = await page.$$(SELECTOR).catch(() => []);
    const vis = [];
    for (const el of els) {
      if (await el.isVisible().catch(() => false)) vis.push(el);
    }
    if (!vis.length) {
      // last resort: whatever the page styles as clickable
      const pointer = await page.$$('*').catch(() => []);
      for (const el of pointer) {
        const isPointer = await el
          .evaluate((e) => getComputedStyle(e).cursor === 'pointer' && e.offsetParent !== null)
          .catch(() => false);
        if (isPointer) vis.push(el);
        if (vis.length >= 8) break;
      }
    }
    if (!vis.length) break;
    const el = vis[i % vis.length];
    try {
      await el.click({ timeout: 3000 });
      interaction.clicked++;
    } catch { break; }
    await page.waitForTimeout(700);
    const nowHtml = await page.evaluate(() => document.body.innerHTML).catch(() => '');
    if (nowHtml && nowHtml !== prevHtml) interaction.domChanged = true;
    prevHtml = nowHtml;
  }
  interaction.errorsAfterClick = errors.length - errBefore;
}

const after = await snap();

if (shot) {
  try { await page.screenshot({ path: shot, fullPage: false }); } catch { /* 截图失败不致命 */ }
}
await browser.close();

/* ---------- 判定 ---------- */
const s = after || before || { text: 0, nodes: 0, canvas: 0, svg: 0, imgs: 0, clipped: [] };
const reasons = [];
// 白屏：正文近乎空 且 没有 canvas/svg 这类图形承载
const whiteScreen = s.text < 20 && s.canvas === 0 && s.svg === 0 && s.nodes < 5;
if (!loaded) reasons.push('页面无法加载');
if (whiteScreen) reasons.push(`白屏：正文仅 ${s.text} 字、DOM 仅 ${s.nodes} 个节点（脚本很可能在初始化时就抛错了）`);
const runtimeErrs = errors.filter((e) => e.startsWith('运行时异常'));
if (runtimeErrs.length) reasons.push(`有 ${runtimeErrs.length} 处运行时异常`);

// 空 <svg> 标签 = 图表没画出来。标签在不代表图在。
const emptySvg = (s.svg || 0) - (s.svgDrawn || 0);
if (s.svg > 0 && s.svgDrawn === 0) {
  reasons.push(`${s.svg} 个 <svg> 标签全是空的——图表一个都没画出来`);
}

// 内容被裁剪：最阴险的一类——不报错、不白屏，但主内容整块看不见
const clipped = s.clipped || [];
for (const c of clipped) {
  reasons.push(
    `内容被裁剪：容器 .${c.sel} 高度只有 ${c.h}px，里面却有 ${c.contentH}px 的内容 —— 整块被 overflow:hidden 裁掉了，页面上看不见`
  );
}

const ok = loaded && !whiteScreen && runtimeErrs.length === 0 && clipped.length === 0
  && !(s.svg > 0 && s.svgDrawn === 0);
console.log(JSON.stringify({
  ok,
  reasons,
  loaded,
  whiteScreen,
  bodyText: s.text,
  domNodes: s.nodes,
  canvas: s.canvas,
  svg: s.svg,
  svgDrawn: s.svgDrawn || 0,
  emptySvg,
  clipped,
  images: s.imgs,
  audioContext: !!s.audio,
  rafCalls: s.raf || 0,
  interaction,
  externalRequests: external,
  errors,
  screenshot: shot || null,
  chrome: exe || 'playwright-default',
}));
