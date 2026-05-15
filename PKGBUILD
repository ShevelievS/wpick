# Maintainer: ShevelievS <shevelievs@gmail.com>

pkgname=wpick
pkgver=0.4.1
pkgrel=1
pkgdesc="Native Wayland live wallpaper daemon for Wallpaper Engine (Steam) content"
arch=(x86_64)
url="https://github.com/ShevelievS/wpick"
license=(MIT)
depends=(ffmpeg libpulse wayland)
makedepends=(rust cargo git)
source=("$pkgname::git+https://github.com/ShevelievS/wpick#tag=v$pkgver")
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

    # Binaries
    install -Dm755 target/release/wpick        "$pkgdir/usr/bin/wpick"
    install -Dm755 target/release/wpick-daemon "$pkgdir/usr/bin/wpick-daemon"

    # License
    install -Dm644 LICENSE \
        "$pkgdir/usr/share/licenses/$pkgname/LICENSE"

    # systemd user service
    install -Dm644 dist/systemd/wpick-daemon.service \
        "$pkgdir/usr/lib/systemd/user/wpick-daemon.service"

    # Shell completions (generated from the built binary)
    target/release/wpick completions bash \
        > "$srcdir/wpick.bash"
    target/release/wpick completions zsh \
        > "$srcdir/_wpick"
    target/release/wpick completions fish \
        > "$srcdir/wpick.fish"

    install -Dm644 "$srcdir/wpick.bash" \
        "$pkgdir/usr/share/bash-completion/completions/wpick"
    install -Dm644 "$srcdir/_wpick" \
        "$pkgdir/usr/share/zsh/site-functions/_wpick"
    install -Dm644 "$srcdir/wpick.fish" \
        "$pkgdir/usr/share/fish/vendor_completions.d/wpick.fish"

    # Man page
    target/release/wpick man > "$srcdir/wpick.1"
    install -Dm644 "$srcdir/wpick.1" \
        "$pkgdir/usr/share/man/man1/wpick.1"
}
