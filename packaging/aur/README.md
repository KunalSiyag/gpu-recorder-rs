# AUR packages

These are the sources for the [AUR](https://aur.archlinux.org/) packages:

- `hypergsr-bin/` installs the prebuilt binary from the GitHub release tarball.
- `hypergsr-git/` builds the latest `main` from source.

Both install `/usr/bin/gpu-recorder-rs` (plus a `hypergsr` symlink), the capture helper in
`/usr/lib/hypergsr/`, UI icons in `/usr/share/hypergsr/assets/`, and a desktop entry.

## Releasing a new `-bin` version

1. Publish the GitHub release with `hypergsr-v<version>-linux-x86_64.tar.gz`.
2. Update `pkgver` (reset `pkgrel=1`) and `sha256sums` in `hypergsr-bin/PKGBUILD`.
3. Regenerate metadata: `makepkg --printsrcinfo > .SRCINFO`.
4. Copy `PKGBUILD` and `.SRCINFO` into the AUR clone (`ssh://aur@aur.archlinux.org/hypergsr-bin.git`), commit, and push.

`hypergsr-git` only needs a push when its `PKGBUILD` changes; `pkgver()` tracks the repository.
