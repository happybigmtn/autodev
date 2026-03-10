#!/usr/bin/env python3
"""Sync generic autodev doctrine into Hermes memory and skills."""

from __future__ import annotations

import argparse
import json
import pathlib
import time
from typing import Any

DEFAULT_HERMES_HOME = pathlib.Path.home() / ".hermes"
DEFAULT_MEMORY_DIR = DEFAULT_HERMES_HOME / "memories"
DEFAULT_SKILLS_DIR = DEFAULT_HERMES_HOME / "skills"
MEMORY_MARKER = "[repo-autodev-framework]"


def build_memory_entry() -> str:
    """Build the generic durable memory entry."""
    return (
        f"{MEMORY_MARKER} Hermes can supervise any repo that adopts the autodev "
        "framework. Treat dev.md as the manual, HERMES.md as the live board, "
        "HERMES_WORKFLOW.json as the machine contract, refresh infrastructure "
        "truth before launching more work, execute the current board-selected "
        "lane item before expanding plans, and keep interactive steering "
        "recovery-only."
    )


def build_skill_text() -> str:
    """Build the generic reusable Hermes skill."""
    return """---
name: repo-autodev-supervisor
description: Supervise any repo that adopts the Hermes/Codex autodev framework. Use the manual/board/contract split, board-first infrastructure refresh, proof-backed lane execution, and recovery-only interactive steering.
version: 0.1.0
author: Codex
license: MIT
---

# Repo Autodev Supervisor

Use this skill when a repo contains `dev.md`, `HERMES.md`, `HERMES_WORKFLOW.json`,
and lane planning surfaces.

## Core rules

1. Treat `dev.md` as the stable manual.
2. Treat `HERMES.md` as the live board and queue.
3. Treat `HERMES_WORKFLOW.json` as the machine-readable policy contract.
4. Refresh infrastructure truth before changing plans or launching more work.
5. Execute the board-selected lane item before expanding plans unless a fresh review proves it is wrong.
6. Treat interactive steering as fallback and recovery only.
"""


def upsert_memory_entry(memory_dir: pathlib.Path, entry: str) -> bool:
    """Add or replace the framework memory entry."""
    memory_dir.mkdir(parents=True, exist_ok=True)
    memory_path = memory_dir / "MEMORY.md"
    existing = memory_path.read_text(encoding="utf-8") if memory_path.exists() else ""
    entries = [item.strip() for item in existing.split("\n§\n") if item.strip()]
    updated = False
    replaced = False
    next_entries: list[str] = []
    for item in entries:
        if MEMORY_MARKER in item:
            next_entries.append(entry)
            updated = True
            replaced = True
        else:
            next_entries.append(item)
    if not replaced:
        next_entries.append(entry)
        updated = True
    memory_path.write_text("\n§\n".join(next_entries) + ("\n" if next_entries else ""), encoding="utf-8")
    user_path = memory_dir / "USER.md"
    if not user_path.exists():
        user_path.write_text("", encoding="utf-8")
    return updated


def write_skill(skills_dir: pathlib.Path, content: str) -> bool:
    """Write the synced skill content."""
    skill_path = skills_dir / "autonomous-ai-agents" / "repo-autodev-supervisor" / "SKILL.md"
    skill_path.parent.mkdir(parents=True, exist_ok=True)
    previous = skill_path.read_text(encoding="utf-8") if skill_path.exists() else ""
    if previous == content:
        return False
    skill_path.write_text(content, encoding="utf-8")
    return True


def sync_doctrine(
    *,
    memory_dir: pathlib.Path = DEFAULT_MEMORY_DIR,
    skills_dir: pathlib.Path = DEFAULT_SKILLS_DIR,
) -> dict[str, Any]:
    """Synchronize generic doctrine into Hermes memory and skills."""
    memory_updated = upsert_memory_entry(memory_dir, build_memory_entry())
    skill_updated = write_skill(skills_dir, build_skill_text())
    return {
        "generated_at": round(time.time(), 3),
        "memory_updated": memory_updated,
        "skill_updated": skill_updated,
        "memory_dir": str(memory_dir),
        "skills_dir": str(skills_dir),
    }


def parse_args() -> argparse.Namespace:
    """Parse CLI args."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--memory-dir", default=str(DEFAULT_MEMORY_DIR))
    parser.add_argument("--skills-dir", default=str(DEFAULT_SKILLS_DIR))
    return parser.parse_args()


def main() -> int:
    """CLI entrypoint."""
    args = parse_args()
    payload = sync_doctrine(
        memory_dir=pathlib.Path(args.memory_dir),
        skills_dir=pathlib.Path(args.skills_dir),
    )
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

