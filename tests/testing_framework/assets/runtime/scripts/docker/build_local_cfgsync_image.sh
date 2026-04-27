#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STACK_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
REPO_ROOT="$(cd "${STACK_DIR}/../../../.." && pwd)"
TESTING_ROOT="$(cd "${REPO_ROOT}/../logos-blockchain-testing" && pwd)"

IMAGE_TAG="${1:-logos-blockchain-cfgsync-testing:local}"
FORCE_REBUILD="${LOGOS_FORCE_IMAGE_REBUILD:-0}"

if [[ "${IMAGE_TAG}" == "--force" ]]; then
  IMAGE_TAG="${2:-logos-blockchain-cfgsync-testing:local}"
  FORCE_REBUILD=1
fi

if [[ "${FORCE_REBUILD}" != "1" ]] && docker image inspect "${IMAGE_TAG}" >/dev/null 2>&1; then
  echo "Using existing image ${IMAGE_TAG}"
  exit 0
fi

echo "Building cfgsync image ${IMAGE_TAG} from ${STACK_DIR}/Dockerfile.cfgsync"
docker buildx build \
  --load \
  --pull \
  --build-context "logos_blockchain_testing=${TESTING_ROOT}" \
  -f "${STACK_DIR}/Dockerfile.cfgsync" \
  -t "${IMAGE_TAG}" \
  "${REPO_ROOT}"
