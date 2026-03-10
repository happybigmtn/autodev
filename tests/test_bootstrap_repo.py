#!/usr/bin/env python3
"""Tests for the generic repo bootstrap scaffold."""

from __future__ import annotations

import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "scripts" / "hermes_autodev"))

import bootstrap_repo


class TestBootstrapRepo(unittest.TestCase):
    def test_bootstrap_repo_creates_core_surfaces(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            target = pathlib.Path(td) / "sample"
            payload = bootstrap_repo.bootstrap_repo(
                target=target,
                lanes=["backend", "frontend", "ops"],
                force=False,
            )

            self.assertTrue(payload["ok"])
            self.assertTrue((target / "dev.md").exists())
            self.assertTrue((target / "HERMES.md").exists())
            self.assertTrue((target / "HERMES_WORKFLOW.json").exists())
            self.assertTrue((target / "lanes" / "backend" / "PLANS.md").exists())
            self.assertTrue((target / "lanes" / "frontend" / "SPEC.md").exists())
            self.assertTrue((target / "lanes" / "ops" / "IMPLEMENTATION.md").exists())
            self.assertTrue((target / "log" / "autonomy" / "results" / "backend" / ".gitkeep").exists())
            text = (target / "dev.md").read_text(encoding="utf-8")
            self.assertIn("- backend", text)
            workflow = (target / "HERMES_WORKFLOW.json").read_text(encoding="utf-8")
            self.assertIn('"backend"', workflow)


if __name__ == "__main__":
    unittest.main()

