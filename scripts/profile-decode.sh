#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: this profiling helper requires Linux perf" >&2
  exit 1
fi

if ! command -v perf >/dev/null 2>&1; then
  echo "error: perf is not installed or not on PATH" >&2
  exit 1
fi

PERF_RUNS="${PERF_RUNS:-5}"
SAMPLE_COUNT="${SAMPLE_COUNT:-10}"
SAMPLE_SIZE="${SAMPLE_SIZE:-1000}"
RECORD_SAMPLE_SIZE="${RECORD_SAMPLE_SIZE:-5000}"

cargo bench --bench decode --no-run

bench_binary="$({
  find target/release/deps -maxdepth 1 -type f -executable -name 'decode-*' -print0 \
    | xargs -0 -r ls -1t
} | head -n 1)"

if [[ -z "${bench_binary}" ]]; then
  echo "error: could not locate the compiled decode benchmark" >&2
  exit 1
fi

events="cycles,instructions,branches,branch-misses,cache-references,cache-misses"

run_stat() {
  local filter="$1"
  echo
  echo "== ${filter} =="
  perf stat \
    -r "${PERF_RUNS}" \
    -e "${events}" \
    -- "${bench_binary}" \
    --bench \
    "${filter}" \
    --sample-count "${SAMPLE_COUNT}" \
    --sample-size "${SAMPLE_SIZE}"
}

# Profile Brutli on each corpus, then the two direct comparators on the binary
# corpus where hosted-CI measurements disagree most strongly.
run_stat 'binary::brutli_direct$'
run_stat 'repetitive::brutli_direct$'
run_stat 'text::brutli_direct$'
run_stat 'binary::google_brotli_direct$'
run_stat 'binary::rust_brotli_direct$'

echo
echo "== recording binary::brutli_direct =="
perf record \
  -F 999 \
  -g \
  --call-graph dwarf \
  -o perf-brutli-binary.data \
  -- "${bench_binary}" \
  --bench \
  'binary::brutli_direct$' \
  --sample-count "${SAMPLE_COUNT}" \
  --sample-size "${RECORD_SAMPLE_SIZE}"

perf report \
  --stdio \
  --no-children \
  -i perf-brutli-binary.data \
  > perf-brutli-binary.txt

echo
echo "wrote perf-brutli-binary.data"
echo "wrote perf-brutli-binary.txt"
