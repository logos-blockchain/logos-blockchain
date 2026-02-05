#!/bin/sh

set -e

export PYTHONPATH=/opt/explorer:/opt/explorer/src
export NBE_NODE_API="${NBE_NODE_API:-http}"
export NBE_NODE_MANAGER="${NBE_NODE_MANAGER:-noop}"
export NBE_NODE_API_HOST="${NBE_NODE_API_HOST:-127.0.0.1}"
export NBE_NODE_API_PORT="${NBE_NODE_API_PORT:-8080}"

exec /opt/explorer/.venv/bin/python /opt/explorer/src/main.py
