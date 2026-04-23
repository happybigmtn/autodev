#!/usr/bin/env python3
from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import shlex
import subprocess
import sys
from pathlib import Path

OUTPUT_TAIL_BYTES = 16 * 1024
REDACTION_VERSION = 1


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
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.decoder.JSONDecodeError as exc:
        fail(f"corrupted receipt at {path}: {exc}")


def write_receipt(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def stream_summary(path: str | None, stream_name: str) -> dict:
    raw = b""
    if path:
        raw = Path(path).read_bytes()
    text = raw.decode("utf-8", errors="replace")
    redacted = redact_output(text)
    redacted_bytes = redacted.encode("utf-8")
    truncated = len(redacted_bytes) > OUTPUT_TAIL_BYTES
    tail_bytes = redacted_bytes[-OUTPUT_TAIL_BYTES:] if truncated else redacted_bytes
    tail = tail_bytes.decode("utf-8", errors="replace")
    return {
        f"{stream_name}_tail": tail,
        f"{stream_name}_bytes": len(raw),
        f"{stream_name}_truncated": truncated,
    }


def output_summary(stdout_file: str | None, stderr_file: str | None) -> dict:
    summary = {
        **stream_summary(stdout_file, "stdout"),
        **stream_summary(stderr_file, "stderr"),
        "redaction_version": REDACTION_VERSION,
    }
    return summary


def redact_output(text: str) -> str:
    redacted = text
    redacted = re.sub(
        r"(?i)\b([A-Z0-9_]*(?:TOKEN|PASSWORD|SECRET|API_KEY|AUTH)[A-Z0-9_]*)=([^\s]+)",
        r"\1=[REDACTED]",
        redacted,
    )
    redacted = re.sub(
        r"(?i)(Authorization:\s*Bearer\s+)[^\s]+",
        r"\1[REDACTED]",
        redacted,
    )
    redacted = re.sub(r"\bgh[pousr]_[A-Za-z0-9_]{20,}\b", "[REDACTED_GITHUB_TOKEN]", redacted)
    redacted = re.sub(r"\bsk-ant-[A-Za-z0-9_-]{20,}\b", "[REDACTED_ANTHROPIC_KEY]", redacted)
    redacted = re.sub(r"\bsk-[A-Za-z0-9_-]{20,}\b", "[REDACTED_OPENAI_KEY]", redacted)
    return redacted


def runner_summary(command: str, argv: list[str], output: dict) -> dict | None:
    try:
        parsed_argv = argv or shlex.split(command)
    except ValueError:
        return None
    kind = runner_kind(parsed_argv)
    if kind is None:
        return None
    combined_output = f"{output['stdout_tail']}\n{output['stderr_tail']}"
    zero_test_detected = detects_zero_tests(kind, combined_output)
    summary = {
        "kind": kind,
        "zero_test_detected": zero_test_detected,
    }
    count = discovered_test_count(kind, combined_output)
    if count is not None:
        summary["tests_discovered"] = count
        summary["tests_run"] = count
    return summary


def runner_kind(argv: list[str]) -> str | None:
    if len(argv) >= 2 and argv[0] == "cargo" and argv[1] == "test":
        return "cargo-test"
    if len(argv) >= 3 and argv[0] == "cargo" and argv[1] == "nextest" and argv[2] == "run":
        return "cargo-nextest"
    if argv and Path(argv[0]).name == "pytest":
        return "pytest"
    if (
        len(argv) >= 3
        and Path(argv[0]).name in {"python", "python3"}
        and argv[1:3] == ["-m", "pytest"]
    ):
        return "pytest"
    return None


def detects_zero_tests(kind: str, output: str) -> bool:
    normalized = output.lower()
    if kind in {"cargo-test", "cargo-nextest"}:
        return bool(
            re.search(r"\brunning\s+0\s+tests\b", normalized)
            or re.search(r"\btest result:\s+ok\.\s+0 passed\b", normalized)
            or re.search(r"\b0\s+tests?\s+run\b", normalized)
        )
    if kind == "pytest":
        return bool(
            re.search(r"\bcollected\s+0\s+items\b", normalized)
            or re.search(r"\b0\s+items\s+collected\b", normalized)
            or re.search(r"\bno tests ran\b", normalized)
        )
    return False


def discovered_test_count(kind: str, output: str) -> int | None:
    normalized = output.lower()
    if kind in {"cargo-test", "cargo-nextest"}:
        match = re.search(r"\brunning\s+(\d+)\s+tests?\b", normalized)
        return int(match.group(1)) if match else None
    if kind == "pytest":
        match = re.search(r"\bcollected\s+(\d+)\s+items\b", normalized)
        return int(match.group(1)) if match else None
    return None


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
            entries[entry["command"]] = entry

    timestamp = dt.datetime.now(dt.timezone.utc).isoformat()
    captured_output = output_summary(args.stdout_file, args.stderr_file)
    command_entry = {
        "command": command,
        "argv": args.argv,
        "exit_code": args.exit_code,
        "output_summary": captured_output,
        "recorded_at": timestamp,
        "status": "passed" if args.exit_code == 0 else "failed",
    }
    captured_runner = runner_summary(command, args.argv, captured_output)
    if captured_runner is not None:
        command_entry["runner_summary"] = captured_runner

    entries[command] = command_entry

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
    record_parser.add_argument("--argv", action="append", default=[])
    record_parser.add_argument("--stdout-file")
    record_parser.add_argument("--stderr-file")
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
