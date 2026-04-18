# Maintainer: ederadar <shevelievs@gmail.com>

pkgname=wpick
pkgver=0.1.0
pkgrel=1
pkgdesc="Native Wayland live wallpaper manager for Wallpaper Engine"
arch=(x86_64)
url="https://github.com/ederadar/wpick"
license=(MIT)
depends=(ffmpeg wayland)
makedepends=(rust cargo git)
source=("$pkgname::git+https://github.com/ederadar/wpick#tag=v$pkgver")
sha256sums=(SKIP)

prepare() {
    cd "$pkgname"
    export RUSTUP_TOOLCHAIN=stable
    cargo fetch --locked --target "$CARCH-unknown-linux-gnu"
}

build() {
    cd "$pkgname"
    export RUSTUP_TOOLCHAIN=stable
    export CARGO_TARGET_DIR=target
    cargo build --workspace --release --locked --offline
}

package() {
    cd "$pkgname"
    install -Dm755 target/release/wpick        "$pkgdir/usr/bin/wpick"
    install -Dm755 target/release/wpick-daemon "$pkgdir/usr/bin/wpick-daemon"
    install -Dm644 LICENSE                     "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
