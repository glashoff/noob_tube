# Noob Tube

A multiplayer first-person shooter built with [Bevy](https://bevy.org) 0.19. The server simulates
authoritatively; clients predict, reconcile and render.

Named after the underslung grenade launcher — the weapon that lets players with no aim whatsoever
collect kills anyway.

This project reuses the player model and movement tuning of
[`webgame`](../webgame), a browser-based shooter written in TypeScript. Everything else is built
from scratch in Rust.

---

## Goal of the first step

Two or more players meet on a single very large flat plane, move around, see each other, and shoot
at each other. No terrain, no map editor, no round logic. This is deliberately the smallest scope
that exercises the whole networking path end to end — input, authoritative simulation, replication
and hit detection.

---

## What is reused from `webgame`

### Player model

The character and its animations come from [Adobe Mixamo](https://www.mixamo.com) and already exist
locally under `webgame/assets/`:

| Item | Detail |
|---|---|
| Character | `Swat.fbx` — mesh plus Mixamo skeleton |
| Animations | 49 GLB clips, of which **19 are used** |
| Clip indices | `0` idle, `1–8` movement directions (F/B/L/R plus diagonals), `9` jump, `10` crouch idle, `11–18` crouch directions |

Animation selection is **not** a 2D blendspace. One clip is chosen from the WASD input flags and
cross-faded against the previous one — see `Pawn.ts:355-388` in `webgame`. Bevy's
`AnimationTransitions` maps onto this directly.

#### Root motion

Mixamo's locomotion clips carry the figure forward inside the animation. World position is
server-driven here, so that displacement has to go — but it is worth measuring first.

**Measure.** The horizontal displacement of the `Hips` position track, first keyframe to last,
times the model scale, over the clip duration, gives the speed the clip advances at. For this pack:
**4.606 m/s** for the run clips, **1.956 m/s** for the crouch clips. (`hipsHorizNaturalSpeed` in
`webgame/client/src/ThreeRenderer.ts`.)

**Strip.** Zero X and Z on the `Hips` position track. **Keep Y** — that is the hip bob, and without
it the feet do not stay planted. No other bone carries root motion, so no other position track is
touched.

**Foot lock.** Play the clip at `actual ground speed / natural speed`. At `MAX_SPEED` that is about
1.19×. Without it the feet slide, because stride frequency no longer matches travel.

In Bevy the stripping is best done *after* animation evaluation rather than by patching clip
assets: a system in `PostUpdate`, ordered after the animation systems and before transform
propagation, resets X and Z on the hips bone entity. That cannot drift out of sync with the assets
the way `webgame`'s two separate strip sites can — it does the same thing in the renderer and in
its skeleton extractor, and the two have to stay identical or the server hit capsules drift away
from the drawn figure.

Foot lock is `ActiveAnimation::set_speed`. The natural speeds are constants for these clips; there
is no need to read them back out of Bevy's curve API.

**Scale.** `webgame` applies a model scale of 0.01, meaning the Mixamo data is in centimetres and
`fbx2gltf` does not correct it. The same correction is needed here, either on the model root or
during conversion.

### Movement constants

Taken verbatim from `webgame/shared/physics/movement.ts` and
`webgame/shared/game/physics.ts`:

| Constant | Value | Meaning |
|---|---|---|
| `MAX_SPEED` | 5.5 m/s | ground speed |
| `MAX_SPEED_AIR` | 3.0 m/s | airborne speed |
| `CROUCH_SPEED` | 2.6 m/s | while ducking |
| `GRAVITY` | −20.0 m/s² | |
| `JUMP_VELOCITY` | 6.5 m/s | |
| `MOVE_ACCEL_TIME` | 0.3 s | ramp to full speed, and back to a stop |
| Capsule | r = 0.35, half-height 0.5 / 0.25 | 1.7 m standing, 1.2 m crouched |
| Eye height | 1.59 / 1.10 m | standing / crouched |
| Tick rate | 64 Hz | server simulation step |

---

## Technology choices

All candidates below were checked against Bevy 0.19 and already support it.

| Concern | Choice | Rationale |
|---|---|---|
| Netcode | **lightyear 0.29** | Ships prediction, rollback and interpolation — the same model `webgame` implements by hand. `bevy_replicon 0.43` only replicates; prediction would have to be written from scratch. |
| Collision | **rapier3d 0.35, queries only** | Shape casts against level meshes, with no solver and no simulation step. See [Two kinds of physics](#two-kinds-of-physics). |
| Ragdolls | **rapier3d, second world** | Client-side cosmetic only. Rigid bodies with angle-limited joints, simulated in a world of its own. |
| Assets | glTF/GLB, loaded natively | Bevy reads GLB including skeletal animation. Bevy cannot read FBX, so `Swat.fbx` needs converting. |

### Two kinds of physics

The project needs physics twice, under opposite constraints. Keeping the two apart is the single
most important structural decision here.

**Gameplay collision — stateless, shared, rollback-safe.** Player movement collides against level
meshes using nothing but shape casts. Rapier is two halves: parry for geometry queries, plus a
solver and pipeline for simulation. Only the first half is used — `PhysicsPipeline::step()` is
never called, so no solver state exists that a rollback would have to restore. The level BVH is
built once and never changes, so it needs no snapshotting either. The entire rollback state stays
at four fields per player: `position`, `velocity`, `on_ground`, `crouching`.

This uses `rapier3d` directly, **not** `bevy_rapier3d`. The Bevy plugin brings ECS integration,
collider components and a running simulation — all of it dead weight to work around. It also has a
Bevy dependency, and `shared/` has to run on the headless server.

**Ragdolls — stateful, client-only, cosmetic.** When a player dies, the body falls under its own
simulation. This carries state by definition, and that is fine: it never touches gameplay, is never
replicated, and is never rolled back. Determinism is irrelevant — two clients may see the same
corpse land differently and nothing breaks.

This runs in a **second rapier world**, separate from the gameplay one, existing only on the client
and only for corpses. It has its own `RigidBodySet` and pipeline, and here `step()` actually runs.
The level colliders go in as a static copy. The gameplay world stays query-only, so the separation
between stateful and stateless physics is preserved — it now runs between two worlds rather than
between two libraries, and needs no second dependency.

One rigid body per hit capsule, joined by angle-limited joints:

| Joint | Type | Limit |
|---|---|---|
| hips ↔ torso ↔ head | spherical | narrow cone |
| shoulders, hip joints | spherical | wide cone |
| elbows, knees | revolute | one-sided, e.g. 0°–150° |

### Why not position-based dynamics

`webgame` solves this differently — one Verlet particle per joint, connected by distance
constraints along the hit-capsule segments — and the result is poor in three ways that turn out to
share one cause: limbs bend and twist the wrong way, the body sags in place instead of falling, and
it jitters without settling.

A particle network has neither orientation nor inertia. Distance constraints cannot express a
joint limit, because bending an elbow backwards leaves every distance unchanged, and they cannot
see torsion at all, because rotating a bone about its own axis changes no distance either. The
`webgame` implementation compensates with bend diagonals, rigid anchors to the pelvis swing, and
minimum-separation constraints between limbs and torso — three constraint families that pull
against each other, so six Gauss-Seidel iterations oscillate rather than converge. Hence the
jitter, and hence a sleep threshold that has to end the oscillation by decree.

Rigid bodies put the stiffness in the model instead of in patches on top of it. Joints also
disable contacts between connected bodies automatically, and sleeping is built in.

Jitter is still possible with rigid bodies — overly stiff joints, too few solver iterations, badly
proportioned masses between neighbouring bodies. The difference is that those are tuning problems
with known remedies, not a contradiction inside the model.

### The movement algorithm

`webgame`'s `sweepShape` is a collide-and-slide loop, at most three iterations:

1. Sweep the capsule along the desired motion vector.
2. No hit — take the full motion and stop.
3. Hit — advance to just short of the surface (a `SKIN` of 0.01 m keeps the capsule from sticking),
   then project the remaining motion onto the surface. That projection is what sliding along a wall
   is.
4. Repeat with the projected remainder.

One subtlety worth preserving: on a zero-distance hit — already touching, e.g. standing on the
floor — the motion is only projected if it points *into* the surface, otherwise jumping would be
impossible. Subsequent casts then skip the touched surface so the next obstacle can be found, which
is what makes walking into a wall while standing on the ground behave correctly.

Four stateless probes go with it: `isGrounded`, `canStandUp`, `maxStepUpY` (stairs) and
`findGroundContactY`.

### Collision meshes

Level collision uses a **separate, simplified mesh**, not the render mesh — the same split
`webgame` makes. Fewer triangles to cast against, and no catching on decorative geometry.

```
GLB → Bevy Mesh asset → ATTRIBUTE_POSITION + indices
    → ColliderBuilder::trimesh → ColliderSet
    → QueryPipeline::update()        once, then immutable
```

**Use the `=0.35.0-glamx0.2` build of rapier.** rapier and parry are compiled against glam, but
which glam matters: the plain 0.35.3 release uses glam 0.33 while Bevy 0.19 uses 0.32, so every
vector would need translating at the boundary. The `-glamx0.2` build — the one `bevy_rapier3d`
depends on — is compiled against glam 0.32, which makes `rapier3d::math::Vector` literally
`bevy::math::Vec3`. A test in `shared/src/lib.rs` asserts this so a careless version bump fails
loudly instead of silently costing conversions everywhere.

The BVH is built by calling `BroadPhaseBvh::update` directly. rapier supports this explicitly —
its documentation names the case of a broad-phase "driven without the physics pipeline".

---

## Repository layout

A Cargo workspace, mirroring how `webgame` splits its code:

```
.
├── shared/    movement constants, wire protocol   — used by both sides
├── client/    rendering, input, prediction        — Bevy with default features
├── server/    headless, authoritative             — Bevy with default features off
└── tools/     development helpers, not shipped
```

Run them in two terminals:

```bash
cargo run -p noob_tube_server
cargo run -p noob_tube_client
```

---

## Looking inside a running build

The `remote` feature serves the [Bevy Remote Protocol](https://docs.rs/bevy_remote), a JSON-RPC
endpoint that reads and writes the live ECS. It is off by default and must stay that way in
anything released: the protocol can spawn entities and mutate components, so an enabled endpoint is
a way into the process, even bound to localhost as it is here.

```bash
cargo run -p noob_tube_server --features remote   # BRP on 127.0.0.1:15712
cargo run -p noob_tube_client --features remote   # BRP on 127.0.0.1:15702
```

The client takes the protocol's default port so third-party tools find it without configuration.

A second instance on the same port fails to bind **silently** — no log line, no warning. Queries
then answer from whichever process got there first, which may be a stale build still running from
an earlier session. If a value looks impossible, check who actually owns the port before suspecting
the code:

```bash
ss -ltnp | grep 15702
readlink -f /proc/<pid>/exe    # `(deleted)` means the binary has been rebuilt since
```

`bevy_remote` is a direct dependency rather than the `bevy/bevy_remote` feature. That feature also
switches on `serialize` across `bevy_internal`, so turning `remote` on or off would change the
feature set of nearly every Bevy crate and invalidate the whole cached tree — an eleven minute
rebuild every time the flag is toggled. The price is keeping the version in lockstep with Bevy's by
hand, which the facade would have done for us.

This does *not* give the server a wgpu-free build. `bevy_remote` depends on `bevy_dev_tools`
unconditionally, for `schedule_data`, and that reaches `bevy_core_pipeline` and so `bevy_render`.
There is no way around it short of forking. What keeps the shipped server headless is that the
feature is off: without it the server's dependency tree contains no `bevy_render` at all.

A second client on the same machine needs its own port: `BRP_EXTRAS_PORT=15704`. Not 15703 — that
belongs to the first client's render sub-app, which `bevy_remote` binds at `port + 1` and which
serves a different, nearly empty world. Querying it returns no game entities and looks like a
broken client.

`tools/brp` is a dependency-free client for it:

```bash
tools/brp list                              # registered component types
tools/brp query noob_tube_client::local_player::LocalPlayer
tools/brp get <entity> <type>...            # current values
tools/brp watch <entity> <type>...          # stream every change until interrupted
tools/brp --port 15712 list                 # the server instead
```

### An inspector window

`--features inspector` adds `bevy-inspector-egui`, an egui panel listing every entity and component
with editable values. It is a separate mechanism from BRP: it runs *inside* the process, so it
cannot see the server, and it draws over the game.

```bash
cargo run -p noob_tube_client --features inspector   # F1 the world, F2 named entities
```

**F1** is the world as a tree: roots at the top level, expanding into their children. What makes it
readable is that the level has a hierarchy — `Level` holds the ground, the props and the sun — so
the top level is `Client`, `Level`, `LocalPlayer`, the pointer and the monitors, not every entity at
once. The panel already hides observers and the entities Bevy 0.19 uses to store resources, so the
563 entities a BRP query reports are never all on screen.

**F2** filters on `With<Authored>`: a flat list of exactly what this crate spawns. Good for reaching
one known entity, but it ignores the hierarchy and shows parents beside their own children.

`Authored` is a marker we add at every spawn site, because the ECS draws no line between our
entities and the engine's — `DefaultPlugins` is some forty plugins writing into the same world we
do. Filtering on `Name` came close, since we name what we spawn, but that is a coincidence rather
than a rule: `bevy_gizmos_render` names its three draw-phase placeholders too, and they appeared in
the list looking like ours. The marker has to be remembered at each `spawn`, which is the price of
Bevy not tracking who created an entity.

Both start hidden because egui takes the pointer while one is up, which fights the locked cursor
mouse look needs. Both show the same set of types BRP does — whatever derives `Reflect` and is
registered.

Names come from the `Name` component. Where there is none, the inspector guesses from a fixed table
of well-known components — `PointerId` shows as "Pointer", `Observer` as "Observer" — so a name in
the tree is not proof that anything set one.

Its `bevy_egui` and `egui` versions are pinned to this exact release, so it moves in lockstep with
Bevy the way `bevy_remote` does.

### Seeing the collision shapes

`F3` draws the shapes the movement step reasons about, using Bevy's gizmos — immediate-mode lines
that live for one frame:

| | |
|---|---|
| capsule | green while grounded, red while airborne |
| downward line | the probe `is_grounded` casts, `GROUND_SNAP_DIST` long |
| cross | where `ground_height_below` says the ground is |

None of this geometry exists as an entity. The capsule is a shape handed to a query and the probe is
a ray cast and discarded, so there was nothing to look at while the game ran — which is how the M1
freeze stayed hidden: the capsule had settled 1.6 mm into the floor, and that only showed up in the
numbers after an afternoon of comparing logs. A gap between the cross and the capsule's base is
exactly that failure, visible at a glance.

Two limits in first person: the capsule surrounds the camera at 0.35 m, so at a 90 degree field of
view it fills the screen, and the probe is vertical, so looking straight down projects it to a
point. The cross is the part that reads. Seen from outside — other players in M3, or a detached
camera — all three would.

### Driving the game from an agent

`--features remote` also pulls in `bevy_brp_extras`, which adds BRP methods for screenshots and
synthetic keyboard and mouse input. It owns the HTTP transport in the client, on the same port
`RemoteInspectPlugin` would have used, which is why `RemoteTypesPlugin` exists: two plugins adding
`RemoteHttpPlugin` is a warning at best.

Paired with the `bevy_brp_mcp` server in `.mcp.json`, that makes the running game reachable from a
coding agent — 47 tools covering queries, mutation, watches, launching and shutting the app down,
reading its logs, and injecting input. Install it with `cargo install bevy_brp_mcp`; Claude Code
reads `.mcp.json` at startup, so it takes a restart to appear.

This overlaps with `harness.rs`, which does scripted input and screenshots from the inside. The
harness stays for now: it runs in CI without an agent, and BRP samples at the frame rate, which is
not enough for the tick-exact checks M4 will need.

### Tuning the network

Every network setting is configurable without a rebuild. They are settings and not constants on
purpose: what a shooter feels like at 30 ms and at 150 ms are different games, and finding out which
trade is right means changing a number and playing, not changing a number and waiting for a link
step. They all live in `shared/src/tuning.rs` as one `NetConfig`.

Three layers, each overriding the one before:

1. the defaults in `NetConfig::default`,
2. **`noob_tube.toml`** — or wherever `NOOB_TUBE_CONFIG` points,
3. **environment variables**, one per field.

The file is for the settings you keep, the environment for the one you are changing right now.
`cp noob_tube.example.toml noob_tube.toml` to start; that name is gitignored, so local experiments
stay local.

```toml
ping_ms = 100             # simulated round trip; each end delays half of it
jitter_ms = 10            # random variation on each leg, ± this
loss = 0.02               # packet loss probability, 0.0 to 1.0
send_hz = 32.0            # how often the server replicates            (server only)
interp_ratio = 1.7        # interpolation delay, in send intervals     (client only)
interp_min_ms = 5         # floor under that delay                     (client only)
input_delay_min_ticks = 0 # soonest the server may act on an input     (client only)
input_delay_max_ticks = 0 # ping covered by delay before predicting    (client only)
max_predicted_ticks = 100 # how far ahead the client may simulate      (client only)
```

```bash
NOOB_TUBE_PING_MS=200 cargo run -p noob_tube_client    # try one value, edit nothing
```

Both binaries read the same file and take the fields they need, so one file describes a whole
session.

**The conditioner has to be in effect on every process.** It delays only what a process *receives* —
the server's copy delays inputs coming in, each client's copy delays snapshots coming in — so a file
read by the server alone gives a half-duplex link that behaves like nothing real.

Nothing here fails quietly, which is the point:

```
$ NOOB_TUBE_CONFIG=typo.toml cargo run -p noob_tube_server
cannot parse typo.toml: TOML parse error at line 1, column 1
  |
1 | pign_ms = 100
  | ^^^^^^^
unknown field `pign_ms`, expected one of `ping_ms`, `jitter_ms`, `loss`, `send_hz`, ...
```

A misspelled key reads as "no latency", a `NOOB_TUBE_CONFIG` pointing at nothing reads as "no
latency", and both make a run that tested nothing look like a netcode success. So a file that exists
but will not parse, a named config that is missing, and an unparsable environment value all refuse
to start. A *missing* `noob_tube.toml` is normal and silent — the defaults are playable.

And every start logs what is actually in effect, including where it came from:

```
link untouched, sending at 32 Hz, interpolating at 1.7× [defaults]
ping 100 ms, jitter ±10 ms per leg, loss 0, sending at 20 Hz, interpolating at 1.7× [noob_tube.toml]
```

#### The three delays, and which knob moves which

They are separate, and confusing them is how netcode gets tuned in the wrong direction.

| what you feel | how long | knob |
|---|---|---|
| your own movement reacting | **zero** — the client predicts it | none; this is what prediction buys |
| the server learning what you did | half the ping | `NOOB_TUBE_PING_MS` |
| seeing another player's move | half the ping + interpolation delay | ping, `SEND_HZ`, `INTERP_RATIO` |

The third is the one worth spending time on. At the defaults it is `1.7 / 32 Hz ≈ 53 ms` on top of
the network. Raising `send_hz` shortens it and costs bandwidth; lowering `interp_ratio` shortens it
and starts letting remote players freeze between updates, because the next one has not arrived yet.

Lightyear **clamps rather than extrapolates** when it does run dry, so a too-short delay shows up as
players stuttering to a halt and jumping, not as them sliding through walls.

#### Input delay: buying stability with responsiveness

The second row is not fixed either, and this is where the genre decision lives. The client stamps
each input with the tick it is *meant for*, and the server acts on that tick. Stamp it for `now`
and your movement is instant but the server may not have the packet in time, so it guesses and the
client rolls back. Stamp it for `now + 4` and the packet has 62 ms to arrive, the server never
guesses, nothing ever rewinds — and every keypress starts 62 ms late.

Three knobs, all on the client:

- `input_delay_min_ticks` — never act on an input sooner than this, however good the link is.
- `input_delay_max_ticks` — how much ping to cover with delay before prediction takes over.
- `max_predicted_ticks` — the ceiling on how far ahead the client may run, and so on rollback depth.

The defaults are `0 / 0 / 100`: cover every millisecond with prediction, delay nothing. That is the
shooter answer, and it is why the earlier measurement found the client running 0.60 m ahead of the
server. Fighting games and RTSs take the opposite end — `input_delay_max_ticks` high, or
`max_predicted_ticks = 0` for full lockstep, where nothing is predicted and every input waits out
the round trip.

For a fixed delay regardless of ping, set the min and the max to the same number. A min above the
max is refused at startup rather than asserted on from inside a lightyear system.

The configured pair is a floor and a ceiling, not the answer: between them lightyear picks a delay
from the round trip it measures. So the client logs what it actually settled on, once the clocks
agree. Measured at a 100 ms ping:

| setting | effective delay |
|---|---|
| `0 .. 0` — the shooter default | 0 ticks |
| `4 .. 4` — fixed | 4 ticks, 62.5 ms |
| `0 .. 20` — cover the ping | 15 ticks, 234 ms |
| `0 .. 20`, `max_predicted_ticks = 0` — lockstep | 17 ticks, 266 ms |

Note the third row: asked to cover a 100 ms ping, lightyear chose 234 ms. It is budgeting for jitter
and sync error on top of the round trip, and it is generous about it. Tune against the number the
client reports, not against the one you wrote down.

#### What it makes visible

At a 100 ms ping the client runs about half a metre ahead of the server while walking, and rolls
back when a lost input makes the server diverge. Both numbers are meaningless without a conditioner:
on localhost the client and server agree to 0.000000, which says only that two identical
computations with nothing disturbing them produce the same answer.

### Recording what the ECS does

The `+watch` methods hold the connection open and emit one JSON line per change, so redirecting
`watch` to a file is a recording that can be analysed afterwards. They read Bevy's own change
detection rather than polling — see `is_changed` in `bevy_remote`'s `builtin_methods.rs` — and
report removals separately through `RemovedComponents`.

Two limits are worth knowing before trusting a recording:

- Change detection triggers on *mutable access*, not on a changed value. A component written every
  tick with an identical value is reported as changed every tick.
- `world.get_components+watch` follows one entity and a fixed list of components. There is no
  whole-world recorder; several entities mean several streams.
- Requests are processed once per frame, so a recording is sampled at the frame rate, not the tick
  rate. Measured against the client: six seconds produced 356 rows where 64 Hz would be 384 ticks,
  because two ticks falling in one frame collapse into a single report. Good enough to watch a value
  drift, not good enough to reconstruct a tick-exact history — which is what prediction and rollback
  will need in M4, and what the harness will have to keep doing.

Nothing shows up over BRP unless the type derives `Reflect` and is registered — a component that
works perfectly is simply invisible otherwise. `RemoteInspectPlugin` registers the shared types,
`LocalPlayerPlugin` registers its own.

---

## Milestones

### M0 — Scaffolding ✔
Cargo workspace with `shared`, `client` and `server`. The server binds a UDP socket and listens;
the client connects over netcode. Neither draws anything yet.

Four things about lightyear 0.29 that cost time to work out:

- `udp` and `netcode` are **not** default features. Without them there is no transport and no
  connection layer.
- Transport plugins come from `SharedPlugins`, which both `ClientPlugins` and `ServerPlugins`
  include. Adding `UdpPlugin` by hand panics with "plugin was already added".
- The headless server needs `StatesPlugin` explicitly. lightyear registers states and
  `MinimalPlugins` does not bring it, which otherwise panics on the missing `StateTransition`
  schedule.
- **`PingManager` is only auto-registered for server-side `ClientOf` entities.** The client must
  add its own. Without it the connection still establishes, but the server's pings arrive
  unanswered — visible only as a repeated `Unhandled messages "Ping"` warning — and the timelines
  never synchronise. That would surface much later as broken prediction in M4.

The client entity therefore carries: `Client`, `ReplicationReceiver`, `Link`, `PingManager`,
`NetcodeClient`, `UdpIo`, `LocalAddr`, `PeerAddr`, plus a `PredictionManager` resource. The server
attaches `ReplicationSender` to each incoming connection in its `Add<LinkOf>` observer.

### M1 — Local movement ✔
A 500 m ground plane with a few crates, a first-person camera, mouse look with cursor grab, and
WASD plus jump and crouch. Movement lives in `shared/` and runs on a 64 Hz fixed timestep.

The ground is a collision trimesh from the start and movement goes through the collide-and-slide
sweep, so adding real level geometry later needs no rewrite.

Mouse look is applied in `PostUpdate`, not on the fixed timestep — the view follows the frame rate
while movement ticks at 64 Hz. Looking around at 64 Hz feels noticeably worse than moving at it.

Two bugs the tests caught, both of which would have been confusing to diagnose by feel:

- The yaw rotation had the wrong handedness, so the player walked backwards.
- The ground probe reaches 0.12 m below the feet, so one tick into a jump the player is still
  inside it. Counting that as grounded let a held jump key re-trigger every tick, pinning the
  player just above the floor.

Standing on the floor settles the capsule about 5 mm in — one tick of gravity, because the first
cast finds no overlap when the capsule starts exactly on the surface. Tests assert this is a
one-off and not a slow descent through the level.

#### Verifying visually

Tests say the movement maths is right; they say nothing about whether the camera is where it
should be. Setting `NOOB_TUBE_HARNESS=<path>` makes the client walk a scripted route, log its
foot position, save a screenshot and exit. Without the variable the harness is inert. `webgame`
solves the same problem with its `client/scenarios/` directory.

### M2 — Character and animation
Convert `Swat.fbx` to GLB with `fbx2gltf` (`webgame` already uses it), load the model, build an
`AnimationGraph` over the 19 clips, port the selection logic from `Pawn.ts`, and cross-fade with
`AnimationTransitions`. A third-person view helps verify this.

Strip root motion and apply foot lock as described under [Player model](#player-model) — without
it the character drifts away from its own position, which is confusing to debug later.

Also read in the joint and hit-capsule definitions. `webgame` generates these into
`shared/assets/skeleton.json`, which can be reused as-is; M3 needs them for the ragdoll and later
per-bone hit detection builds on the same data.

### M3 — Multiplayer and shooting
The server simulates authoritatively at 64 Hz and replicates player entities. Clients send input
and interpolate remote players.

Replication and input are in place. The server spawns a player entity per connected peer, simulates
it authoritatively at 64 Hz from the inputs that arrive, and replicates the result; clients draw each
player as a capsule.

Inputs travel as their own component through lightyear's input plugin, which sends the last N ticks
with every packet rather than one input per packet — a dropped packet then costs no movement, and
the same history is what a rollback replays from.

Which player belongs to whom is the server's decision, not a comparison the client makes: the server
puts `ControlledBy` on the player entity pointing at that connection, and it arrives at exactly one
client as `Controlled`. That is the entity the client attaches its input marker to.

The level geometry moved to `shared`, so both sides collide against the same numbers. They have to:
where the two disagree, the client's prediction and the server's authority disagree about where a
player can stand, and every step near the difference becomes a correction the player feels. The
visible meshes stay in the client, built from the same constants.

Both sides step the same function against collision geometry built from the same constants. That
identity is the precondition for prediction, and M4 depends on it: replaying an input locally has to
land where the server will put it, or every replay produces a correction.

Lightyear replicates *components*, not snapshots, which is worth stating because it is the opposite
of how `webgame` worked. There is no per-tick blob of the whole world: each component is registered
on its own and gets its own treatment.

| component | mode | why |
|---|---|---|
| `PlayerState` | `replicate` + predict + interpolate | position and velocity, changing every tick |
| `Aim` | `replicate` + predict + interpolate | where the player looks, changing every tick |
| `Player` | `replicate_once` | which peer owns this entity, never changes |

`Aim` is separate from `PlayerState` rather than a field in it, and that separation is the point of
the component-wise model. The two need opposite treatment: the server is the authority on position,
but for the local player's own aim the client is, and rolling it back would make the view snap on
every packet. Keeping them apart is also what lets `PlayerState` be predicted while `Aim` is
interpolated — one struct would have forced them to share a strategy.

Recoil will complicate this: once a weapon pulls the crosshair, the simulation changes where the
player looks, and that part does belong in the rollback snapshot. The way out is to keep the
simulated part as its own field in `PlayerState` and add it to `Aim` when drawing, so the mouse
component stays out.

Instead of a separate entity for prediction, lightyear marks entities that arrived over the network
with `client::Remote`, and marks the one this client owns `Predicted`. Both are filters the client
draws with: `Remote` says an entity came from the server, `Without<Predicted>` leaves our own body
out, since the camera sits inside it.

#### Interpolating the other players

Updates arrive at discrete server ticks, and drawing each one the moment it lands makes remote
players advance in steps. So the client does not draw the newest state it has: it keeps a history of
received values and renders a moment slightly in the past, blending the two samples that bracket it.
The delay is `max(send_interval × 1.7, 5 ms)` plus a jitter margin — short enough not to be felt,
long enough that the next sample has almost always arrived.

Almost. When it has not, lightyear **clamps rather than extrapolates**: the player freezes at the
last known state until the packet turns up. A guess would put them somewhere they never were, and on
a hitscan game that is a shot at a phantom.

It only does anything because the server sends *less often than it ticks*. Lightyear's default
replication interval is zero — an update every frame — and with no gap between updates there is
nothing to interpolate across. The server therefore sets `ReplicationMetadata` to `SEND_RATE`, 32 Hz
against a 64 Hz simulation. That number is the dial for the whole trade: it decides the bandwidth,
and the interpolation delay follows from it as `send_interval × 1.7`.

Measured with two clients walking the same route at 60 ms of simulated latency, sampling both
players' positions on the same client at ~60 Hz:

| | samples where the position did not change | trailing the server |
|---|---|---|
| own player, not interpolated | 47 % | 0.425 m |
| other player, interpolated | 1.7 % | 0.464 m |

The 47 % is the stepping, seen directly: at 32 Hz updates and 60 Hz sampling, roughly every second
sample finds the same value still sitting there. The interpolated player moves on all but 1.7 % of
them. The cost is the 4 cm it trails further behind — smaller than the `× 1.7` formula predicts, and
not worth chasing.

Which players get this is the server's decision, per client. The player entity carries
`InterpolationTarget::to_clients(NetworkTarget::AllExceptSingle(peer))`, so everyone sees everyone
else smoothed, and nobody sees a smoothed copy of themselves — the owner has a local simulation that
is ahead of the network, and blending toward a version of themselves that trails it by design would
only drag them backwards.

Unlike older lightyear versions, this happens **in place**. There is no confirmed/interpolated entity
pair to keep in step: the received values go into a `ConfirmedHistory<C>`, and the blend is written
back onto the same component of the same entity. The drawing code reads `PlayerState` and `Aim` and
never learns that anything happened — it only has to run *after* `InterpolationSystems::All`, or it
would render the previous frame's sample.

What can be blended is decided per component, because most state cannot be. Position and velocity
lerp; `on_ground` and `crouching` are discrete and hold the earlier sample's value until the timeline
reaches the later one, which is the choice that never shows a stance before it happened. `Aim` needs
its own function again: yaw wraps, so a player turning past π reports `+3.1` on one tick and `-3.1`
on the next, and interpolating those *numbers* would spin the body almost all the way round, the
wrong way, on every crossing. The interpolation takes the short way around the circle instead.

Shooting is hitscan: the client reports where it aimed, the server raycasts against player capsules
(not per bone yet), applies damage, and handles death and respawn.

Death drops a ragdoll: the animated model is swapped for the rigid-body skeleton described above,
placed at the pose the death animation reached, seeded with the player's velocity plus the shot
impulse. Bone transforms are then read back from the body poses each frame. This runs purely on
each client — the server only replicates that the player died.

**Two players fighting each other on the plane completes the first step.**

Hit detection here is *not* lag-compensated — the server tests against the position it currently
holds, not against what the shooter saw. Against moving targets that means visibly needing to lead
your shots. Fixing it needs a position history on the server to rewind into, plus the client reporting its
interpolation delay — lightyear has `InputConfig::lag_compensation` for exactly this, currently off.

### M4 — Prediction ✔
The local player is simulated on the client without waiting for the round trip, and reconciled
against the server by rolling back and replaying. Without it, movement feels mushy above roughly
50 ms of latency.

The shape of it is one sentence: **the client's own player entity is the server's entity.** There is
no local copy running alongside a replicated one. The entity arrives over the network carrying
`Predicted`, the client steps it every tick from its own input, lightyear keeps a
`PredictionHistory<C>` of what it guessed, and when a confirmed state arrives that disagrees, it
rewinds to that tick and re-runs `FixedMain` forward to the present.

That replay is why the movement step lives in `shared/src/simulation.rs` as *one* system rather than
one per binary. The server runs `step_players::<()>`, the client runs
`step_players::<With<Predicted>>`, and the filter is the only difference. Two copies of the same six
lines is precisely how prediction stops working: one of them gains a condition, and the drift shows
up as a correction the player feels rather than as a compile error.

What is deliberately *not* predicted is the mouse. The look angles live on the client's camera
entity, outside anything replicated, and travel to the server as input. Being thrown back a fifth of
a second of mouse movement is far worse than the position error a rollback would be fixing. `Aim` —
the replicated angles other players see — *is* predicted, because it is a pure function of the input
and replays to exactly the same value.

Measured at 60 ms of simulated latency with 8 ms of jitter, walking 44 m:

| | |
|---|---|
| client running ahead of the server | +0.60 m |
| rollbacks, clean link | **0** |
| rollbacks, 10 % packet loss | 1, replaying 15 ticks |

The lead is not an error — it is the point. The client simulates roughly a round trip into the
future so that its own input takes effect immediately; the server is where it should be, an
`RTT/2` behind. Before prediction the same measurement showed a 1.1 m gap between two simulations
that had no way to reconcile.

The zero deserves the packet-loss row beside it, because zero rollbacks is also what a broken
rollback check would report. Under 10 % loss the server misses an input, falls back to repeating the
last one, and diverges — and the client notices and replays. The machinery fires exactly when it
should and not otherwise.

Still open: **visual correction**. A rollback currently snaps the camera to the corrected position.
Lightyear can decay the error over several frames (`add_correction`), which needs `PlayerState` to
implement `Diffable`. At one rollback per eight seconds of lossy link this is not yet visible, but it
will be the moment players collide with each other.

---

## Risks

**Bone path matching — the significant one.** Bevy binds animations by bone *path*. The 49 clip
GLBs and the converted `Swat.glb` must agree on hierarchy and naming. Mixamo rigs are consistent,
but FBX-to-GLB conversion can name the root node differently. Verify this first thing in M2,
before building anything on top of it.

**lightyear's learning curve.** Capable, but the API shifts noticeably between minor versions and
the documentation has gaps. Budget time for reading the examples.

**Mixamo licensing.** The assets may not be redistributed, which is why `webgame` keeps them out of
its repository. The same applies here: they stay in `.gitignore` and each developer downloads them.
Shipping them inside a packaged build is a separate question and less clear-cut than the web case —
worth settling before cutting releases. It does not affect development.

---

## Character assets: an open question

The player model described above comes from Mixamo, which permits use but not redistribution. For
an open-source project that means the assets stay out of the repository and every contributor
downloads them, exactly as `webgame` does with `setup-assets.sh`.

**Quaternius** was evaluated as a CC0 alternative in August 2026. What the free Standard packs
actually contain:

| Pack | Clips | Locomotion | Weapon |
|---|---|---|---|
| Universal Animation Library | 43 | forward only | pistol |
| Universal Animation Library 2 | 43 | forward only | sword, shield |
| Toon Shooter Game Kit | 17 per character | forward only | rifle poses |

The 8-directional locomotion the marketing describes exists only in the paid Source tier. Two
further findings:

- **Rigs differ between packs.** The Animation Library and the Universal Base Characters share a
  bit-identical 65-joint rig in Unreal naming (`root`, `pelvis`, `spine_01`). The Toon Shooter Kit
  uses a separate 43-joint rig with one name in common, so its clothed characters cannot be driven
  by the Animation Library without retargeting.
- **The licence changed on 28 August 2026.** Quaternius replaced CC0 with the *Quaternius Asset
  License v1.0*, which forbids redistributing the assets themselves while explicitly permitting
  distribution of a finished product that incorporates them. Downloaded archives still carry CC0
  notices and the site FAQ still says CC0, so the situation is inconsistent; QAL §7 states that the
  version in force at download time governs.

Net effect: **no source evaluated so far allows the assets into a public repository**, so the
contributor-downloads-them step is unavoidable either way. Quaternius is the clearer of the two on
shipping builds, where Mixamo leaves a grey area.

One upside of the Quaternius packs regardless: each library ships twice, once with root motion
baked in and once with it disabled, which removes the stripping step described under
[Root motion](#root-motion) entirely.

This decision is deliberately deferred until M2. M0 and M1 use a capsule placeholder.

## Still to settle

- **Character assets** — see above.
- **Lag compensation** — M4 adds the position history; whether to rewind by full snapshot or per
  entity is an open design question.
