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
