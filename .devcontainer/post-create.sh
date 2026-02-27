#!/bin/bash

# This script is executed after the devcontainer is created
cargo fetch

curl -o- https://fnm.vercel.app/install | bash

FNM_PATH="/home/vscode/.local/share/fnm"
if [ -d "$FNM_PATH" ]; then
  export PATH="$FNM_PATH:$PATH"
  eval "`fnm env`"
fi
fnm install 20
fnm use 20

corepack enable

# Setup Python virtual environment for mssql-py-core development
WORKSPACE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
echo "Setting up Python virtual environment..."
python3 -m venv "$WORKSPACE_DIR/myvenv"
source "$WORKSPACE_DIR/myvenv/bin/activate"

# Install maturin (build tool) and dev dependencies from pyproject.toml
pip install --upgrade pip
pip install maturin
pip install -e "$WORKSPACE_DIR/mssql-py-core[dev]"

echo "Python venv ready at $WORKSPACE_DIR/myvenv"
echo "Run 'source $WORKSPACE_DIR/myvenv/bin/activate' to activate"
