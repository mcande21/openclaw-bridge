class OpenclawBridge < Formula
  desc "CLI bridge connecting Claude Code to OpenClaw gateways"
  homepage "https://github.com/mcande21/openclaw-bridge"
  version "0.2.1"

  on_macos do
    url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.2.1/openclaw-bridge-0.2.1-universal-apple-darwin.tar.xz"
    sha256 "4b385070228daf5a947258c364fe342c5dcb2c290d4f0990b1fdcedcc13aaf33"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.2.1/openclaw-bridge-aarch64-unknown-linux-musl.tar.xz"
      sha256 "372050fd022cefdeb0bbeb19b0dcb1d4d26f989d9fba7e56c1cd728eb231aa97"
    elsif Hardware::CPU.intel?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.2.1/openclaw-bridge-x86_64-unknown-linux-musl.tar.xz"
      sha256 "2efcf659a0839d01f6ba6a4a1abb954954bf78382a04d387ffa88130c9171b72"
    end
  end

  license "MIT"

  def install
    bin.install "ocb"
  end

  test do
    assert_match "ocb", shell_output("#{bin}/ocb --version")
  end
end
