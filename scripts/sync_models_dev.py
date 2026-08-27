#!/usr/bin/env python3
"""Compact https://models.dev/api.json into a model-metadata snapshot.

Writes crates/codegen/xai-grok-shell/src/compat/data/models_dev_reasoning.json
(committed bake-at-release fallback). Users refresh at runtime with
`/sync-models-dev` (writes ~/.grok/models-dev-reasoning.json).

Each entry may include:
  kind/values/default — reasoning-effort menu (omitted when the model is off)
  context / output    — limit.context and limit.output

Network failure must not overwrite an existing snapshot.

Usage:
  python3 scripts/sync_models_dev.py
  python3 scripts/sync_models_dev.py --input /tmp/api.json --output /tmp/out.json
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.request
from pathlib import Path
from typing import Any

MODELS_DEV_URL = "https://models.dev/api.json"
KNOWN = ("none", "minimal", "low", "medium", "high", "xhigh", "max")
DATE_SUFFIX = re.compile(r"-\d{4}-\d{2}-\d{2}$")
MIN_CONTEXT = 4_096
MAX_CONTEXT = 16_000_000
MAX_OUTPUT = 16_000_000


def normalize_model_id(model_id: str) -> str:
    s = model_id.strip().lower().replace("_", "-")
    if "/" in s:
        s = s.rsplit("/", 1)[-1]
    s = s.replace(".", "-")
    s = DATE_SUFFIX.sub("", s)
    return s


def effort_values(options: list[Any] | None) -> tuple[str, list[str]] | None:
    """Return (kind, values) or None if this model should stay off."""
    if not options:
        return None
    efforts: list[str] = []
    has_toggle = False
    for opt in options:
        if not isinstance(opt, dict):
            continue
        kind = opt.get("type")
        if kind == "toggle":
            has_toggle = True
        elif kind == "effort":
            raw = opt.get("values") or []
            for item in raw:
                if item is None:
                    continue
                token = str(item).strip().lower()
                if token in KNOWN and token not in efforts:
                    efforts.append(token)
        # budget_tokens ignored
    if efforts:
        return ("effort", efforts)
    if has_toggle:
        return ("toggle", ["none", "high"])
    return None


def pick_default(kind: str, values: list[str]) -> str:
    if kind == "toggle":
        return "high"
    if "none" in values:
        return "none"
    return values[-1]


def _as_int(value: Any) -> int | None:
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float) and value.is_integer():
        return int(value)
    return None


def parse_limit(model: dict[str, Any]) -> tuple[int | None, int | None]:
    limit = model.get("limit")
    if not isinstance(limit, dict):
        return None, None
    context = _as_int(limit.get("context"))
    output = _as_int(limit.get("output"))
    if context is None or context < MIN_CONTEXT or context > MAX_CONTEXT:
        context = None
    if output is None or output < 1 or output > MAX_OUTPUT:
        output = None
    return context, output


PREFERRED_PROVIDERS = (
    "openai",
    "anthropic",
    "xai",
    "zai",
    "minimax",
    "google",
    "deepseek",
)


def ingest_provider(
    out: dict[str, dict[str, Any]],
    claimed: set[str],
    provider_id: str,
    provider: dict[str, Any],
    *,
    preferred: bool,
) -> None:
    models = provider.get("models") or {}
    if not isinstance(models, dict):
        return
    for model_id, model in models.items():
        if not isinstance(model, dict):
            continue
        key = normalize_model_id(str(model.get("id") or model_id))
        if not key:
            continue
        parsed = effort_values(model.get("reasoning_options"))
        context, output = parse_limit(model)
        if preferred:
            claimed.add(key)
        if parsed is None and context is None and output is None:
            continue
        if key in out:
            continue
        if key in claimed and not preferred:
            continue
        entry: dict[str, Any] = {}
        if parsed is not None:
            kind, values = parsed
            entry["kind"] = kind
            entry["values"] = values
            entry["default"] = pick_default(kind, values)
        if context is not None:
            entry["context"] = context
        if output is not None:
            entry["output"] = output
        out[key] = entry


def compact_catalog(api: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, dict[str, Any]] = {}
    claimed: set[str] = set()
    for pid in PREFERRED_PROVIDERS:
        provider = api.get(pid)
        if isinstance(provider, dict):
            ingest_provider(out, claimed, pid, provider, preferred=True)
    for pid, provider in api.items():
        if pid in PREFERRED_PROVIDERS or not isinstance(provider, dict):
            continue
        ingest_provider(out, claimed, pid, provider, preferred=False)
    return dict(sorted(out.items()))


def load_api(source: str | None) -> dict[str, Any]:
    if source:
        with open(source, encoding="utf-8") as f:
            return json.load(f)
    req = urllib.request.Request(
        MODELS_DEV_URL,
        headers={"User-Agent": "grok-build-compat-models-dev-sync"},
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.load(resp)


def default_output() -> Path:
    root = Path(__file__).resolve().parents[1]
    return root / "crates/codegen/xai-grok-shell/src/compat/data/models_dev_reasoning.json"


def _self_test() -> None:
    api = {
        "openai": {
            "models": {
                "gpt-5.6-sol": {
                    "id": "gpt-5.6-sol",
                    "reasoning_options": [
                        {"type": "effort", "values": ["none", "low", "medium", "high", "xhigh", "max"]}
                    ],
                    "limit": {"context": 1050000, "output": 128000},
                },
                "gpt-4o": {
                    "id": "gpt-4o",
                    "reasoning": False,
                    "limit": {"context": 128000, "output": 16384},
                },
            }
        },
        "other": {
            "models": {
                "openai/gpt-5.6-sol": {
                    "id": "openai/gpt-5.6-sol",
                    "reasoning_options": [{"type": "effort", "values": ["low", "high"]}],
                },
                "minimax-m3": {
                    "id": "minimax-m3",
                    "reasoning_options": [{"type": "toggle"}],
                },
            }
        },
    }
    compact = compact_catalog(api)
    assert compact["gpt-5-6-sol"]["values"] == [
        "none",
        "low",
        "medium",
        "high",
        "xhigh",
        "max",
    ], compact
    assert compact["gpt-5-6-sol"]["default"] == "none"
    assert compact["gpt-5-6-sol"]["context"] == 1_050_000
    assert compact["gpt-5-6-sol"]["output"] == 128_000
    assert compact["gpt-4o"] == {"context": 128000, "output": 16384}
    assert compact["minimax-m3"]["kind"] == "toggle"
    assert compact["minimax-m3"]["values"] == ["none", "high"]
    assert compact["minimax-m3"]["default"] == "high"
    preferred = {
        "zai": {
            "models": {
                "glm-5.3": {
                    "id": "glm-5.3",
                    "reasoning_options": [
                        {"type": "effort", "values": ["low", "high", "max"]},
                    ],
                }
            }
        },
        "dump": {
            "models": {
                "glm-5.3": {
                    "id": "glm-5.3",
                    "reasoning_options": [
                        {"type": "effort", "values": list(KNOWN)},
                    ],
                }
            }
        },
    }
    glm = compact_catalog(preferred)["glm-5-3"]
    assert glm["values"] == ["low", "high", "max"], glm
    assert glm["default"] == "max"
    assert normalize_model_id("anthropic/claude-sonnet-4.6") == "claude-sonnet-4-6"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", help="Local models.dev api.json (skip network)")
    parser.add_argument("--output", help="Snapshot path")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        _self_test()
        print("ok", file=sys.stderr)
        return 0
    output = Path(args.output) if args.output else default_output()
    try:
        api = load_api(args.input)
        compact = compact_catalog(api)
        if not compact:
            raise SystemExit("compact catalog is empty")
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(compact, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"wrote {len(compact)} models -> {output}", file=sys.stderr)
        return 0
    except Exception as e:
        if output.exists() and not args.input:
            print(f"sync failed ({e}); keeping existing {output}", file=sys.stderr)
            return 0
        print(f"sync failed: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
