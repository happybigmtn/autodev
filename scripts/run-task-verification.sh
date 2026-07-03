#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: scripts/run-task-verification.sh <task-id> -- <cmd...>" >&2
}

if [[ $# -lt 2 ]]; then
  usage
  exit 1
fi

task_id=$1
shift
if [[ ${1:-} == "--" ]]; then
  shift
fi
if [[ $# -lt 1 ]]; then
  usage
  exit 1
fi

printf -v command "%q " "$@"
command=${command% }

repo_root=$(git rev-parse --show-toplevel)
stdout_file=$(mktemp)
stderr_file=$(mktemp)
cleanup() {
  rm -f "$stdout_file" "$stderr_file"
}
trap cleanup EXIT

cd "$repo_root"

set +e
"$@" > >(tee "$stdout_file") 2> >(tee "$stderr_file" >&2)
status=$?
set -e

if ! python3 - "$task_id" "$command" "$status" "$stdout_file" "$stderr_file" -- "$@" <<'PY'
from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
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
    if root.name == "repo":
        ancestors = list(root.parents)
        if any(parent.name == "lanes" for parent in ancestors):
            for parent in ancestors:
                if parent.name == ".auto":
                    return parent / "symphony" / "verification-receipts"
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
    tmp = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    tmp.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(tmp, path)


def git_output(root: Path, args: list[str], *, text: bool = True) -> str | bytes | None:
    result = subprocess.run(
        ["git", "-C", str(root), *args],
        check=False,
        capture_output=True,
        text=text,
    )
    if result.returncode != 0:
        return None
    return result.stdout


def sha256_bytes(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def file_sha256(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def current_commit(root: Path) -> str | None:
    output = git_output(root, ["rev-parse", "HEAD"])
    return output.strip() if isinstance(output, str) and output.strip() else None


def dirty_state(root: Path) -> dict | None:
    output = git_output(root, ["status", "--porcelain=v1", "-z"], text=False)
    if not isinstance(output, bytes):
        return None
    entries = [
        entry.decode("utf-8", errors="replace")
        for entry in output.split(b"\0")
        if entry
    ]
    return {
        "status": "clean" if not entries else "dirty",
        "fingerprint": sha256_bytes(output),
        "entries": entries,
    }


def normalize_plan_status_markers(text: str) -> str:
    return (
        text.replace("[x]", "[ ]")
        .replace("[X]", "[ ]")
        .replace("[~]", "[ ]")
        .replace("[!]", "[ ]")
    )


def plan_hash(root: Path) -> str | None:
    path = root / "IMPLEMENTATION_PLAN.md"
    if not path.exists():
        return None
    text = path.read_text(encoding="utf-8")
    return hashlib.sha256(normalize_plan_status_markers(text).encode("utf-8")).hexdigest()


def declared_completion_artifacts(root: Path, task_id: str) -> list[str]:
    plan_path = root / "IMPLEMENTATION_PLAN.md"
    if not plan_path.exists():
        return []
    lines = plan_path.read_text(encoding="utf-8").splitlines()
    start = next(
        (
            index
            for index, line in enumerate(lines)
            if re.search(rf"`{re.escape(task_id)}`", line)
        ),
        None,
    )
    if start is None:
        return []
    block: list[str] = []
    for line in lines[start + 1 :]:
        if re.match(r"\s*-\s+\[[ x~!]\]\s+`[^`]+`", line):
            break
        block.append(line)
    artifacts: list[str] = []
    collecting = False
    for line in block:
        if line.strip().startswith("Completion artifacts:"):
            collecting = True
            remainder = line.split("Completion artifacts:", 1)[1]
            artifacts.extend(artifact_paths_from_line(remainder))
            continue
        if collecting and re.match(r"\s*[A-Z][A-Za-z /-]*:", line):
            break
        if collecting:
            artifacts.extend(artifact_paths_from_line(line))
    return sorted(set(artifact for artifact in artifacts if artifact != "none"))


def artifact_paths_from_line(line: str) -> list[str]:
    fragments = re.findall(r"`([^`]+)`", line)
    if not fragments:
        fragments = re.split(r"[\s,;]+", line)
    paths: list[str] = []
    for fragment in fragments:
        candidate = fragment.strip().strip("`").strip(",;")
        if candidate.endswith(".") and not candidate.startswith("."):
            candidate = candidate[:-1]
        if not candidate or candidate == "none":
            continue
        if "/" in candidate or candidate.endswith((".md", ".rs", ".json", ".toml", ".lock")):
            paths.append(candidate)
    return paths


def declared_artifact_path(root: Path, receipt_file: Path, relative: str) -> Path | None:
    direct = root / relative
    if direct.exists():
        return direct
    prefix = ".auto/symphony/verification-receipts/"
    if relative.startswith(prefix):
        candidate = receipt_file.parent / relative[len(prefix) :]
        if candidate.exists():
            return candidate
    return None


def artifact_hash(path: Path) -> str | None:
    if path.is_file():
        return file_sha256(path)
    if not path.is_dir():
        return None
    hasher = hashlib.sha256()
    for child in sorted(p for p in path.rglob("*") if p.is_file()):
        relative = child.relative_to(path).as_posix()
        hasher.update(relative.encode("utf-8"))
        hasher.update(b"\0")
        hasher.update(file_sha256(child).encode("ascii"))
        hasher.update(b"\0")
    return hasher.hexdigest()


def declared_artifact_hashes(root: Path, receipt_file: Path, task_id: str) -> list[dict]:
    artifacts = []
    for relative in declared_completion_artifacts(root, task_id):
        path = declared_artifact_path(root, receipt_file, relative)
        if path is None:
            continue
        artifact = {"path": relative}
        try:
            if path.resolve() == receipt_file.resolve():
                artifact["sha256"] = None
                artifact["reason"] = "receipt-self"
            else:
                artifact["sha256"] = artifact_hash(path)
        except FileNotFoundError:
            continue
        artifacts.append(artifact)
    return artifacts


def stream_summary(path: str | None, stream_name: str) -> dict:
    raw = b""
    if path and Path(path).exists():
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
    return {
        **stream_summary(stdout_file, "stdout"),
        **stream_summary(stderr_file, "stderr"),
        "redaction_version": REDACTION_VERSION,
    }


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
        counts = cargo_running_test_counts(normalized)
        if counts:
            return not any(count > 0 for count in counts)
        return bool(
            re.search(r"\btest result:\s+ok\.\s+0 passed\b", normalized)
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
        counts = cargo_running_test_counts(normalized)
        return sum(counts) if counts else None
    if kind == "pytest":
        match = re.search(r"\bcollected\s+(\d+)\s+items\b", normalized)
        return int(match.group(1)) if match else None
    return None


def cargo_running_test_counts(normalized_output: str) -> list[int]:
    return [
        int(match.group(1))
        for match in re.finditer(r"\brunning\s+(\d+)\s+tests?\b", normalized_output)
    ]


def split_supersedes(raw: str | None) -> list[str]:
    if not raw:
        return []
    return [item.strip() for item in raw.splitlines() if item.strip()]


def record() -> None:
    if len(sys.argv) < 7:
        fail("internal invocation missing arguments")
    task_id = sys.argv[1].strip()
    command = sys.argv[2].strip()
    try:
        exit_code = int(sys.argv[3])
    except ValueError:
        fail(f"invalid exit code: {sys.argv[3]}")
    stdout_file = sys.argv[4]
    stderr_file = sys.argv[5]
    try:
        separator = sys.argv.index("--", 6)
        argv = sys.argv[separator + 1 :]
    except ValueError:
        argv = sys.argv[6:]
    if not task_id:
        fail("task id must not be empty")
    if not command:
        fail("command must not be empty")

    root = repo_root()
    path = receipt_path(root, task_id)
    receipt = load_receipt(path)
    entries = {}
    for entry in receipt.get("commands", []):
        if isinstance(entry, dict) and isinstance(entry.get("command"), str):
            entries[entry["command"]] = entry

    timestamp = dt.datetime.now(dt.timezone.utc).isoformat()
    captured_output = output_summary(stdout_file, stderr_file)
    try:
        expected_argv = shlex.split(command)
    except ValueError:
        expected_argv = []
    command_entry = {
        "command": command,
        "argv": argv,
        "expected_argv": expected_argv,
        "exit_code": exit_code,
        "output_summary": captured_output,
        "recorded_at": timestamp,
        "status": "passed" if exit_code == 0 else "failed",
    }
    supersedes = split_supersedes(os.environ.get("AUTO_SUPERSEDES"))
    if supersedes:
        command_entry["supersedes"] = supersedes
    captured_runner = runner_summary(command, argv, captured_output)
    if captured_runner is not None:
        command_entry["runner_summary"] = captured_runner

    entries[command] = command_entry
    payload = {
        "task_id": task_id,
        "plan_path": "IMPLEMENTATION_PLAN.md",
        "plan_hash": plan_hash(root),
        "commit": current_commit(root),
        "dirty_state": dirty_state(root),
        "declared_artifacts": declared_artifact_hashes(root, path, task_id),
        "recorded_at": timestamp,
        "commands": [entries[key] for key in sorted(entries)],
    }
    write_receipt(path, payload)
    print(f"verification-receipt: recorded {task_id} -> {command} ({exit_code})")


if __name__ == "__main__":
    record()
PY
then
  if [[ "$status" -eq 0 ]]; then
    echo "verification-receipt: error: command passed but receipt recording failed" >&2
    exit 1
  fi
  echo "verification-receipt: warning: failed to record receipt" >&2
fi

exit "$status"
