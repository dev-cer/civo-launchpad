#!/usr/bin/env bash
# Usage: ADMIN_TOKEN=... HACKSPACE=https://<launchpad-host> ./flash-sale/run-official.sh participants.csv
# participants.csv lines: "Full name,ACCESS_CODE,http://workshop.<cluster-id>.k8s.civo.com"
# Individual leaderboard: one row per participant (keyed by access code).
set -euo pipefail
while IFS=, read -r name code target; do
  [ -z "$name" ] && continue
  ( k6 run -q -e TARGET="$target" --summary-export "/tmp/k6-${code}.json" flash-sale/official.js >/dev/null 2>&1 || true
    f="/tmp/k6-${code}.json"
    p95=$(jq '.metrics.http_req_duration["p(95)"] | floor' "$f"); fails=$(jq '.metrics.http_req_failed.passes // 0' "$f"); rps=$(jq '.metrics.http_reqs.rate | floor' "$f")
    curl -s -X POST "$HACKSPACE/api/leaderboard" -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
      -d "$(jq -n --arg n "$name" --arg c "$code" --argjson p "$p95" --argjson e "$fails" --argjson r "$rps" '{name:$n,code:$c,p95_ms:$p,errors:$e,rps:$r}')" >/dev/null
    echo "posted $name ($code): p95=${p95}ms failed=${fails} rps=${rps}" ) &
done < "${1:-participants.csv}"
wait
