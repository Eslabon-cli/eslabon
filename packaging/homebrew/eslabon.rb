# eslabon: Homebrew formula template for the public tap (P7.2). This file is a
# template only; the tap is not published from this repository and none of the
# REPLACE_ME markers work as-is.
#
# Publishing it:
#   1. run scripts/eslabon/release/build.sh on each platform and collect the
#      tar.gz + .sha256 pairs from dist/;
#   2. create the `Eslabon-cli/homebrew-tap` repository with this file at
#      Formula/eslabon.rb, replacing RELEASE_TAG (e.g. 0.170.0) and every
#      REPLACE_ME url/sha256 with the release values;
#   3. users then run `brew tap Eslabon-cli/homebrew-tap` and `brew install eslabon`.
#
# On each new release, update the version in the urls and the sums; `brew
# bump-formula-pr` cannot infer a version from the asset names, so do it by hand.
class Eslabon < Formula
  desc "Coding agent for the terminal that runs on local models first"
  homepage "https://github.com/Eslabon-cli/eslabon"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/Eslabon-cli/eslabon/releases/download/vRELEASE_TAG/eslabon-RELEASE_TAG-macos-arm64.tar.gz"
      sha256 "REPLACE_ME"
    end
    on_intel do
      url "https://github.com/Eslabon-cli/eslabon/releases/download/vRELEASE_TAG/eslabon-RELEASE_TAG-macos-x86_64.tar.gz"
      sha256 "REPLACE_ME"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Eslabon-cli/eslabon/releases/download/vRELEASE_TAG/eslabon-RELEASE_TAG-linux-arm64.tar.gz"
      sha256 "REPLACE_ME"
    end
    on_intel do
      url "https://github.com/Eslabon-cli/eslabon/releases/download/vRELEASE_TAG/eslabon-RELEASE_TAG-linux-x86_64.tar.gz"
      sha256 "REPLACE_ME"
    end
  end

  def install
    bin.install "eslabon"
  end

  test do
    assert_match "eslabon", shell_output("#{bin}/eslabon --version 2>&1")
  end
end
