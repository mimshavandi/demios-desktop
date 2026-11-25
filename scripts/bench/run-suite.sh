#!/usr/bin/env bash
# Benchmark suite (T-INF-21): runs `dn bench --quality` on the models for this machine's
# memory tier, optionally the same GGUFs behind OpenAI-compatible servers (llama-server,
# mistral.rs), and writes a markdown table with `dn bench-report`.
#
# Instructions: docs/benchmarks/README.md. Works with the bash that ships with macOS (3.2)
# and on Linux. On Windows, run it from Git Bash, or run the `dn` commands it prints by hand.
#
#   scripts/bench/run-suite.sh [--tier 8|16|32] [--out DIR] [--reps N] [--yes]
#                              [--models "id id …"]
#
# Server comparison: for each GGUF, every entry in SERVERS is started, benchmarked and
# stopped. One entry per line, "name|command", where the command may use {gguf}, {dir},
# {file} and {port}. Example:
#
#   export SERVERS='llama-server|llama-server -m {gguf} --host 127.0.0.1 --port {port} -c 4096 --jinja
#   mistralrs|mistralrs serve -f {gguf} --host 127.0.0.1 -p {port}'
#
# Requests use the model name "default" (mistral.rs's name for its single model; llama-server
# ignores the name).
#
# DN (default: dn on PATH) picks the dn binary; PORT (default 18080) the server port.

set -uo pipefail

DN=${DN:-dn}
PORT=${PORT:-18080}
SERVERS=${SERVERS:-}
TIER=auto
OUT=
REPS=3
YES=0
MODELS=

while [ $# -gt 0 ]; do
  case "$1" in
    --tier) TIER=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    --reps) REPS=$2; shift 2 ;;
    --models) MODELS=$2; shift 2 ;;
    --yes) YES=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

command -v "$DN" >/dev/null 2>&1 || { echo "dn not found; set DN=/path/to/dn" >&2; exit 1; }

ram_gib() {
  if [ "$(uname)" = Darwin ]; then
    echo $(( $(sysctl -n hw.memsize) / 1073741824 ))
  else
    awk '/^MemTotal:/ { printf "%d\n", $2 / 1048576 }' /proc/meminfo
  fi
}

RAM=$(ram_gib)
if [ "$TIER" = auto ]; then
  if [ "$RAM" -lt 12 ]; then TIER=8; elif [ "$RAM" -lt 24 ]; then TIER=16; else TIER=32; fi
fi

# Catalogue ids, or hf:<owner/name>:<quant>:<file>:<GB> for a Hugging Face variant.
MODELS_8="qwen2.5-0.5b-instruct-q4_k_m llama-3.2-3b-instruct-q4_k_m qwen3-4b-q4_k_m phi-4-mini-instruct-q4_k_m gemma-4-e2b-it-q4_k_m"
# The 16 GB tier adds the MoE showcase at a quant that fits with experts in RAM (T-INF-20).
MODELS_16="$MODELS_8 llama-3.1-8b-instruct-q4_k_m qwen3-8b-q4_k_m hf:unsloth/Qwen3-30B-A3B-GGUF:Q2_K:Qwen3-30B-A3B-Q2_K.gguf:11.3"
MODELS_32="$MODELS_16 gpt-oss-20b-q4_k_m mistral-small-24b-instruct-2501-q4_k_m gemma-4-26b-a4b-it-q4_k_m qwen3-30b-a3b-q4_k_m"
case "$TIER" in
  8) DEFAULT=$MODELS_8 ;;
  16) DEFAULT=$MODELS_16 ;;
  32) DEFAULT=$MODELS_32 ;;
  *) echo "--tier must be 8, 16 or 32" >&2; exit 2 ;;
esac
MODELS=${MODELS:-$DEFAULT}

OUT=${OUT:-bench-results/$(uname -s | tr 'A-Z' 'a-z')-$(uname -m)-${RAM}gb-$(date +%Y%m%d)}
mkdir -p "$OUT"

LIST=$("$DN" models list)
DIR=$(printf '%s\n' "$LIST" | sed -n 's/^models dir: //p')
JSON=$("$DN" models list --json)

# Catalogue field for an id from `dn models list --json` (pretty-printed, one key per line).
cat_field() {
  printf '%s\n' "$JSON" | awk -v id="\"id\": \"$1\"," -v key="\"$2\":" '
    index($0, id) { found = 1; next }
    found && index($0, key) { sub(/^[^:]*: */, ""); gsub(/[",]/, ""); print; exit }'
}

# Echo "<gguf path>|<size GB>|<downloaded 0/1>" for a model spec.
describe() {
  case "$1" in
    hf:*)
      local rest=${1#hf:} repo quant file gb
      repo=${rest%%:*}; rest=${rest#*:}
      quant=${rest%%:*}; rest=${rest#*:}
      file=${rest%%:*}; gb=${rest#*:}
      local path="$DIR/hf/$(printf '%s' "$repo" | sed 's|/|--|')/$file"
      local have=0
      [ -f "$path" ] && [ -f "$path.source.json" ] && have=1
      echo "$path|$gb|$have"
      ;;
    *)
      local file size have=0
      file=$(cat_field "$1" file)
      size=$(cat_field "$1" size)
      [ -n "$file" ] || { echo "||"; return; }
      printf '%s\n' "$LIST" | awk -v id="$1" '$1 == id && $NF == "yes" { f = 1 } END { exit !f }' && have=1
      echo "$DIR/$file|$(awk -v s="$size" 'BEGIN { printf "%.1f", s / 1e9 }')|$have"
      ;;
  esac
}

echo "Machine: $(uname -s) $(uname -m), ${RAM} GiB RAM → tier ${TIER} GB"
echo "Results: $OUT"
echo
TOTAL=0
for m in $MODELS; do
  IFS='|' read -r path gb have <<EOF
$(describe "$m")
EOF
  if [ -z "$path" ]; then echo "unknown model: $m" >&2; exit 1; fi
  if [ "$have" = 1 ]; then
    echo "  $m (downloaded)"
  else
    echo "  $m (download $gb GB)"
    TOTAL=$(awk -v t="$TOTAL" -v g="$gb" 'BEGIN { print t + g }')
  fi
done
echo
echo "To download: $TOTAL GB (resumable, checked against SHA-256)."
if [ "$YES" != 1 ]; then
  printf 'Continue? [y/N] '
  read -r answer
  case "$answer" in y|Y|yes) ;; *) echo "Stopped."; exit 0 ;; esac
fi

start_server() { # name command gguf → sets SERVER_PID
  local cmd=$2 dir file
  dir=$(dirname "$3"); file=$(basename "$3")
  cmd=${cmd//\{gguf\}/$3}; cmd=${cmd//\{dir\}/$dir}; cmd=${cmd//\{file\}/$file}
  cmd=${cmd//\{port\}/$PORT}
  echo "[suite] starting $1: $cmd"
  # Word splitting is intended: the template is a command line from the user's own SERVERS.
  # shellcheck disable=SC2086
  $cmd >"$OUT/server-$1.log" 2>&1 &
  SERVER_PID=$!
  local i
  for i in $(seq 1 300); do
    curl -sf "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1 && return 0
    kill -0 "$SERVER_PID" 2>/dev/null || { echo "[suite] $1 exited; see $OUT/server-$1.log"; return 1; }
    sleep 1
  done
  echo "[suite] $1 did not answer within 5 minutes"
  kill "$SERVER_PID" 2>/dev/null
  return 1
}

FAILED=
for m in $MODELS; do
  IFS='|' read -r path gb have <<EOF
$(describe "$m")
EOF
  name=$(basename "$path" .gguf)
  echo
  echo "== $m"
  case "$m" in
    hf:*)
      rest=${m#hf:}; repo=${rest%%:*}; rest=${rest#*:}; quant=${rest%%:*}
      "$DN" models pull "$repo" --quant "$quant" || { FAILED="$FAILED $m(pull)"; continue; }
      target=$path
      ;;
    *)
      "$DN" models pull "$m" || { FAILED="$FAILED $m(pull)"; continue; }
      target=$m
      ;;
  esac
  # In-process llama.cpp, with the fit estimator's settings. A model that won't fit is
  # refused; that is a result too, kept in the log.
  "$DN" bench "$target" --quality --reps "$REPS" -o "$OUT/bench-$name.json" \
    2>&1 | tee "$OUT/bench-$name.log" || true
  [ -f "$OUT/bench-$name.json" ] || FAILED="$FAILED $m(llamacpp)"

  if [ -n "$SERVERS" ]; then
    while IFS='|' read -r sname scmd; do
      [ -n "$sname" ] || continue
      if start_server "$sname" "$scmd" "$path"; then
        "$DN" bench default --endpoint "http://127.0.0.1:$PORT/v1" --server-pid "$SERVER_PID" \
          --backend-name "$sname" --gguf "$path" --quality --reps "$REPS" \
          -o "$OUT/bench-$name-$sname.json" 2>&1 | tee "$OUT/bench-$name-$sname.log" || true
        kill "$SERVER_PID" 2>/dev/null; wait "$SERVER_PID" 2>/dev/null
      fi
      [ -f "$OUT/bench-$name-$sname.json" ] || FAILED="$FAILED $m($sname)"
    done <<EOF
$SERVERS
EOF
  fi
done

"$DN" bench-report "$OUT" -o "$OUT/report.md" && echo && cat "$OUT/report.md"
echo
[ -z "$FAILED" ] || echo "No report for:$FAILED (see the logs in $OUT)"
echo "Done: $OUT"
