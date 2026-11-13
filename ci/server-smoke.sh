#!/usr/bin/env bash
# Builds the server image and checks what the deployment guide promises:
# it starts, prints its key, answers on its port, does not run as root, stops
# cleanly when asked, and keeps the same key across restarts.
#
#   ci/server-smoke.sh            (needs Docker; run from the repository root)
set -euo pipefail

image=rc-server:smoke
name=rc-server-smoke
volume=rc-server-smoke-data
port=21117

cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker build -f deploy/Dockerfile -t "$image" .
docker volume create "$volume" >/dev/null

start() {
  # The same restrictions as deploy/compose.yaml.
  docker run -d --name "$name" -p "$port:21117" -v "$volume:/data" \
    --read-only --cap-drop ALL --security-opt no-new-privileges:true \
    "$image" >/dev/null
}
key() { docker logs "$name" 2>&1 | grep -o 'Server key: [0-9A-F-]*' | tail -1; }
wait_for_key() {
  for _ in $(seq 1 30); do
    if key | grep -q .; then return 0; fi
    sleep 1
  done
  echo "the server never printed its key" >&2
  docker logs "$name" >&2
  return 1
}

start
wait_for_key
first="$(key)"
echo "started: $first"

timeout 5 bash -c "</dev/tcp/127.0.0.1/$port"
echo "port $port answers"

uid="$(docker exec "$name" id -u)"
[ "$uid" = "10001" ] || { echo "runs as uid $uid, expected 10001" >&2; exit 1; }
echo "runs as uid $uid"

began=$(date +%s)
docker stop "$name" >/dev/null
took=$(( $(date +%s) - began ))
code="$(docker inspect "$name" --format '{{.State.ExitCode}}')"
[ "$code" = "0" ] || { echo "exit code $code after stop" >&2; docker logs "$name" >&2; exit 1; }
[ "$took" -lt 8 ] || { echo "took $took s to stop: it was killed, not stopped" >&2; exit 1; }
echo "stopped cleanly in $took s"

docker rm "$name" >/dev/null
start
wait_for_key
second="$(key)"
[ "$first" = "$second" ] || { echo "key changed across a restart: $first then $second" >&2; exit 1; }
echo "same key after a restart"

size="$(docker image inspect "$image" --format '{{.Size}}')"
echo "::notice title=Server image::$first; image $(( size / 1000000 )) MB; uid $uid; stopped in $took s; key kept across restart"
