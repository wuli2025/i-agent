#!/usr/bin/env python3
"""External, deterministic grader for the MiniMax-M3 smoke benchmark."""

from __future__ import annotations

import argparse
import ast
import csv
import importlib.util
import json
import subprocess
import sys
from pathlib import Path
from typing import Callable


class Grade:
    def __init__(self, task: str) -> None:
        self.task = task
        self.score = 0.0
        self.maximum = 0.0
        self.checks: list[dict[str, object]] = []

    def check(self, name: str, points: float, fn: Callable[[], None]) -> None:
        self.maximum += points
        try:
            fn()
        except Exception as exc:  # noqa: BLE001 - report each independent check
            self.checks.append({"name": name, "points": 0, "max": points, "detail": str(exc)})
        else:
            self.score += points
            self.checks.append({"name": name, "points": points, "max": points, "detail": "ok"})

    def result(self) -> dict[str, object]:
        return {
            "task": self.task,
            "score": self.score,
            "max_score": self.maximum,
            "passed": self.score == self.maximum,
            "checks": self.checks,
        }


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def grade_bugfix(workspace: Path) -> dict[str, object]:
    grade = Grade("bugfix")
    module_path = workspace / "inventory.py"
    holder: dict[str, object] = {}

    def load() -> None:
        require(module_path.is_file(), "inventory.py missing")
        spec = importlib.util.spec_from_file_location("bench_inventory", module_path)
        require(spec is not None and spec.loader is not None, "cannot load inventory.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        require(callable(getattr(module, "summarize", None)), "summarize is not callable")
        holder["fn"] = module.summarize

    grade.check("module imports and exposes summarize", 1, load)

    def fn():
        require("fn" in holder, "module did not import")
        return holder["fn"]

    def basic() -> None:
        rows = [
            {"sku": " pen ", "qty": "2", "unit_price": "1.25", "discount": "0"},
            {"sku": "PEN", "qty": "3", "unit_price": "1.25", "discount": "0.20"},
        ]
        require(fn()(rows) == {"PEN": {"qty": 5, "net": "5.50"}}, "normalization/aggregation mismatch")

    grade.check("normalizes and aggregates SKU", 2, basic)

    def aggregate_then_round() -> None:
        rows = [
            {"sku": "x", "qty": 1, "unit_price": "0.335", "discount": "0"},
            {"sku": "X", "qty": 1, "unit_price": "0.335", "discount": "0"},
            {"sku": " x ", "qty": 1, "unit_price": "0.335", "discount": "0"},
        ]
        require(fn()(rows) == {"X": {"qty": 3, "net": "1.01"}}, "must round half-up only after aggregation")

    grade.check("uses exact aggregate-then-round money", 2, aggregate_then_round)

    def returns_and_order() -> None:
        rows = [
            {"sku": "B", "qty": 2, "unit_price": "10", "discount": "0.10"},
            {"sku": "a", "qty": -1, "unit_price": "2.345", "discount": ""},
            {"sku": "B", "qty": -1, "unit_price": "10", "discount": "0"},
        ]
        out = fn()(rows)
        require(list(out) == ["A", "B"], f"keys not sorted: {list(out)}")
        require(out == {"A": {"qty": -1, "net": "-2.35"}, "B": {"qty": 1, "net": "8.00"}}, "return handling mismatch")

    grade.check("handles returns and sorted output", 2, returns_and_order)

    def validation() -> None:
        invalid = [
            {"sku": " ", "qty": 1, "unit_price": "1", "discount": "0"},
            {"sku": "A", "qty": 1, "unit_price": "0", "discount": "0"},
            {"sku": "A", "qty": 1, "unit_price": "-1", "discount": "0"},
            {"sku": "A", "qty": 1, "unit_price": "1", "discount": "1.01"},
            {"sku": "A", "qty": 1, "unit_price": "1", "discount": "-0.01"},
        ]
        for row in invalid:
            try:
                fn()([row])
            except ValueError:
                continue
            raise AssertionError(f"expected ValueError for {row}")

    grade.check("validates SKU, price and discount", 3, validation)

    def no_float_calls() -> None:
        tree = ast.parse(module_path.read_text(encoding="utf-8"))
        calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)]
        require(
            not any(isinstance(node.func, ast.Name) and node.func.id == "float" for node in calls),
            "float() call found; use Decimal end-to-end",
        )

    grade.check("does not call float", 1, no_float_calls)

    def public_tests() -> None:
        completed = subprocess.run(
            [sys.executable, "-m", "unittest", "-v"],
            cwd=workspace,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        require(completed.returncode == 0, (completed.stdout + completed.stderr)[-600:])

    grade.check("passes public unittest suite", 1, public_tests)
    return grade.result()


def read_csv(path: Path) -> tuple[list[str], list[dict[str, str]]]:
    with path.open(newline="", encoding="utf-8-sig") as handle:
        reader = csv.DictReader(handle)
        return list(reader.fieldnames or []), list(reader)


def grade_data_report(workspace: Path) -> dict[str, object]:
    grade = Grade("data-report")
    required = ["cleaned_orders.csv", "rejected_orders.csv", "summary.json", "report.md"]

    def files_exist() -> None:
        missing = [name for name in required if not (workspace / name).is_file()]
        require(not missing, f"missing files: {missing}")

    grade.check("creates all four deliverables", 1, files_exist)

    cleaned_path = workspace / "cleaned_orders.csv"
    rejected_path = workspace / "rejected_orders.csv"
    summary_path = workspace / "summary.json"
    report_path = workspace / "report.md"

    def cleaned_columns() -> None:
        fields, _ = read_csv(cleaned_path)
        require(
            fields == ["order_id", "order_date", "region", "product", "quantity", "unit_price", "status", "line_total"],
            f"unexpected cleaned columns: {fields}",
        )

    grade.check("uses exact cleaned CSV schema", 1, cleaned_columns)

    def cleaned_rows() -> None:
        _, rows = read_csv(cleaned_path)
        ids = [row["order_id"] for row in rows]
        require(ids == ["A001", "A002", "A003", "A004", "A005", "A006", "A007", "A010"], f"unexpected IDs/order: {ids}")
        require(len(set(ids)) == len(ids), "duplicate ID remains in cleaned output")

    grade.check("keeps eight valid unique rows in order", 2, cleaned_rows)

    def normalization() -> None:
        _, rows = read_csv(cleaned_path)
        by_id = {row["order_id"]: row for row in rows}
        require(by_id["A001"]["region"] == "华东", "region whitespace not trimmed")
        require(by_id["A002"]["status"] == "paid", "status not normalized")
        require(by_id["A010"]["status"] == "paid", "status whitespace not trimmed")
        require(all(row["unit_price"].count(".") == 1 and len(row["unit_price"].split(".")[1]) == 2 for row in rows), "price is not fixed to two decimals")

    grade.check("normalizes fields and money formatting", 2, normalization)

    def line_totals() -> None:
        _, rows = read_csv(cleaned_path)
        actual = {row["order_id"]: row["line_total"] for row in rows}
        expected = {
            "A001": "25.00", "A002": "12.00", "A003": "12.50", "A004": "12.00",
            "A005": "50.00", "A006": "6.00", "A007": "8.00", "A010": "25.00",
        }
        require(actual == expected, f"line totals mismatch: {actual}")

    grade.check("computes exact line totals", 1, line_totals)

    def rejected_rows() -> None:
        fields, rows = read_csv(rejected_path)
        require(fields == ["source_line", "order_id", "reason"], f"unexpected rejected columns: {fields}")
        require({int(row["source_line"]) for row in rows} == {10, 11, 12, 14}, f"wrong rejected lines: {rows}")
        require(all(row["reason"].strip() for row in rows), "empty rejection reason")

    grade.check("records the four invalid source rows", 2, rejected_rows)

    def summary() -> None:
        data = json.loads(summary_path.read_text(encoding="utf-8-sig"))
        expected = {
            "valid_orders": 8,
            "duplicate_orders": 1,
            "rejected_orders": 4,
            "paid_orders": 6,
            "paid_units": 23,
            "revenue": "132.00",
            "revenue_by_region": {"华东": "33.00", "华北": "62.00", "华南": "37.00"},
            "revenue_by_product": {"Folder": "20.00", "Notebook": "100.00", "Pen": "12.00"},
        }
        require(data == expected, f"summary mismatch: {data}")

    grade.check("produces the exact summary", 3, summary)

    def report() -> None:
        text = report_path.read_text(encoding="utf-8-sig")
        require(text.lstrip().startswith("# 销售清洗报告"), "wrong report title")
        for needle in ["8", "1", "4", "6", "23", "132.00", "华北"]:
            require(needle in text, f"report missing {needle}")

    grade.check("writes a consistent Markdown report", 2, report)
    return grade.result()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--task", required=True, choices=["bugfix", "data-report"])
    parser.add_argument("--workspace", required=True, type=Path)
    args = parser.parse_args()

    workspace = args.workspace.resolve()
    result = grade_bugfix(workspace) if args.task == "bugfix" else grade_data_report(workspace)
    print(json.dumps(result, ensure_ascii=False))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
