#!/usr/bin/env python3
"""Tests for the generic Hermes doctrine sync."""

from __future__ import annotations

import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "scripts" / "hermes_autodev"))

import doctrine_sync


class TestDoctrineSync(unittest.TestCase):
    def test_sync_updates_memory_and_skill(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            memory_dir = root / "memories"
            skills_dir = root / "skills"
            payload = doctrine_sync.sync_doctrine(memory_dir=memory_dir, skills_dir=skills_dir)

            self.assertTrue(payload["memory_updated"])
            self.assertTrue(payload["skill_updated"])
            memory_text = (memory_dir / "MEMORY.md").read_text(encoding="utf-8")
            self.assertIn("[repo-autodev-framework]", memory_text)
            skill_text = (
                skills_dir
                / "autonomous-ai-agents"
                / "repo-autodev-supervisor"
                / "SKILL.md"
            ).read_text(encoding="utf-8")
            self.assertIn("Execute the board-selected lane item", skill_text)


if __name__ == "__main__":
    unittest.main()

