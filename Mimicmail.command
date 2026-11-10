#!/bin/zsh
set -eu
cd -- "${0:A:h}"
if [[ ! -x target/release/mimicmail ]]; then
  cargo_bin="$(command -v cargo || true)"
  if [[ -z "$cargo_bin" ]]; then
    cargo_bin="$HOME/.cargo/bin/cargo"
  fi
  if [[ ! -x "$cargo_bin" ]]; then
    print -u2 'Rust를 설치한 뒤 cargo build --release --locked를 실행하세요.'
    exit 1
  fi
  "$cargo_bin" build --release --locked
fi
exec ./target/release/mimicmail gui
