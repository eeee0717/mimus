#!/usr/bin/env python3
"""Independent acceptance checks for the disposable incremental-write PoC."""

from __future__ import annotations

import json
import re
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / ".context/m0-lab/poc"
FIXTURES = ROOT / "corpus/fixtures"
REF = re.compile(r"^(\d+) (\d+) R$")


def run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )


def qpdf(path: Path) -> tuple[dict[str, object], dict[str, object]]:
    payload = json.loads(run("qpdf", "--json=2", str(path)).stdout)
    return payload, payload["qpdf"][1]


def object_value(objects: dict[str, object], reference: str) -> dict[str, object]:
    entry = objects[f"obj:{reference}"]
    assert isinstance(entry, dict)
    value = entry.get("value")
    assert isinstance(value, dict), f"{reference} is not a dictionary"
    return value


def stream_dict(objects: dict[str, object], reference: str) -> dict[str, object]:
    entry = objects[f"obj:{reference}"]
    assert isinstance(entry, dict)
    stream = entry.get("stream")
    assert isinstance(stream, dict), f"{reference} is not a stream"
    value = stream.get("dict")
    assert isinstance(value, dict)
    return value


def object_number(reference: str) -> int:
    match = REF.fullmatch(reference)
    assert match, f"not an indirect reference: {reference}"
    return int(match.group(1))


def stream_data(path: Path, reference: str) -> str:
    return run(
        "qpdf",
        f"--show-object={object_number(reference)}",
        "--filtered-stream-data",
        str(path),
    ).stdout


def xref(path: Path) -> str:
    return run("qpdf", "--show-xref", str(path)).stdout


def extracted_pages(path: Path) -> list[str]:
    text = run("pdftotext", str(path), "-").stdout
    pages = []
    for page in text.split("\f"):
        normalized = "\n".join(line.strip() for line in page.splitlines() if line.strip())
        if normalized:
            pages.append(normalized)
    return pages


def mupdf_text(path: Path) -> tuple[str, str]:
    result = run("mutool", "draw", "-F", "stext", str(path))
    root = ET.fromstring(result.stdout)
    text = "".join(element.attrib["c"] for element in root.iter("char"))
    return text, result.stderr


def assert_pgm_has_ink(data: bytes, renderer: str, path: Path) -> None:
    match = re.match(rb"P5\s+(?:#[^\n]*\s+)*(\d+)\s+(\d+)\s+(\d+)\s", data)
    assert match, f"{renderer} did not produce a binary PGM"
    assert int(match.group(3)) == 255
    pixels = data[match.end() :]
    assert pixels and min(pixels) < 250, f"{renderer} rendered {path.name} as a blank page"


def assert_nonblank(path: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="mimus-poc-render-") as directory:
        directory_path = Path(directory)
        poppler_prefix = directory_path / "poppler"
        mupdf_path = directory_path / "mupdf.pgm"
        run("pdftoppm", "-gray", "-singlefile", "-r", "72", str(path), str(poppler_prefix))
        run("mutool", "draw", "-F", "pgm", "-r", "72", "-o", str(mupdf_path), str(path))
        assert_pgm_has_ink(poppler_prefix.with_suffix(".pgm").read_bytes(), "Poppler", path)
        assert_pgm_has_ink(mupdf_path.read_bytes(), "MuPDF", path)


def assert_prefix(input_path: Path, output_path: Path) -> None:
    input_bytes = input_path.read_bytes()
    output_bytes = output_path.read_bytes()
    assert output_bytes.startswith(input_bytes), f"{output_path.name} lost the input prefix"
    run("qpdf", "--check", str(output_path))


def verify_primary() -> None:
    input_path = FIXTURES / "unit-base-03-structured/unit-base-03-structured.pdf"
    output_path = WORK / "incremental-output.pdf"
    assert_prefix(input_path, output_path)
    input_json, input_objects = qpdf(input_path)
    output_json, output_objects = qpdf(output_path)

    for number in [1, 2, *range(4, 19)]:
        key = f"obj:{number} 0 R"
        assert output_objects[key] == input_objects[key], f"original object {number} changed"

    page = output_json["pages"][0]
    assert page["object"] == "3 0 R"
    assert page["contents"] == ["21 0 R"]
    resources = object_value(output_objects, "3 0 R")["/Resources"]
    assert resources == "19 0 R"
    assert object_value(output_objects, resources)["/Font"]["/F2"] == "20 0 R"
    assert "/F2 12 Tf" in stream_data(output_path, "21 0 R")
    assert extracted_pages(output_path) == ["POC"]
    text, diagnostics = mupdf_text(output_path)
    assert text == "POC"
    assert "error" not in diagnostics.lower() and "warning" not in diagnostics.lower()


def verify_embedded_shared_font() -> None:
    path = FIXTURES / "unit-write-02-shared-resources/unit-write-02-shared-resources.pdf"
    _, objects = qpdf(path)
    descriptor = object_value(objects, "7 0 R")
    assert descriptor["/FontFile2"] == "8 0 R"
    stream_dict(objects, "8 0 R")
    assert extracted_pages(path) == ["MIMUS", "MIMUSC"]
    text, diagnostics = mupdf_text(path)
    assert text == "MIMUSMIMUSC"
    assert "embedded font" not in diagnostics.lower()
    assert "not a stream" not in diagnostics.lower()


def verify_xobject_companion() -> None:
    input_path = FIXTURES / "unit-write-04-xobj-in-objstm/unit-write-04-xobj-in-objstm.pdf"
    output_path = WORK / "companion-objstm-output.pdf"
    assert_prefix(input_path, output_path)
    input_json, _ = qpdf(input_path)
    output_json, objects = qpdf(output_path)
    max_input = input_json["qpdf"][0]["maxobjectid"]

    page = object_value(objects, "3 0 R")
    assert output_json["pages"][0]["contents"] == ["9 0 R"]
    assert "/X1 Do" in stream_data(output_path, "9 0 R")
    resources_ref = page["/Resources"]
    assert object_number(resources_ref) > max_input
    form_ref = object_value(objects, resources_ref)["/XObject"]["/X1"]
    assert object_number(form_ref) > max_input
    form = stream_dict(objects, form_ref)
    assert form["/BBox"] == [0, 0, 72, 16]
    form_resources_ref = form["/Resources"]
    assert object_number(form_resources_ref) > max_input
    assert "11/0: compressed" in xref(input_path)
    assert f"{object_number(form_resources_ref)}/0: uncompressed" in xref(output_path)
    font_ref = object_value(objects, form_resources_ref)["/Font"]["/F2"]
    assert object_number(font_ref) > max_input
    content = stream_data(output_path, form_ref)
    assert "/F2 12 Tf" in content and "(FORM COW) Tj" in content
    pages = extracted_pages(output_path)
    assert len(pages) == 1
    assert sorted(pages[0].splitlines()) == ["FORM COW", "MIMUS"]
    text, diagnostics = mupdf_text(output_path)
    assert "FORM COW" in text and "MIMUS" in text
    assert "error" not in diagnostics.lower() and "warning" not in diagnostics.lower()
    assert_nonblank(output_path)


def verify_geometry_companion() -> None:
    input_path = FIXTURES / "unit-geom-05-nonzero-origin-boxes/unit-geom-05-nonzero-origin-boxes.pdf"
    output_path = WORK / "companion-geometry-output.pdf"
    assert_prefix(input_path, output_path)
    _, input_objects = qpdf(input_path)
    output_json, output_objects = qpdf(output_path)
    input_page = object_value(input_objects, "3 0 R")
    output_page = object_value(output_objects, "3 0 R")
    assert output_page["/MediaBox"] == input_page["/MediaBox"]
    assert output_page["/CropBox"] == input_page["/CropBox"]
    content_ref = output_json["pages"][0]["contents"][0]
    assert "1 0 0 1 150 220 Tm" in stream_data(output_path, content_ref)
    assert extracted_pages(output_path) == ["POC"]
    text, diagnostics = mupdf_text(output_path)
    assert text == "POC"
    assert "error" not in diagnostics.lower() and "warning" not in diagnostics.lower()
    assert_nonblank(output_path)


def verify_object_graph_companions() -> None:
    generation_input = FIXTURES / "unit-write-03-resources-gen-nonzero/unit-write-03-resources-gen-nonzero.pdf"
    generation_output = WORK / "companion-generation-output.pdf"
    assert_prefix(generation_input, generation_output)
    _, generation_input_objects = qpdf(generation_input)
    _, generation_output_objects = qpdf(generation_output)
    assert generation_output_objects["obj:4 7 R"] == generation_input_objects["obj:4 7 R"]
    assert object_value(generation_output_objects, "3 0 R")["/Resources"] != "4 7 R"

    free_input = FIXTURES / "unit-write-06-free-object-slot/unit-write-06-free-object-slot.pdf"
    free_output = WORK / "companion-free-output.pdf"
    assert_prefix(free_input, free_output)
    free_json, free_objects = qpdf(free_output)
    free_page = object_value(free_objects, "3 0 R")
    assert object_number(free_page["/Resources"]) > 10
    assert object_number(free_json["pages"][0]["contents"][0]) > 10

    shared_input = FIXTURES / "unit-write-02-shared-resources/unit-write-02-shared-resources.pdf"
    shared_output = WORK / "companion-shared-output.pdf"
    assert_prefix(shared_input, shared_output)
    _, shared_input_objects = qpdf(shared_input)
    shared_json, shared_output_objects = qpdf(shared_output)
    second_page_ref = shared_json["pages"][1]["object"]
    assert object_value(shared_output_objects, second_page_ref)["/Resources"] == "5 0 R"
    assert shared_output_objects["obj:5 0 R"] == shared_input_objects["obj:5 0 R"]
    assert extracted_pages(shared_output) == ["POC", "MIMUSC"]


def verify_failure_atomicity() -> None:
    known_good = (FIXTURES / "unit-base-03-structured/unit-base-03-structured.pdf").read_bytes()
    for name in ["resource", "font", "save"]:
        path = WORK / f"failure-{name}.pdf"
        assert path.read_bytes() == known_good, f"{name} failure replaced its actual destination"
        run("qpdf", "--check", str(path))
    assert not list(WORK.glob(".*.tmp")), "failure injection left a temporary output"


def main() -> None:
    verify_primary()
    verify_embedded_shared_font()
    verify_xobject_companion()
    verify_geometry_companion()
    verify_object_graph_companions()
    verify_failure_atomicity()
    print("independent PoC verification passed")


if __name__ == "__main__":
    main()
