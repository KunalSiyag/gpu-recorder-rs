# Flatpak bundle

`io.github.KunalSiyag.gpu_recorder_rs.yml` builds the self-hosted `hypergsr.flatpak`
bundle that the Flatpak workflow attaches to each GitHub release.

This manifest was written with AI assistance. Flathub's Generative AI policy does not
accept AI-assisted manifests or AI-opened submissions, so do not submit this file to
Flathub. A Flathub submission needs a manifest written and submitted by a person.

## Build and install locally

```bash
flatpak install --user flathub org.flatpak.Builder org.gnome.Sdk//50 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak run org.flatpak.Builder --user --install --force-clean --install-deps-from=flathub \
  build-dir packaging/flatpak/io.github.KunalSiyag.gpu_recorder_rs.yml
flatpak run io.github.KunalSiyag.gpu_recorder_rs
```

Run the commands from the repository root. To produce a bundle file instead of
installing:

```bash
flatpak run org.flatpak.Builder --repo=repo --force-clean build-dir \
  packaging/flatpak/io.github.KunalSiyag.gpu_recorder_rs.yml
flatpak build-bundle repo hypergsr.flatpak io.github.KunalSiyag.gpu_recorder_rs \
  --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
```

## Updating dependencies

`cargo-sources.json` lets the build run offline. Regenerate it whenever `Cargo.lock`
changes, using `flatpak-cargo-generator.py` from
[flatpak-builder-tools](https://github.com/flatpak/flatpak-builder-tools):

```bash
python3 flatpak-cargo-generator.py Cargo.lock -o packaging/flatpak/cargo-sources.json
```
