#!/usr/bin/env python3
"""Install the exact Linux corpus verifier toolchain from a pinned manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tomllib
import urllib.parse
import urllib.request


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
GIT_COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")


def fail(message: str) -> None:
    raise RuntimeError(message)


def load_manifest(path: Path) -> dict:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if manifest.get("schema_version") != 1:
        fail("ci-toolchain.toml schema_version must be 1")

    homebrew = manifest.get("homebrew")
    if not isinstance(homebrew, dict):
        fail("ci-toolchain.toml is missing [homebrew]")
    validate_download("homebrew", homebrew)
    validate_revision("homebrew", homebrew)
    if homebrew.get("prefix") != "/home/linuxbrew/.linuxbrew":
        fail("homebrew.prefix must be /home/linuxbrew/.linuxbrew")
    core = homebrew.get("core")
    if not isinstance(core, dict):
        fail("ci-toolchain.toml is missing [homebrew.core]")
    validate_download("homebrew.core", core)
    validate_revision("homebrew.core", core)
    if not isinstance(core.get("portable_ruby_version"), str):
        fail("homebrew.core.portable_ruby_version must be a string")
    validate_sha("homebrew.core.portable_ruby_sha256", core.get("portable_ruby_sha256"))

    formulae = homebrew.get("formula")
    if not isinstance(formulae, list) or not formulae:
        fail("ci-toolchain.toml must declare homebrew.formula entries")
    ensure_unique_ids("homebrew.formula", formulae)
    for formula in formulae:
        if not isinstance(formula.get("version"), str) or not formula["version"]:
            fail(f"homebrew formula {formula.get('id')} has no version")
        validate_relative_path(
            f"homebrew formula {formula.get('id')} path", formula.get("path")
        )
        validate_sha(
            f"homebrew formula {formula.get('id')} x86_64_linux_sha256",
            formula.get("x86_64_linux_sha256"),
        )
        validate_bytes(
            f"homebrew formula {formula.get('id')} x86_64_linux_bytes",
            formula.get("x86_64_linux_bytes"),
        )

    archives = manifest.get("archive")
    if not isinstance(archives, list) or not archives:
        fail("ci-toolchain.toml must declare archive entries")
    ensure_unique_ids("archive", archives)
    for archive in archives:
        validate_download(f"archive {archive.get('id')}", archive)
        if archive.get("strip_components") not in (0, 1):
            fail(f"archive {archive.get('id')} has unsupported strip_components")
        for key in ("destination", "bin_dir"):
            validate_relative_path(f"archive {archive.get('id')} {key}", archive.get(key))
    return manifest


def ensure_unique_ids(label: str, entries: list[dict]) -> None:
    ids = [entry.get("id") for entry in entries]
    if any(not isinstance(item, str) or not item for item in ids):
        fail(f"{label} entries require non-empty string ids")
    if len(ids) != len(set(ids)):
        fail(f"{label} ids must be unique")


def validate_download(label: str, item: dict) -> None:
    url = item.get("url")
    if not isinstance(url, str) or not url.startswith("https://"):
        fail(f"{label} URL must use https")
    validate_sha(f"{label} sha256", item.get("sha256"))
    validate_bytes(f"{label} bytes", item.get("bytes"))


def validate_revision(label: str, item: dict) -> None:
    revision = item.get("revision")
    if not isinstance(revision, str) or GIT_COMMIT_RE.fullmatch(revision) is None:
        fail(f"{label} revision must be a lowercase 40-character Git commit")
    if not item["url"].endswith(revision):
        fail(f"{label} URL must end with its pinned revision")


def validate_relative_path(label: str, value: object) -> None:
    if not isinstance(value, str) or not value:
        fail(f"{label} must be a non-empty relative path")
    path = Path(value)
    if path.is_absolute() or ".." in path.parts:
        fail(f"{label} must not escape its owning directory")


def validate_sha(label: str, value: object) -> None:
    if not isinstance(value, str) or SHA256_RE.fullmatch(value) is None:
        fail(f"{label} must be a lowercase SHA-256 digest")


def validate_bytes(label: str, value: object) -> None:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        fail(f"{label} must be a positive integer")


def manifest_digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def download(item: dict, downloads: Path) -> Path:
    suffixes = "".join(Path(urllib.parse.urlparse(item["url"]).path).suffixes)
    target = downloads / f"{item.get('id', 'archive')}{suffixes}"
    if target.is_file() and verified(target, item):
        return target
    target.unlink(missing_ok=True)
    temporary = target.with_suffix(target.suffix + ".partial")
    temporary.unlink(missing_ok=True)
    print(
        f"download {item.get('id', 'archive')}: {item['url']}",
        file=sys.stderr,
        flush=True,
    )
    request = urllib.request.Request(item["url"], headers={"User-Agent": "mimus-corpus-ci/1"})
    with urllib.request.urlopen(request) as response, temporary.open("wb") as handle:
        shutil.copyfileobj(response, handle)
    if not verified(temporary, item):
        temporary.unlink(missing_ok=True)
        fail(f"download verification failed for {item.get('id', 'archive')}")
    temporary.replace(target)
    return target


def verified(path: Path, item: dict) -> bool:
    if path.stat().st_size != item["bytes"]:
        return False
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest() == item["sha256"]


def extract(archive: Path, destination: Path, strip_components: int) -> None:
    if destination.exists():
        fail(f"refusing to replace existing toolchain directory: {destination}")
    destination.mkdir(parents=True)
    subprocess.run(
        [
            "tar",
            "-xJf" if archive.name.endswith(".xz") else "-xzf",
            str(archive),
            f"--strip-components={strip_components}",
            "-C",
            str(destination),
        ],
        check=True,
    )


def brew_environment(prefix: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment.update(
        {
            "HOMEBREW_NO_ANALYTICS": "1",
            "HOMEBREW_NO_AUTO_UPDATE": "1",
            "HOMEBREW_NO_ENV_HINTS": "1",
            "HOMEBREW_NO_INSTALL_CLEANUP": "1",
            "HOMEBREW_NO_INSTALL_FROM_API": "1",
            "HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK": "1",
            "PATH": f"{prefix / 'bin'}:{prefix / 'sbin'}:{environment['PATH']}",
        }
    )
    return environment


def install_homebrew(manifest: dict, downloads: Path) -> Path:
    homebrew = manifest["homebrew"]
    prefix = Path(homebrew["prefix"])
    if prefix.exists():
        fail(f"Homebrew prefix already exists without a matching cache stamp: {prefix}")
    prefix.mkdir(parents=True)
    extract_into(download(homebrew, downloads), prefix)

    core = homebrew["core"]
    core_path = prefix / "Library/Taps/homebrew/homebrew-core"
    core_path.mkdir(parents=True)
    extract_into(download({"id": "homebrew-core", **core}, downloads), core_path)

    for formula in homebrew["formula"]:
        if not (core_path / formula["path"]).is_file():
            fail(f"pinned homebrew-core is missing {formula['path']}")

    portable_ruby_version = prefix / "Library/Homebrew/vendor/portable-ruby-version"
    if portable_ruby_version.read_text().strip() != core["portable_ruby_version"]:
        fail("pinned Homebrew portable Ruby version does not match the manifest")
    portable_ruby_path = prefix / "Library/Homebrew/vendor/portable-ruby-x86_64-linux"
    expected_portable = (
        f"ruby_TAG=x86_64_linux\n"
        f"ruby_SHA={core['portable_ruby_sha256']}\n"
    )
    if portable_ruby_path.read_text() != expected_portable:
        fail("pinned Homebrew portable Ruby checksum does not match the manifest")

    brew = prefix / "bin/brew"
    environment = brew_environment(prefix)
    formulae = homebrew["formula"]
    info = subprocess.run(
        [str(brew), "info", "--json=v2", *[item["id"] for item in formulae]],
        check=True,
        capture_output=True,
        text=True,
        env=environment,
    )
    actual = {item["name"]: item for item in json.loads(info.stdout)["formulae"]}
    for expected in formulae:
        formula = actual.get(expected["id"])
        if formula is None:
            fail(f"pinned homebrew-core has no {expected['id']} formula")
        if formula["versions"]["stable"] != expected["version"]:
            fail(f"unexpected {expected['id']} version in pinned homebrew-core")
        bottle = formula["bottle"]["stable"]["files"]["x86_64_linux"]
        if bottle["sha256"] != expected["x86_64_linux_sha256"]:
            fail(f"unexpected {expected['id']} bottle checksum in pinned homebrew-core")

    subprocess.run(
        [str(brew), "install", *[item["id"] for item in formulae]],
        check=True,
        env=environment,
        stdout=sys.stderr,
    )
    return prefix


def extract_into(archive: Path, destination: Path) -> None:
    subprocess.run(
        ["tar", "-xzf", str(archive), "--strip-components=1", "-C", str(destination)],
        check=True,
    )


def expected_paths(manifest: dict, tool_root: Path) -> list[Path]:
    homebrew = Path(manifest["homebrew"]["prefix"])
    return [homebrew / "bin", homebrew / "sbin", *[tool_root / item["bin_dir"] for item in manifest["archive"]]]


def stamp_paths(manifest: dict, tool_root: Path) -> tuple[Path, Path]:
    return (
        tool_root / ".manifest-sha256",
        Path(manifest["homebrew"]["prefix"]) / ".mimus-corpus-manifest-sha256",
    )


def cache_is_current(manifest: dict, tool_root: Path, digest: str) -> bool:
    stamps = stamp_paths(manifest, tool_root)
    return all(path.is_file() and path.read_text().strip() == digest for path in stamps) and all(
        path.is_dir() for path in expected_paths(manifest, tool_root)
    )


def install(manifest: dict, manifest_path: Path, tool_root: Path) -> list[Path]:
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "AMD64"):
        fail("the hosted corpus toolchain supports Linux x86_64 only")
    if not tool_root.is_absolute():
        fail("--tool-root must be absolute")
    if tool_root.name != "mimus-corpus-toolchain":
        fail("--tool-root must end in mimus-corpus-toolchain")
    digest = manifest_digest(manifest_path)
    if cache_is_current(manifest, tool_root, digest):
        print("corpus CI toolchain cache matches the manifest", file=sys.stderr)
        return expected_paths(manifest, tool_root)

    tool_root.mkdir(parents=True, exist_ok=True)
    downloads = tool_root / "downloads"
    downloads.mkdir(exist_ok=True)
    install_homebrew(manifest, downloads)
    for archive in manifest["archive"]:
        extract(
            download(archive, downloads),
            tool_root / archive["destination"],
            archive["strip_components"],
        )

    shutil.rmtree(downloads)
    for stamp in stamp_paths(manifest, tool_root):
        stamp.write_text(f"{digest}\n")
    return expected_paths(manifest, tool_root)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--tool-root", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if args.check == (args.tool_root is not None):
        parser.error("choose exactly one of --check or --tool-root")
    return args


def main() -> int:
    try:
        args = parse_args()
        manifest_path = args.manifest.resolve()
        manifest = load_manifest(manifest_path)
        if args.check:
            print("corpus CI toolchain manifest is valid")
            return 0
        paths = install(manifest, manifest_path, args.tool_root.resolve())
        print("\n".join(str(path) for path in paths))
        return 0
    except (OSError, RuntimeError, subprocess.CalledProcessError, KeyError, json.JSONDecodeError) as error:
        print(f"corpus CI toolchain error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
