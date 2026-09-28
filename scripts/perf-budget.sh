#!/usr/bin/env bash
# Measure the running `yplay serve` (and its mpv/python children), or the
# menu-bar app `Yplayer`, against the spec's performance budget.
# Usage: perf-budget.sh idle|playing|app-closed|app-open
# Exit 0 on PASS, 1 on FAIL, 2 on usage or setup errors.
set -euo pipefail

STATE="${1:-}"
case "$STATE" in
    idle | playing | app-closed | app-open) ;;
    *)
        echo "usage: $0 idle|playing|app-closed|app-open" >&2
        exit 2
        ;;
esac

if [ "$STATE" = app-closed ] || [ "$STATE" = app-open ]; then
    PIDS="$(pgrep -x Yplayer || true)"
    COUNT="$(printf '%s' "$PIDS" | grep -c . || true)"
    if [ "$COUNT" -eq 0 ]; then
        echo "FAIL: no running 'Yplayer' process" >&2
        exit 2
    fi
    if [ "$COUNT" -gt 1 ]; then
        echo "FAIL: more than one 'Yplayer' process: $(echo "$PIDS" | tr '\n' ' ')" >&2
        exit 2
    fi
    APP="$PIDS"

    echo "State: $STATE; Yplayer pid $APP"
    echo "Sampling top for 20 s..."

    top -l 21 -s 1 -stats pid,command,cpu,idlew,mem,threads |
        awk -v state="$STATE" -v app="$APP" '
        function mb(v,   n, u) {
            gsub(/[+-]/, "", v)
            u = substr(v, length(v))
            n = substr(v, 1, length(v) - 1) + 0
            if (u == "B") return n / 1048576
            if (u == "K") return n / 1024
            if (u == "M") return n
            if (u == "G") return n * 1024
            return v / 1048576
        }
        /^Processes:/ { sample++ }
        $1 == app {
            idlew = $(NF - 2)
            gsub(/[^0-9]/, "", idlew)
            if (!seen) { first = idlew; seen = 1 }
            last = idlew
            if (sample > 1) { cpu += $(NF - 3); ncpu++ }
            m = mb($(NF - 1))
            if (m > maxmem) maxmem = m
        }
        END {
            if (!seen) { print "FAIL: Yplayer not seen by top"; exit 1 }
            fail = 0
            w = (last - first) / 20
            avg = ncpu ? cpu / ncpu : 0
            printf "%-8s %-16s %8s %12s %10s\n", "PID", "COMMAND", "avg CPU%", "wakeups/s", "max MB"
            printf "%-8s %-16s %8.2f %12.2f %10.1f\n", app, "Yplayer", avg, w, maxmem
            print ""
            if (state == "app-closed") {
                if (w <= 0.2) printf "PASS  Yplayer idle wakeups/s %.2f <= 0.2\n", w
                else { printf "FAIL  Yplayer idle wakeups/s %.2f > 0.2\n", w; fail = 1 }
                if (maxmem < 40) printf "PASS  Yplayer memory %.1f MB < 40 MB\n", maxmem
                else { printf "FAIL  Yplayer memory %.1f MB >= 40 MB\n", maxmem; fail = 1 }
            } else {
                if (w <= 1.5) printf "PASS  Yplayer idle wakeups/s %.2f <= 1.5\n", w
                else { printf "FAIL  Yplayer idle wakeups/s %.2f > 1.5\n", w; fail = 1 }
            }
            print ""
            print (fail ? "FAIL" : "PASS")
            exit fail
        }'
    exit 0
fi

PIDS="$(pgrep -f "yplay serve" || true)"
COUNT="$(printf '%s' "$PIDS" | grep -c . || true)"
if [ "$COUNT" -eq 0 ]; then
    echo "FAIL: no running 'yplay serve' process" >&2
    exit 2
fi
if [ "$COUNT" -gt 1 ]; then
    echo "FAIL: more than one 'yplay serve' process: $(echo "$PIDS" | tr '\n' ' ')" >&2
    exit 2
fi
SERVICE="$PIDS"
CHILDREN="$(pgrep -P "$SERVICE" | tr '\n' ' ' || true)"

echo "State: $STATE; service pid $SERVICE; children: ${CHILDREN:-none}"
echo "Sampling top for 20 s..."

# Columns: PID COMMAND %CPU IDLEW MEM #TH. COMMAND may contain spaces, so the
# numeric columns are taken from the end. IDLEW is cumulative; MEM has units
# and a +/- change marker.
top -l 21 -s 1 -stats pid,command,cpu,idlew,mem,threads |
    awk -v state="$STATE" -v service="$SERVICE" -v children="$CHILDREN" '
    function mb(v,   n, u) {
        gsub(/[+-]/, "", v)
        u = substr(v, length(v))
        n = substr(v, 1, length(v) - 1) + 0
        if (u == "B") return n / 1048576
        if (u == "K") return n / 1024
        if (u == "M") return n
        if (u == "G") return n * 1024
        return v / 1048576
    }
    BEGIN {
        want[service] = 1
        split(children, c, " ")
        for (i in c) if (c[i] != "") want[c[i]] = 1
    }
    /^Processes:/ { sample++ }
    ($1 in want) {
        pid = $1
        cmd = $2
        for (i = 3; i <= NF - 4; i++) cmd = cmd " " $i
        name[pid] = cmd
        idlew = $(NF - 2)
        gsub(/[^0-9]/, "", idlew)
        if (!(pid in first)) first[pid] = idlew
        last[pid] = idlew
        # The first sample has no interval, so its %CPU is not averaged.
        if (sample > 1) { cpu[pid] += $(NF - 3); ncpu[pid]++ }
        m = mb($(NF - 1))
        if (m > maxmem[pid]) maxmem[pid] = m
    }
    END {
        fail = 0
        printf "%-8s %-16s %8s %12s %10s\n", "PID", "COMMAND", "avg CPU%", "wakeups/s", "max MB"
        for (pid in want) {
            if (!(pid in first)) continue
            w[pid] = (last[pid] - first[pid]) / 20
            avg = ncpu[pid] ? cpu[pid] / ncpu[pid] : 0
            printf "%-8s %-16s %8.2f %12.2f %10.1f\n", pid, name[pid], avg, w[pid], maxmem[pid]
            if (pid != service) {
                if (tolower(name[pid]) ~ /mpv/) mpv = pid
                else if (tolower(name[pid]) ~ /python/) py = pid
            }
        }
        if (!(service in first)) { print "FAIL: service not seen by top"; exit 1 }
        print ""
        if (w[service] <= 0.2) printf "PASS  service idle wakeups/s %.2f <= 0.2\n", w[service]
        else { printf "FAIL  service idle wakeups/s %.2f > 0.2\n", w[service]; fail = 1 }
        if (state == "idle") {
            if (maxmem[service] < 10) printf "PASS  service memory %.1f MB < 10 MB\n", maxmem[service]
            else { printf "FAIL  service memory %.1f MB >= 10 MB\n", maxmem[service]; fail = 1 }
            if (mpv == "") print "PASS  mpv not running"
            else { printf "FAIL  mpv running (pid %s)\n", mpv; fail = 1 }
            if (py == "") print "PASS  python worker not running"
            else { printf "FAIL  python worker running (pid %s)\n", py; fail = 1 }
        } else {
            if (mpv == "") { print "FAIL  mpv not running"; fail = 1 }
            else if (maxmem[mpv] <= 60) printf "PASS  mpv memory %.1f MB <= 60 MB\n", maxmem[mpv]
            else { printf "FAIL  mpv memory %.1f MB > 60 MB\n", maxmem[mpv]; fail = 1 }
        }
        print ""
        print (fail ? "FAIL" : "PASS")
        exit fail
    }'
