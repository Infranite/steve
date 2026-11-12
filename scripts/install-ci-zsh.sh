#!/bin/sh
# Linux CI needs /bin/zsh for the legacy release fixtures. Fetch a fixed, hermetic build from
# GitHub instead of relying on the Ubuntu package mirrors on every run.
set -eu

archive=zsh-5.8-linux-x86_64.tar.gz
sha256=6df668fb6e9a12874e0d80518d582f2e99e512d4a4532fa73d938360aaddc838
url="https://github.com/romkatv/zsh-bin/releases/download/v6.1.1/$archive"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

curl --fail --location --silent --show-error --retry 3 --retry-all-errors \
  --output "$tmp/$archive" "$url"
printf '%s  %s\n' "$sha256" "$tmp/$archive" | sha256sum --check --status
tar -xzf "$tmp/$archive" -C "$tmp" bin/zsh
sudo install -m 0755 "$tmp/bin/zsh" /bin/zsh
/bin/zsh -fc 'zmodload zsh/system; zmodload zsh/datetime; print -r -- "$ZSH_VERSION"'
