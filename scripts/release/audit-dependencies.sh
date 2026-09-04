#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "usage: audit-dependencies.sh PLATFORM MIMUS_BINARY PDFIUM_LIBRARY [ONNXRUNTIME_LIBRARY]" >&2
  exit 2
fi

platform="$1"
binary="$2"
pdfium="$3"
onnxruntime="${4:-}"

[[ -f "$binary" ]] || { echo "missing mimus binary: $binary" >&2; exit 1; }
[[ -f "$pdfium" ]] || { echo "missing PDFium library: $pdfium" >&2; exit 1; }
if [[ -n "$onnxruntime" ]]; then
  [[ -f "$onnxruntime" ]] || { echo "missing ONNX Runtime library: $onnxruntime" >&2; exit 1; }
fi

audit_macos_file() {
  local path="$1"
  local dependency
  while IFS= read -r dependency; do
    if [[ -n "$onnxruntime" && "$dependency" == "@executable_path/$(basename "$onnxruntime")" ]]; then
      continue
    fi
    case "$dependency" in
      /System/Library/*|/usr/lib/*|./libpdfium.dylib) ;;
      *)
        echo "non-system macOS dependency is not bundled: $dependency" >&2
        return 1
        ;;
    esac
  done < <(otool -L "$path" | tail -n +2 | awk '{print $1}')
}

audit_linux_file() {
  local path="$1"
  local dependency
  while IFS= read -r dependency; do
    case "$dependency" in
      linux-vdso.so.*|ld-linux-x86-64.so.*|libc.so.*|libdl.so.*|libgcc_s.so.*|libm.so.*|libpthread.so.*|librt.so.*|libstdc++.so.*) ;;
      *)
        echo "non-system Linux dependency is not bundled: $dependency" >&2
        return 1
        ;;
    esac
  done < <(
    ldd "$path" \
      | awk '/=>/ {print $1} !/=>/ && $1 ~ /^(linux-vdso|ld-linux)/ {sub(/^.*\//, "", $1); print $1}'
  )
}

audit_windows_file() {
  local path="$1"
  local dependency upper
  while IFS= read -r dependency; do
    upper="$(printf '%s' "$dependency" | tr '[:lower:]' '[:upper:]')"
    case "$upper" in
      ADVAPI32.DLL|BCRYPT.DLL|BCRYPTPRIMITIVES.DLL|CRYPT32.DLL|DNSAPI.DLL|GDI32.DLL|IPHLPAPI.DLL|KERNEL32.DLL|MSVCRT.DLL|NORMALIZ.DLL|NTDLL.DLL|OLE32.DLL|OLEAUT32.DLL|POWRPROF.DLL|RPCRT4.DLL|SECUR32.DLL|SHELL32.DLL|USER32.DLL|USERENV.DLL|WS2_32.DLL|API-MS-WIN-CORE-*.DLL|API-MS-WIN-CRT-*.DLL) ;;
      *)
        echo "non-system Windows dependency is not bundled: $dependency" >&2
        return 1
        ;;
    esac
  done < <(objdump -p "$path" | awk '/DLL Name:/ {print $3}')
}

printf 'mimus release dependency audit\n'
printf 'platform: %s\n' "$platform"
if [[ -n "$onnxruntime" ]]; then
  printf 'ONNX Runtime: loaded at runtime from the adjacent %s\n' "$(basename "$onnxruntime")"
else
  printf 'ONNX Runtime: statically linked by ort-sys; no runtime library required\n'
fi
printf 'PDFium: loaded at runtime from the adjacent %s\n\n' "$(basename "$pdfium")"

case "$platform" in
  macos-*)
    if [[ -n "$onnxruntime" ]]; then
      nm -gU "$onnxruntime" | grep '_OrtGetApiBase$' > /dev/null || {
        echo "ONNX Runtime API symbol is missing from the bundled macOS library" >&2
        exit 1
      }
      otool -L "$binary" | awk '{print $1}' \
        | grep -Fx "@executable_path/$(basename "$onnxruntime")" > /dev/null || {
          echo "mimus does not resolve the adjacent ONNX Runtime library" >&2
          exit 1
        }
    else
      nm -gU "$binary" | grep '_OrtGetApiBase$' > /dev/null || {
        echo "ONNX Runtime static symbol is missing from the macOS binary" >&2
        exit 1
      }
    fi
    audit_macos_file "$binary"
    audit_macos_file "$pdfium"
    if [[ -n "$onnxruntime" ]]; then
      audit_macos_file "$onnxruntime"
    fi
    printf '%s\n' '--- mimus: otool -L ---'
    otool -L "$binary"
    printf '\n%s\n' '--- PDFium: otool -L ---'
    otool -L "$pdfium"
    if [[ -n "$onnxruntime" ]]; then
      printf '\n%s\n' '--- ONNX Runtime: otool -L ---'
      otool -L "$onnxruntime"
    fi
    ;;
  linux-*)
    nm -g "$binary" | grep 'OrtGetApiBase$' > /dev/null || {
      echo "ONNX Runtime static symbol is missing from the Linux binary" >&2
      exit 1
    }
    audit_linux_file "$binary"
    audit_linux_file "$pdfium"
    printf '%s\n' '--- mimus: ldd ---'
    ldd "$binary"
    printf '\n%s\n' '--- PDFium: ldd ---'
    ldd "$pdfium"
    ;;
  windows-*)
    if objdump -p "$binary" | awk '/DLL Name:/ {print $3}' | grep -qi onnxruntime; then
      echo "ONNX Runtime must not remain a Windows runtime dependency" >&2
      exit 1
    fi
    audit_windows_file "$binary"
    audit_windows_file "$pdfium"
    printf '%s\n' '--- mimus: objdump -p imports ---'
    objdump -p "$binary" | awk '/DLL Name:/ {print}'
    printf '\n%s\n' '--- PDFium: objdump -p imports ---'
    objdump -p "$pdfium" | awk '/DLL Name:/ {print}'
    ;;
  *)
    echo "unsupported release platform: $platform" >&2
    exit 2
    ;;
esac
