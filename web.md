# Web plan — Noob Tube

Design notes for running the client in a browser. Written 2026-09-04, and **most of it is built**
since — the commits from *Speak the transport a browser can speak* onwards. What is still a plan is
marked where it appears; §4's asset work and §8's deployment are the two large pieces left. Where a
decision below turned out differently once it met a browser, the section says so rather than being
quietly rewritten.

The simulation is not the problem. The server stays exactly what it is — a native headless binary —
and the shared crate already keeps the client from reading anything the server owns: the map arrives
over the wire, not off a disk ([`terrain.rs`](shared/src/terrain.rs), §5 of
[`terrain.md`](terrain.md)). What has to change is everything around the edge of the process: the
socket, the two seconds before `App::new`, the files the client opens by path, the renderer feature
that was never chosen because a desktop build did not have to choose, and the assets, which are
sized for a local SSD.

Five decisions are taken up front, because each one removes a fork in the road that would otherwise
run through every section below:

> **One transport: WebTransport, on native as well.** No UDP path kept in parallel, no WebSocket
> fallback (§1).
>
> **One renderer: WebGPU.** No WebGL2 build until somebody actually needs one (§3).
>
> **One bootstrap: HTTP and JSON, same origin, fetched before the wasm module starts** — and the
> native client uses the same endpoint (§2).
>
> **Assets stay lazy.** The browser downloads what a round touches, which is already about 25 MB of
> the 355 MB in `assets/`; the work is format and decode cost, not culling (§4).
>
> **Pointer lock comes from the menu's Resume button** — a real click, which is what the browser
> requires (§7).

---

## 0. What already survives the move

Worth stating first, because it is most of the project:

- **The simulation, replication, prediction and rollback.** Nothing in `shared/` touches a socket,
  a file or a clock outside Bevy's.
- **Maps.** A client is forbidden to read one for itself; the terrain comes over the connection.
  `maps/` never has to be published at all.
- **Shaders.** [`ground.wgsl`](assets/shaders/ground.wgsl) and [`water.wgsl`](assets/shaders/water.wgsl)
  use no storage buffers, no compute, no bindless — the three things that would have forced WebGPU
  even if WebGL2 were wanted.
- **The metadata endpoint is already HTTP.** [`metadata.rs`](shared/src/metadata.rs) hand-writes an
  HTTP/1.1 response over a `TcpListener`. Only the *client* half of it is native-only.
- **Netcode.** Lightyear's netcode layer is transport-agnostic; `Authentication::Manual`, the
  protocol id and the placeholder key are unaffected by §1.

---

## 1. Transport: WebTransport, and only WebTransport

A browser has no UDP socket, so [`main.rs`](client/src/main.rs#L348)'s `UdpIo` + `LocalAddr` +
`PeerAddr` cannot survive. Lightyear 0.29 offers two replacements; this plan takes one of them and
drops the other.

**Not WebSocket.** It is TCP. A lost packet stalls every packet behind it, which is precisely the
failure a rollback shooter is built to avoid: the correction that would have fixed a predicted body
waits behind a retransmission of an input the client has already moved past. WebSocket is the
transport you keep for the browser that cannot do better, and as of Safari 26.4 (March 2026) there
is no such browser left among the ones that matter. Keeping it would mean carrying a second
`Link` type, a second port, a second conditioner path and a second set of timing bugs, for an
audience of nobody.

**WebTransport on native too.** The question is whether the native client and server should keep
their UDP path beside the browser's. The argument against a second path is not tidiness, it is
rot: everything that runs locally here — the harness, the bots, two clients side by side — runs
native, so a native-only UDP path is the path that gets exercised and the browser path is the one
that quietly breaks. One wire means every local test run is also a test of what a browser player
gets.

What it costs:

- **QUIC instead of raw datagrams.** A handshake, a few bytes of framing and encryption per packet,
  `quinn` and `rustls` in the server's dependency tree. At 64 Hz with the payloads this game sends,
  the per-packet cost is not measurable next to the link conditioner; the handshake is once.
- **Certificates on localhost.** This is the real friction. A native client talking to
  `127.0.0.1` now needs to accept a certificate. Lightyear has `webtransport_self_signed` and
  `webtransport_dangerous_configuration` for exactly this; the dev path uses a generated cert and
  the shipped path uses the real one.
- **Netcode's encryption becomes redundant** — QUIC already encrypts. Harmless, and not worth
  unpicking: netcode is also what does connection tokens, client ids and the protocol-id refusal
  that makes a tick-rate mismatch fail safely.

**No escape hatch, in the end.** The plan was to keep `UdpIo` behind a cargo feature for one
milestone so a latency regression could be bisected against the transport. It was not built: the
flag needs both lightyear IO features compiled in and a `cfg` fork in each of the two binaries plus
the config, and every change after it would have had to keep both alive — which is the second path
this section just refused, wearing a different hat. Git holds the last UDP commit, and bisecting
against it costs one checkout.

**Certificate.** WebTransport requires TLS with no exceptions. Two shapes:

- *Real cert*, ECDSA, from Let's Encrypt on a name the server already has
  (`deploy.sh` points at `fkirchhoff.com`). The browser needs nothing special. The catch is
  renewal: the QUIC server holds the cert files open, so a renewal every ~60 days has to restart or
  reload the game server. That belongs in [`deploy.sh`](deploy.sh) as a systemd reload hook, not as
  something anyone remembers to do.
- *Self-signed with `serverCertificateHashes`*, which the browser accepts if the certificate is
  valid for at most 14 days. Fine for a dev box on a LAN, wrong for the v-server, because the hash
  has to reach the JS side and be rotated fortnightly.

Take the real cert for the deployment and self-signed for local work.

**Ports.** WebTransport is QUIC, so UDP, and it wants its own port — not 5000, which is spoken for.
The metadata endpoint (§2) is TCP and stays where it is. `noob_tube.toml`, `noob_tube_vserver.toml`
and `deploy.sh` each grow one number; `follow_the_game_port()` in
[`tuning.rs`](shared/src/tuning.rs#L455) already has an opinion about how ports move together and
should keep it.

---

## 2. Bootstrap: the config has to be there before `App::new`

[`configure()`](client/src/main.rs#L186) fetches the server's `NetConfig` over TCP and blocks until
it arrives, deliberately: the tick rate goes into the lightyear plugin group and into `Time<Fixed>`,
and the conditioner goes onto the transport, so none of it can be learned from a connection that
does not exist yet. `metadata.rs` explains why this cannot move later, and that reasoning does not
change on the web.

What changes is that **wasm cannot block**. `main()` must return to the JS event loop; there is no
synchronous fetch and no raw TCP. So the fetch moves out of Rust and in front of it:

```
index.html
  → fetch('/net-config')        JSON, same origin, HTTPS
  → stash the object on window
  → start the wasm module
main()
  → read the object out of the DOM, synchronously, as today
  → App::new
```

The order that `metadata.rs` protects is preserved exactly. Only the mechanism moves.

**Why JSON and not the TOML that is served today.** The parse now happens in JavaScript, before Rust
exists, and a browser parses JSON for free while TOML would mean shipping a parser to do one thing
once. `serde_json` is already a workspace dependency, so the server side is a one-line change in
`answer()`.

**Why one implementation and not two.** The server's half is already an HTTP server; the browser
speaks HTTP natively; the native client's `fetch()` is ten lines of hand-written HTTP/1.0 over
`TcpStream` that work unchanged against a JSON body. There is nothing to unify — the two consumers
already share the endpoint. What gets deleted is nothing; what gets added is a JSON body and a `.json`
content type.

**The payload grew two fields, and they arrived with §1** rather than here, because native needed
them first: `token_addr`, the address a connect token must name, and `cert_digest`, the certificate
to pin. Both are facts about the running process rather than settings, which is why they sit beside
`NetConfig` in a `ServerInfo` instead of inside it — and why neither goes through
`adopt_from_server`, whose rule is about who owns a *setting*. See `shared/src/metadata.rs`.

**Can this go over WebTransport instead?** No, and it is worth writing down why so the question does
not come back: the config is what the connection is *built from*. Tick duration decides the plugin
group, which decides the app, which is what would open the connection. Anything learned over the
game connection is learned too late by construction — which is the same sentence `metadata.rs`
already opens with.

**Same origin, and this is not optional.** The page is HTTPS (§1 requires a secure context), so a
plain-HTTP endpoint on another port is blocked as mixed content before CORS is even reached. Serve
`/net-config` from the same origin as the wasm bundle, with the reverse proxy forwarding to the
metadata port on loopback. That also means no CORS headers and no preflight.

**When it is missing.** Keep the current behaviour: a client that cannot reach it says so in as many
words and runs on its own settings, with the tick rate still failing safely through the protocol id.
On the web that message has nowhere to go — `println!` before `LogPlugin` lands in the JS console,
which is fine, but the loading screen should say it too.

---

## 3. Renderer: WebGPU, and what WebGL2 would have been for

The client builds Bevy with `features = ["default", "jpeg"]`, and `default` contains **neither
`webgl2` nor `webgpu`**. A wasm build with neither renders nothing at all, so one has to be chosen.

Where WebGPU stands in September 2026:

| Browser | WebGPU | WebTransport |
|---|---|---|
| Chrome / Edge, desktop + Android | yes, since 113 | yes, since 97 |
| Safari, macOS 26 / iOS 26 | yes | 26.4 (March 2026) |
| Firefox, Windows | 141 | 114 |
| Firefox, macOS ARM | 145 | 114 |
| **Firefox, Linux** | **no — expected during 2026** | 114 |

WebTransport reached Baseline when Safari 26.4 shipped, which means §1 already excludes everyone on
an older Safari or an older iOS — and those are most of the people who also lack WebGPU. The two
requirements overlap almost perfectly.

**The one real gap is Firefox on Linux**, which has had WebTransport since 2023 and still has no
WebGPU. That is not a hypothetical audience for this project. It is also, for now, a temporary one:
Mozilla expects to ship it during 2026.

**Measured, and it is closer than it looked.** The ground material asks for 18 sampled textures in
the fragment stage, and WebGPU only *guarantees* 16 — so a headless Chromium, whose software adapter
reports exactly the guaranteed minimum, refuses to build the PBR pipeline and the client quits. On
real hardware there is room (this laptop's Intel UHD offers 32 texture units and the client renders),
which is why this is a warning rather than a bug report. But 18 against a floor of 16 is not a margin
anybody chose, and it is the same eight bindings — four colour maps and four packed maps at 101–112 —
that would sink a WebGL2 build outright.

The fix, when it is wanted, is to make those eight bindings two: one `texture_2d_array` for the
colour maps and one for the packed maps, built when the layers are loaded. That takes the fragment
stage to 12, puts a floor-limit device back in range, and is the prerequisite for §3's fallback
bundle. It is not built.

So: **WebGPU only**, and Chromium is the browser to develop against. If Firefox on Linux is still
without WebGPU when this ships and somebody actually wants to play there, the fallback is a second
wasm bundle built with `webgl2` and three lines of JS picking on `navigator.gpu`. That is a milestone
of its own, and it is not free:

- [`ground.wgsl`](assets/shaders/ground.wgsl) binds four textures, four samplers and four packed
  maps at 101–112, on top of everything the PBR bind group already holds. WebGL2 guarantees only 16
  texture units per stage. It may fit; it is exactly the kind of thing that fits on one driver and
  not the next.
- WebGL2 has no storage buffers, so Bevy takes a different batching path — a different set of
  performance characteristics to tune against on a target that is already single-threaded (§6).

Also drop `draw_with_the_integrated_gpu()` on the web build.
[`PowerPreference::LowPower`](client/src/main.rs#L127) exists because of one laptop's NVK driver;
in a browser the adapter choice belongs to the browser, and asking for low power on a machine with a
discrete GPU is asking for the slow answer.

---

## 4. Assets: the browser already downloads only what it needs

`assets/` is 355 MB, of which `textures/` is 281 MB across 143 uncompressed 1K PNGs — and that
number is a red herring. Bevy's asset server fetches over HTTP on the web exactly as it reads from
disk on native: one request per `assets.load()`, nothing speculative. What a round actually touches:

| What | Size |
|---|---|
| `characters/swat.glb` | 4.3 MB |
| `anims/` — 19 clips used of 51 present | 3.0 MB total for the directory |
| Three default ground layers, `_Color` + `_Packed` each | 16.3 MB |
| **First round, before a vehicle exists** | **~24 MB** |
| `models/warthog.glb` + `machine_gun.glb`, on demand | +17 MB |

So the answer to "can it just download what it needs" is that it already does, and the honest first
load is about 25 MB of assets on top of the wasm bundle (§8). The 281 MB is mostly files nothing
references — every pack ships both `_NormalDX` and `_NormalGL` at ~5.8 MB, and only one of each pair
is ever used.

That leaves four things that are actually worth doing, in order:

1. **`AssetMetaCheck::Never`.** Bevy asks for a `.meta` file beside every asset. On disk that is a
   failed `stat`; over HTTP it is a second request and a 404 per asset, doubling the request count
   and filling the console. This is one line and it is the single largest win per character typed.
2. **A preload gate.** On native, an asset arriving late is a frame; over a link it is seconds, and
   a player model that appears after the fight started is worse than a loading screen. The menu
   should hold until the character, its clips and the ground layers for the loaded map are in.
   `remote_players.rs` and `character.rs` already tolerate a body that is not there yet — this is
   about what the player is shown while that is true.
3. **KTX2 + Basis, and the reason is not size.** A 5.8 MB PNG must be *decoded* before it becomes a
   texture, on the one thread the web build has (§6). A supercompressed KTX2 is transcoded straight
   to a GPU format: smaller over the wire, smaller in VRAM, and — the part that matters — no
   full-resolution PNG decode hitching the frame that a player walks into a new layer on. This is an
   offline bake, so it belongs beside [`bake_ground_maps`](tools/bake_ground_maps) as a tool, with
   the same shape: run it, commit the output.
4. **Delete the unused normal-map variant** and stop shipping what nothing loads. Free, and it makes
   the directory tell the truth about what the game uses.

Brotli on the static host, cache headers keyed on content, and the ordinary things. Not a decision,
just a checklist item for §8.

**One thing here is a real blocker, not a budget.** [`Kit::present()`](client/src/character.rs#L313)
probes the filesystem — `Path::new(ASSETS).join(body).exists()` — to decide which character kit to
use, deliberately, because the asset server would answer asynchronously and the first player needs a
body now. On wasm that probe compiles and always answers `false`, so the web build would silently
pick the fallback kit forever. [`vehicle.rs:366`](client/src/vehicle.rs#L366) does the same for the
vehicle model and gun, and [`character.rs:487`](client/src/character.rs#L487) once more. All three
need a web answer: the honest one is a small manifest generated at build time saying which kits
shipped, read from the same preloaded blob as the config in §2.

---

## 5. The native APIs underneath

None of these are hard. All of them fail quietly if missed, which is why they are a list.

| Where | What | Web answer |
|---|---|---|
| [`main.rs:364`](client/src/main.rs#L364) | `SystemTime::now()` for the client id — **panics** on `wasm32-unknown-unknown` | `js_sys::Date`, or `getrandom` with the `wasm_js` backend, which lightyear already pulls in |
| [`settings.rs`](client/src/settings.rs) | `XDG_CONFIG_HOME`/`HOME`, `fs::write` + `rename` | `localStorage`, keyed by the same names |
| [`recording.rs`](client/src/recording.rs) | `File::create`, `read_to_string` for traces | `cfg` it out for web, or hand the JSON-lines out as a blob download |
| `NOOB_TUBE_*` everywhere | env vars: headless, bot, server host, harness, record, replay | query parameters — the browser's equivalent — decoded once at startup into the same struct |
| [`main.rs`](client/src/main.rs#L110) | `ASSETS` is an absolute `CARGO_MANIFEST_DIR` path | a relative `"assets"` URL; the doc comment there already calls this out as the packaging question it was deferring |
| `--features remote`, `inspector` | `bevy_remote` runs an HTTP *server* | stay off; they are off by default |

`std::env::var` and `std::fs` compile on wasm and return errors, so nothing in this table breaks the
build. Only the first line panics. Everything else is a feature that silently stops existing, which
is worse.

---

## 6. One thread

`wasm32-unknown-unknown` is single-threaded unless the build enables atomics and shared memory,
which needs COOP/COEP headers on every response and a toolchain configuration that is its own
project. Not for this one.

So Bevy's schedule, Avian's physics step, terrain meshing, grass instancing and every rollback
replay share one core. The parts most likely to show it:

- **Rollback.** `cl_max_predicted_ticks` bounds how many ticks a correction replays, and each of
  those is a full fixed-step with physics. What is comfortable on a desktop core at 64 Hz is the
  first thing to measure in a browser.
- **Grass** ([`grass.rs`](client/src/grass.rs)) and terrain tile rebuilds, which are bursty by
  nature.
- **PNG decode**, which §4 removes.

The settings file already exists to hold this kind of tuning
([`settings.rs`](client/src/settings.rs)), so the web build gets its own defaults for grass reach,
view distance and tile counts rather than a special code path.

---

## 7. Input: pointer lock from Resume

The menu already solves this. A browser grants pointer lock only in response to a real user gesture,
and clicking **Resume** in the map menu is exactly that — the same shape `webgame` uses. There is no
need for a separate click-to-play screen, and `map_menu.rs` is already where the grab is requested
([`map_menu.rs:723`](client/src/map_menu.rs#L723)).

Three details that will bite anyway:

- **`grab_mode` is a request, not a fact.** [`local_player.rs`](client/src/local_player.rs#L311)
  and `map_menu.rs` both read `cursor.grab_mode != CursorGrabMode::None` as "the game has the
  mouse". On the web the browser can refuse, and does: a lock requested without a recent click, or
  within the cooldown Chrome enforces after Escape, is denied and the field still says `Locked`.
  The result is a game that thinks it is grabbed while the cursor sits free on the desktop — look
  moves, clicks land elsewhere. The state has to come from what actually happened
  (`pointerlockchange`), not from what was asked for.
- **Escape always releases**, and that is the browser's decision, not the game's. Which is fine —
  it is what the menu key does anyway — but nothing may assume the game still has the mouse after
  a frame it did not request anything in.
- **Bevy applies the grab a frame later**, inside the `requestAnimationFrame` callback rather than
  in the click handler. Transient activation lasts several seconds, so a click → next frame → grab
  works; a grab requested at startup with no click behind it does not.

Some keys never arrive (browser-reserved combinations), and fullscreen needs its own gesture — the
same Resume click can carry it.

---

## 8. Build and hosting

- **Release only.** A dev-profile wasm build of Bevy + Avian + lightyear is unusable in size and
  speed. Release, then `wasm-opt -Os`, then Brotli. Expect roughly 25–40 MB before compression, and
  budget for it as a one-time download beside the ~25 MB of §4.
- **Toolchain.** `wasm-bindgen` plus a hand-written `index.html` — which §2 needs anyway for the
  config preload — or `trunk` if the extra machinery earns its keep. `getrandom` wants its
  `wasm_js` backend (`RUSTFLAGS=--cfg getrandom_backend="wasm_js"`); `aeronet_webtransport` may want
  `--cfg=web_sys_unstable_apis`, which its own docs build sets.
- **Hosting.** One HTTPS origin serving the bundle, the assets and `/net-config` (proxied to the
  metadata port on loopback), plus the QUIC port from §1 with the same certificate. `deploy.sh` is
  already the provisioning script; this is another idempotent stanza in it, plus the reload-on-renew
  hook.

---

## 9. Order of work

Each stage is meant to end somewhere you could stop.

1. **Spike: does it build and connect at all.** Switch native to WebTransport, add the `webgpu`
   feature, stub the §5 table, and get `cargo build --target wasm32-unknown-unknown` through to a
   connected client on a self-signed cert with whatever it renders. This is the only stage with a
   genuine unknown in it — the whole lightyear/Avian/Bevy stack on wasm — so it goes first and stays
   ugly.
2. **Bootstrap.** JSON body on the metadata endpoint, preload in `index.html`, kit manifest from §4.
3. **The §5 table properly**, `localStorage` settings, query parameters instead of env vars.
4. **Assets.** `AssetMetaCheck::Never`, preload gate, the KTX2 bake tool, delete the unused normals.
5. **Input and web presets.** Pointer lock state from the browser, grass and view distance defaults
   for one thread.
6. **Deployment.** Cert, proxy, `deploy.sh`, renewal reload.
7. **Then measure**, and only then decide whether the UDP feature flag from §1 gets deleted or
   defended.

---

## Open questions

- **Whether Bevy 0.19 can hold both `webgl2` and `webgpu` in one binary and choose at runtime.**
  If it can, §3's fallback is cheap enough to keep in reserve; if not, it is a second bundle. Worth
  five minutes before committing to WebGPU-only, even though the decision is unlikely to change.
- **What lightyear's WebTransport IO does about flush timing** compared with `UdpIo`. Whether a
  packet leaves in the same frame it was written matters at 64 Hz, and it is not obvious from the
  outside.
- **How the harness and bots reach a browser build**, if they should at all. Query parameters get
  the flags in; driving a canvas from outside is a different tool than BRP.
- **Whether the native client keeps talking to the metadata port directly**, or goes through the
  same HTTPS origin as the browser. Directly is simpler — no TLS client — and means the two
  consumers use different ports for the same bytes. That is tolerable; it is not tidy.
