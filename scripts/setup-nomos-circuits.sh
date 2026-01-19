#!/bin/bash
#
# Setup script for nomos-circuits
#
# Usage: ./setup-nomos-circuits.sh [VERSION] [INSTALL_DIR]
#
# Arguments:
#   VERSION      - Optional. Version to install (default: v0.3.1)
#   INSTALL_DIR  - Optional. Installation directory (default: $HOME/.nomos-circuits)
#
# Examples:
#   ./setup-nomos-circuits.sh                    # Install default version to default location
#   ./setup-nomos-circuits.sh v0.2.0             # Install specific version to default location
#   ./setup-nomos-circuits.sh v0.2.0 /opt/circuits  # Install to custom location

set -e

# Default values
VERSION="${1:-v0.3.2}"
DEFAULT_INSTALL_DIR="$HOME/.nomos-circuits"
INSTALL_DIR="${2:-$DEFAULT_INSTALL_DIR}"
REPO="logos-blockchain/logos-blockchain-circuits"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_info() {
    echo -e "${BLUE}ℹ${NC} $1"
}

print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

# Detect OS and architecture
detect_platform() {
    local os=""
    local arch=""

    # Detect OS
    case "$(uname -s)" in
        Linux*)     os="linux";;
        Darwin*)    os="macos";;
        MINGW*|MSYS*|CYGWIN*)    os="windows";;
        *)          print_error "Unsupported operating system: $(uname -s)"; exit 1;;
    esac

    # Detect architecture
    case "$(uname -m)" in
        x86_64)     arch="x86_64";;
        aarch64)    arch="aarch64";;
        arm64)      arch="aarch64";;
        *)          print_error "Unsupported architecture: $(uname -m)"; exit 1;;
    esac

    echo "${os}-${arch}"
}

# Check if installation directory exists and get confirmation
check_existing_installation() {
    if [ -d "$INSTALL_DIR" ]; then
        print_warning "Installation directory already exists: $INSTALL_DIR"

        # Check if it has a VERSION file
        if [ -f "$INSTALL_DIR/VERSION" ]; then
            local current_version=$(cat "$INSTALL_DIR/VERSION")
            print_info "Currently installed version: $current_version"
        fi

        # In non-interactive environments (CI), automatically overwrite
        if [ ! -t 0 ]; then
            print_info "Non-interactive environment detected, automatically overwriting..."
        else
            # Interactive environment - ask for confirmation
            echo
            read -p "Do you want to overwrite it? (y/N): " -n 1 -r
            echo
            if [[ ! $REPLY =~ ^[Yy]$ ]]; then
                print_info "Installation cancelled."
                exit 0
            fi
        fi

        print_info "Removing existing installation..."
        rm -rf "$INSTALL_DIR"
    fi
}

# Download and extract the release
download_release() {
    local platform="$1"
    local artifact="nomos-circuits-${VERSION}-${platform}.tar.gz"
    local url="https://github.com/${REPO}/releases/download/${VERSION}/${artifact}"
    local temp_dir=$(mktemp -d)

    print_info "Downloading nomos-circuits ${VERSION} for ${platform}..."
    print_info "URL: $url"

    # Build curl command with optional authentication
    local curl_cmd="curl -L"
    if [ -n "$GITHUB_TOKEN" ]; then
        curl_cmd="$curl_cmd --header 'authorization: Bearer ${GITHUB_TOKEN}'"
    fi
    curl_cmd="$curl_cmd -o ${temp_dir}/${artifact} $url"

    if ! eval "$curl_cmd"; then
        print_error "Failed to download release artifact"
        print_error "Please check that version ${VERSION} exists for platform ${platform}"
        print_error "Available releases: https://github.com/${REPO}/releases"
        rm -rf "$temp_dir"
        exit 1
    fi

    print_success "Download complete"

    print_info "Extracting to ${INSTALL_DIR}..."
    mkdir -p "$INSTALL_DIR"

    if ! tar -xzf "${temp_dir}/${artifact}" -C "$INSTALL_DIR" --strip-components=1; then
        print_error "Failed to extract archive"
        rm -rf "$temp_dir"
        exit 1
    fi

    rm -rf "$temp_dir"
    print_success "Extraction complete"
}

# Handle macOS code signing/quarantine issues
handle_macos_quarantine() {
    print_info "macOS detected: Removing quarantine attributes from executables..."

    # Remove quarantine attribute from all executable files
    if find "$INSTALL_DIR" -type f -perm +111 -exec xattr -d com.apple.quarantine {} \; 2>/dev/null; then
        print_success "Quarantine attributes removed"
    else
        print_warning "Could not remove quarantine attributes (they may not exist)"
    fi
}

# Main installation process
main() {
    print_info "Setting up nomos-circuits ${VERSION}"
    print_info "Installation directory: $INSTALL_DIR"
    echo

    # Detect platform
    local platform=$(detect_platform)
    print_info "Detected platform: $platform"

    # Check existing installation
    check_existing_installation

    # Download and extract
    download_release "$platform"

    # Handle macOS quarantine if needed
    if [[ "$platform" == macos-* ]]; then
        echo
        handle_macos_quarantine
    fi

    echo
    print_success "Installation complete!"
    echo
    print_info "nomos-circuits ${VERSION} is now installed at: $INSTALL_DIR"
    print_info "The following circuits are available:"

    # Discover circuits by finding directories that contain a witness_generator
    for dir in "$INSTALL_DIR"/*/; do
        if [ -d "$dir" ]; then
            local circuit_name=$(basename "$dir")
            if [ -f "$dir/witness_generator" ]; then
                echo "  • $circuit_name"
            fi
        fi
    done

    # Only show export instructions if not using the default location
    if [ "$INSTALL_DIR" != "$DEFAULT_INSTALL_DIR" ]; then
        echo
        print_info "Since you're using a custom installation directory, set the environment variable:"
        print_info "  export NOMOS_CIRCUITS=$INSTALL_DIR"
        echo
    fi
}

# Run main
main
