#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 3 ]; then
  echo "Usage: $0 <tag> <arm64_sha256> <repo_owner/repo_name>" >&2
  exit 1
fi

TAG="$1"                # e.g. v0.1.0
ARM_SHA="$2"
REPO="$3"               # e.g. MaydayV/Terminal-Translation

VERSION="${TAG#v}"

cat <<FORMULA
class Tetr < Formula
  desc "Terminal real-time translation assistant"
  homepage "https://github.com/${REPO}"
  license "MIT"
  version "${VERSION}"
  depends_on arch: :arm64

  url "https://github.com/${REPO}/releases/download/v#{version}/tetr-v#{version}-macos-arm64.tar.gz"
  sha256 "${ARM_SHA}"

  def install
    bin.install "tetr"
    bin.install "tetr-ui"
  end

  test do
    assert_match "Terminal Translation Assistant", shell_output("#{bin}/tetr --help")
  end
end
FORMULA
