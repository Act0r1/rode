#!/bin/sh
set -eu

prefix=${PREFIX:-/usr/local}
install_root=${DESTDIR:-}
unset DESTDIR

cargo build --release
install -Dm755 target/release/rode "$install_root$prefix/bin/rode"
install -Dm644 packaging/dev.rode.Rode.desktop \
  "$install_root$prefix/share/applications/dev.rode.Rode.desktop"
install -Dm644 packaging/dev.rode.Rode.svg \
  "$install_root$prefix/share/icons/hicolor/scalable/apps/dev.rode.Rode.svg"
install -Dm644 packaging/dev.rode.Rode.metainfo.xml \
  "$install_root$prefix/share/metainfo/dev.rode.Rode.metainfo.xml"
