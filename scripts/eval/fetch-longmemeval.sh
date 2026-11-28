#!/usr/bin/env bash
# Downloads LongMemEval (Wu et al., 2024; MIT licence) for `dn eval longmemeval`, from a
# pinned revision of the cleaned release on Hugging Face, and checks each file's SHA-256.
# The data is never committed: it goes to a folder outside the repository.
#
#   scripts/eval/fetch-longmemeval.sh [--dir DIR] [--with-m]
#
# Default folder: ${XDG_CACHE_HOME:-~/.cache}/demios-eval/longmemeval. Files:
#   longmemeval_oracle.json       15 MB   only the sessions that hold the answer
#   longmemeval_s_cleaned.json   277 MB   ~115k tokens of history per question
#   longmemeval_m_cleaned.json   2.7 GB   ~500 sessions per question (only with --with-m)
#
# Works with the bash that ships with macOS (3.2) and on Linux; needs curl and shasum or
# sha256sum. A file already there with the right checksum isn't downloaded again; one with
# the wrong checksum is removed and the script fails.

set -euo pipefail

REPO="xiaowu0162/longmemeval-cleaned"
# Revision pinned 2026-11-27 (#343). Changing it means new checksums below.
REV="98d7416c24c778c2fee6e6f3006e7a073259d48f"
BASE="https://huggingface.co/datasets/${REPO}/resolve/${REV}"

DIR="${XDG_CACHE_HOME:-$HOME/.cache}/demios-eval/longmemeval"
WITH_M=0
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR="$2"; shift 2 ;;
    --with-m) WITH_M=1; shift ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

FILES="longmemeval_oracle.json 821a2034d219ab45846873dd14c14f12cfe7776e73527a483f9dac095d38620c
longmemeval_s_cleaned.json d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442"
if [ "$WITH_M" = 1 ]; then
  FILES="$FILES
longmemeval_m_cleaned.json 9d79e5524794a2e6900a3aa9cb7d9152c5a3e8319c9a87c25494ba1eacee495f"
fi

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Refuse a folder inside a git work tree: the data must not be committed.
mkdir -p "$DIR"
if git -C "$DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "$DIR is inside a git work tree; choose a folder outside any repository (--dir)" >&2
  exit 1
fi

echo "$FILES" | while read -r name want; do
  out="$DIR/$name"
  if [ -f "$out" ] && [ "$(sha256 "$out")" = "$want" ]; then
    echo "ok      $name (already there)"
    continue
  fi
  echo "fetch   $name"
  curl -fL --retry 3 -o "$out.part" "$BASE/$name"
  got="$(sha256 "$out.part")"
  if [ "$got" != "$want" ]; then
    rm -f "$out.part"
    echo "checksum mismatch for $name: expected $want, got $got" >&2
    exit 1
  fi
  mv "$out.part" "$out"
  echo "ok      $name"
done

echo "LongMemEval is in $DIR"
