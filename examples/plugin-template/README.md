# A RustCloud plugin template

A plugin built on its own, without the server's code: `wit/app.wit` (the
contract, copied from the server of the version in `manifest.toml`) and
`wit-bindgen` are all it needs. This one uses the server's ffmpeg
(`use.ffmpeg`) to tell what a photo is and to make its thumbnail.

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
mkdir -p dist/photo-info && cp manifest.toml dist/photo-info/
cp target/wasm32-wasip2/release/photo_info.wasm dist/photo-info/app.wasm
(cd dist && zip -r photo-info.zip photo-info)
```

Install `photo-info.zip` on Administration → Plugins and enable it (that
approves `files = "folder"` and `use.ffmpeg`). `.github/workflows/build.yml`
does the same in Actions. See the server's `docs/dev/plugins.md` and
`docs/dev/use.md`.

The template is licensed under MIT OR Apache-2.0 (`LICENSE-MIT`,
`LICENSE-APACHE`): a plugin made from it may use any licence, a closed one
included. Replace the licence files and the `license` field with your own.
