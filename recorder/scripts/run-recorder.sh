#!/usr/bin/env bash
# Supervise hl-recorder: restart it on abnormal exit and notify Discord.
#
# Usage: scripts/run-recorder.sh [hl-recorder args...]
#   e.g. scripts/run-recorder.sh --cc --daily --l2-shards 4 --out sessions/long-run
#
# Behaviour:
#   - exit code 0 (operator stop via SIGTERM/SIGINT/duration) -> supervisor stops
#   - nonzero exit (crash, reconnect exhaustion)              -> notify + restart
#   - restart backoff doubles 5s..300s, resets after a stable (>10 min) run
#   - all recorder output is appended to $HL_RECORDER_LOG (default:
#     hl-recorder.log next to this repo's recorder root)
#   - DISCORD_WEBHOOK_URL is read from the environment or ./.env (never log it)

set -u

cd "$(dirname "$0")/.."
BIN=target/release/hl-recorder
LOG="${HL_RECORDER_LOG:-hl-recorder.log}"

# .env as a fallback for DISCORD_WEBHOOK_URL etc. (real env vars win).
if [ -f .env ]; then
    set -a
    # shellcheck disable=SC1091
    . ./.env
    set +a
fi

notify() {
    [ -n "${DISCORD_WEBHOOK_URL:-}" ] || return 0
    # JSON-encode via python to survive arbitrary characters in the message.
    local payload
    payload=$(python3 -c 'import json,sys; print(json.dumps({"content": sys.argv[1]}))' "$1") || return 0
    curl -sS -m 10 -H 'Content-Type: application/json' -d "$payload" \
        "$DISCORD_WEBHOOK_URL" >/dev/null 2>&1 || true
}

child=0
terminating=0
on_signal() {
    terminating=1
    if [ "$child" -ne 0 ]; then
        kill -TERM "$child" 2>/dev/null || true
    fi
}
trap on_signal TERM INT HUP

backoff=5
host=$(hostname)

echo "[supervisor] starting hl-recorder (log: $LOG)" | tee -a "$LOG"
while true; do
    start=$(date +%s)
    "$BIN" "$@" >>"$LOG" 2>&1 &
    child=$!
    wait "$child"
    code=$?
    child=0
    elapsed=$(( $(date +%s) - start ))

    if [ "$terminating" -eq 1 ] || [ "$code" -eq 0 ]; then
        echo "[supervisor] recorder stopped cleanly (exit $code); not restarting" | tee -a "$LOG"
        notify "🟡 **hl-recorder** on \`$host\` stopped cleanly (exit $code); supervisor exiting."
        exit "$code"
    fi

    # Stable runs reset the backoff so a once-a-day crash restarts fast.
    [ "$elapsed" -gt 600 ] && backoff=5

    echo "[supervisor] recorder died (exit $code after ${elapsed}s); restarting in ${backoff}s" | tee -a "$LOG"
    notify "🔴 **hl-recorder** on \`$host\` died (exit $code after ${elapsed}s). Restarting in ${backoff}s. Last log lines:
\`\`\`
$(tail -c 1200 "$LOG")
\`\`\`"
    sleep "$backoff"
    backoff=$(( backoff * 2 ))
    [ "$backoff" -gt 300 ] && backoff=300
done
