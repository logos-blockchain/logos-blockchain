#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STACK_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
REPO_ROOT="$(cd "${STACK_DIR}/../../../.." && pwd)"
TESTING_ROOT="$(cd "${REPO_ROOT}/../nomos-testing" && pwd)"
CIRCUITS_DIR="${LOGOS_BLOCKCHAIN_CIRCUITS:-$HOME/.logos-blockchain-circuits}"
CIRCUITS_VERSION_FILE="${CIRCUITS_DIR}/VERSION"

IMAGE_TAG="${1:-logos-blockchain-node-testing:local}"
FORCE_REBUILD="${LOGOS_FORCE_IMAGE_REBUILD:-0}"

if [[ "${IMAGE_TAG}" == "--force" ]]; then
  IMAGE_TAG="${2:-logos-blockchain-node-testing:local}"
  FORCE_REBUILD=1
fi

if [[ "${FORCE_REBUILD}" != "1" ]] && docker image inspect "${IMAGE_TAG}" >/dev/null 2>&1; then
  echo "Using existing image ${IMAGE_TAG}"
  exit 0
fi

if [[ ! -d "${CIRCUITS_DIR}" ]]; then
  echo "Circuits directory not found: ${CIRCUITS_DIR}" >&2
  echo "Set LOGOS_BLOCKCHAIN_CIRCUITS or install circuits into ~/.logos-blockchain-circuits" >&2
  exit 1
fi

if [[ ! -f "${CIRCUITS_VERSION_FILE}" ]]; then
  echo "Circuits version file not found: ${CIRCUITS_VERSION_FILE}" >&2
  exit 1
fi

CIRCUITS_VERSION="$(tr -d '[:space:]' < "${CIRCUITS_VERSION_FILE}")"

if [[ -z "${CIRCUITS_VERSION}" ]]; then
  echo "Circuits version file is empty: ${CIRCUITS_VERSION_FILE}" >&2
  exit 1
fi

echo "Building node image ${IMAGE_TAG} from ${STACK_DIR}/Dockerfile.node"
docker buildx build \
  --load \
  --pull \
  --build-arg "LOGOS_CIRCUITS_VERSION=${CIRCUITS_VERSION}" \
  --build-context "nomos_testing=${TESTING_ROOT}" \
  -f "${STACK_DIR}/Dockerfile.node" \
  -t "${IMAGE_TAG}" \
  "${REPO_ROOT}"
