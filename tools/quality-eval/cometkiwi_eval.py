#!/usr/bin/env python3
"""Deterministic COMETKiwi sidecar evaluation for public mimus IL artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
DEFAULT_MODEL = "Unbabel/wmt20-comet-qe-da"
PLACEHOLDER = re.compile(r"(?:</?b\d+>|\{[vl]\d+\})")


def paragraph_pairs(source_il: dict[str, Any], translated_il: dict[str, Any]) -> list[dict[str, Any]]:
    translated = {
        (page["index"], paragraph["reading_order"]): paragraph
        for page in translated_il["pages"]
        for paragraph in page["paragraphs"]
    }
    pairs: list[dict[str, Any]] = []
    for page in source_il["pages"]:
        for paragraph in page["paragraphs"]:
            chars = paragraph["text"]["chars"]
            if not any(_policy(char) == "translate" for char in chars):
                continue
            output = translated.get((page["index"], paragraph["reading_order"]))
            if not output or output.get("preserved") is not None or not output.get("translated_text"):
                continue
            formula_units = _formula_units(chars)
            source = _source_text(chars)
            translation = output["translated_text"]
            source = _clean(source, formula_units)
            translation = _clean(translation, formula_units)
            if not source or not translation or source == translation:
                continue
            pairs.append(
                {
                    "page_index": page["index"],
                    "reading_order": paragraph["reading_order"],
                    "source": source,
                    "translation": translation,
                }
            )
    return sorted(pairs, key=lambda pair: (pair["page_index"], pair["reading_order"]))


def _formula_units(chars: list[dict[str, Any]]) -> list[str]:
    units: list[str] = []
    current: list[str] = []
    for char in chars:
        if _label(char) in {"inline_formula", "display_formula"}:
            current.append(char.get("unicode") or "")
        elif current:
            units.append("".join(current))
            current = []
    if current:
        units.append("".join(current))
    return [unit for unit in units if unit]


def _source_text(chars: list[dict[str, Any]]) -> str:
    output: list[str] = []
    previous: tuple[int, dict[str, Any]] | None = None
    for index, char in enumerate(chars):
        if _policy(char) != "translate" or _label(char) in {"inline_formula", "display_formula"}:
            continue
        if previous is not None and _word_break(previous[0], previous[1], index, char):
            output.append(" ")
        output.append(char.get("unicode") or "")
        previous = (index, char)
    return "".join(output)


def _word_break(
    previous_index: int,
    previous: dict[str, Any],
    index: int,
    current: dict[str, Any],
) -> bool:
    if index != previous_index + 1:
        return True
    previous_baseline = float((previous.get("baseline_origin") or {}).get("y", 0.0))
    current_baseline = float((current.get("baseline_origin") or {}).get("y", 0.0))
    font_size = max(float(previous.get("font_size", 0.0)), float(current.get("font_size", 0.0)), 1.0)
    if abs(previous_baseline - current_baseline) > font_size * 0.35:
        return True
    previous_right = float((previous.get("box") or {}).get("right", 0.0))
    current_left = float((current.get("box") or {}).get("left", 0.0))
    return current_left - previous_right > font_size * 0.15


def _clean(text: str, formula_units: list[str]) -> str:
    text = PLACEHOLDER.sub("", text)
    for unit in sorted({unit for unit in formula_units if len(unit) > 1}, key=len, reverse=True):
        text = text.replace(unit, "")
    return " ".join(text.split())


def _layout(char: dict[str, Any]) -> dict[str, Any]:
    return char.get("layout") or {}


def _policy(char: dict[str, Any]) -> str:
    return str(_layout(char).get("policy", ""))


def _label(char: dict[str, Any]) -> str:
    return str(_layout(char).get("label", ""))


def percentile(values: list[float], quantile: float) -> float:
    if not values:
        raise ValueError("cannot compute a percentile of no scores")
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    fraction = position - lower
    return ordered[lower] * (1.0 - fraction) + ordered[upper] * fraction


def tree_sha256(root: Path) -> tuple[str, list[dict[str, str]]]:
    digest = hashlib.sha256()
    files: list[dict[str, str]] = []
    for path in sorted(item for item in root.rglob("*") if item.is_file()):
        relative = path.relative_to(root).as_posix()
        file_hash = hashlib.sha256(path.read_bytes()).hexdigest()
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(bytes.fromhex(file_hash))
        files.append({"path": relative, "sha256": file_hash})
    return digest.hexdigest(), files


def evaluate(args: argparse.Namespace) -> dict[str, Any]:
    source_il = json.loads(args.source_il.read_text(encoding="utf-8"))
    translated_il = json.loads(args.translated_il.read_text(encoding="utf-8"))
    pairs = paragraph_pairs(source_il, translated_il)
    if args.extract_only:
        return {"schema_version": SCHEMA_VERSION, "pairs": pairs}

    from comet import download_model, load_from_checkpoint

    checkpoint = Path(download_model(args.model))
    model_root = checkpoint.parent.parent if checkpoint.parent.name == "checkpoints" else checkpoint.parent
    model_hash, model_files = tree_sha256(model_root)
    if args.expected_model_tree_sha256 and model_hash != args.expected_model_tree_sha256:
        raise RuntimeError(
            f"model tree SHA-256 mismatch: expected {args.expected_model_tree_sha256}, got {model_hash}"
        )
    model = load_from_checkpoint(str(checkpoint))
    samples = [{"src": pair["source"], "mt": pair["translation"]} for pair in pairs]
    prediction = model.predict(
        samples,
        batch_size=args.batch_size,
        gpus=0,
        num_workers=1,
        progress_bar=False,
    )
    scores = [float(score) for score in prediction.scores]
    if len(scores) != len(pairs):
        raise RuntimeError(f"model returned {len(scores)} scores for {len(pairs)} pairs")
    scored = [dict(pair, score=round(score, 6)) for pair, score in zip(pairs, scores)]
    scored.sort(key=lambda pair: (pair["score"], pair["page_index"], pair["reading_order"]))
    checkpoint_hash = hashlib.sha256(checkpoint.read_bytes()).hexdigest()
    model_revision = model_root.name if model_root.parent.name == "snapshots" else None
    return {
        "schema_version": SCHEMA_VERSION,
        "metric": "COMET reference-free quality estimation",
        "model": args.model,
        "model_source": f"https://huggingface.co/{args.model}",
        "model_revision": model_revision,
        "model_tree_sha256": model_hash,
        "checkpoint_sha256": checkpoint_hash,
        "model_files": model_files,
        "device": "cpu",
        "deterministic_order": "page_index, reading_order",
        "preprocessing": "strip placeholders and exact IL-labelled formula units; collapse whitespace",
        "paragraph_count": len(scored),
        "distribution": {
            "min": round(min(scores), 6),
            "p10": round(percentile(scores, 0.10), 6),
            "median": round(percentile(scores, 0.50), 6),
        },
        "lowest": scored[: args.lowest_n],
        "threshold": {"status": "proposal-pending-user-adjudication", "value": None},
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-il", type=Path, required=True)
    parser.add_argument("--translated-il", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--batch-size", type=int, default=4)
    parser.add_argument("--lowest-n", type=int, default=10)
    parser.add_argument("--expected-model-tree-sha256")
    parser.add_argument("--extract-only", action="store_true")
    args = parser.parse_args()
    report = evaluate(args)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
