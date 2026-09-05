#!/usr/bin/env bash
set -euo pipefail

required=(
  RELEASE_PLATFORM
  RUST_TARGET
  PDFIUM_ARCHIVE
  PDFIUM_ARCHIVE_SHA256
  PDFIUM_LIBRARY_PATH
  PDFIUM_LIBRARY_NAME
  PDFIUM_LIBRARY_SHA256
  ARCHIVE_FORMAT
)
for variable in "${required[@]}"; do
  [[ -n "${!variable:-}" ]] || { echo "missing required environment variable: $variable" >&2; exit 2; }
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temp_root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
if [[ "$RELEASE_PLATFORM" == windows-* ]] && command -v cygpath >/dev/null 2>&1; then
  temp_root="$(cygpath -u "$temp_root")"
fi
work_dir="$(mktemp -d "$temp_root/mimus-release.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT
output_dir="${RELEASE_OUTPUT_DIR:-$repo_root/dist}"
mkdir -p "$output_dir"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum < "$1" | awk '{print $1}'
  else
    shasum -a 256 < "$1" | awk '{print $1}'
  fi
}

download_or_copy() {
  local provided="$1"
  local url="$2"
  local destination="$3"
  if [[ -n "$provided" ]]; then
    cp "$provided" "$destination"
  else
    curl --fail --location --retry 3 --output "$destination" "$url"
  fi
}

windows_path_to_unix() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    printf '%s\n' "$1"
  fi
}

find_vswhere() {
  local program_files_x86 candidate
  if command -v vswhere.exe >/dev/null 2>&1; then
    command -v vswhere.exe
    return
  fi
  if command -v vswhere >/dev/null 2>&1; then
    command -v vswhere
    return
  fi
  program_files_x86="$(printenv 'ProgramFiles(x86)' 2>/dev/null || true)"
  [[ -n "$program_files_x86" ]] || return 1
  candidate="$(windows_path_to_unix "$program_files_x86")/Microsoft Visual Studio/Installer/vswhere.exe"
  [[ -x "$candidate" ]] || return 1
  printf '%s\n' "$candidate"
}

expected_vc_runtime_sha256() {
  case "${1^^}" in
    MSVCP140.DLL) printf '%s\n' "${VC_MSVCP140_SHA256:-}" ;;
    MSVCP140_1.DLL) printf '%s\n' "${VC_MSVCP140_1_SHA256:-}" ;;
    VCRUNTIME140.DLL) printf '%s\n' "${VC_VCRUNTIME140_SHA256:-}" ;;
    VCRUNTIME140_1.DLL) printf '%s\n' "${VC_VCRUNTIME140_1_SHA256:-}" ;;
    *) return 1 ;;
  esac
}

bundle_windows_vc_runtime() {
  local binary_path="$1"
  local stage_dir="$2"
  local vswhere vs_install toolset_version toolset_series redist_base redist_dir
  local actual_vc_redist_version vc_crt_dir variable
  local dependency source actual_sha expected_sha invalid=0
  local -a redist_candidates

  for variable in VC_REDIST_VERSION VC_MSVCP140_SHA256 VC_MSVCP140_1_SHA256 \
    VC_VCRUNTIME140_SHA256 VC_VCRUNTIME140_1_SHA256; do
    [[ -n "${!variable:-}" ]] || {
      echo "missing required VC runtime environment variable: $variable" >&2
      return 2
    }
  done

  vswhere="$(find_vswhere)" || {
    echo "could not locate vswhere.exe" >&2
    return 1
  }
  vs_install="$("$vswhere" -latest -products '*' \
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
    -property installationPath | tr -d '\r')"
  [[ -n "$vs_install" ]] || {
    echo "vswhere.exe did not find a Visual Studio installation with the x64 VC tools" >&2
    return 1
  }
  vs_install="$(windows_path_to_unix "$vs_install")"
  toolset_version="$(tr -d '\r\n' < "$vs_install/VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt")"
  toolset_series="${toolset_version%.*}"
  redist_base="$vs_install/VC/Redist/MSVC"
  shopt -s nullglob
  redist_candidates=("$redist_base/$toolset_series".*)
  shopt -u nullglob
  ((${#redist_candidates[@]} > 0)) || {
    echo "no VC Redist directory matches compiler toolset series $toolset_series under $redist_base" >&2
    return 1
  }
  redist_dir="$(printf '%s\n' "${redist_candidates[@]}" | sort -V | tail -n 1)"
  actual_vc_redist_version="$(basename "$redist_dir")"
  vc_crt_dir="$redist_dir/x64/Microsoft.VC143.CRT"
  [[ -d "$vc_crt_dir" ]] || {
    echo "missing VC143 CRT directory: $vc_crt_dir" >&2
    return 1
  }

  printf '%s\n' '--- mimus.exe: objdump -p imports ---'
  objdump -p "$binary_path" | awk '/DLL Name:/ {print}'
  printf 'VC compiler toolset version: %s\n' "$toolset_version"
  printf 'VC Redist directory version: %s\n' "$actual_vc_redist_version"
  if [[ "$actual_vc_redist_version" != "$VC_REDIST_VERSION" ]]; then
    echo "VC Redist version mismatch: expected $VC_REDIST_VERSION, got $actual_vc_redist_version" >&2
    invalid=1
  fi

  while IFS= read -r dependency; do
    case "${dependency^^}" in
      MSVCP140*.DLL|VCRUNTIME140*.DLL|CONCRT140.DLL) ;;
      *) continue ;;
    esac
    expected_sha="$(expected_vc_runtime_sha256 "$dependency")" || {
      echo "imported VC runtime DLL has no configured SHA-256 pin: $dependency" >&2
      invalid=1
      continue
    }
    source="$(find "$vc_crt_dir" -maxdepth 1 -type f -iname "$dependency" -print -quit)"
    [[ -n "$source" ]] || {
      echo "imported VC runtime DLL is absent from $vc_crt_dir: $dependency" >&2
      invalid=1
      continue
    }
    actual_sha="$(sha256_file "$source")"
    printf 'VC runtime DLL: %s SHA-256 %s\n' "$(basename "$source")" "$actual_sha"
    cp "$source" "$stage_dir/$(basename "$source")"
    if [[ "$actual_sha" != "$expected_sha" ]]; then
      echo "VC runtime SHA-256 mismatch for $dependency: expected $expected_sha, got $actual_sha" >&2
      invalid=1
    fi
  done < <(objdump -p "$binary_path" | awk '/DLL Name:/ {print $3}')

  return "$invalid"
}

cd "$repo_root"

package_id="$(cargo pkgid --locked -p mimus)"
version="${package_id##*#}"
version="${version##*@}"
"$repo_root/scripts/release/check-tag-version.sh" "$version"

pdfium_archive="$work_dir/$PDFIUM_ARCHIVE"
download_or_copy "${PDFIUM_ARCHIVE_PATH:-}" \
  "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8009/$PDFIUM_ARCHIVE" \
  "$pdfium_archive"
actual_pdfium_sha256="$(sha256_file "$pdfium_archive")"
[[ "$actual_pdfium_sha256" == "$PDFIUM_ARCHIVE_SHA256" ]] || {
  echo "PDFium archive SHA-256 mismatch: expected $PDFIUM_ARCHIVE_SHA256, got $actual_pdfium_sha256" >&2
  exit 1
}
mkdir "$work_dir/pdfium"
tar -xzf "$pdfium_archive" -C "$work_dir/pdfium"
pdfium_source="$work_dir/pdfium/$PDFIUM_LIBRARY_PATH"
actual_pdfium_library_sha256="$(sha256_file "$pdfium_source")"
[[ "$actual_pdfium_library_sha256" == "$PDFIUM_LIBRARY_SHA256" ]] || {
  echo "PDFium library SHA-256 mismatch: expected $PDFIUM_LIBRARY_SHA256, got $actual_pdfium_library_sha256" >&2
  exit 1
}

ort_source=''
if [[ -n "${ORT_ARCHIVE:-}" ]]; then
  for variable in ORT_ARCHIVE_URL ORT_ARCHIVE_SHA256 ORT_LIBRARY_PATH ORT_LIBRARY_NAME ORT_LIBRARY_SHA256; do
    [[ -n "${!variable:-}" ]] || {
      echo "missing required ONNX Runtime environment variable: $variable" >&2
      exit 2
    }
  done

  ort_archive="$work_dir/$ORT_ARCHIVE"
  download_or_copy "${ORT_ARCHIVE_PATH:-}" "$ORT_ARCHIVE_URL" "$ort_archive"
  actual_ort_archive_sha256="$(sha256_file "$ort_archive")"
  [[ "$actual_ort_archive_sha256" == "$ORT_ARCHIVE_SHA256" ]] || {
    echo "ONNX Runtime archive SHA-256 mismatch: expected $ORT_ARCHIVE_SHA256, got $actual_ort_archive_sha256" >&2
    exit 1
  }
  mkdir "$work_dir/onnxruntime"
  tar -xzf "$ort_archive" -C "$work_dir/onnxruntime"
  ort_source="$work_dir/onnxruntime/$ORT_LIBRARY_PATH"
  actual_ort_library_sha256="$(sha256_file "$ort_source")"
  [[ "$actual_ort_library_sha256" == "$ORT_LIBRARY_SHA256" ]] || {
    echo "ONNX Runtime library SHA-256 mismatch: expected $ORT_LIBRARY_SHA256, got $actual_ort_library_sha256" >&2
    exit 1
  }
  export ORT_LIB_PATH="$(dirname "$ort_source")"
  export ORT_PREFER_DYNAMIC_LINK=1
fi

layout_model="$work_dir/inference.onnx"
download_or_copy "${LAYOUT_MODEL_PATH:-}" \
  'https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx/resolve/46bbdf188bb0a772c08aed74882ce7e51a8f1ea6/inference.onnx' \
  "$layout_model"
expected_model_sha256='45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba'
actual_model_sha256="$(sha256_file "$layout_model")"
[[ "$actual_model_sha256" == "$expected_model_sha256" ]] || {
  echo "layout model SHA-256 mismatch: expected $expected_model_sha256, got $actual_model_sha256" >&2
  exit 1
}

cargo build --release --locked --target "$RUST_TARGET"
archive_root="mimus-v${version}-${RELEASE_PLATFORM}"
stage_parent="$work_dir/stage"
stage="$stage_parent/$archive_root"
mkdir -p "$stage/licenses/pdfium"

executable_suffix=''
if [[ "$RELEASE_PLATFORM" == windows-* ]]; then
  executable_suffix='.exe'
fi
binary="$repo_root/target/$RUST_TARGET/release/mimus$executable_suffix"
cp "$binary" "$stage/mimus$executable_suffix"
cp "$pdfium_source" "$stage/$PDFIUM_LIBRARY_NAME"
cp "$repo_root/LICENSE" "$stage/LICENSE"
cp "$repo_root/THIRD_PARTY_NOTICES" "$stage/THIRD_PARTY_NOTICES"
cp "$repo_root/release/README.md" "$stage/README.md"
cp "$repo_root/crates/mimus/tests/assets/fonts/LICENSE-OFL-1.1.txt" "$stage/licenses/OFL-1.1.txt"
cp "$work_dir/pdfium/LICENSE" "$stage/licenses/pdfium/pdfium-binaries-LICENSE"
cp -R "$work_dir/pdfium/licenses/." "$stage/licenses/pdfium/"
if [[ "$RELEASE_PLATFORM" == windows-* ]]; then
  bundle_windows_vc_runtime "$stage/mimus$executable_suffix" "$stage"
fi
if [[ -n "$ort_source" ]]; then
  cp "$ort_source" "$stage/$ORT_LIBRARY_NAME"
  install_name_tool -id "@executable_path/$ORT_LIBRARY_NAME" \
    "$stage/$ORT_LIBRARY_NAME"
  install_name_tool -change "@rpath/$ORT_LIBRARY_NAME" \
    "@executable_path/$ORT_LIBRARY_NAME" "$stage/mimus$executable_suffix"
fi

"$repo_root/scripts/release/rust-dependency-licenses.sh" > "$stage/RUST_DEPENDENCIES.txt"
"$repo_root/scripts/release/audit-dependencies.sh" \
  "$RELEASE_PLATFORM" "$stage/mimus$executable_suffix" "$stage/$PDFIUM_LIBRARY_NAME" \
  "${ORT_LIBRARY_NAME:+$stage/$ORT_LIBRARY_NAME}" \
  > "$stage/DEPENDENCIES.txt"

"$stage/mimus$executable_suffix" --help > /dev/null
version_output="$("$stage/mimus$executable_suffix" --version)"
[[ "$version_output" == "mimus $version" ]] || {
  echo "unexpected --version output: $version_output" >&2
  exit 1
}

smoke="$output_dir/${RELEASE_PLATFORM}-inspect.ndjson"
smoke_stderr="$work_dir/inspect.stderr"
"$stage/mimus$executable_suffix" --json inspect \
  "$repo_root/corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf" \
  --layout-model "$layout_model" > "$smoke" 2> "$smoke_stderr"
[[ ! -s "$smoke_stderr" ]] || {
  echo "JSON smoke test wrote to stderr" >&2
  cat "$smoke_stderr" >&2
  exit 1
}
terminal_count="$(jq -s '[.[] | select(.event == "result" or .event == "error")] | length' "$smoke")"
last_event="$(tail -n 1 "$smoke" | jq -r '.event')"
[[ "$terminal_count" == 1 && "$last_event" == result ]] || {
  echo "inspect smoke test did not end in exactly one result event" >&2
  exit 1
}

if find "$stage" -type f \( -name '*.onnx' -o -name '*.ttf' -o -name '*.otf' -o -name 'SKILL.md' \) | grep . > /dev/null; then
  echo "release archive unexpectedly contains a model, font, or Agent Skill" >&2
  exit 1
fi

cp "$stage/DEPENDENCIES.txt" "$output_dir/${RELEASE_PLATFORM}-dependencies.txt"
archive="$output_dir/$archive_root.$ARCHIVE_FORMAT"
case "$ARCHIVE_FORMAT" in
  tar.gz)
    tar -czf "$archive" -C "$stage_parent" "$archive_root"
    ;;
  zip)
    (cd "$stage_parent" && 7z a -bd -tzip "$archive" "$archive_root" > /dev/null)
    ;;
  *)
    echo "unsupported archive format: $ARCHIVE_FORMAT" >&2
    exit 2
    ;;
esac

archive_sha256="$(sha256_file "$archive")"
printf '%s  %s\n' "$archive_sha256" "$(basename "$archive")" \
  > "$archive.sha256"
printf 'archive=%s\nsha256=%s\n' "$archive" "$archive_sha256"
