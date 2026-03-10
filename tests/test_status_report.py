#!/usr/bin/env python3
"""Tests for the generic status report helper."""

from __future__ import annotations

import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "scripts" / "hermes_autodev"))

import status_report


def write_json(path: pathlib.Path, payload: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")


class TestStatusReport(unittest.TestCase):
    def test_build_status_reads_repo_state_and_reviews(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            write_json(
                root / "log" / "autonomy" / "repo_state.json",
                {
                    "attention_lanes": 0,
                    "runtime_inventory": {"lanes": {"frontend": {"item_id": "FRT-P0-02"}}},
                },
            )
            write_json(
                root / "log" / "autonomy" / "confidence_report.json",
                {
                    "balanced_scorecard": {
                        "overall": {"status": "pass", "score": 0.9},
                        "promotion_gate": {"status": "watch"},
                    },
                    "lane_scorecard": {
                        "lanes": {
                            "frontend": {
                                "status": "watch",
                                "session_up": True,
                                "selected_item_id": "FRT-P0-02",
                            }
                        }
                    },
                },
            )
            write_json(
                root / "log" / "autonomy" / "reviews" / "non_interactive" / "frontend" / "latest.json",
                {
                    "review_mode": "non_interactive_review",
                    "review_state": "reviewed_no_change",
                    "recommended_next_action": "Re-steer frontend",
                },
            )

            payload = status_report.build_status(root)

            self.assertEqual(payload["overall_status"], "pass")
            self.assertEqual(payload["promotion_gate"], "watch")
            self.assertEqual(payload["lanes"]["frontend"]["selected_item_id"], "FRT-P0-02")
            self.assertEqual(payload["lanes"]["frontend"]["review_mode"], "non_interactive_review")


if __name__ == "__main__":
    unittest.main()
