class OpenclawBridge < Formula
  desc "CLI bridge connecting Claude Code to OpenClaw gateways"
  homepage "https://github.com/mcande21/openclaw-bridge"
  version "0.2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.2.0/openclaw-bridge-aarch64-apple-darwin.tar.xz"
      sha256 "6e78f454fc6bf08f7e7d1b834eb14ea49f21140869908f79e70d86b55f2d5fdf"
    elsif Hardware::CPU.intel?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.2.0/openclaw-bridge-x86_64-apple-darwin.tar.xz"
      sha256 "56a6aa3f8297be9e39198a9f0ca1d26680fbc132f5cd2c334f1ef152ba79d7f5"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.2.0/openclaw-bridge-aarch64-unknown-linux-musl.tar.xz"
      sha256 "ecc00756c4c2f01ac7d50a267ad4cd442ebf86f1f89b4c9134d720dc16c47504"
    elsif Hardware::CPU.intel?
      url "https://github.com/mcande21/openclaw-bridge/releases/download/v0.2.0/openclaw-bridge-x86_64-unknown-linux-musl.tar.xz"
      sha256 "abb7938b808664f5ea104365edaeb6d87da73a2cdae7dbd2c2c9f059067c8dfb"
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
