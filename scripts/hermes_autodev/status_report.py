#!/usr/bin/env python3
"""Summarize autodev runtime status for any adopted repo."""

from __future__ import annotations

import argparse
import json
import pathlib
from typing import Any


def parse_args() -> argparse.Namespace:
    """Parse CLI args."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", default=".")
    return parser.parse_args()


def load_json(path: pathlib.Path, default: Any) -> Any:
    """Load JSON or return a default."""
    if not path.exists():
        return default
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return default


def build_status(repo_root: pathlib.Path) -> dict[str, Any]:
    """Build a compact status summary."""
    repo_state = load_json(repo_root / "log" / "autonomy" / "repo_state.json", {})
    confidence = load_json(repo_root / "log" / "autonomy" / "confidence_report.json", {})
    runtime_inventory = repo_state.get("runtime_inventory") if isinstance(repo_state, dict) else {}
    lanes_map = runtime_inventory.get("lanes") if isinstance(runtime_inventory, dict) else {}
    lane_cards = {}
    if isinstance(confidence, dict):
        lane_cards = ((confidence.get("lane_scorecard") or {}).get("lanes") or {})
        lane_cards = lane_cards if isinstance(lane_cards, dict) else {}
    lanes: dict[str, Any] = {}
    lane_names = set(lanes_map.keys()) | set(lane_cards.keys())
    for lane in sorted(lane_names):
        lane_review = load_json(
            repo_root / "log" / "autonomy" / "reviews" / "non_interactive" / lane / "latest.json",
            {},
        )
        card = lane_cards.get(lane) if isinstance(lane_cards.get(lane), dict) else {}
        inventory = lanes_map.get(lane) if isinstance(lanes_map.get(lane), dict) else {}
        lanes[lane] = {
            "status": card.get("status"),
            "session_up": card.get("session_up"),
            "selected_item_id": card.get("selected_item_id") or inventory.get("item_id"),
            "review_mode": lane_review.get("review_mode"),
            "review_state": lane_review.get("review_state"),
            "recommended_next_action": lane_review.get("recommended_next_action"),
        }
    overall = (((confidence.get("balanced_scorecard") or {}).get("overall")) or {}) if isinstance(confidence, dict) else {}
    promotion_gate = (((confidence.get("balanced_scorecard") or {}).get("promotion_gate")) or {}) if isinstance(confidence, dict) else {}
    return {
        "overall_status": overall.get("status"),
        "overall_score": overall.get("score"),
        "promotion_gate": promotion_gate.get("status"),
        "attention_lanes": repo_state.get("attention_lanes"),
        "lanes": lanes,
    }


def main() -> int:
    """CLI entrypoint."""
    args = parse_args()
    payload = build_status(pathlib.Path(args.repo_root).resolve())
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

