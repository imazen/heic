#!/usr/bin/env bash
#
# vastai-vaapi-run.sh — spin up a cheap vast.ai GPU instance, run the
# heic VA-API probe + tests on real Linux+GPU, and ALWAYS destroy
# the instance before exit (trap on EXIT/INT/TERM/timeout).
#
# Why: WSL doesn't have /dev/dri so the VA-API probe always reports
# unavailable locally. A 5-10 min cycle on a $0.06/hr RTX 3060 is
# enough to verify the probe + future runtime FFI against real
# NVIDIA-via-nvidia-vaapi-driver hardware.
#
# Self-timeout design: we drive create + destroy from THIS box (so
# the vast.ai API key never leaves it). The instance only runs the
# onstart script + prints to stdout (captured by `vastai logs`).
# Three independent kill-switches:
#   1. MAX_MINUTES wall-clock budget (default 15 min); the watcher
#      loop exits + the EXIT trap destroys.
#   2. Job-done marker `__HEIC_VAAPI_DONE__` in the log; watcher
#      shuts down early on success.
#   3. EXIT/INT/TERM trap — Ctrl-C this script and the instance dies.
#
# Cost ceiling: 15 min × $0.06/hr ≈ $0.015 per run. Even pessimistic
# 30 min runs cost less than a cup of coffee.
#
# Usage:
#   ./scripts/vastai-vaapi-run.sh
#   MAX_MINUTES=30 ./scripts/vastai-vaapi-run.sh

set -euo pipefail

MAX_MINUTES="${MAX_MINUTES:-15}"
GPU_MODEL="${GPU_MODEL:-RTX_3060}"     # cheap, has HEVC decode
LOG_FILE="${LOG_FILE:-/tmp/vastai-vaapi-$(date -u +%Y%m%dT%H%M%SZ).log}"

require() {
  command -v "$1" >/dev/null 2>&1 || { echo >&2 "missing: $1"; exit 1; }
}
require vastai
require jq

# Find the cheapest verified RTX 3060 with HEVC-decode-capable hardware,
# CUDA >= 12 (for nvidia-vaapi-driver), reasonable disk + bandwidth.
echo "==> Searching vast.ai for cheap ${GPU_MODEL}..."
OFFER_JSON=$(vastai search offers \
  "gpu_name=${GPU_MODEL} verified=true num_gpus=1 cuda_vers>=12.0 disk_space>=15 inet_down>=200 reliability>0.95" \
  -o dph_total --limit 1 --raw)
OFFER_ID=$(echo "${OFFER_JSON}" | jq -r '.[0].id // empty')
OFFER_PRICE=$(echo "${OFFER_JSON}" | jq -r '.[0].dph_total // empty')
if [ -z "${OFFER_ID}" ]; then
  echo >&2 "No vast.ai offer matched. Try a different GPU_MODEL env."
  exit 1
fi
echo "    offer ${OFFER_ID}  @  \$${OFFER_PRICE}/hr"

# Onstart script — runs on the vast.ai box, prints to its stdout,
# emits __HEIC_VAAPI_DONE__ when finished so the watcher knows to
# tear down early.
ONSTART=$(cat <<'EOF'
set -e
echo "=== heic VA-API runtime probe ==="
echo "host: $(hostname)  gpu: $(nvidia-smi --query-gpu=name --format=csv,noheader | head -1)"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq \
  build-essential pkg-config curl git \
  libva2 libva-drm2 libva-dev vainfo \
  nvidia-vaapi-driver mesa-va-drivers 2>&1 | tail -5
echo
echo "--- vainfo ---"
LIBVA_DRIVER_NAME=nvidia vainfo 2>&1 | head -40 || true
echo
echo "--- install rust ---"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
echo "--- clone heic ---"
git clone --depth=1 https://github.com/imazen/heic.git /tmp/heic
cd /tmp/heic
echo "--- cargo test -p heic-backend-vaapi ---"
cargo test --release -p heic-backend-vaapi 2>&1 | tail -20
echo
echo "--- probe_backends with backend-vaapi enabled ---"
cargo run --release --example probe_backends \
  --features 'backend-rust,backend-vaapi,std' 2>&1 | tail -20 || true
echo
echo "__HEIC_VAAPI_DONE__"
# Give the watcher 30 s to read logs before the instance gets destroyed
sleep 30
EOF
)

echo "==> Creating instance..."
CREATE_JSON=$(vastai create instance "${OFFER_ID}" \
  --image nvidia/cuda:12.6.0-runtime-ubuntu24.04 \
  --disk 15 \
  --onstart-cmd "${ONSTART}" \
  --raw)
INSTANCE_ID=$(echo "${CREATE_JSON}" | jq -r '.new_contract // empty')
if [ -z "${INSTANCE_ID}" ]; then
  echo >&2 "vastai create failed: ${CREATE_JSON}"
  exit 1
fi
echo "    instance ${INSTANCE_ID} created"

# ALWAYS destroy on exit — success, failure, timeout, Ctrl-C, kill -9 won't catch
# but everything else does.
cleanup() {
  local exit_code=$?
  echo
  echo "==> Destroying instance ${INSTANCE_ID} (exit_code=${exit_code})..."
  vastai destroy instance "${INSTANCE_ID}" >/dev/null 2>&1 || true
  echo "    done."
  echo "    log saved at ${LOG_FILE}"
}
trap cleanup EXIT INT TERM

# Poll status + logs. Bail on `__HEIC_VAAPI_DONE__` marker OR MAX_MINUTES.
deadline=$(( $(date +%s) + MAX_MINUTES * 60 ))
echo "==> Waiting (deadline: $(date -u -d @${deadline} +%FT%TZ), $MAX_MINUTES min budget)..."
while [ $(date +%s) -lt ${deadline} ]; do
  status=$(vastai show instance "${INSTANCE_ID}" --raw 2>/dev/null | jq -r '.actual_status // "unknown"')
  echo "    [$(date -u +%T)] status=${status}"
  if [ "${status}" = "running" ]; then
    # Capture logs — vastai logs is best-effort, can be empty until onstart writes.
    logs=$(vastai logs "${INSTANCE_ID}" 2>/dev/null || true)
    if [ -n "${logs}" ]; then
      echo "${logs}" > "${LOG_FILE}"
      if echo "${logs}" | grep -q '__HEIC_VAAPI_DONE__'; then
        echo
        echo "==> Job complete; full log:"
        echo "----------------------------------------"
        echo "${logs}"
        echo "----------------------------------------"
        exit 0
      fi
    fi
  fi
  sleep 15
done
echo
echo "!!! MAX_MINUTES (${MAX_MINUTES}) reached without __HEIC_VAAPI_DONE__"
vastai logs "${INSTANCE_ID}" 2>/dev/null > "${LOG_FILE}" || true
exit 2
