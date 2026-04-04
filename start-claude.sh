#!/bin/bash
# Anthropic Rich Presence - macOS/Linux launcher

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Check Python
if ! command -v python3 &>/dev/null; then
    echo "Python 3 is not installed."
    echo "Install it from https://www.python.org/ or via: brew install python"
    exit 1
fi

# Check if Discord is running
if ! pgrep -xi "discord" &>/dev/null; then
    echo ""
    echo "[ERROR] Discord is not running."
    echo "Please open Discord before launching Anthropic Rich Presence."
    exit 1
fi

# Install dependencies
python3 -c "import pypresence" &>/dev/null
if [ $? -ne 0 ]; then
    echo "Installing dependencies..."
    pip3 install -r requirements.txt --quiet
fi

# First-run setup
if [ ! -f ".env" ]; then
    echo ""
    echo "=== First-time setup ==="
    echo ""
    echo "You need a Discord Application ID:"
    echo "  1. Go to https://discord.com/developers/applications"
    echo "  2. Create a new application (e.g. \"Claude AI\")"
    echo "  3. Copy the Application ID"
    echo ""
    read -p "Paste your Discord Application ID here: " CLIENT_ID
    echo "DISCORD_CLIENT_ID=${CLIENT_ID}" > .env
    echo "CLAUDE_DIR_PATH=~/.claude" >> .env
    echo "CLAUDE_MODEL=claude-opus-4-6" >> .env
    echo ""
    echo "Saved to .env"
fi

echo "Starting Anthropic Rich Presence..."
python3 main.py
