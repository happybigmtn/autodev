#!/usr/bin/env python3
"""Scaffold the Hermes/Codex autodev framework into a target repo."""

from __future__ import annotations

import argparse
import json
import pathlib
from datetime import date
from typing import Any

FRAMEWORK_ROOT = pathlib.Path(__file__).resolve().parents[2]
TEMPLATES_ROOT = FRAMEWORK_ROOT / "templates"


def parse_args() -> argparse.Namespace:
    """Parse CLI args."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--lanes", required=True, help="Comma-separated lane names")
    parser.add_argument("--force", action="store_true")
    return parser.parse_args()


def lane_prefix(lane: str) -> str:
    """Return a stable lane prefix for starter item IDs."""
    letters = "".join(char for char in lane.upper() if char.isalpha())
    return (letters[:3] or "LAN").ljust(3, "X")


def lane_title(lane: str) -> str:
    """Return a readable lane title."""
    return lane.replace("-", " ").title()


def read_template(relative_path: str) -> str:
    """Read a template from the framework repo."""
    return (TEMPLATES_ROOT / relative_path).read_text(encoding="utf-8")


def render(text: str, lanes: list[str], lane: str | None = None) -> str:
    """Render shared template placeholders."""
    lane_bullets = "\n".join(f"- {item}" for item in lanes)
    lane_inline = ", ".join(lanes)
    lane_json = json.dumps(lanes, indent=4)
    lane_scope = {
        item: {
            "owned_roots": [f"src/{item}"],
            "adjacent_roots": ["docs", "log/autonomy"],
            "product_truth_surfaces": [f"src/{item}", f"lanes/{item}/SPEC.md"],
        }
        for item in lanes
    }
    rendered = (
        text.replace("{{LANE_BULLETS}}", lane_bullets)
        .replace("{{LANE_INLINE}}", lane_inline)
        .replace("{{LANE_JSON}}", lane_json)
        .replace("{{LANE_SCOPE_JSON}}", json.dumps(lane_scope, indent=4))
        .replace("{{LAST_REFRESHED}}", date.today().isoformat())
    )
    if lane is not None:
        rendered = (
            rendered.replace("{{LANE_NAME}}", lane)
            .replace("{{LANE_TITLE}}", lane_title(lane))
            .replace("{{LANE_PREFIX}}", lane_prefix(lane))
        )
    return rendered


def write_file(path: pathlib.Path, text: str, force: bool) -> bool:
    """Write one file unless it already exists."""
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists() and not force:
        return False
    path.write_text(text, encoding="utf-8")
    return True


def touch_gitkeep(path: pathlib.Path) -> None:
    """Create a directory placeholder."""
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        path.write_text("", encoding="utf-8")


def bootstrap_repo(target: pathlib.Path, lanes: list[str], force: bool) -> dict[str, Any]:
    """Scaffold the framework into a target repo."""
    changed: list[str] = []
    for relative_path in ("dev.md", "HERMES.md", "HERMES_WORKFLOW.json"):
        if write_file(target / relative_path, render(read_template(relative_path), lanes), force):
            changed.append(relative_path)
    for lane in lanes:
        for template_name in ("PLANS.md", "SPEC.md", "IMPLEMENTATION.md"):
            relative = pathlib.Path("lanes") / lane / template_name
            text = render(read_template(f"lanes/{template_name}"), lanes, lane)
            if write_file(target / relative, text, force):
                changed.append(str(relative))
    gitkeeps = [
        target / "log" / "autonomy" / "results" / ".gitkeep",
        target / "log" / "autonomy" / "learning" / "attempts" / ".gitkeep",
        target / "log" / "autonomy" / "reviews" / "lanes" / ".gitkeep",
        target / "log" / "autonomy" / "reviews" / "non_interactive" / ".gitkeep",
        target / "log" / "autonomy" / "control" / "packets" / ".gitkeep",
        target / "log" / "autonomy" / "control" / "modes" / ".gitkeep",
    ]
    for lane in lanes:
        gitkeeps.extend(
            [
                target / "log" / "autonomy" / "results" / lane / ".gitkeep",
                target / "log" / "autonomy" / "reviews" / "lanes" / lane / ".gitkeep",
                target / "log" / "autonomy" / "reviews" / "non_interactive" / lane / ".gitkeep",
                target / "log" / "autonomy" / "control" / "packets" / lane / ".gitkeep",
                target / "log" / "autonomy" / "control" / "modes" / lane / ".gitkeep",
            ]
        )
    for path in gitkeeps:
        touch_gitkeep(path)
    return {
        "ok": True,
        "target": str(target),
        "lanes": lanes,
        "changed_files": changed,
    }


def main() -> int:
    """CLI entrypoint."""
    args = parse_args()
    lanes = [item.strip() for item in args.lanes.split(",") if item.strip()]
    if not lanes:
        raise SystemExit("at least one lane is required")
    payload = bootstrap_repo(pathlib.Path(args.target), lanes, args.force)
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

