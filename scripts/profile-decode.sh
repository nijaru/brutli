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
RECORD_SAMPLE_SIZE="${RECORD_SAMPLE_SIZE:-20000}"
PROFILE_CPU="${PROFILE_CPU:-}"

# Hybrid Intel hosts expose the performance-core PMU as cpu_core. Pinning the
# benchmark to one of those CPUs avoids mixing P-core and E-core counters.
if [[ -z "${PROFILE_CPU}" && -r /sys/bus/event_source/devices/cpu_core/cpus ]]; then
  cpu_list="$(cat /sys/bus/event_source/devices/cpu_core/cpus)"
  PROFILE_CPU="$(printf '%s\n' "${cpu_list}" | sed 's/,.*//; s/-.*//')"
fi

runner=()
if [[ -n "${PROFILE_CPU}" ]]; then
  if ! command -v taskset >/dev/null 2>&1; then
    echo "error: PROFILE_CPU is set but taskset is not installed" >&2
    exit 1
  fi
  runner=(taskset -c "${PROFILE_CPU}")
  echo "profiling on CPU ${PROFILE_CPU}"
else
  echo "profiling without CPU pinning"
fi

GOOGLE_BROTLI_BUILD="${GOOGLE_BROTLI_BUILD:-}"
bench_args=(--bench decode)
if [[ -n "${GOOGLE_BROTLI_BUILD}" ]]; then
  bench_args=(--features current-google-reference "${bench_args[@]}")
  RUSTFLAGS="${RUSTFLAGS:-} -L native=${GOOGLE_BROTLI_BUILD} -l static=brotlidec -l static=brotlicommon -l m" \
    cargo bench "${bench_args[@]}" --no-run
else
  echo "GOOGLE_BROTLI_BUILD is not set; skipping the Google Brotli comparator"
  cargo bench "${bench_args[@]}" --no-run
fi

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
    -- "${runner[@]}" "${bench_binary}" \
    --bench \
    "${filter}" \
    --sample-count "${SAMPLE_COUNT}" \
    --sample-size "${SAMPLE_SIZE}"
}

record_case() {
  local case_name="$1"
  local data_file="perf-brutli-${case_name}.data"
  local report_file="perf-brutli-${case_name}.txt"

  echo
  echo "== recording ${case_name}::brutli_direct =="
  perf record \
    -F 999 \
    -g \
    --call-graph dwarf \
    -o "${data_file}" \
    -- "${runner[@]}" "${bench_binary}" \
    --bench \
    "${case_name}::brutli_direct$" \
    --sample-count "${SAMPLE_COUNT}" \
    --sample-size "${RECORD_SAMPLE_SIZE}"

  perf report \
    --stdio \
    --no-children \
    -i "${data_file}" \
    > "${report_file}"

  echo "wrote ${data_file}"
  echo "wrote ${report_file}"
}

for case_name in binary repetitive text; do
  run_stat "${case_name}::brutli_direct$"
  if [[ -n "${GOOGLE_BROTLI_BUILD}" ]]; then
    run_stat "${case_name}::google_brotli_direct$"
  fi
  run_stat "${case_name}::rust_brotli_direct$"
done

for case_name in binary repetitive text; do
  record_case "${case_name}"
done
