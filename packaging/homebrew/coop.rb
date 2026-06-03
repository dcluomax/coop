# Homebrew formula for Coop (coopd daemon + coop CLI).
#
# This file is a template: the release workflow (.github/workflows/release.yml,
# the `homebrew` job) substitutes __VERSION__ and the four __*_SHA256__ tokens
# with the published release's version and per-asset checksums, then publishes
# the result to the `dcluomax/homebrew-coop` tap.
#
#   brew install dcluomax/coop/coop
class Coop < Formula
  desc "Self-hosted AI agent farm OS (coopd daemon + coop CLI)"
  homepage "https://github.com/dcluomax/coop"
  version "__VERSION__"
  license "Apache-2.0"

  on_macos do
    url "https://github.com/dcluomax/coop/releases/download/v__VERSION__/coop-v__VERSION__-universal-apple-darwin.tar.gz"
    sha256 "__MACOS_UNIVERSAL_SHA256__"
  end

  on_linux do
    on_arm do
      url "https://github.com/dcluomax/coop/releases/download/v__VERSION__/coop-v__VERSION__-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "__LINUX_ARM64_SHA256__"
    end
    on_intel do
      url "https://github.com/dcluomax/coop/releases/download/v__VERSION__/coop-v__VERSION__-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "__LINUX_X86_64_SHA256__"
    end
  end

  def install
    bin.install "coopd"
    bin.install "coop"
  end

  test do
    assert_match "coop", shell_output("#{bin}/coop --help")
    assert_match "coopd", shell_output("#{bin}/coopd --help")
  end
end
