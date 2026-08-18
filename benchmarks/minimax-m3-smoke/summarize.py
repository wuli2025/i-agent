#!/usr/bin/env python3
"""Normalize benchmark metadata, external grades and CLI-reported usage."""

from __future__ import annotations

import argparse
import json
import platform
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from grade import grade_bugfix, grade_data_report


IAGENT_USAGE = re.compile(
    r"\[用量\]\s*请求\s*(\d+)\s*次\s*\|\s*输入\s*(\d+)\s*tok"
    r"（新算\s*(\d+)\s*\+\s*缓存命中\s*(\d+)）\|\s*输出\s*(\d+)\s*tok"
)


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8-sig", errors="replace")
    except OSError:
        return ""


def as_int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return int(value)
    return None


def parse_iagent(stdout: str, stderr: str) -> dict[str, int] | None:
    match = IAGENT_USAGE.search(stdout + "\n" + stderr)
    if not match:
        return None
    requests, total_input, fresh, cached, output = map(int, match.groups())
    return {
        "requests": requests,
        "input_tokens": total_input,
        "fresh_input_tokens": fresh,
        "cached_input_tokens": cached,
        "output_tokens": output,
    }


def parse_claude(stdout: str) -> dict[str, int] | None:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    usage = payload.get("usage") or {}
    fresh = as_int(usage.get("input_tokens")) or 0
    cached = as_int(usage.get("cache_read_input_tokens")) or 0
    cache_write = as_int(usage.get("cache_creation_input_tokens")) or 0
    output = as_int(usage.get("output_tokens")) or 0
    if not any([fresh, cached, cache_write, output]):
        return None
    result = {
        "input_tokens": fresh + cached + cache_write,
        "fresh_input_tokens": fresh,
        "cached_input_tokens": cached,
        "cache_write_input_tokens": cache_write,
        "output_tokens": output,
    }
    turns = as_int(payload.get("num_turns"))
    if turns is not None:
        result["requests"] = turns
    return result


def json_lines(text: str) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            rows.append(value)
    return rows


def parse_codex(stdout: str) -> dict[str, int] | None:
    usage: dict[str, Any] | None = None
    for event in json_lines(stdout):
        if event.get("type") == "turn.completed" and isinstance(event.get("usage"), dict):
            usage = event["usage"]
    if usage is None:
        return None
    total_input = as_int(usage.get("input_tokens")) or 0
    cached = as_int(usage.get("cached_input_tokens")) or 0
    output = as_int(usage.get("output_tokens")) or 0
    return {
        "input_tokens": total_input,
        "fresh_input_tokens": max(0, total_input - cached),
        "cached_input_tokens": cached,
        "output_tokens": output,
    }


def parse_opencode(stdout: str) -> dict[str, int] | None:
    totals = {"requests": 0, "input_tokens": 0, "fresh_input_tokens": 0, "cached_input_tokens": 0, "cache_write_input_tokens": 0, "output_tokens": 0}
    for event in json_lines(stdout):
        if event.get("type") != "step_finish":
            continue
        part = event.get("part") or {}
        tokens = part.get("tokens") or {}
        if not isinstance(tokens, dict):
            continue
        cache = tokens.get("cache") or {}
        fresh = as_int(tokens.get("input")) or 0
        cached = as_int(cache.get("read")) or 0 if isinstance(cache, dict) else 0
        cache_write = as_int(cache.get("write")) or 0 if isinstance(cache, dict) else 0
        output = (as_int(tokens.get("output")) or 0) + (as_int(tokens.get("reasoning")) or 0)
        totals["requests"] += 1
        totals["fresh_input_tokens"] += fresh
        totals["cached_input_tokens"] += cached
        totals["cache_write_input_tokens"] += cache_write
        totals["input_tokens"] += fresh + cached + cache_write
        totals["output_tokens"] += output
    return totals if totals["requests"] else None


def usage_for(agent: str, stdout: str, stderr: str) -> dict[str, int] | None:
    if agent == "iagent":
        return parse_iagent(stdout, stderr)
    if agent == "claude":
        return parse_claude(stdout)
    if agent == "codex":
        return parse_codex(stdout)
    if agent == "opencode":
        return parse_opencode(stdout)
    return None


def artifacts_for(task: str, workspace: Path) -> list[dict[str, Any]]:
    expected = {
        "bugfix": ["inventory.py", "test_inventory.py"],
        "data-report": ["cleaned_orders.csv", "rejected_orders.csv", "summary.json", "report.md"],
    }[task]
    rows = []
    for name in expected:
        path = workspace / name
        rows.append({"name": name, "exists": path.is_file(), "bytes": path.stat().st_size if path.is_file() else 0})
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    runs_root = args.runs.resolve()
    runs: list[dict[str, Any]] = []
    versions: dict[str, str] = {}
    for meta_path in sorted(runs_root.glob("*/*/_meta.json")):
        meta = json.loads(meta_path.read_text(encoding="utf-8-sig"))
        task = meta["task"]
        agent = meta["agent"]
        workspace = meta_path.parent
        stdout = read_text(workspace / "_stdout.txt")
        stderr = read_text(workspace / "_stderr.txt")
        grade = grade_bugfix(workspace) if task == "bugfix" else grade_data_report(workspace)
        usage = usage_for(agent, stdout, stderr)
        versions[agent] = meta.get("version", "unknown")
        runs.append(
            {
                "task": task,
                "agent": agent,
                "version": meta.get("version", "unknown"),
                "protocol": meta.get("protocol", "unknown"),
                "exit_code": meta.get("exit_code"),
                "timed_out": bool(meta.get("timed_out")),
                "wall_seconds": meta.get("wall_seconds"),
                "grade": grade,
                "usage": usage,
                "artifacts": artifacts_for(task, workspace),
            }
        )

    totals = []
    for agent in ["iagent", "claude", "codex", "opencode"]:
        selected = [run for run in runs if run["agent"] == agent]
        if not selected:
            continue
        usages = [run["usage"] for run in selected]
        usage_complete = all(usage is not None for usage in usages)
        totals.append(
            {
                "agent": agent,
                "tasks_passed": sum(1 for run in selected if run["grade"]["passed"]),
                "tasks_run": len(selected),
                "score": sum(run["grade"]["score"] for run in selected),
                "max_score": sum(run["grade"]["max_score"] for run in selected),
                "wall_seconds": round(sum(float(run["wall_seconds"] or 0) for run in selected), 1),
                "usage_complete": usage_complete,
                "input_tokens": sum((usage or {}).get("input_tokens", 0) for usage in usages) if usage_complete else None,
                "fresh_input_tokens": sum((usage or {}).get("fresh_input_tokens", 0) for usage in usages) if usage_complete else None,
                "cached_input_tokens": sum((usage or {}).get("cached_input_tokens", 0) for usage in usages) if usage_complete else None,
                "cache_write_input_tokens": sum((usage or {}).get("cache_write_input_tokens", 0) for usage in usages) if usage_complete else None,
                "output_tokens": sum((usage or {}).get("output_tokens", 0) for usage in usages) if usage_complete else None,
                "requests": sum((usage or {}).get("requests", 0) for usage in usages) if usage_complete and all("requests" in (usage or {}) for usage in usages) else None,
            }
        )

    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "scope": "single-run exploratory smoke test; two deterministic tasks per CLI",
        "model": "MiniMax-M3",
        "protocol_note": "i-agent, Claude Code and OpenCode use Anthropic Messages; Codex uses MiniMax Responses",
        "environment": {
            "platform": platform.platform(),
            "python": sys.version.split()[0],
            "versions": versions,
        },
        "runs": runs,
        "totals": totals,
    }
    args.output.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"runs": len(runs), "output": str(args.output), "totals": totals}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
