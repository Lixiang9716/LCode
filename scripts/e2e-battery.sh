#!/usr/bin/env bash
# LCode E2E battery — the sustainable form of the reversible-engineering
# protocol (docs/perf-protocol.md). Runs the offline dimensions always,
# the real-API task set only when LCODE_E2E_API_KEY is set, and emits a
# JSON report plus a PASS/FAIL verdict against the regression tripwires.
#
# Usage:
#   scripts/e2e-battery.sh [out-dir]     # out-dir defaults to target/e2e-battery
#   LCODE_E2E_API_KEY=sk-... scripts/e2e-battery.sh
#
# Exit code: 0 = all dimensions passed, 1 = threshold breached or a
# hard error. Offline thresholds are tripwires tuned for shared CI
# runners (order-of-magnitude regressions, not absolutes — see the
# scrub-latency lesson in docs/perf-baseline.md).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/target/e2e-battery}"
case "$OUT" in /*) ;; *) OUT="$PWD/$OUT" ;; esac
REPORT="$OUT/report.json"
# NOTE: the report is initialized after `cargo clean` — a clean wipes
# everything under target/, including a report created up front.

pass=0
fail=0

record() { # record <dimension> <key> <value>
  python3 - "$REPORT" "$1" "$2" "$3" <<'EOF'
import json, sys
path, dim, key, value = sys.argv[1:5]
try:
    value = float(value)
except ValueError:
    pass
report = json.load(open(path))
report.setdefault(dim, {})[key] = value
json.dump(report, open(path, "w"), indent=2)
EOF
}

verdict() { # verdict <ok:0/1> <dimension> <note>
  if [ "$1" -eq 0 ]; then
    pass=$((pass + 1))
    record "$2" "verdict" "pass"
    echo "✅ $2: ${3:-ok}"
  else
    fail=$((fail + 1))
    record "$2" "verdict" "fail"
    echo "❌ $2: ${3:-threshold breached}"
  fi
}

echo "== LCode E2E battery (out: $OUT) =="

# --- P1: cold release build -------------------------------------------
cd "$ROOT"
cargo clean -q
mkdir -p "$OUT"
echo '{}' > "$REPORT"
t0=$(date +%s%N)
cargo build --release -q
t1=$(date +%s%N)
p1_ms=$(( (t1 - t0) / 1000000 ))
record "perf" "cold_build_ms" "$p1_ms"
p1_ok=0; [ "$p1_ms" -le 90000 ] || p1_ok=1   # baseline 56.9s; tripwire 90s
verdict "$p1_ok" "perf.cold_build" "$((p1_ms / 1000)).$((p1_ms % 1000 / 100))s"

# --- P2: binary size --------------------------------------------------
p2_bytes=$(stat -c %s target/release/lcode)
record "perf" "binary_bytes" "$p2_bytes"
p2_ok=0; [ "$p2_bytes" -le 10000000 ] || p2_ok=1
verdict "$p2_ok" "perf.binary_size" "$p2_bytes bytes"

# --- P3: CLI startup (ns, 5 runs) -------------------------------------
startup_ns=0
for _ in 1 2 3 4 5; do
  s=$(date +%s%N)
  target/release/lcode --help > /dev/null
  e=$(date +%s%N)
  startup_ns=$((startup_ns + e - s))
done
p3_ms=$(( startup_ns / 5 / 1000000 ))
record "perf" "startup_ms" "$p3_ms"
p3_ok=0; [ "$p3_ms" -le 200 ] || p3_ok=1
verdict "$p3_ok" "perf.startup" "${p3_ms}ms"

# --- P4: test suite ---------------------------------------------------
t0=$(date +%s%N)
if cargo nextest run > "$OUT/nextest.log" 2>&1; then
  p4_passed=$(grep -oE "[0-9]+ passed" "$OUT/nextest.log" | tail -1 | grep -oE "[0-9]+" || echo 0)
else
  p4_passed=0
fi
t1=$(date +%s%N)
p4_ms=$(( (t1 - t0) / 1000000 ))
record "regression" "tests_passed" "$p4_passed"
record "regression" "suite_ms" "$p4_ms"
p4_ok=0; [ "$p4_passed" -ge 500 ] || p4_ok=1
verdict "$p4_ok" "regression.tests" "$p4_passed passed in ${p4_ms}ms"

# --- Regression gates -------------------------------------------------
if cargo clippy --all-targets 2>&1 | grep -qE "^warning: [a-z]"; then
  verdict 1 "regression.clippy" "warnings found"
else
  verdict 0 "regression.clippy" "clean"
fi
if cargo fmt --check > /dev/null 2>&1; then
  verdict 0 "regression.fmt" "clean"
else
  verdict 1 "regression.fmt" "formatting drift"
fi
if scripts/check-style.sh > "$OUT/style.log" 2>&1; then
  verdict 0 "regression.style" "clean"
else
  verdict 1 "regression.style" "style limits breached"
fi

# --- P5: scrub latency tripwire (the test asserts < 2s itself) --------
p5_ms=$(grep -oE "[0-9.]+s" "$OUT/nextest.log" | head -1 || true)
record "perf" "scrub_bound_note" "asserted by scrub_10mb_text_under_200ms"

# --- Real-API task set (opt-in via LCODE_E2E_API_KEY) -----------------
if [ -n "${LCODE_E2E_API_KEY:-}" ]; then
  echo "== real-API tasks (LCODE_E2E_API_KEY set) =="
  task() { # task <name> <turns-budget> <prompt> [verify...]
    local name="$1" turns="$2" prompt="$3"; shift 3
    local work; work=$(mktemp -d)
    cat > "$work/.lcode.toml" <<EOF
[llm]
provider = "openai_compatible"
model = "deepseek-v4-flash"
api_base = "https://api.deepseek.com"
reasoning_effort = "low"
[agent]
require_approval = false
EOF
    local log="$OUT/task_$name.log"
    local t0 t1 used
    t0=$(date +%s%N)
    (cd "$work" && LCODE_LLM_API_KEY="$LCODE_E2E_API_KEY" \
      "$ROOT/target/release/lcode" run --auto-approve --max-turns "$turns" "$prompt" \
      > "$log" 2>&1)
    t1=$(date +%s%N)
    used=$(( (t1 - t0) / 1000000 ))
    local got; got=$(grep -oE "Task completed in [0-9]+ turns" "$log" | grep -oE "[0-9]+" || echo 0)
    local ok=1
    if [ "$got" -eq 0 ]; then ok=0; fi
    for v in "$@"; do
      [ -e "$work/$v" ] || ok=0
    done
    record "e2e" "$name.turns" "$got"
    record "e2e" "$name.ms" "$used"
    if [ "$ok" -eq 1 ]; then
      pass=$((pass + 1))
      record "e2e" "$name.verdict" "pass"
      echo "✅ e2e.$name: $got turns, ${used}ms"
    else
      fail=$((fail + 1))
      record "e2e" "$name.verdict" "fail"
      echo "❌ e2e.$name: $got turns, ${used}ms — see $log"
      tail -5 "$log" | sed 's/^/    /'
    fi
  }

  task T1 5 "Reply with exactly: OK"
  task T2 10 "Create notes.md containing three lines: alpha, beta, gamma. Then change the second line to bravo. Then read notes.md and confirm the final content in your reply." notes.md
  task T3 10 "Search the web for the latest stable Rust version and its release month, then write a one-sentence answer to rust.txt." rust.txt
  task T4 10 "Use bash to run: rustc --version, git --version, and ls of the current directory. Write the outputs to checks.txt, one per line." checks.txt
  # Mean-turn tripwire (protocol: baseline mean ~3.3, +30% ≈ 4.3).
  mean_turns=$(python3 - "$REPORT" <<'EOF'
import json, sys
report = json.load(open(sys.argv[1]))
turns = [v for k, v in report.get("e2e", {}).items() if k.endswith(".turns")]
mean = sum(turns) / len(turns) if turns else 0.0
print(f"{mean:.1f}")
EOF
)
  record "e2e" "mean_turns" "$mean_turns"
  if python3 -c "import sys; sys.exit(0 if float('$mean_turns') <= 4.5 else 1)"; then
    verdict 0 "e2e.mean_turns" "mean ${mean_turns} turns (tripwire 4.5)"
  else
    verdict 1 "e2e.mean_turns" "mean ${mean_turns} turns exceeds 4.5"
  fi
else
  echo "== real-API tasks skipped (set LCODE_E2E_API_KEY to enable) =="
fi

# --- Summary ----------------------------------------------------------
echo
echo "== verdict: $pass pass, $fail fail =="
record "summary" "pass" "$pass"
record "summary" "fail" "$fail"
[ "$fail" -eq 0 ]
