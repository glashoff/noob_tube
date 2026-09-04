# What is not built yet

Work that has been thought through but not done. Everything here was decided while getting the
client into a browser; what got built along the way is in the code and its comments, and this is
what was left standing.

---

## 1. Bake the ground maps to KTX2

**The reason is not size, it is the decode.** A 5.8 MB PNG has to be turned into pixels before it
can become a texture, and in a browser that happens on the one thread the whole game runs on — the
same thread stepping physics and drawing. A supercompressed KTX2 is transcoded straight into a GPU
format instead: smaller over the wire, smaller in VRAM, and no full-resolution decode hitching the
frame a player walks into a new layer on.

It is an offline bake, so it belongs beside [`bake_ground_maps`](tools/bake_ground_maps) as a tool
with the same shape: run it, commit what comes out.

Note what this interacts with: [`ground_material.rs`](client/src/ground_material.rs) builds the mip
chains itself, on the CPU, because the PNG loader does not. A KTX2 carries its mips already, so
`build_the_mipmaps` becomes dead code for any layer that is baked — and `stack_the_layers` insists
every layer have the same size, format *and* mip count, so a half-baked set of layers will fail its
own assertion rather than draw wrongly. Bake all of them or none.

## 2. Hold the first frame until what a round needs has arrived

On native, an asset arriving late costs a frame. Over a link it costs seconds, and a player model
that turns up after the shooting started is worse than a loading screen would have been.

What a first round actually pulls is about 25 MB: the character (4.3 MB), the nineteen clips it
uses, and the loaded map's ground layers. Vehicles come later and on demand — that part is already
right, because Bevy's asset server fetches over HTTP exactly as it reads from disk, one request per
`load()` and nothing speculative.

So this is not about downloading less. It is about what the player is shown while it happens. The
menu should hold until the character, its clips and the map's layers are in.
[`remote_players.rs`](client/src/remote_players.rs) and [`character.rs`](client/src/character.rs)
already tolerate a body that is not there yet; this is the other half of that.

## 3. Stop shipping the normal maps nothing loads

`assets/textures/` is 283 MB of the tree's 357 MB. Three packs ship both a `_NormalDX` and a
`_NormalGL` — 35 MB of normal maps between them — and only one of each pair is ever read, since
`bake_ground_maps` folds the one it wants into the packed map.

Deleting the unused variant is free and makes the directory tell the truth about what the game uses.
Do it after the KTX2 bake, not before: the bake reads these files.

## 4. Serve it from the v-server, behind the nginx that is already there

`deploy.sh` provisions the machine already; this is another idempotent stanza in it, not a new
mechanism. What has to be true:

- **One HTTPS origin** serving the wasm bundle, the assets and `net-config` — the last proxied to
  the metadata port on loopback, which is what [`web/serve.py`](web/serve.py) does locally and what
  the reverse proxy has to do there.
- **The QUIC port beside it**, with the same certificate the origin uses. The server reads
  `NOOB_TUBE_CERT` and `NOOB_TUBE_KEY` (see [`certificate.rs`](server/src/certificate.rs)); certbot
  renewal has to reach it, and today that means restarting the server, because nothing re-reads a
  certificate in place.
- **The assets copied rather than linked.** [`web/build.sh`](web/build.sh) symlinks them for local
  iteration so that editing a shader and reloading works; a deployment wants a self-contained tree.
- Brotli on the static files, cache headers keyed on content. Ordinary, but on the list.

**One decision is still open and it is not mine to make:** a subdomain
(`noobtube.fkirchhoff.com`, which needs a DNS record) or a path under the existing
`game.fkirchhoff.com`. Nothing on the server has been touched either way.

## 5. Compress the map on its way to a client

A client waits about **fourteen seconds** for the ground after it connects, and the arithmetic is
not subtle: the height field is 526 338 bytes, lightyear cuts it into roughly 458 fragments of
about 1150, and the server sends at 32 Hz with one fragment to a packet. Nothing is wrong; there is
just no compression anywhere on that path. It is not a browser problem either — the same sum holds
natively, where nobody noticed because one waits a moment after starting anyway.

Measured on real maps, losslessly:

| Map | raw | zlib | delta + zlib | delta + lzma |
|---|---|---|---|---|
| `test` — barely sculpted | 526 KB | **534 B** (986×) | 1.1 KB | 216 B |
| `aaaaa` — sculpted | 526 KB | **33 KB** (15.8×) | 30 KB (17.8×) | 21 KB |
| `Terrain004_8K` — imported | 2.1 MB | 1.94 MB (**1.1×**) | 1.47 MB (1.4×) | 1.23 MB |

So the first thing to do is the cheap one: **delta-encode along rows and deflate**, in
[`TerrainBaseline::of`](shared/src/terrain.rs) and back out in `adopt`, with the length check moved
after the decompression. `flate2` with `rust_backend` is pure Rust and builds for wasm. Nothing
about the data changes; a sculpted map goes from fourteen seconds to under one.

**The third row is the honest one, and it points somewhere else.** An imported heightfield does not
compress because its low bits are noise: `Terrain004_8K` spans 128 m over 65 536 steps, which is
1.95 mm of vertical resolution on a grid whose samples are 1 m apart. Quantising to 12 bits — 3.1 cm,
still far finer than anything the grid can express — takes it to 798 KB, and `aaaaa` to 15 KB.

But **that must not happen on the wire.** The height field is shared simulation state: a client
predicts its own movement against it, and one holding a coarser field than the server would
mispredict systematically and be corrected back on ground that looks different on the two machines.
If the precision is worth reducing, reduce it in
[`import_heightmap`](tools/import_heightmap) — once, in the file, where both sides then read the
same numbers and the transfer benefits as a side effect.

## 6. Get rid of the Bevy patch

The `[patch.crates-io]` block at the end of [`Cargo.toml`](Cargo.toml) is sixty-seven lines that
exist for one expression. Bevy 0.19.1 promises a `min_binding_size` of 16 bytes for the mesh view
bind group's visibility-range binding whichever type that binding was given, which is right only on
the storage path; on WebGL2 the shader declares a fixed `array<vec4<f32>, 64>` and needs all 1024,
so no PBR pipeline can be built and the client quits.

Reported upstream as [bevyengine/bevy#21309](https://github.com/bevyengine/bevy/issues/21309) in
October 2025. The fix lives on two branches of a fork — one off `main` for the pull request, one off
the `v0.19.1` tag which is what the pin points at — and a reproduction that builds both in a browser
is at [`bevy-webgl2-visibility-range-repro`](https://github.com/glashoff/bevy-webgl2-visibility-range-repro).

**Delete the whole block** the day a Bevy release carries the fix. Until then it is pinned by
revision and never by branch, so that what this tree builds against cannot change underneath it.
Every Bevy crate is patched rather than the two that differ, because patching a subset pulls half
the tree out of the repository through its `path` dependencies and leaves two copies of thirty-five
crates whose types are not each other's — six hundred compiler errors that say nothing about the
patch.
