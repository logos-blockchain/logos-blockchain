#!/bin/sh

set -e

role="${1:-node}"

resolve_binary() {
  name="$1"
  if command -v "$name" >/dev/null 2>&1; then
    command -v "$name"
    return
  fi
  if [ -x "/usr/bin/$name" ]; then
    echo "/usr/bin/$name"
    return
  fi
  if [ -x "/usr/local/bin/$name" ]; then
    echo "/usr/local/bin/$name"
    return
  fi
  echo "/usr/bin/$name"
}

bin_for_role() {
  case "$1" in
    node) resolve_binary "logos-blockchain-node" ;;
    *) echo "Unknown role: $1" >&2; exit 2 ;;
  esac
}

check_binary_arch() {
  bin_path="$1"
  label="$2"
  if ! command -v file >/dev/null 2>&1; then
    echo "Warning: 'file' command not available; skipping ${label} arch check" >&2
    return
  fi
  bin_info="$(file -b "${bin_path}" 2>/dev/null || true)"
  host_arch="$(uname -m)"
  case "$bin_info" in
    *"Mach-O"*) echo "${label} binary is Mach-O (host bundle) but container requires Linux ELF for ${host_arch}" >&2; exit 126 ;;
    *"ELF"*) : ;;
    *) echo "${label} binary missing or unreadable; info='${bin_info}'" >&2; exit 126 ;;
  esac
  case "$host_arch" in
    x86_64) expected="x86-64|x86_64" ;;
    aarch64|arm64) expected="arm64|aarch64" ;;
    *) expected="" ;;
  esac
  if [ -n "$expected" ] && ! echo "$bin_info" | grep -Eqi "$expected"; then
    echo "${label} binary architecture mismatch: host=${host_arch}, file='${bin_info}'" >&2
    exit 126
  fi
}

bin_path="$(bin_for_role "$role")"
check_binary_arch "$bin_path" "logos-blockchain-${role}"

host_identifier_default="${role}-$(hostname -i)"

export CFG_FILE_PATH="/config.yaml" \
      CFG_DEPLOYMENT_PATH="/deployment.yaml" \
      CFG_SERVER_ADDR="${CFG_SERVER_ADDR:-http://cfgsync:${LOGOS_BLOCKCHAIN_CFGSYNC_PORT:-4400}}" \
       CFG_HOST_IP=$(hostname -i) \
       CFG_HOST_KIND="${CFG_HOST_KIND:-$role}" \
       CFG_HOST_IDENTIFIER="${CFG_HOST_IDENTIFIER:-$host_identifier_default}" \
       CFG_NETWORK_PORT="${CFG_NETWORK_PORT:-60000}" \
       CFG_BLEND_PORT="${CFG_BLEND_PORT:-3400}" \
       CFG_API_PORT="${CFG_API_PORT:-18080}" \
       LOGOS_BLOCKCHAIN_TIME_BACKEND="${LOGOS_BLOCKCHAIN_TIME_BACKEND:-monotonic}" \
       LOG_LEVEL="${LOG_LEVEL:-INFO}"

mkdir -p /recovery

cfgsync_bin="$(resolve_binary "cfgsync-client")"
attempt=0
max_attempts=30
sleep_seconds=3
until "${cfgsync_bin}"; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge "$max_attempts" ]; then
    echo "cfgsync-client failed after ${max_attempts} attempts, giving up"
    exit 1
  fi
  echo "cfgsync not ready yet (attempt ${attempt}/${max_attempts}), retrying in ${sleep_seconds}s..."
  sleep "$sleep_seconds"
done

exec "${bin_path}" /config.yaml --deployment /deployment.yaml
