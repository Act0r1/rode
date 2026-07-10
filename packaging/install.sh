#!/bin/sh
set -eu

prefix=${PREFIX:-/usr/local}
destdir=${DESTDIR:-}

cargo build --release
install -Dm755 target/release/rode "$destdir$prefix/bin/rode"
install -Dm644 packaging/dev.rode.Rode.desktop \
  "$destdir$prefix/share/applications/dev.rode.Rode.desktop"
install -Dm644 packaging/dev.rode.Rode.svg \
  "$destdir$prefix/share/icons/hicolor/scalable/apps/dev.rode.Rode.svg"
install -Dm644 packaging/dev.rode.Rode.metainfo.xml \
  "$destdir$prefix/share/metainfo/dev.rode.Rode.metainfo.xml"
