#!/bin/bash
#
# fatx-rs setup script for macOS
# Installs Rust (if needed), builds all tools, and optionally installs them.
#

set -e

BOLD='\033[1m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo ""
echo -e "${BOLD}========================================${NC}"
echo -e "${BOLD}  fatx-rs setup — Xbox FATX for macOS   ${NC}"
echo -e "${BOLD}========================================${NC}"
echo ""

# ---------------------------------------------------------------------------
# Step 1: Check / install Rust
# ---------------------------------------------------------------------------
echo -e "${BOLD}[1/4] Checking for Rust toolchain...${NC}"

if command -v cargo &> /dev/null; then
    RUST_VER=$(rustc --version)
    echo -e "  ${GREEN}Found: $RUST_VER${NC}"
else
    echo -e "  ${YELLOW}Rust not found. Installing via rustup...${NC}"
    echo ""
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    echo ""
    echo -e "  ${GREEN}Installed: $(rustc --version)${NC}"
fi
echo ""

# ---------------------------------------------------------------------------
# Step 2: Build
# ---------------------------------------------------------------------------
echo -e "${BOLD}[2/4] Building all tools (release mode)...${NC}"
echo ""

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

cargo build --release 2>&1

echo ""
echo -e "  ${GREEN}Built successfully:${NC}"
echo "    target/release/fatx         (main CLI)"
echo "    target/release/fatx-mount   (NFS mount server)"
echo "    target/release/fatx-mkimage (test image generator)"
echo ""

# ---------------------------------------------------------------------------
# Step 3: Optional install to /usr/local/bin
# ---------------------------------------------------------------------------
echo -e "${BOLD}[3/4] Install to /usr/local/bin? (makes 'fatx' available system-wide)${NC}"
read -p "  Install? (y/n) [n]: " INSTALL_CHOICE

BINARIES="fatx fatx-mount fatx-mkimage"

if [[ "$INSTALL_CHOICE" == "y" || "$INSTALL_CHOICE" == "Y" ]]; then
    for bin in $BINARIES; do
        SRC="$SCRIPT_DIR/target/release/$bin"
        if [ -f "$SRC" ]; then
            if [[ -w /usr/local/bin ]]; then
                cp "$SRC" "/usr/local/bin/$bin"
            else
                echo "  Need sudo to copy to /usr/local/bin..."
                sudo cp "$SRC" "/usr/local/bin/$bin"
            fi
            echo -e "  ${GREEN}Installed /usr/local/bin/$bin${NC}"
        fi
    done
else
    echo "  Skipped. You can run it directly:"
    echo "    sudo ./target/release/fatx"
fi
echo ""

# ---------------------------------------------------------------------------
# Step 4: Quick help
# ---------------------------------------------------------------------------
echo -e "${BOLD}[4/4] Quick start${NC}"
echo ""
echo "  Interactive mode (guided — prompts for everything):"
echo -e "    ${GREEN}sudo fatx${NC}"
echo ""
echo "  Subcommands:"
echo "    sudo fatx scan /dev/rdisk4"
echo "    sudo fatx ls /dev/rdisk4 --partition \"Data (E)\" / -l"
echo "    sudo fatx read /dev/rdisk4 --partition \"Data (E)\" /saves/game.sav -o game.sav"
echo "    sudo fatx mount /dev/rdisk4 --partition \"360 Data\" -v --mount"
echo "    fatx mkimage test.img --size 1G --populate"
echo ""
echo "  For full help:"
echo "    fatx --help"
echo ""
echo -e "${BOLD}Important notes:${NC}"
echo "  - Use /dev/rdiskN (raw device) for best performance"
echo "  - Unmount the disk first: diskutil unmountDisk /dev/diskN"
echo "  - sudo is required for raw device access and mounting"
echo ""
echo -e "${GREEN}Setup complete!${NC}"
