#!/usr/bin/env bash
# =============================================================================
# LCode installer — downloads the latest release binary from GitHub.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Lixiang9716/LCode/main/install.sh | bash
#   # or:  bash install.sh [--dir /custom/path]
#
# Environment:
#   LCODE_INSTALL_DIR  install directory (default: ~/.local/bin)
#   LCODE_VERSION      specific version tag to install (default: latest)
# =============================================================================

set -euo pipefail

REPO="Lixiang9716/LCode"
API="https://api.github.com/repos/${REPO}/releases/latest"
BASE_URL="https://github.com/${REPO}/releases/download"

INSTALL_DIR="${LCODE_INSTALL_DIR:-${HOME}/.local/bin}"
VERSION="${LCODE_VERSION:-latest}"

# --- Platform detection -----------------------------------------------------

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "${OS}" in
  linux)
    case "${ARCH}" in
      x86_64 | amd64) ASSET="lcode-linux-x86_64.tar.gz" ;;
      aarch64 | arm64) ASSET="lcode-linux-aarch64.tar.gz" ;;
      *)
        echo "❌ Unsupported architecture: ${ARCH} (linux)" >&2
        echo "   Available: x86_64, aarch64" >&2
        exit 1
        ;;
    esac
    ;;
  darwin)
    case "${ARCH}" in
      x86_64) ASSET="lcode-macos-x86_64.tar.gz" ;;
      arm64) ASSET="lcode-macos-aarch64.tar.gz" ;;
      *)
        echo "❌ Unsupported architecture: ${ARCH} (macos)" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "❌ Unsupported OS: ${OS}" >&2
    echo "   Windows users: download lcode-windows-x86_64.exe from the release page" >&2
    exit 1
    ;;
esac

# --- Resolve version ---------------------------------------------------------

if [ "${VERSION}" = "latest" ]; then
  echo "🔍 Fetching the latest release..."
  # No jq dependency: parse tag_name with grep/cut.
  VERSION="$(curl -fsSL "${API}" | grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | cut -d'"' -f4)"
  if [ -z "${VERSION}" ]; then
    echo "❌ Failed to determine the latest version" >&2
    exit 1
  fi
fi

URL="${BASE_URL}/${VERSION}/${ASSET}"
echo "📦 Installing LCode ${VERSION} (${ASSET})"

# --- Download & install ------------------------------------------------------

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

echo "   Downloading ${URL} ..."
curl -fsSL "${URL}" -o "${TMP_DIR}/${ASSET}"

mkdir -p "${INSTALL_DIR}"
case "${ASSET}" in
  *.tar.gz)
    tar -xzf "${TMP_DIR}/${ASSET}" -C "${TMP_DIR}"
    install -m 755 "${TMP_DIR}/lcode" "${INSTALL_DIR}/lcode"
    ;;
  *.exe)
    install -m 755 "${TMP_DIR}/${ASSET}" "${INSTALL_DIR}/lcode.exe"
    ;;
esac

# --- PATH hint ---------------------------------------------------------------

echo "🎉 Installed lcode ${VERSION} to ${INSTALL_DIR}"

if ! command -v lcode >/dev/null 2>&1; then
  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
      echo ""
      echo "💡 Add ${INSTALL_DIR} to your PATH:"
      case "${SHELL}" in
        *zsh) echo "   echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.zshrc && source ~/.zshrc" ;;
        *bash) echo "   echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc && source ~/.bashrc" ;;
        *) echo "   export PATH=\"${INSTALL_DIR}:\$PATH\"" ;;
      esac
      ;;
  esac
fi

echo ""
echo "🚀 Run 'lcode --version' to verify, and 'lcode update' to self-update later."
