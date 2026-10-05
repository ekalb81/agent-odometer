#!/usr/bin/env bash
set -euo pipefail
lane="$1"; arm="$2"; record="$3"
case "$lane:$arm" in check:apt|msrv:apt|coverage:apt|check:image|msrv:image|coverage:image) ;; *) exit 2 ;; esac
mkdir -p "$record" "$HOME/.npm"
record=$(realpath "$record")
export CARGO_HOME="$HOME/.cargo" RUSTUP_HOME="$HOME/.rustup"
export CARGO_TARGET_DIR="$GITHUB_WORKSPACE/src-tauri/target"
export PATH="$(dirname "$(command -v node)"):$CARGO_HOME/bin:/usr/local/bin:/usr/bin:/bin"
probe=$(cat <<'PROBE'
set -e
id -u
printf '%s\n' "$HOME" "$PWD" "$CARGO_TARGET_DIR"
node --version
rustc --version
cargo --version
pkg-config --exists webkit2gtk-4.1 gtk+-3.0 libsoup-3.0 openssl
dpkg-query -W -f='${Package}=${Version}\n' libwebkit2gtk-4.1-dev libsoup-3.0-dev librsvg2-dev libayatana-appindicator3-dev libgtk-3-dev libssl-dev build-essential pkg-config
if [ "$LANE" = coverage ]; then cargo llvm-cov --version; rustc --print sysroot; fi
PROBE
)
if [[ "$arm" = image ]]; then
  [[ "$IMAGE" =~ ^ghcr\.io/ekalb81/agent-odometer-ci-probe@sha256:[a-f0-9]{64}$ ]] || exit 2
  docker image inspect "$IMAGE" > "$record/before-image.json" 2>/dev/null || true
  docker image ls --digests --no-trunc > "$record/before-images.txt"
  start=$(date +%s%3N)
  timeout 300 docker pull "$IMAGE" 2>&1 | tee "$record/pull.log"
  printf '%s\n' "$(( $(date +%s%3N)-start ))" > "$record/pull-ms.txt"
  start=$(date +%s%3N)
  timeout 120 docker pull "$IMAGE" 2>&1 | tee "$record/cached-pull.log"
  printf '%s\n' "$(( $(date +%s%3N)-start ))" > "$record/cached-pull-ms.txt"
  docker image inspect "$IMAGE" > "$record/image.json"
  opts=(--rm --cap-drop ALL --security-opt no-new-privileges --user "$(id -u):$(id -g)"
    --workdir "$GITHUB_WORKSPACE" --env HOME --env PATH --env CARGO_HOME --env RUSTUP_HOME --env CARGO_TARGET_DIR
    --env "LANE=$lane" --env "RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:?}"
    --volume "$GITHUB_WORKSPACE:$GITHUB_WORKSPACE"
    --volume "$CARGO_HOME:$CARGO_HOME" --volume "$RUSTUP_HOME:$RUSTUP_HOME"
    --volume "$HOME/.npm:$HOME/.npm" --volume /opt/hostedtoolcache:/opt/hostedtoolcache:ro)
  docker run "${opts[@]}" "$IMAGE" bash -c 'test "$(cat /etc/agent-odometer-ci-image)" = agent-odometer-ci-linux'
  docker run "${opts[@]}" "$IMAGE" bash -c "$probe" > "$record/identity.txt"
  docker run "${opts[@]}" "$IMAGE" bash scripts/ci-image-lane.sh "$lane" "$record"
else
  LANE="$lane" bash -c "$probe" > "$record/identity.txt"
  bash scripts/ci-image-lane.sh "$lane" "$record"
fi
