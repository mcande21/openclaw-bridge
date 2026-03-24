class OpenclawBridge < Formula
  desc "CLI bridge connecting Claude Code to OpenClaw gateways"
  homepage "https://github.com/mcande21/openclaw-bridge"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.1.0/openclaw-bridge-aarch64-apple-darwin.tar.xz"
      sha256 "0bafc5779e99159c9ae7b7faadbe23a419788dd07c099f04008c5884daead7e1"
    elsif Hardware::CPU.intel?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.1.0/openclaw-bridge-x86_64-apple-darwin.tar.xz"
      sha256 "4f5bd321e0a5816ba356f1d45df8bea498854643c7826c7d19af13ab4370eb25"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.1.0/openclaw-bridge-aarch64-unknown-linux-musl.tar.xz"
      sha256 "bb9860747e488836c1bf65d879000948f4c487bb1942a25f1f923e08c77793b2"
    elsif Hardware::CPU.intel?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.1.0/openclaw-bridge-x86_64-unknown-linux-musl.tar.xz"
      sha256 "e4516e52dd79511437518e99dc91af818f578679e2535db3602b29f03a6aaef0"
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
