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

### Simulating a bad network

On localhost there is no latency, no jitter and no loss, so every netcode behaviour worth having is
invisible and every measurement is meaningless. Three environment variables put a conditioner on the
link, read by both binaries:

```bash
NOOB_TUBE_LATENCY_MS=60 NOOB_TUBE_JITTER_MS=8 NOOB_TUBE_LOSS=0.02 \
  cargo run -p noob_tube_server
```

The conditioner delays incoming payloads only, so the same values on both ends give a symmetric link
with a round trip of twice the latency.

What it makes visible: at 120 ms round trip the client's own capsule trails its camera by about
1.1 m while walking, and catches up when it stops. Measured server against local simulation, both
read from the running client:

```
moving   server z =  1.97   local z =  3.09   1.12 m apart
         server z =  9.71   local z = 10.82   1.12 m apart
stopped  server z = 13.84   local z = 13.84   0.00 m apart
```

That gap is the latency itself — the client simulates ahead, and the server state it reads is
another half round trip old. It is exactly the artefact prediction and reconciliation exist to hide,
and it cannot be seen without a conditioner. The 0.000000 agreement measured earlier says only that
two identical computations with nothing disturbing them produce the same answer.

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

Measured across the two processes after 22 metres of walking and strafing, the server's position and
the client's local simulation agree to the last digit — maximum difference 0.000000. They are
computed independently; the client's own player is a local entity that replication never touches.
That identity is the precondition for M4: prediction is only worth doing if replaying an input
locally lands where the server will.

Lightyear replicates *components*, not snapshots, which is worth stating because it is the opposite
of how `webgame` worked. There is no per-tick blob of the whole world: each component is registered
on its own and gets its own treatment.

| component | mode | why |
|---|---|---|
| `PlayerState` | `replicate` | position and velocity, changing every tick |
| `Aim` | `replicate` | where the player looks, changing every tick |
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
with `client::Remote`. That is what the client filters on to avoid ever drawing a capsule on the
local player.

Shooting is hitscan: the client reports where it aimed, the server raycasts against player capsules
(not per bone yet), applies damage, and handles death and respawn.

Death drops a ragdoll: the animated model is swapped for the rigid-body skeleton described above,
placed at the pose the death animation reached, seeded with the player's velocity plus the shot
impulse. Bone transforms are then read back from the body poses each frame. This runs purely on
each client — the server only replicates that the player died.

**Two players fighting each other on the plane completes the first step.**

Hit detection here is *not* lag-compensated — the server tests against the position it currently
holds, not against what the shooter saw. Against moving targets that means visibly needing to lead
your shots. Fixing it needs a position history to rewind into, which arrives with M4.

### M4 — Prediction
The local player is predicted and reconciled against server snapshots via rollback and replay.
Without it, movement feels mushy above roughly 50 ms of latency.

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
