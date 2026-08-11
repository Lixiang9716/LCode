#!/usr/bin/env bash
# Style limits enforcement for LCode.
#
# Rules:
#   - Files: no .rs file may exceed 500 lines
#   - Functions: no function may exceed 50 lines
#     (enforced via clippy::too_many_lines, threshold in clippy.toml)
#   - Indentation: no business-code line may exceed 5 levels (20 spaces)
#
# Usage: scripts/check-style.sh

set -euo pipefail
cd "$(dirname "$0")/.."

FAIL=0

echo "== Checking file line limits (<= 500 lines) =="
while IFS= read -r file; do
    lines=$(wc -l < "$file")
    if [ "$lines" -gt 500 ]; then
        echo "❌ $file: $lines lines (max 500)"
        FAIL=1
    fi
done < <(find src tests -name '*.rs')
echo "✅ File line limits OK"

echo "== Checking indentation depth (<= 5 levels, business code) =="
# Skip: test modules, empty lines, comment lines, string-literal lines
# (test data like inline JSON must not be counted as code indentation),
# and multi-line string continuations.
VIOLATIONS=$(awk '
/^#\[cfg\(test\)\]/ { in_test = 1 }
in_test { next }
/^[ \t]*$/ { next }
/^[ \t]*(\/\/|\/\*|\*)/ { next }
/^[ \t]*["'"'"']/ { next }
/\\[ \t]*$/ { next }
{
    match($0, /^ */)
    level = RLENGTH / 4
    if (level > 5) print FILENAME ":" FNR ": " level " levels: " substr($0, 1, 60)
}' $(find src -name '*.rs'))

if [ -n "$VIOLATIONS" ]; then
    echo "$VIOLATIONS"
    FAIL=1
else
    echo "✅ Indentation depth OK"
fi

echo "== Checking test code separation (src/ must contain no tests) =="
# Test code belongs in tests/; src/ is for source code only.
TEST_VIOLATIONS=$(grep -rn "#\[cfg(test)\]" src/ 2>/dev/null || true)
if [ -n "$TEST_VIOLATIONS" ]; then
    echo "$TEST_VIOLATIONS"
    FAIL=1
else
    echo "✅ src/ contains no test code"
fi

if [ "$FAIL" -eq 0 ]; then
    echo "🎉 All style checks passed"
else
    echo "❌ Style checks failed"
fi
exit $FAIL
