#!/usr/bin/env python3
from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
from pathlib import Path


def fail(message: str) -> None:
    print(f"verification-receipt: {message}", file=sys.stderr)
    raise SystemExit(1)


def repo_root() -> Path:
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True,
        capture_output=True,
        text=True,
    )
    return Path(result.stdout.strip())


def receipt_root(root: Path) -> Path:
    if (
        root.name == "repo"
        and root.parent.parent.name == "lanes"
        and root.parent.parent.parent.name == "parallel"
        and root.parent.parent.parent.parent.name == ".auto"
    ):
        return root.parent.parent.parent.parent / "symphony" / "verification-receipts"
    return root / ".auto" / "symphony" / "verification-receipts"


def receipt_path(root: Path, task_id: str) -> Path:
    return receipt_root(root) / f"{task_id}.json"


def load_receipt(path: Path) -> dict:
    if not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def write_receipt(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def record(args: argparse.Namespace) -> None:
    root = repo_root()
    task_id = args.task_id.strip()
    command = args.command.strip()
    if not task_id:
        fail("task id must not be empty")
    if not command:
        fail("command must not be empty")

    path = receipt_path(root, task_id)
    receipt = load_receipt(path)
    entries = {}
    for entry in receipt.get("commands", []):
        if isinstance(entry, dict) and isinstance(entry.get("command"), str):
            if entry.get("status") == "failed":
                continue
            entries[entry["command"]] = entry

    timestamp = dt.datetime.now(dt.timezone.utc).isoformat()
    entries[command] = {
        "command": command,
        "exit_code": args.exit_code,
        "recorded_at": timestamp,
        "status": "passed" if args.exit_code == 0 else "failed",
    }

    payload = {
        "task_id": task_id,
        "plan_path": "IMPLEMENTATION_PLAN.md",
        "recorded_at": timestamp,
        "commands": [entries[key] for key in sorted(entries)],
    }
    write_receipt(path, payload)
    print(f"verification-receipt: recorded {task_id} -> {command} ({args.exit_code})")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    record_parser = subparsers.add_parser("record")
    record_parser.add_argument("task_id")
    record_parser.add_argument("command")
    record_parser.add_argument("exit_code", type=int)
    record_parser.set_defaults(func=record)
    return parser


def main() -> None:
    args = build_parser().parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
