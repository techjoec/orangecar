#!/usr/bin/env bash
#
# session_survival_test.sh — does the CarPlay session SURVIVE?
#
# Runs the real sender (`rascar-carplay --carplay-endpoint`) against the local
# CarPlay receiver simulator and asserts the session lives well past the ~6 s
# mark where the Jaguar F-PACE tore it down, with video actually flowing.
#
# The simulator reproduces the car's fatal behaviour: if the sender hands
# MainAudio to the accessory in `modesChanged` and then SETUPs a MainAudio
# stream anyway, it stops answering and drops the link — exactly as the car does.
#
# Usage:  session_survival_test.sh [path-to-rascar-carplay]
set -u

BIN=${1:-$(dirname "$(readlink -f "$0")")/../../target/release/rascar-carplay}
SIM=$(dirname "$(readlink -f "$0")")/carplay_receiver_sim.py
OUT=$(mktemp -d)
PORT=${PORT:-7000}
RUN_SECS=${RUN_SECS:-18}
MIN_LIFE=${MIN_LIFE:-15}
MIN_FRAMES=${MIN_FRAMES:-100}

[[ -x $BIN ]] || { echo "no binary at $BIN"; exit 2; }

fail=0
# NOFRAMES=1: no video expected (RASCAR_VIDEO_MODE=none).
# HANDBACK=1: "screen car" to 127.0.0.1:7704 at 6 s; the sim answers with
# requestUI 3 s after the hand-back and checks the whole transition.
run_case() {
    local label=$1; shift
    local handback=0 sim_extra=()
    if [[ ${1:-} == HANDBACK=1 ]]; then handback=1; sim_extra+=(--expect-handback); shift; fi
    if [[ ${1:-} == NOFRAMES=1 ]]; then sim_extra+=(--min-frames 0); shift; fi
    echo "── $label"
    timeout $((RUN_SECS + 20)) python3 "$SIM" --port "$PORT" \
        --run-secs "$RUN_SECS" --min-survival-secs "$MIN_LIFE" \
        --min-frames "$MIN_FRAMES" "${sim_extra[@]}" > "$OUT/$label.sim" 2>&1 &
    local sim=$!
    sleep 1.5
    env "$@" timeout $((RUN_SECS + 10)) "$BIN" --carplay-endpoint "127.0.0.1:$PORT" \
        > "$OUT/$label.snd" 2>&1 &
    if [[ $handback -eq 1 ]]; then
        # "screen car" on 127.0.0.1:7704 hands the screen to the car's own UI.
        ( sleep 6; printf 'screen car' > /dev/udp/127.0.0.1/7704 ) &
    fi
    wait $sim; local rc=$?
    grep -E "session lifetime|video frames|MainAudio entity|modesChanged after|audio trap fired|audio SETUPs|phoneCall entity|audio packets|screen timeline|screen handback|FAIL" "$OUT/$label.sim" | sed 's/^/    /'
    if [[ $rc -eq 0 ]]; then
        echo "    PASS"
    else
        echo "    FAIL (see $OUT/$label.sim and $OUT/$label.snd)"
        fail=1
    fi
    echo
}

echo "session survival gate — binary: $BIN"
echo
# Screen only (this build never claims MainAudio, so never asks for its stream).
run_case screen_only RASCAR_VIDEO_ENCODER=${RASCAR_VIDEO_ENCODER:-x264}
# Screen handed to the car's native UI at ~6 s, taken back on the car's requestUI.
run_case handback HANDBACK=1 RASCAR_VIDEO_ENCODER=${RASCAR_VIDEO_ENCODER:-x264}
# No video at all: the session must survive with MainScreen left to the car.
run_case video_none NOFRAMES=1 RASCAR_VIDEO_MODE=none

if [[ $fail -eq 0 ]]; then
    echo "RESULT: session survives in every configuration."
else
    echo "RESULT: session did NOT survive."
fi
exit $fail
