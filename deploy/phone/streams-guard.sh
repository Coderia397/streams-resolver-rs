#!/data/data/com.termux/files/usr/bin/sh
# streams-guard: keeps the Streams resolver and its Cloudflare tunnel running on this phone, puts new builds live only after they
# have answered on a side port, rolls a failed build back, updates itself from the repository, caps its logs and writes its state
# where the resolver reports it (/api/version, "guard"). It is the only file in ~/.termux/boot (Termux:Boot runs every file there).
#
# Install (see deploy/README.md):  cp streams-guard.sh ~/.termux/boot/ && chmod 700 ~/.termux/boot/streams-guard.sh && sh ~/.termux/boot/streams-guard.sh
# Settings for the resolver: ~/.streams-env (or ~/.pstream-env from before the rename), lines like  export SUBDL_API_KEY='...'
# Test hooks (no effect unless set): STREAMS_HOME, STREAMS_INTERVAL, STREAMS_NO_SELF_UPDATE, STREAMS_RELEASE, STREAMS_GUARD_URL.

GUARD_VERSION=1

H=${STREAMS_HOME:-$HOME}
STATE=$H/.streams
BOOT=$H/.termux/boot
SELF=$BOOT/streams-guard.sh
BIN=$STATE/streams-resolver
RELEASE=${STREAMS_RELEASE:-https://github.com/Coderia397/streams-resolver-rs/releases/latest/download}
ASSET=streams-resolver-aarch64
GUARD_URL=${STREAMS_GUARD_URL:-https://raw.githubusercontent.com/Coderia397/streams-resolver-rs/main/deploy/phone/streams-guard.sh}
PORT=8790
CANARY_PORT=8791
METRICS=127.0.0.1:20241
INTERVAL=${STREAMS_INTERVAL:-15}      # seconds between checks
UPDATE_EVERY=20                       # checks between release looks (5 minutes)
SELF_EVERY=60                         # checks between looks at the guard in the repository (15 minutes)
LOG_CAP=5000000                       # bytes per log before it is rotated
REJECT_FOR=21600                      # seconds a refused build is not tried again

mkdir -p "$STATE"
LOG=$STATE/guard.log

now() { date +%s; }
stamp() { date -u +%Y-%m-%dT%H:%M:%SZ; }
log() { echo "$(stamp) $*" >> "$LOG"; echo "$(stamp) $*" >> "$STATE/events"; tail -n 20 "$STATE/events" > "$STATE/events.tmp" && mv "$STATE/events.tmp" "$STATE/events"; }
alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }
# a saved PID is ours only while its command line is still ours (after a reboot the number can belong to anything)
ours() { alive "$1" && ps -o args= -p "$1" 2>/dev/null | grep -q "$2"; }
pid_of() { cat "$STATE/$1.pid" 2>/dev/null; }
size_of() { wc -c < "$1" 2>/dev/null | tr -d ' '; }
sha_of() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" 2>/dev/null | cut -d' ' -f1; else shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1; fi; }
ping_port() { curl -fsS --max-time 5 "http://127.0.0.1:$1/api/ping" >/dev/null 2>&1; }

# ── one guard at a time ──────────────────────────────────────────────────────
LOCK=$STATE/guard.lock
take_lock() {
    if mkdir "$LOCK" 2>/dev/null; then echo $$ > "$LOCK/pid"; return 0; fi
    other=$(cat "$LOCK/pid" 2>/dev/null)
    if [ "$other" != "$$" ] && alive "$other"; then return 1; fi
    # the guard that held it is gone (a crash, a reboot): take it over
    rm -rf "$LOCK"; mkdir "$LOCK" 2>/dev/null && echo $$ > "$LOCK/pid"
}
if ! take_lock; then echo "streams-guard: already running (pid $(cat "$LOCK/pid"))"; exit 0; fi

# ── settings for the resolver ────────────────────────────────────────────────
set -a
for f in "$H/.streams-env" "$H/.pstream-env"; do [ -f "$f" ] && . "$f"; done
set +a

# ── the tunnel: the credentials file in ~/.cloudflared names it ──────────────
TUNNEL=${TUNNEL:-$(ls "$H/.cloudflared" 2>/dev/null | grep -E '^[0-9a-f-]{36}\.json$' | head -n 1 | sed 's/\.json$//')}

# ── first run on a phone set up before the guard ─────────────────────────────
if [ ! -f "$STATE/migrated" ]; then
    log "first run: stopping the old boot scripts' loops, tunnels and resolvers"
    mkdir -p "$H/boot-old"
    for f in "$BOOT"/*; do [ -f "$f" ] && [ "$f" != "$SELF" ] && mv "$f" "$H/boot-old/" && log "moved $(basename "$f") to ~/boot-old"; done
    pkill -f 'while true' 2>/dev/null
    sleep 1
    pkill cloudflared 2>/dev/null
    for name in pstream-resolver-rs streams-resolver-rs; do for p in $(pidof "$name" 2>/dev/null); do kill "$p" 2>/dev/null; done; done
    for old in "$H/pstream-resolver-rs" "$H/streams-resolver-rs"; do
        if [ ! -x "$BIN" ] && [ -x "$old" ]; then cp "$old" "$BIN" && chmod 755 "$BIN" && log "binary seeded from $(basename "$old")"; fi
    done
    touch "$STATE/migrated"
fi
command -v termux-wake-lock >/dev/null 2>&1 && termux-wake-lock

# ── counters kept across guard restarts ──────────────────────────────────────
count() { cat "$STATE/count.$1" 2>/dev/null || echo 0; }
bump() { echo $(( $(count "$1") + 1 )) > "$STATE/count.$1"; }

# ── the resolver ─────────────────────────────────────────────────────────────
start_resolver() {
    if [ ! -x "$BIN" ]; then fetch_release "$BIN" || { log "no binary and the release cannot be fetched"; return 1; }; fi
    PORT=$PORT nohup "$BIN" >> "$STATE/resolver.log" 2>&1 < /dev/null &
    echo $! > "$STATE/resolver.pid"
    bump resolver_starts
    log "resolver started (pid $!)"
}
stop_pid() {
    p=$1; [ -n "$p" ] || return 0
    kill "$p" 2>/dev/null
    i=0; while alive "$p" && [ $i -lt 10 ]; do sleep 1; i=$((i + 1)); done
    alive "$p" && kill -9 "$p" 2>/dev/null
    return 0
}
RES_FAILS=0
check_resolver() {
    p=$(pid_of resolver)
    if ! ours "$p" streams-resolver; then log "resolver not running: starting"; bump resolver_restarts; start_resolver; RES_FAILS=0; return; fi
    if ping_port "$PORT"; then RES_FAILS=0; return; fi
    RES_FAILS=$((RES_FAILS + 1))
    if [ $RES_FAILS -ge 3 ]; then
        log "resolver not answering ($RES_FAILS checks): restarting"
        stop_pid "$p"; kill -CONT "$p" 2>/dev/null; kill -9 "$p" 2>/dev/null
        bump resolver_restarts; start_resolver; RES_FAILS=0
    fi
}

# ── the tunnel ───────────────────────────────────────────────────────────────
start_tunnel() {
    if [ -z "$TUNNEL" ]; then log "no tunnel credentials in ~/.cloudflared"; return 1; fi
    nohup cloudflared tunnel --no-autoupdate --protocol http2 --metrics "$METRICS" run --url "http://localhost:$PORT" "$TUNNEL" >> "$STATE/tunnel.log" 2>&1 < /dev/null &
    echo $! > "$STATE/tunnel.pid"
    now > "$STATE/tunnel.started"
    log "tunnel started (pid $!)"
}
TUN_FAILS=0
check_tunnel() {
    p=$(pid_of tunnel)
    if ! ours "$p" cloudflared; then log "tunnel not running: starting"; bump tunnel_restarts; start_tunnel; TUN_FAILS=0; return; fi
    started=$(cat "$STATE/tunnel.started" 2>/dev/null || echo 0)
    [ $(( $(now) - started )) -lt 60 ] && return
    if curl -fsS --max-time 5 "http://$METRICS/ready" >/dev/null 2>&1; then TUN_FAILS=0; return; fi
    TUN_FAILS=$((TUN_FAILS + 1))
    if [ $TUN_FAILS -ge 3 ]; then
        log "tunnel not ready ($TUN_FAILS checks): restarting"
        stop_pid "$p"; bump tunnel_restarts; start_tunnel; TUN_FAILS=0
    fi
}

# ── updates: checksum, side port, then live, else back ───────────────────────
fetch_release() {
    want=$(curl -fsSL --max-time 30 "$RELEASE/$ASSET.sha256" 2>/dev/null | tr -d '[:space:]')
    [ -n "$want" ] || return 1
    curl -fsSL --max-time 300 -o "$1.part" "$RELEASE/$ASSET" 2>/dev/null || { rm -f "$1.part"; return 1; }
    [ "$(sha_of "$1.part")" = "$want" ] || { rm -f "$1.part"; log "download did not match its checksum"; return 1; }
    chmod 755 "$1.part" && mv "$1.part" "$1"
}
canary() {
    PORT=$CANARY_PORT nohup "$1" >> "$STATE/canary.log" 2>&1 < /dev/null &
    cp=$!
    ok=1; i=0
    while [ $i -lt 20 ]; do
        sleep 1; i=$((i + 1))
        if ping_port "$CANARY_PORT" && curl -fsS --max-time 5 "http://127.0.0.1:$CANARY_PORT/api/version" | grep -q '"version"'; then ok=0; break; fi
        alive "$cp" || break
    done
    stop_pid "$cp"
    return $ok
}
update_binary() {
    want=$(curl -fsSL --max-time 30 "$RELEASE/$ASSET.sha256" 2>/dev/null | tr -d '[:space:]')
    [ -n "$want" ] || return 0
    [ "$want" = "$(sha_of "$BIN")" ] && return 0
    if grep -q "^$want " "$STATE/rejected" 2>/dev/null; then
        at=$(grep "^$want " "$STATE/rejected" | tail -n 1 | cut -d' ' -f2)
        [ $(( $(now) - at )) -lt $REJECT_FOR ] && return 0
    fi
    log "new build ${want%"${want#????????????}"}: trying it on port $CANARY_PORT"
    fetch_release "$STATE/candidate" || { log "new build could not be fetched"; return 0; }
    if ! canary "$STATE/candidate"; then
        echo "$want $(now)" >> "$STATE/rejected"; rm -f "$STATE/candidate"
        log "new build refused: it did not answer on the side port; the live one keeps serving"
        return 0
    fi
    cp "$BIN" "$BIN.prev" 2>/dev/null
    mv "$STATE/candidate" "$BIN"
    stop_pid "$(pid_of resolver)"; start_resolver
    i=0; while [ $i -lt 30 ]; do sleep 1; i=$((i + 1)); ping_port "$PORT" && break; done
    if ping_port "$PORT"; then
        stamp > "$STATE/last_update"; log "new build live"
    else
        log "new build did not answer once live: rolling back"
        echo "$want $(now)" >> "$STATE/rejected"
        mv "$BIN.prev" "$BIN"; stop_pid "$(pid_of resolver)"; start_resolver
        stamp > "$STATE/last_rollback"; bump rollbacks
    fi
}

# ── the guard updates itself; the resolver and the tunnel keep running ──────
self_update() {
    [ -n "$STREAMS_NO_SELF_UPDATE" ] && return 0
    curl -fsSL --max-time 30 -o "$STATE/guard.new" "$GUARD_URL" 2>/dev/null || return 0
    [ "$(sha_of "$STATE/guard.new")" = "$(sha_of "$SELF")" ] && { rm -f "$STATE/guard.new"; return 0; }
    if ! sh -n "$STATE/guard.new" 2>/dev/null || ! grep -q '^GUARD_VERSION=' "$STATE/guard.new"; then
        log "guard in the repository does not pass a syntax check: kept the running one"; rm -f "$STATE/guard.new"; return 0
    fi
    cp "$SELF" "$STATE/guard.prev" 2>/dev/null
    chmod 700 "$STATE/guard.new" && mv "$STATE/guard.new" "$SELF"
    log "guard updated from the repository: restarting it in place"
    rm -rf "$LOCK"
    exec sh "$SELF"
}

# ── logs ─────────────────────────────────────────────────────────────────────
rotate_logs() {
    for f in guard.log resolver.log tunnel.log canary.log; do
        s=$(size_of "$STATE/$f"); [ -n "$s" ] && [ "$s" -gt $LOG_CAP ] && mv "$STATE/$f" "$STATE/$f.1"
    done
}

# ── state for /api/version ───────────────────────────────────────────────────
write_status() {
    events=$(sed 's/\\/\\\\/g; s/"/\\"/g; s/^/"/; s/$/"/' "$STATE/events" 2>/dev/null | paste -sd, -)
    cat > "$STATE/status.tmp" <<JSON
{"version":$GUARD_VERSION,"pid":$$,"checked":"$(stamp)","resolver_pid":"$(pid_of resolver)","tunnel_pid":"$(pid_of tunnel)","resolver_restarts":$(count resolver_restarts),"tunnel_restarts":$(count tunnel_restarts),"rollbacks":$(count rollbacks),"last_update":"$(cat "$STATE/last_update" 2>/dev/null)","last_rollback":"$(cat "$STATE/last_rollback" 2>/dev/null)","events":[${events}]}
JSON
    mv "$STATE/status.tmp" "$STATE/status.json"
}

# ── the loop ─────────────────────────────────────────────────────────────────
log "guard $GUARD_VERSION started (pid $$)"
ours "$(pid_of resolver)" streams-resolver || start_resolver
ours "$(pid_of tunnel)" cloudflared || start_tunnel
n=0
while :; do
    check_resolver
    check_tunnel
    n=$((n + 1))
    [ $((n % UPDATE_EVERY)) -eq 0 ] && update_binary
    [ $((n % 40)) -eq 0 ] && rotate_logs
    write_status
    [ $((n % SELF_EVERY)) -eq 0 ] && self_update
    sleep "$INTERVAL"
done
