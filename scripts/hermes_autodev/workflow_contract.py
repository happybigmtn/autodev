#!/usr/bin/env python3
"""Load the repo-owned Hermes workflow contract for any adopted repo."""

from __future__ import annotations

import json
import pathlib
from dataclasses import dataclass
from typing import Any

DEFAULT_WORKFLOW_PATH = "HERMES_WORKFLOW.json"
DEFAULT_ACTIVE_STATES = (
    "review_due",
    "recon_running",
    "bug_sweep_running",
    "skeptic_running",
    "nemesis_feynman_running",
    "nemesis_state_running",
    "product_truth_running",
    "synthesizing",
    "execution_ready",
)
DEFAULT_TERMINAL_STATES = (
    "reviewed_changed",
    "reviewed_no_change",
    "blocked_review",
)


@dataclass(frozen=True)
class ReviewMode:
    """Typed review mode."""

    name: str
    passes: tuple[str, ...]
    max_attempts: int


@dataclass(frozen=True)
class LaneScope:
    """Typed lane scope for expansive audits."""

    owned_roots: tuple[str, ...]
    adjacent_roots: tuple[str, ...]
    product_truth_surfaces: tuple[str, ...]


@dataclass(frozen=True)
class WorkflowContract:
    """Typed workflow contract."""

    path: pathlib.Path
    lanes: tuple[str, ...]
    product_lanes: tuple[str, ...]
    review_active_states: tuple[str, ...]
    review_terminal_states: tuple[str, ...]
    lane_modes: dict[str, ReviewMode]
    portfolio_mode: ReviewMode
    lane_scopes: dict[str, LaneScope]


def load_json(path: pathlib.Path) -> dict[str, Any]:
    """Load a JSON object from disk."""
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise ValueError(f"missing workflow contract: {path}") from exc
    except json.JSONDecodeError as exc:
        raise ValueError(f"invalid workflow contract JSON: {path}: {exc}") from exc
    if not isinstance(payload, dict):
        raise ValueError(f"workflow contract must be a JSON object: {path}")
    return payload


def string_list(value: Any, default: tuple[str, ...] = ()) -> tuple[str, ...]:
    """Normalize a list of strings."""
    if not isinstance(value, list):
        return default
    items = [str(item).strip() for item in value if str(item).strip()]
    return tuple(items) or default


def parse_mode(name: str, raw: Any, default_passes: tuple[str, ...]) -> ReviewMode:
    """Parse one review mode."""
    if not isinstance(raw, dict):
        return ReviewMode(name=name, passes=default_passes, max_attempts=1)
    passes = string_list(raw.get("passes"), default_passes)
    max_attempts = max(1, int(raw.get("max_attempts", 1) or 1))
    return ReviewMode(name=name, passes=passes, max_attempts=max_attempts)


def parse_lane_scopes(raw: Any) -> dict[str, LaneScope]:
    """Parse lane scope metadata."""
    if not isinstance(raw, dict):
        return {}
    scopes: dict[str, LaneScope] = {}
    for lane, value in raw.items():
        if not isinstance(value, dict):
            continue
        scopes[str(lane)] = LaneScope(
            owned_roots=string_list(value.get("owned_roots")),
            adjacent_roots=string_list(value.get("adjacent_roots")),
            product_truth_surfaces=string_list(value.get("product_truth_surfaces")),
        )
    return scopes


def load_contract(
    repo_root: pathlib.Path,
    contract_path: pathlib.Path | None = None,
) -> WorkflowContract:
    """Load the workflow contract for an adopted repo."""
    path = contract_path or (repo_root / DEFAULT_WORKFLOW_PATH)
    payload = load_json(path)
    review = payload.get("review")
    if not isinstance(review, dict):
        raise ValueError("workflow contract is missing `review`")
    lanes = string_list(review.get("lanes"))
    if not lanes:
        raise ValueError("workflow contract must declare at least one lane")
    states = review.get("states")
    states = states if isinstance(states, dict) else {}
    raw_modes = review.get("lane_modes")
    raw_modes = raw_modes if isinstance(raw_modes, dict) else {}
    lane_modes = {
        "maintenance_review": parse_mode(
            "maintenance_review",
            raw_modes.get("maintenance_review"),
            ("maintenance_synthesis",),
        ),
        "expansive_audit_review": parse_mode(
            "expansive_audit_review",
            raw_modes.get("expansive_audit_review"),
            (
                "recon",
                "bugs_finder",
                "bugs_skeptic",
                "nemesis_feynman",
                "nemesis_state",
                "product_truth",
                "synthesis",
            ),
        ),
    }
    return WorkflowContract(
        path=path,
        lanes=lanes,
        product_lanes=string_list(review.get("product_lanes"), lanes),
        review_active_states=string_list(states.get("active"), DEFAULT_ACTIVE_STATES),
        review_terminal_states=string_list(states.get("terminal"), DEFAULT_TERMINAL_STATES),
        lane_modes=lane_modes,
        portfolio_mode=parse_mode(
            "portfolio_review",
            review.get("portfolio"),
            ("portfolio_synthesis",),
        ),
        lane_scopes=parse_lane_scopes(review.get("lane_scope")),
    )


__all__ = [
    "DEFAULT_WORKFLOW_PATH",
    "LaneScope",
    "ReviewMode",
    "WorkflowContract",
    "load_contract",
]

