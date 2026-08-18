#!/usr/bin/env python3
"""Build an offline HTML evidence bundle from retained benchmark workspaces.

Only an explicit allowlist of final task artifacts is embedded. CLI homes,
sessions, raw stdout/stderr, pycache files and credentials are never included.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import mimetypes
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable

from grade import grade_bugfix, grade_data_report


HERE = Path(__file__).resolve().parent
AGENTS = ("iagent", "claude", "codex", "opencode")
TASKS = ("bugfix", "data-report")
EXPECTED_OUTPUTS = {
    "bugfix": ("inventory.py", "test_inventory.py"),
    "data-report": ("cleaned_orders.csv", "rejected_orders.csv", "summary.json", "report.md"),
}
MEDIA_TYPES = {
    ".py": "text/x-python",
    ".md": "text/markdown",
    ".csv": "text/csv",
    ".json": "application/json",
}
SENSITIVE_PATTERNS = (
    ("API key", re.compile(r"\bsk-[A-Za-z0-9_-]{20,}")),
    ("authorization bearer", re.compile(r"(?i)\bBearer\s+[A-Za-z0-9._~-]{20,}")),
    ("private key", re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----")),
    ("credential URL", re.compile(r"(?i)https?://[^\s/:@]+:[^\s/@]+@")),
    ("Windows user path", re.compile(r"(?i)\b[A-Z]:\\(?:Users|Documents and Settings)\\")),
    ("Unix user path", re.compile(r"/(?:home|Users)/[^/\s]+")),
    (
        "assigned credential",
        re.compile(
            r"(?i)(?:ANTHROPIC_(?:AUTH_TOKEN|API_KEY)|MINIMAX_(?:API_KEY|BENCH_API_KEY))"
            r"\s*[:=]\s*[\"']?[A-Za-z0-9_-]{16,}"
        ),
    ),
)


class BuildError(RuntimeError):
    pass


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def safe_text(path: Path, data: bytes) -> str:
    try:
        text = data.decode("utf-8-sig")
    except UnicodeDecodeError as exc:
        raise BuildError(f"{path.name} is not valid UTF-8: {exc}") from exc
    for label, pattern in SENSITIVE_PATTERNS:
        match = pattern.search(text)
        if match:
            raise BuildError(f"refusing to embed {path.name}: matched {label}")
    return text


def media_type(path: Path) -> str:
    return MEDIA_TYPES.get(path.suffix.lower()) or mimetypes.guess_type(path.name)[0] or "text/plain"


def encode_file(path: Path, *, task: str, agent: str | None, role: str, display_path: str) -> dict[str, Any]:
    if not path.is_file():
        raise BuildError(f"missing artifact: {display_path}")
    raw = path.read_bytes()
    safe_text(path, raw)
    return {
        "task": task,
        "agent": agent,
        "role": role,
        "path": display_path.replace("\\", "/"),
        "name": path.name,
        "media_type": media_type(path),
        "bytes": len(raw),
        "sha256": sha256_bytes(raw),
        "content_base64": base64.b64encode(raw).decode("ascii"),
    }


def normalize_grade(value: dict[str, Any]) -> dict[str, Any]:
    """Normalize numeric JSON representation before equality checks."""

    def walk(item: Any) -> Any:
        if isinstance(item, dict):
            return {key: walk(val) for key, val in item.items()}
        if isinstance(item, list):
            return [walk(val) for val in item]
        if isinstance(item, float) and item.is_integer():
            return int(item)
        return item

    return walk(value)


def regrade(task: str, workspace: Path) -> dict[str, Any]:
    return grade_bugfix(workspace) if task == "bugfix" else grade_data_report(workspace)


def helper_outputs(task: str, workspace: Path) -> list[Path]:
    if task != "data-report":
        return []
    return sorted(path for path in workspace.glob("*.py") if path.is_file())


def benchmark_commit(repo_root: Path, results_path: Path) -> str:
    try:
        relative = results_path.resolve().relative_to(repo_root.resolve())
        completed = subprocess.run(
            ["git", "-C", str(repo_root), "log", "-1", "--format=%H", "--", str(relative)],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        commit = completed.stdout.strip()
        if completed.returncode == 0 and re.fullmatch(r"[0-9a-f]{40}", commit):
            return commit
    except (OSError, ValueError, subprocess.SubprocessError):
        pass
    return "unknown"


def collect_payload(results_path: Path, runs_root: Path, fixtures_root: Path) -> dict[str, Any]:
    results_raw = results_path.read_bytes()
    safe_text(results_path, results_raw)
    results = json.loads(results_raw.decode("utf-8-sig"))
    published_runs = {(run["task"], run["agent"]): run for run in results["runs"]}
    expected_keys = {(task, agent) for task in TASKS for agent in AGENTS}
    if set(published_runs) != expected_keys:
        raise BuildError(f"results.json run matrix mismatch: {sorted(published_runs)}")

    runs: list[dict[str, Any]] = []
    all_artifacts: list[dict[str, Any]] = []
    for task in TASKS:
        for agent in AGENTS:
            published = published_runs[(task, agent)]
            workspace = runs_root / task / agent
            if not workspace.is_dir():
                raise BuildError(f"missing workspace: {task}/{agent}")

            fresh_grade = regrade(task, workspace)
            if normalize_grade(fresh_grade) != normalize_grade(published["grade"]):
                raise BuildError(f"fresh grade differs from results.json for {task}/{agent}")

            recorded = {item["name"]: item for item in published.get("artifacts", [])}
            for name in EXPECTED_OUTPUTS[task]:
                path = workspace / name
                item = recorded.get(name)
                if item is None:
                    raise BuildError(f"results.json does not record {task}/{agent}/{name}")
                exists = path.is_file()
                if bool(item.get("exists")) != exists:
                    raise BuildError(f"existence mismatch for {task}/{agent}/{name}")
                actual_bytes = path.stat().st_size if exists else 0
                if int(item.get("bytes", -1)) != actual_bytes:
                    raise BuildError(
                        f"byte-size mismatch for {task}/{agent}/{name}: "
                        f"results={item.get('bytes')} actual={actual_bytes}"
                    )

            selected = [workspace / name for name in EXPECTED_OUTPUTS[task]]
            selected.extend(path for path in helper_outputs(task, workspace) if path not in selected)
            artifacts = [
                encode_file(
                    path,
                    task=task,
                    agent=agent,
                    role="task-output" if path.name in EXPECTED_OUTPUTS[task] else "helper-script",
                    display_path=f"runs/{task}/{agent}/{path.name}",
                )
                for path in selected
            ]
            all_artifacts.extend(artifacts)
            runs.append(
                {
                    "task": task,
                    "agent": agent,
                    "version": published["version"],
                    "protocol": published["protocol"],
                    "exit_code": published["exit_code"],
                    "timed_out": published["timed_out"],
                    "wall_seconds": published["wall_seconds"],
                    "grade": fresh_grade,
                    "usage": published.get("usage"),
                    "artifacts": artifacts,
                }
            )

    fixtures: list[dict[str, Any]] = []
    for task in TASKS:
        task_root = fixtures_root / task
        if not task_root.is_dir():
            raise BuildError(f"missing fixture directory: {task}")
        for path in sorted(item for item in task_root.iterdir() if item.is_file()):
            fixtures.append(
                encode_file(
                    path,
                    task=task,
                    agent=None,
                    role="fixture",
                    display_path=f"fixtures/{task}/{path.name}",
                )
            )

    repo_root = HERE.parent.parent
    return {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "benchmark_commit": benchmark_commit(repo_root, results_path),
        "results_sha256": sha256_bytes(results_raw),
        "scope": results["scope"],
        "model": results["model"],
        "protocol_note": results["protocol_note"],
        "environment": results["environment"],
        "totals": results["totals"],
        "runs": runs,
        "fixtures": fixtures,
        "manifest": [
            {key: artifact[key] for key in ("task", "agent", "role", "path", "bytes", "sha256")}
            for artifact in [*fixtures, *all_artifacts]
        ],
        "exclusions": [
            ".homes and provider/auth configuration",
            ".i-agent session state",
            "__pycache__ and binary files",
            "raw stdout/stderr and CLI session identifiers",
        ],
        "limitations": [
            "Only two deterministic tasks and one successful run per CLI were measured.",
            "Codex used MiniMax Responses; the other three used Anthropic Messages.",
            "Token fields come from each CLI/upstream and may not be perfectly comparable.",
            "The OpenCode data-report success followed an interrupted outer orchestration attempt and may have benefited from warm upstream cache.",
        ],
    }


CSS = r"""
:root {
  color-scheme: light dark;
  --bg: #f5f7fa;
  --panel: #ffffff;
  --ink: #17202a;
  --muted: #5b6674;
  --line: #d8dee8;
  --accent: #2357a5;
  --accent-soft: #e8f0fc;
  --ok: #176b45;
  --code: #f0f3f7;
  --shadow: 0 10px 32px rgba(21, 35, 52, .08);
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #0f141b;
    --panel: #171e27;
    --ink: #edf2f7;
    --muted: #aab5c2;
    --line: #334050;
    --accent: #8db8ff;
    --accent-soft: #1e3350;
    --ok: #74d3a7;
    --code: #111820;
    --shadow: none;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--ink);
  font: 15px/1.55 system-ui, -apple-system, "Segoe UI", sans-serif;
}
a { color: var(--accent); }
header, main, footer { width: min(1180px, calc(100% - 32px)); margin-inline: auto; }
header { padding: 46px 0 22px; }
h1 { margin: 0 0 8px; font-size: clamp(28px, 5vw, 46px); letter-spacing: -.035em; }
h2 { margin: 34px 0 14px; font-size: 23px; }
h3 { margin: 0; font-size: 18px; }
p { margin: 8px 0; }
.lede { max-width: 850px; color: var(--muted); font-size: 17px; }
.notice {
  margin: 18px 0;
  padding: 13px 16px;
  border-left: 4px solid var(--accent);
  background: var(--accent-soft);
}
.panel {
  margin: 18px 0;
  padding: 20px;
  border: 1px solid var(--line);
  border-radius: 12px;
  background: var(--panel);
  box-shadow: var(--shadow);
}
.table-wrap { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; }
th, td { padding: 10px 12px; border-bottom: 1px solid var(--line); text-align: left; vertical-align: top; }
th { color: var(--muted); font-size: 12px; letter-spacing: .04em; text-transform: uppercase; }
tr:last-child td { border-bottom: 0; }
.best { color: var(--ok); font-weight: 700; }
.filters { display: flex; flex-wrap: wrap; gap: 12px; align-items: end; }
label { display: grid; gap: 5px; color: var(--muted); font-size: 13px; }
select, button {
  min-height: 38px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--panel);
  color: var(--ink);
  padding: 7px 10px;
  font: inherit;
}
button { cursor: pointer; }
button:hover { border-color: var(--accent); color: var(--accent); }
.run { scroll-margin-top: 18px; }
.run-head { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 10px; align-items: baseline; }
.badges { display: flex; flex-wrap: wrap; gap: 7px; }
.badge { padding: 3px 8px; border-radius: 999px; background: var(--accent-soft); color: var(--accent); font-size: 12px; }
.file { margin: 10px 0; border: 1px solid var(--line); border-radius: 9px; }
.file > summary { cursor: pointer; padding: 11px 13px; font-weight: 650; background: var(--code); }
.file-body { padding: 12px; }
.file-meta { color: var(--muted); font: 12px/1.5 ui-monospace, SFMono-Regular, Consolas, monospace; overflow-wrap: anywhere; }
.actions { margin: 9px 0; }
pre {
  max-height: 460px;
  overflow: auto;
  margin: 10px 0 0;
  padding: 14px;
  border-radius: 8px;
  background: var(--code);
  color: var(--ink);
  white-space: pre;
  tab-size: 2;
  font: 12.5px/1.55 ui-monospace, SFMono-Regular, Consolas, monospace;
}
.csv-preview { margin-top: 10px; max-height: 420px; overflow: auto; border: 1px solid var(--line); border-radius: 8px; }
.csv-preview table { font-size: 13px; background: var(--panel); }
.check-pass { color: var(--ok); font-weight: 700; }
.hash-list { font: 12px/1.6 ui-monospace, SFMono-Regular, Consolas, monospace; overflow-wrap: anywhere; }
.hidden { display: none !important; }
.empty { padding: 25px; color: var(--muted); text-align: center; }
footer { padding: 34px 0 54px; color: var(--muted); font-size: 13px; }
@media (max-width: 700px) {
  header, main, footer { width: min(100% - 20px, 1180px); }
  .panel { padding: 14px; }
  th, td { padding: 8px; }
}
"""


JS = r"""
'use strict';
const payload = JSON.parse(document.getElementById('payload').textContent);
const agents = ['iagent', 'claude', 'codex', 'opencode'];
const labels = {iagent: 'i-agent', claude: 'Claude Code', codex: 'Codex CLI', opencode: 'OpenCode'};
const tasks = {bugfix: 'Python 缺陷修复', 'data-report': 'CSV 清洗与办公报告'};

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}
function formatNumber(value) {
  return value === null || value === undefined ? '—' : Number(value).toLocaleString('en-US');
}
function bytesFromBase64(value) {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}
function textFromArtifact(artifact) {
  return new TextDecoder('utf-8').decode(bytesFromBase64(artifact.content_base64));
}
function downloadArtifact(artifact) {
  const blob = new Blob([bytesFromBase64(artifact.content_base64)], {type: artifact.media_type});
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = `${artifact.agent || 'fixture'}-${artifact.task}-${artifact.name}`;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
function parseCsv(text) {
  const rows = [];
  let row = [], field = '', quoted = false;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"' && text[i + 1] === '"') { field += '"'; i += 1; }
      else if (ch === '"') quoted = false;
      else field += ch;
    } else if (ch === '"') quoted = true;
    else if (ch === ',') { row.push(field); field = ''; }
    else if (ch === '\n') { row.push(field.replace(/\r$/, '')); rows.push(row); row = []; field = ''; }
    else field += ch;
  }
  if (field || row.length) { row.push(field.replace(/\r$/, '')); rows.push(row); }
  return rows;
}
function renderCsv(text) {
  const wrap = element('div', 'csv-preview');
  const table = element('table');
  const rows = parseCsv(text);
  rows.forEach((row, index) => {
    const tr = element('tr');
    row.forEach(value => tr.append(element(index === 0 ? 'th' : 'td', '', value)));
    table.append(tr);
  });
  wrap.append(table);
  return wrap;
}
function renderArtifact(artifact) {
  const details = element('details', 'file');
  details.dataset.path = artifact.path;
  const summary = element('summary', '', `${artifact.name} · ${formatNumber(artifact.bytes)} bytes · ${artifact.role}`);
  details.append(summary);
  const body = element('div', 'file-body');
  body.append(element('div', 'file-meta', `SHA-256  ${artifact.sha256}`));
  const actions = element('div', 'actions');
  const button = element('button', '', '下载原文件');
  button.type = 'button';
  button.addEventListener('click', () => downloadArtifact(artifact));
  actions.append(button);
  body.append(actions);
  let text = textFromArtifact(artifact);
  if (artifact.media_type === 'application/json') {
    try { text = JSON.stringify(JSON.parse(text), null, 2); } catch (_) { /* retain source */ }
  }
  if (artifact.media_type === 'text/csv') body.append(renderCsv(text));
  const pre = element('pre');
  pre.append(element('code', '', text));
  body.append(pre);
  details.append(body);
  return details;
}
function renderTable(headers, rows, className) {
  const wrap = element('div', `table-wrap ${className || ''}`.trim());
  const table = element('table');
  const head = element('tr');
  headers.forEach(value => head.append(element('th', '', value)));
  table.append(head);
  rows.forEach(values => {
    const tr = element('tr');
    values.forEach(value => {
      const td = element('td', value && value.best ? 'best' : '', value && value.text !== undefined ? value.text : value);
      tr.append(td);
    });
    table.append(tr);
  });
  wrap.append(table);
  return wrap;
}
function renderTotals() {
  const root = document.getElementById('totals');
  const rows = payload.totals.map(total => [
    labels[total.agent],
    {text: `${total.score}/${total.max_score}`, best: true},
    {text: `${total.wall_seconds}s`, best: total.agent === 'iagent'},
    {text: formatNumber(total.input_tokens), best: total.agent === 'iagent'},
    `${formatNumber(total.fresh_input_tokens)} / ${formatNumber(total.cached_input_tokens)}`,
    {text: formatNumber(total.output_tokens), best: total.agent === 'iagent'},
    formatNumber(total.requests),
  ]);
  root.append(renderTable(['Agent', '得分', '总耗时', '总输入', 'Fresh / Cache', '输出', '请求'], rows));
}
function renderChecks(run) {
  return renderTable(
    ['Grader 检查', '得分', '结果'],
    run.grade.checks.map(check => [check.name, `${check.points}/${check.max}`, {text: check.detail, best: check.points === check.max}]),
    'checks'
  );
}
function renderRun(run) {
  const article = element('article', 'panel run');
  article.dataset.agent = run.agent;
  article.dataset.task = run.task;
  const head = element('div', 'run-head');
  head.append(element('h3', '', `${labels[run.agent]} · ${tasks[run.task]}`));
  const badges = element('div', 'badges');
  [`${run.grade.score}/${run.grade.max_score}`, `${run.wall_seconds}s`, run.protocol, `exit ${run.exit_code}`]
    .forEach(value => badges.append(element('span', 'badge', value)));
  head.append(badges);
  article.append(head);
  const usage = run.usage || {};
  article.append(renderTable(
    ['输入 token', 'Fresh', 'Cache read', '输出 token', '请求数'],
    [[formatNumber(usage.input_tokens), formatNumber(usage.fresh_input_tokens), formatNumber(usage.cached_input_tokens), formatNumber(usage.output_tokens), formatNumber(usage.requests)]]
  ));
  article.append(renderChecks(run));
  const artifactTitle = element('h3', '', `真实产物（${run.artifacts.length}）`);
  artifactTitle.style.marginTop = '20px';
  article.append(artifactTitle);
  run.artifacts.forEach(artifact => article.append(renderArtifact(artifact)));
  return article;
}
function renderRuns() {
  const root = document.getElementById('runs');
  payload.runs.forEach(run => root.append(renderRun(run)));
}
function renderFixtures() {
  const root = document.getElementById('fixtures');
  payload.fixtures.forEach(artifact => root.append(renderArtifact(artifact)));
}
function renderManifest() {
  const root = document.getElementById('manifest');
  payload.manifest.forEach(item => root.append(element('div', '', `${item.sha256}  ${item.bytes}  ${item.path}`)));
}
function renderProvenance() {
  const root = document.getElementById('provenance');
  [
    ['生成时间（UTC）', payload.generated_at],
    ['模型', payload.model],
    ['Benchmark commit', payload.benchmark_commit],
    ['results.json SHA-256', payload.results_sha256],
    ['平台', payload.environment.platform],
    ['Python', payload.environment.python],
    ['收录文件', payload.manifest.length],
  ].forEach(([key, value]) => {
    const p = element('p');
    const strong = element('strong', '', `${key}：`);
    p.append(strong, document.createTextNode(String(value)));
    root.append(p);
  });
  const excluded = element('ul');
  payload.exclusions.forEach(value => excluded.append(element('li', '', value)));
  root.append(element('h3', '', '明确排除'), excluded);
}
function setupFilters() {
  const agent = document.getElementById('agent-filter');
  const task = document.getElementById('task-filter');
  agents.forEach(value => {
    const option = element('option', '', labels[value]); option.value = value; agent.append(option);
  });
  Object.entries(tasks).forEach(([value, label]) => {
    const option = element('option', '', label); option.value = value; task.append(option);
  });
  const apply = () => {
    let shown = 0;
    document.querySelectorAll('.run').forEach(node => {
      const visible = (!agent.value || node.dataset.agent === agent.value) && (!task.value || node.dataset.task === task.value);
      node.classList.toggle('hidden', !visible);
      if (visible) shown += 1;
    });
    document.getElementById('run-count').textContent = `显示 ${shown} / ${payload.runs.length} 个 run`;
  };
  agent.addEventListener('change', apply);
  task.addEventListener('change', apply);
  const toggle = document.getElementById('toggle-files');
  toggle.addEventListener('click', () => {
    const current = [...document.querySelectorAll('.run:not(.hidden) details.file')];
    const open = current.some(node => !node.open);
    current.forEach(node => { node.open = open; });
    toggle.textContent = open ? '收起当前产物' : '展开当前产物';
  });
  apply();
}
renderTotals();
renderRuns();
renderFixtures();
renderManifest();
renderProvenance();
setupFilters();
"""


def escaped_json(value: dict[str, Any]) -> str:
    text = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    return (
        text.replace("&", "\\u0026")
        .replace("<", "\\u003c")
        .replace(">", "\\u003e")
        .replace(" ", "\\u2028")
        .replace(" ", "\\u2029")
    )


def build_html(payload: dict[str, Any]) -> bytes:
    limitations = "".join(f"<li>{item}</li>" for item in (
        "每个 CLI 每题只有一个成功样本，不代表统计显著的排行榜。",
        "Codex 使用 Responses；其余三家使用 Anthropic Messages。",
        "本文件仅收录安全白名单内的最终产物，不包含 CLI home、会话、认证配置或原始日志。",
    ))
    document = f"""<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="light dark">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; font-src 'none'; media-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'">
<link rel="icon" href="data:,">
<title>MiniMax-M3 四 CLI 压测真实产物</title>
<style>{CSS}</style>
</head>
<body>
<header>
  <h1>MiniMax-M3 四 CLI 压测真实产物</h1>
  <p class="lede">i-agent、Claude Code、Codex CLI 与 OpenCode 的 8 个成功工作区产物，已重新判分、逐文件哈希并打包进这一个离线 HTML。每个文件都可展开预览或下载。</p>
  <div class="notice"><strong>正确率并列：</strong>四家均为 26/26。本页主要展示真实交付物与工程开销，不把两道小题包装成通用能力排名。</div>
</header>
<main>
  <section>
    <h2>总览</h2>
    <div id="totals" class="panel"></div>
  </section>
  <section>
    <h2>筛选真实产物</h2>
    <div class="panel filters">
      <label>Agent<select id="agent-filter"><option value="">全部</option></select></label>
      <label>任务<select id="task-filter"><option value="">全部</option></select></label>
      <button id="toggle-files" type="button">展开当前产物</button>
      <span id="run-count" class="file-meta"></span>
    </div>
    <div id="runs"></div>
  </section>
  <section>
    <h2>共享题目与输入 Fixture</h2>
    <div id="fixtures" class="panel"></div>
  </section>
  <section>
    <h2>方法与限制</h2>
    <div class="panel"><ul>{limitations}</ul><p>首次 data-report/OpenCode 编排在外层 10 分钟限制处中断；这里收录的是删除工作区后重新完整成功的 run，上游缓存可能曾被预热。</p></div>
  </section>
  <section>
    <h2>来源与排除项</h2>
    <div id="provenance" class="panel"></div>
  </section>
  <section>
    <h2>文件完整性清单</h2>
    <div id="manifest" class="panel hash-list"></div>
  </section>
</main>
<footer>离线证据包 · 无外链 · 无网络请求 · 产物预览只使用 textContent</footer>
<script id="payload" type="application/json">{escaped_json(payload)}</script>
<script>{JS}</script>
</body>
</html>
"""
    raw = document.encode("utf-8")
    safe_text(Path("ARTIFACTS.html"), raw)
    return raw


def write_bundle(output: Path, copies: Iterable[Path], raw: bytes) -> str:
    digest = sha256_bytes(raw)
    targets = [output, *copies]
    for target in targets:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(raw)
        target.with_name(target.name + ".sha256").write_text(
            f"{digest}  {target.name}\n", encoding="utf-8"
        )
    return digest


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--results", type=Path, default=HERE / "results.json")
    parser.add_argument("--runs", type=Path, default=HERE / ".runs")
    parser.add_argument("--fixtures", type=Path, default=HERE / "fixtures")
    parser.add_argument("--output", type=Path, default=HERE / "ARTIFACTS.html")
    parser.add_argument("--copy-to", type=Path, action="append", default=[])
    args = parser.parse_args()

    payload = collect_payload(args.results.resolve(), args.runs.resolve(), args.fixtures.resolve())
    raw = build_html(payload)
    digest = write_bundle(args.output.resolve(), [path.resolve() for path in args.copy_to], raw)
    print(
        json.dumps(
            {
                "runs": len(payload["runs"]),
                "files": len(payload["manifest"]),
                "bytes": len(raw),
                "sha256": digest,
                "output": str(args.output),
                "copies": [str(path) for path in args.copy_to],
            },
            ensure_ascii=False,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
