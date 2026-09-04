#!/usr/bin/env python3
"""Run the archived 20-paper corpus sequentially with the conserving fake."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from collections import Counter
from pathlib import Path
from typing import Any


def run(command: list[str], *, env: dict[str, str], stdout: Path, stderr: Path) -> int:
    with stdout.open("wb") as output, stderr.open("wb") as errors:
        result = subprocess.run(command, env=env, stdout=output, stderr=errors, check=False)
    return result.returncode


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def check_pdf(path: Path, evidence: Path) -> None:
    result = subprocess.run(["qpdf", "--check", str(path)], capture_output=True, check=False)
    evidence.write_bytes(result.stdout + result.stderr)
    if result.returncode != 0:
        raise RuntimeError(f"qpdf --check failed for {path}")


def fake_log_slice(path: Path, start: int, output: Path) -> int:
    if not path.exists():
        output.write_text("", encoding="ascii")
        return start
    with path.open("rb") as handle:
        handle.seek(start)
        content = handle.read()
        end = handle.tell()
    output.write_bytes(content)
    return end


def font_attribution(path: Path) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    slots: Counter[str] = Counter()
    scalar_counts: dict[str, Counter[str]] = {}
    missing_slot = 0
    for publication in document.get("publication_ink", []):
        for component in publication.get("components", []):
            if component.get("kind") != "translated_text":
                continue
            for glyph in component.get("glyphs", []):
                slot = glyph.get("font_slot")
                if slot is None:
                    missing_slot += 1
                    continue
                slots[slot] += 1
                scalar_counts.setdefault(slot, Counter())[glyph["unicode"]] += 1
    return {
        "page_count": len(document["pages"]),
        "glyphs_by_slot": dict(sorted(slots.items())),
        "unique_scalars_by_slot": {
            slot: sorted(values, key=ord) for slot, values in sorted(scalar_counts.items())
        },
        "scalar_counts_by_slot": {
            slot: {value: values[value] for value in sorted(values, key=ord)}
            for slot, values in sorted(scalar_counts.items())
        },
        "missing_font_slot": missing_slot,
    }


def translation_command(
    args: argparse.Namespace,
    input_pdf: Path,
    output_pdf: Path,
    debug: Path | None = None,
) -> list[str]:
    font_bold = args.font_bold or args.font
    font_latin = args.font_latin or args.font
    font_latin_bold = args.font_latin_bold or font_latin
    command = [
        "/usr/bin/time", "-lp", str(args.mimus), "translate", str(input_pdf), "--json",
        "-o", str(output_pdf), "--backend", "openai", "--endpoint", args.endpoint,
        "--model", args.model, "--target-language", "zh-CN", "--layout-model", str(args.layout_model),
        "--no-cache", "--no-auto-terms", "--concurrency", "4",
    ]
    for flag, path in (
        ("--font", args.font),
        ("--font-bold", font_bold),
        ("--font-latin", font_latin),
        ("--font-latin-bold", font_latin_bold),
    ):
        if path is not None:
            command.extend([flag, str(path)])
    if debug is not None:
        command.extend(["--debug", str(debug)])
    return command


def measure_one(args: argparse.Namespace, corpus_dir: Path, suffix: str = "") -> dict[str, Any]:
    name = corpus_dir.name
    report_json = args.output_dir / f"{name}{suffix}.json"
    report_md = args.output_dir / f"{name}{suffix}.md"
    failure_json = args.output_dir / f"{name}{suffix}.failure.json"
    for stale in (report_json, report_md, failure_json):
        stale.unlink(missing_ok=True)
    input_pdf = Path((corpus_dir / "input-path").read_text(encoding="utf-8").strip())
    fake_offset = args.fake_log.stat().st_size if args.fake_log.exists() else 0
    env = os.environ.copy()
    env.update(
        {
            "MIMUS_OPENAI_API_KEY": "offline-only",
            "MIMUS_PDFIUM_LIBRARY": str(args.pdfium),
        }
    )
    if args.cache_dir is not None:
        env["MIMUS_CACHE_DIR"] = str(args.cache_dir)
    with tempfile.TemporaryDirectory(prefix=f"scorecard-{name}-", dir=args.temp_root) as temp_name:
        temp = Path(temp_name)
        output_pdf = temp / "output.pdf"
        debug = temp / "debug"
        ndjson = temp / "run.ndjson"
        resource = temp / "run.time"
        process_log = temp / "process.ndjson"
        command = translation_command(args, input_pdf, output_pdf, debug)
        exit_code = run(command, env=env, stdout=ndjson, stderr=resource)
        fake_log_slice(args.fake_log, fake_offset, process_log)
        if exit_code != 0:
            events = [json.loads(line) for line in ndjson.read_text().splitlines() if line.strip()]
            error = next((event for event in reversed(events) if event.get("event") == "error"), None)
            compact_dir = args.output_dir / "evidence"
            compact_dir.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ndjson, compact_dir / f"{name}{suffix}.ndjson")
            shutil.copyfile(resource, compact_dir / f"{name}{suffix}.time")
            failure = {
                "schema_version": 2,
                "evaluation_profile": "conserving-fake",
                "paper": name,
                "producer": (corpus_dir / "layer").read_text(encoding="utf-8").strip(),
                "status": "internal-error",
                "exit_code": exit_code,
                "error": error,
                "output_sha256_recomputed": None,
                "pages": None,
            }
            failure_json.write_text(
                json.dumps(failure, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            return failure
        compact_dir = args.output_dir / "evidence"
        compact_dir.mkdir(parents=True, exist_ok=True)
        check_pdf(output_pdf, compact_dir / f"{name}{suffix}.qpdf-check.txt")
        scorecard_command = [
            str(args.scorecard), "measure", "--ndjson", str(ndjson), "--debug-dir", str(debug),
            "--input-pdf", str(input_pdf), "--output-pdf", str(output_pdf), "--json-out",
            str(report_json), "--markdown-out", str(report_md), "--evaluation-profile",
            "conserving-fake", "--process-log", str(process_log), "--resource-usage", str(resource),
        ]
        subprocess.run(scorecard_command, env=env, check=True)
        shutil.copyfile(ndjson, compact_dir / f"{name}{suffix}.ndjson")
        shutil.copyfile(resource, compact_dir / f"{name}{suffix}.time")
        attribution = font_attribution(debug / "09-write.il.json")
        (compact_dir / f"{name}{suffix}.font-attribution.json").write_text(
            json.dumps(attribution, indent=2, ensure_ascii=False, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        pdffonts = subprocess.run(
            ["pdffonts", str(output_pdf)], capture_output=True, check=False
        )
        (compact_dir / f"{name}{suffix}.pdffonts.txt").write_bytes(
            pdffonts.stdout + pdffonts.stderr
        )
        if pdffonts.returncode != 0:
            raise RuntimeError(f"pdffonts failed for {name}{suffix}")
        report = json.loads(report_json.read_text(encoding="utf-8"))
        report["paper"] = name
        report["producer"] = (corpus_dir / "layer").read_text(encoding="utf-8").strip()
        report["output_sha256_recomputed"] = sha256(output_pdf)
        report["pages"] = attribution["page_count"]
        report["font_attribution"] = attribution
        return report


def hash_only_rerun(
    args: argparse.Namespace, corpus_dir: Path, suffix: str = ".rerun"
) -> dict[str, Any]:
    name = corpus_dir.name
    hash_json = args.output_dir / f"{name}{suffix}.hash.json"
    failure_json = args.output_dir / f"{name}{suffix}.failure.json"
    hash_json.unlink(missing_ok=True)
    failure_json.unlink(missing_ok=True)
    input_pdf = Path((corpus_dir / "input-path").read_text(encoding="utf-8").strip())
    fake_offset = args.fake_log.stat().st_size if args.fake_log.exists() else 0
    env = os.environ.copy()
    env.update(
        {
            "MIMUS_OPENAI_API_KEY": "offline-only",
            "MIMUS_PDFIUM_LIBRARY": str(args.pdfium),
        }
    )
    if args.cache_dir is not None:
        env["MIMUS_CACHE_DIR"] = str(args.cache_dir)
    with tempfile.TemporaryDirectory(prefix=f"scorecard-hash-{name}-", dir=args.temp_root) as temp_name:
        temp = Path(temp_name)
        output_pdf = temp / "output.pdf"
        ndjson = temp / "run.ndjson"
        resource = temp / "run.time"
        process_log = temp / "process.ndjson"
        exit_code = run(
            translation_command(args, input_pdf, output_pdf),
            env=env,
            stdout=ndjson,
            stderr=resource,
        )
        fake_log_slice(args.fake_log, fake_offset, process_log)
        compact_dir = args.output_dir / "evidence"
        compact_dir.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ndjson, compact_dir / f"{name}{suffix}.ndjson")
        shutil.copyfile(resource, compact_dir / f"{name}{suffix}.time")
        events = [json.loads(line) for line in ndjson.read_text().splitlines() if line.strip()]
        error = next((event for event in reversed(events) if event.get("event") == "error"), None)
        if exit_code == 0:
            check_pdf(output_pdf, compact_dir / f"{name}{suffix}.qpdf-check.txt")
        result = {
            "schema_version": 2,
            "evaluation_profile": "conserving-fake",
            "paper": name,
            "status": "published" if exit_code == 0 else "internal-error",
            "exit_code": exit_code,
            "error": error,
            "output_sha256_recomputed": sha256(output_pdf) if exit_code == 0 else None,
        }
        destination = hash_json if exit_code == 0 else failure_json
        destination.write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        return result


def resumed_report(args: argparse.Namespace, corpus_dir: Path) -> dict[str, Any] | None:
    existing = args.output_dir / f"{corpus_dir.name}.json"
    if existing.exists():
        report = json.loads(existing.read_text(encoding="utf-8"))
        report["paper"] = corpus_dir.name
        report["producer"] = (corpus_dir / "layer").read_text(encoding="utf-8").strip()
        report["output_sha256_recomputed"] = report["output_sha256"]
        event_path = args.output_dir / "evidence" / f"{corpus_dir.name}.ndjson"
        events = [json.loads(line) for line in event_path.read_text().splitlines() if line.strip()]
        report["pages"] = next(
            (event.get("pages") for event in reversed(events) if event.get("event") == "result"),
            None,
        )
        attribution_path = (
            args.output_dir / "evidence" / f"{corpus_dir.name}.font-attribution.json"
        )
        if attribution_path.exists():
            report["font_attribution"] = json.loads(
                attribution_path.read_text(encoding="utf-8")
            )
        return report
    existing_failure = args.output_dir / f"{corpus_dir.name}.failure.json"
    if existing_failure.exists():
        return json.loads(existing_failure.read_text(encoding="utf-8"))
    return None


def compact_row(report: dict[str, Any], v1: dict[str, Any] | None) -> dict[str, Any]:
    if report.get("status") == "internal-error":
        return {
            "paper": report["paper"],
            "producer": report["producer"],
            "schema_version": report["schema_version"],
            "v1_total_score": None if v1 is None else v1["total_score"],
            "v2_total_score": None,
            "published": False,
            "internal_errors": 1,
            "internal_reason": report["error"].get("reason") if report.get("error") else None,
            "typed_degraded_paragraphs": None,
            "eligible_paragraphs": None,
            "translation_calls": None,
            "translation_calls_per_eligible_paragraph": None,
            "retry_diagnostics": None,
            "retry_rate": None,
            "echoes": None,
            "echo_rate": None,
            "cache_hits": None,
            "cache_misses": None,
            "cache_hit_rate": None,
            "wall_time_seconds": None,
            "peak_rss_bytes": None,
            "conservation_rate": None,
            "conservation_missing": None,
            "formula_proxy_violations": None,
            "continuity_violations": None,
            "inline_hole_count": None,
            "ink_violations": None,
            "title_author_failures": None,
            "output_sha256": None,
            "pages": None,
            "per_page_timing": None,
            "font_attribution": None,
        }
    dimensions = report["dimensions"]
    conservation = dimensions["mistranslation_risk"]["measurements"][
        "numeric_unit_reference_conservation"
    ]
    formula = dimensions["mistranslation_risk"]["measurements"]["formula_unit_completeness_proxy"]
    continuity = dimensions["layout_drift"]["measurements"]["formula_neighbor_continuity"]
    ink = dimensions["layout_drift"]["measurements"]["final_ink_geometry"]
    title_author = dimensions["structural_fidelity"]["measurements"]["title_author_conservation"]
    process = report["process"]
    return {
        "paper": report["paper"],
        "producer": report["producer"],
        "schema_version": report["schema_version"],
        "v1_total_score": None if v1 is None else v1["total_score"],
        "v2_total_score": report["total_score"],
        "published": process["terminal_result"],
        "internal_errors": process["internal_errors"],
        "typed_degraded_paragraphs": process["typed_degraded_paragraphs"],
        "eligible_paragraphs": process["eligible_paragraphs"],
        "translation_calls": process["translation_calls"],
        "translation_calls_per_eligible_paragraph": process["translation_calls_per_eligible_paragraph"],
        "retry_diagnostics": process["retry_diagnostics"],
        "retry_rate": process["retry_rate"],
        "echoes": process["suspicious_echoes"],
        "echo_rate": process["echo_rate"],
        "cache_hits": process["cache_hits"],
        "cache_misses": process["cache_misses"],
        "cache_hit_rate": process["cache_hit_rate"],
        "wall_time_seconds": process["wall_time_seconds"],
        "peak_rss_bytes": process["peak_rss_bytes"],
        "conservation_rate": conservation["conservation_rate"],
        "conservation_missing": conservation["missing_occurrences"],
        "formula_proxy_violations": formula["violations"],
        "continuity_violations": continuity.get("excessive_gap_count"),
        "inline_hole_count": continuity.get("unexplained_hole_count"),
        "ink_violations": len(ink["violations"]),
        "title_author_failures": title_author.get("failures"),
        "output_sha256": report["output_sha256_recomputed"],
        "pages": report["pages"],
        "per_page_timing": None,
        "font_attribution": report.get("font_attribution"),
    }


def aggregate(rows: list[dict[str, Any]], reproducibility: list[dict[str, Any]]) -> dict[str, Any]:
    typed = sorted(
        row["typed_degraded_paragraphs"]
        for row in rows
        if row["typed_degraded_paragraphs"] is not None
    )
    by_producer: dict[str, dict[str, Any]] = {}
    font_slots: Counter[str] = Counter()
    missing_font_slots = 0
    for row in rows:
        attribution = row.get("font_attribution")
        if attribution is None:
            continue
        font_slots.update(attribution["glyphs_by_slot"])
        missing_font_slots += attribution["missing_font_slot"]
    for producer in sorted({row["producer"] for row in rows}):
        producer_rows = [row for row in rows if row["producer"] == producer]
        producer_typed = sorted(
            row["typed_degraded_paragraphs"]
            for row in producer_rows
            if row["typed_degraded_paragraphs"] is not None
        )
        by_producer[producer] = {
            "papers": len(producer_rows),
            "published": sum(row["published"] for row in producer_rows),
            "internal": sum(row["internal_errors"] > 0 for row in producer_rows),
            "publication_rate": sum(row["published"] for row in producer_rows) / len(producer_rows),
            "internal_rate": sum(row["internal_errors"] > 0 for row in producer_rows) / len(producer_rows),
            "typed_degradation_median": median(producer_typed),
            "typed_degradation_worst": max(producer_typed) if producer_typed else None,
            "ink_violations": sum(
                row["ink_violations"] or 0 for row in producer_rows
            ),
        }
    return {
        "schema_version": 2,
        "papers": rows,
        "cluster": {
            "paper_count": len(rows),
            "publication_rate": sum(row["published"] for row in rows) / len(rows),
            "internal_rate": sum(row["internal_errors"] > 0 for row in rows) / len(rows),
            "typed_degradation_median": (typed[(len(typed) - 1) // 2] + typed[len(typed) // 2]) / 2 if typed else None,
            "typed_degradation_worst": max(typed) if typed else None,
            "ink_violations": sum(row["ink_violations"] or 0 for row in rows),
            "font_glyphs_by_slot": dict(sorted(font_slots.items())),
            "missing_font_slot": missing_font_slots,
            "by_producer": by_producer,
        },
        "reproducibility": reproducibility,
        "process_limitations": {"per_page_timing": "not-applicable: unavailable in public artifacts"},
    }


def median(values: list[int]) -> float | None:
    if not values:
        return None
    midpoint = len(values) // 2
    if len(values) % 2:
        return float(values[midpoint])
    return (values[midpoint - 1] + values[midpoint]) / 2


def markdown(summary: dict[str, Any]) -> str:
    lines = [
        "# Scorecard v2 cluster baseline",
        "",
        "| Paper | Producer | v1 | v2 | Status | Typed | Conservation | Formula | Gap | Hole | Ink | Title/author | Calls/paragraph | Retry | Echo | Cache hit |",
        "| --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in summary["papers"]:
        status = "published" if row["published"] else f"Internal/6: {row.get('internal_reason', 'unknown')}"
        values = [
            row["paper"], row["producer"], display(row["v1_total_score"]), display(row["v2_total_score"]),
            status, display(row["typed_degraded_paragraphs"]), display(row["conservation_rate"]),
            display(row["formula_proxy_violations"]), display(row["continuity_violations"]),
            display(row["inline_hole_count"]), display(row["ink_violations"]),
            display(row["title_author_failures"]),
            display(row["translation_calls_per_eligible_paragraph"]), display(row["retry_rate"]),
            display(row["echo_rate"]), display(row["cache_hit_rate"]),
        ]
        lines.append("| " + " | ".join(str(value) for value in values) + " |")
    cluster = summary["cluster"]
    lines.extend([
        "",
        f"Publication rate: {cluster['publication_rate']:.1%}; Internal/6 rate: {cluster['internal_rate']:.1%}; "
        f"typed degradation median: {display(cluster['typed_degradation_median'])}; "
        f"worst: {display(cluster['typed_degradation_worst'])}; "
        f"INK-01 violations: {display(cluster['ink_violations'])}.",
        "",
        "Per-page timing is not applicable because it is not present in the public artifacts.",
    ])
    return "\n".join(lines) + "\n"


def display(value: Any) -> str:
    if value is None:
        return "N/A"
    if isinstance(value, float):
        return f"{value:.6f}"
    return str(value)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--final-runs", type=Path, required=True)
    parser.add_argument("--v1-baseline", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--temp-root", type=Path, required=True)
    parser.add_argument("--fake-log", type=Path, required=True)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--model", default="conserving-fake")
    parser.add_argument("--mimus", type=Path, required=True)
    parser.add_argument("--scorecard", type=Path, required=True)
    parser.add_argument("--pdfium", type=Path, required=True)
    parser.add_argument("--cache-dir", type=Path)
    parser.add_argument("--font", type=Path)
    parser.add_argument("--font-bold", type=Path)
    parser.add_argument("--font-latin", type=Path)
    parser.add_argument("--font-latin-bold", type=Path)
    parser.add_argument("--layout-model", type=Path, required=True)
    parser.add_argument("--resume", action="store_true")
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    args.temp_root.mkdir(parents=True, exist_ok=True)
    reports = []
    corpus = sorted(path for path in args.final_runs.iterdir() if path.is_dir())
    for index, corpus_dir in enumerate(corpus, 1):
        print(f"[{index:02d}/{len(corpus):02d}] {corpus_dir.name}", flush=True)
        report = resumed_report(args, corpus_dir) if args.resume else None
        if report is not None:
            reports.append(report)
        else:
            reports.append(measure_one(args, corpus_dir))
    reproducibility = []
    for paper_name in ("02-resnet", "09-repliable-onion-routing"):
        original = next(report for report in reports if report["paper"] == paper_name)
        full_rerun = args.output_dir / f"{paper_name}.rerun.json"
        hash_rerun = args.output_dir / f"{paper_name}.rerun.hash.json"
        if args.resume and full_rerun.exists():
            rerun = json.loads(full_rerun.read_text(encoding="utf-8"))
            rerun["output_sha256_recomputed"] = rerun["output_sha256"]
        elif args.resume and hash_rerun.exists():
            rerun = json.loads(hash_rerun.read_text(encoding="utf-8"))
        else:
            rerun = hash_only_rerun(args, args.final_runs / paper_name)
        reproducibility.append(
            {
                "paper": paper_name,
                "first_sha256": original["output_sha256_recomputed"],
                "second_sha256": rerun["output_sha256_recomputed"],
                "byte_identical": original["output_sha256_recomputed"] == rerun["output_sha256_recomputed"],
            }
        )
    rows = []
    for report in reports:
        v1_path = args.v1_baseline / f"{report['paper']}.json"
        v1 = json.loads(v1_path.read_text()) if v1_path.exists() else None
        rows.append(compact_row(report, v1))
    summary = aggregate(rows, reproducibility)
    (args.output_dir / "cluster-summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (args.output_dir / "cluster-summary.md").write_text(markdown(summary), encoding="utf-8")


if __name__ == "__main__":
    main()
