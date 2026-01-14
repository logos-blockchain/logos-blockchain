#!/bin/bash

# L2 Demo - Local Development Runner
# Runs sequencer, archiver, and/or frontend without Docker (works on ARM Mac)
#
# Usage:
#   ./run-local.sh <service> --env-file /path/to/.env-local [--clean]
#
# Services:
#   sequencer  - Run only the sequencer
#   archiver   - Run only the archiver
#   frontend   - Run only the frontend
#   all        - Run all services (default)
#
# Examples:
#   ./run-local.sh all --env-file ~/Eng/offsite-sequencer-env/.env-local
#   ./run-local.sh sequencer --env-file ~/Eng/offsite-sequencer-env/.env-local
#   ./run-local.sh archiver --env-file ~/Eng/offsite-sequencer-env/.env-local
#   ./run-local.sh frontend --env-file ~/Eng/offsite-sequencer-env/.env-local
#   ./run-local.sh all --env-file ~/Eng/offsite-sequencer-env/.env-local --clean
#
# Required env vars:
#   SEQUENCER_NODE_ENDPOINT      - Nomos node HTTP endpoint for sequencer
#   ARCHIVER_NODE_ENDPOINT       - Nomos node HTTP endpoint for archiver
#   TOKEN_NAME                   - Token name (e.g., "MEM")

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DATA_DIR="$SCRIPT_DIR/data"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Parse service argument (first positional arg)
SERVICE="all"
if [[ $# -gt 0 && ! "$1" =~ ^-- ]]; then
    SERVICE="$1"
    shift
fi

# Validate service
case $SERVICE in
    sequencer|archiver|frontend|all)
        ;;
    *)
        echo -e "${RED}Unknown service: $SERVICE${NC}"
        echo "Valid services: sequencer, archiver, frontend, all"
        exit 1
        ;;
esac

# Parse remaining arguments
ENV_FILE=""
CLEAN_START=false
while [[ $# -gt 0 ]]; do
    case $1 in
        --env-file)
            ENV_FILE="$2"
            shift 2
            ;;
        --clean)
            CLEAN_START=true
            shift
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            exit 1
            ;;
    esac
done

# Load env file if provided
if [ -n "$ENV_FILE" ]; then
    if [ -f "$ENV_FILE" ]; then
        echo -e "${BLUE}Loading environment from: $ENV_FILE${NC}"
        set -a
        source "$ENV_FILE"
        set +a
    else
        echo -e "${RED}Error: env file not found: $ENV_FILE${NC}"
        exit 1
    fi
fi

# Validate required env vars
missing_vars=()
[ -z "$SEQUENCER_NODE_ENDPOINT" ] && missing_vars+=("SEQUENCER_NODE_ENDPOINT")
[ -z "$ARCHIVER_NODE_ENDPOINT" ] && missing_vars+=("ARCHIVER_NODE_ENDPOINT")
[ -z "$TOKEN_NAME" ] && missing_vars+=("TOKEN_NAME")

if [ ${#missing_vars[@]} -ne 0 ]; then
    echo -e "${RED}Error: Missing required environment variables:${NC}"
    for var in "${missing_vars[@]}"; do
        echo "  - $var"
    done
    echo ""
    echo "See .env-local.example for the required format."
    exit 1
fi

# Clean data directory if requested
if [ "$CLEAN_START" = true ]; then
    echo -e "${YELLOW}Cleaning data directory...${NC}"
    rm -rf "$DATA_DIR"
fi

# Create data directory (needed for channel ID file)
mkdir -p "$DATA_DIR"

# Handle CHANNEL_ID - check env, then data file, then generate new
CHANNEL_ID_FILE="$DATA_DIR/channel_id"
if [ -n "$CHANNEL_ID" ]; then
    # Use env var and save it
    echo "$CHANNEL_ID" > "$CHANNEL_ID_FILE"
    echo -e "${BLUE}Using CHANNEL_ID from environment${NC}"
elif [ -f "$CHANNEL_ID_FILE" ]; then
    # Read from saved file
    CHANNEL_ID=$(cat "$CHANNEL_ID_FILE")
    echo -e "${BLUE}Using saved CHANNEL_ID from $CHANNEL_ID_FILE${NC}"
else
    # Generate new random one
    CHANNEL_ID=$(openssl rand -hex 32)
    echo "$CHANNEL_ID" > "$CHANNEL_ID_FILE"
    echo -e "${YELLOW}Generated new CHANNEL_ID: ${CHANNEL_ID}${NC}"
fi

# Set both channel ID vars to the same value
export CHANNEL_ID
export SEQUENCER_CHANNEL_ID="$CHANNEL_ID"

# Map shared credentials to what each binary expects
export SEQUENCER_NODE_AUTH_USERNAME="$TESTNET_USERNAME"
export SEQUENCER_NODE_AUTH_PASSWORD="$TESTNET_PASSWORD"
export TESTNET_ENDPOINT="$ARCHIVER_NODE_ENDPOINT"

# Set defaults for sequencer
export SEQUENCER_DB_PATH="${SEQUENCER_DB_PATH:-$DATA_DIR/sequencer.db}"
export SEQUENCER_SIGNING_KEY_PATH="${SEQUENCER_SIGNING_KEY_PATH:-$DATA_DIR/sequencer.key}"

# Set defaults for archiver
export ARCHIVER_BLOCKS_DB_PATH="${ARCHIVER_BLOCKS_DB_PATH:-$DATA_DIR/blocks.database}"
export ARCHIVER_ACCOUNTS_DB_PATH="${ARCHIVER_ACCOUNTS_DB_PATH:-$DATA_DIR/accounts.database}"

# Get local IP for sharing
LOCAL_IP=$(ipconfig getifaddr en0 2>/dev/null || hostname -I 2>/dev/null | awk '{print $1}' || echo "localhost")

# Set VITE URLs using local IP so they work over network
export VITE_SEQUENCER_URL="${VITE_SEQUENCER_URL:-http://$LOCAL_IP:8080}"
export VITE_ARCHIVER_URL="${VITE_ARCHIVER_URL:-http://$LOCAL_IP:8090}"
export VITE_EXPLORER_URL="${VITE_EXPLORER_URL:-http://$LOCAL_IP:8000}"

echo -e "${GREEN}======================================${NC}"
echo -e "${GREEN}  L2 Demo - $SERVICE${NC}"
echo -e "${GREEN}======================================${NC}"
echo ""
echo -e "${BLUE}Configuration:${NC}"
echo "  Sequencer endpoint: $SEQUENCER_NODE_ENDPOINT"
echo "  Archiver endpoint:  $ARCHIVER_NODE_ENDPOINT"
echo "  Channel ID:         $CHANNEL_ID"
echo "  Token:              $TOKEN_NAME"
echo "  Data directory:     $DATA_DIR"
echo ""

# Check if binaries exist, if not build them
SEQUENCER_BIN="$REPO_ROOT/target/release/demo-sequencer"
ARCHIVER_BIN="$REPO_ROOT/target/release/logos-blockchain-archiver"

if [[ "$SERVICE" == "sequencer" || "$SERVICE" == "all" ]] && [ ! -f "$SEQUENCER_BIN" ]; then
    echo -e "${YELLOW}Building sequencer...${NC}"
    cd "$REPO_ROOT"
    cargo build --release -p demo-sequencer
fi

if [[ "$SERVICE" == "archiver" || "$SERVICE" == "all" ]] && [ ! -f "$ARCHIVER_BIN" ]; then
    echo -e "${YELLOW}Building archiver...${NC}"
    cd "$REPO_ROOT"
    cargo build --release -p logos-blockchain-archiver
fi

# Run the selected service(s)
case $SERVICE in
    sequencer)
        echo -e "${GREEN}Starting sequencer...${NC}"
        cd "$REPO_ROOT"
        exec "$SEQUENCER_BIN"
        ;;
    archiver)
        echo -e "${GREEN}Starting archiver...${NC}"
        cd "$REPO_ROOT"
        exec "$ARCHIVER_BIN"
        ;;
    frontend)
        cd "$SCRIPT_DIR/webapp"

        if ! command -v bun &> /dev/null; then
            echo -e "${RED}Error: bun is not installed. Install it with: curl -fsSL https://bun.sh/install | bash${NC}"
            exit 1
        fi

        if [ ! -d "node_modules" ]; then
            echo -e "${YELLOW}Installing frontend dependencies...${NC}"
            bun install
        fi

        echo -e "${GREEN}Starting frontend...${NC}"
        echo ""
        echo -e "${BLUE}Access points:${NC}"
        echo "  Frontend:  http://localhost:5173"
        echo "  Frontend:  http://$LOCAL_IP:5173  (share this with others on same network)"
        echo "  Sequencer: $VITE_SEQUENCER_URL"
        echo "  Archiver:  $VITE_ARCHIVER_URL"
        echo "  Explorer:  $VITE_EXPLORER_URL"
        echo ""
        exec bun run dev --host
        ;;
    all)
        # Trap to kill background processes on exit
        cleanup() {
            echo ""
            echo -e "${YELLOW}Shutting down...${NC}"
            kill $SEQUENCER_PID 2>/dev/null || true
            kill $ARCHIVER_PID 2>/dev/null || true
            exit 0
        }
        trap cleanup SIGINT SIGTERM

        # Start sequencer
        echo -e "${GREEN}Starting sequencer...${NC}"
        cd "$REPO_ROOT"
        "$SEQUENCER_BIN" &
        SEQUENCER_PID=$!
        sleep 2

        # Start archiver
        echo -e "${GREEN}Starting archiver...${NC}"
        "$ARCHIVER_BIN" &
        ARCHIVER_PID=$!
        sleep 2

        # Start frontend dev server
        echo -e "${GREEN}Starting frontend...${NC}"
        cd "$SCRIPT_DIR/webapp"

        if ! command -v bun &> /dev/null; then
            echo -e "${RED}Error: bun is not installed. Install it with: curl -fsSL https://bun.sh/install | bash${NC}"
            cleanup
        fi

        if [ ! -d "node_modules" ]; then
            echo -e "${YELLOW}Installing frontend dependencies...${NC}"
            bun install
        fi

        echo ""
        echo -e "${GREEN}======================================${NC}"
        echo -e "${GREEN}  All services running!${NC}"
        echo -e "${GREEN}======================================${NC}"
        echo ""
        echo -e "${BLUE}Access points:${NC}"
        echo "  Frontend:  http://localhost:5173"
        echo "  Frontend:  http://$LOCAL_IP:5173  (share this with others on same network)"
        echo "  Sequencer: $VITE_SEQUENCER_URL"
        echo "  Archiver:  $VITE_ARCHIVER_URL"
        echo "  Explorer:  $VITE_EXPLORER_URL"
        echo ""
        echo -e "${YELLOW}Press Ctrl+C to stop all services${NC}"
        echo ""

        # Run frontend in foreground
        bun run dev --host
        ;;
esac
