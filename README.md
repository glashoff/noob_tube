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
| Collision | **Avian 0.7** | Level geometry as static bodies; the player is a kinematic capsule that never enters the solver. See [Two kinds of physics](#two-kinds-of-physics). |
| Ragdolls | **Avian, client-side** | Cosmetic only. Dynamic bodies with angle-limited joints, never replicated and never rolled back. |
| Assets | glTF/GLB, loaded natively | Bevy reads GLB including skeletal animation. Bevy cannot read FBX, so `Swat.fbx` needs converting. |

### Two kinds of physics

The project needs physics twice, under opposite constraints. Keeping the two apart is the single
most important structural decision here. What changed is *where* the line runs: it used to separate
two libraries, and it now separates the player from everything else.

**The player — stateless, shared, rollback-safe.** Movement is a design, not a simulation result.
Quake-style acceleration, air control and ground snapping are formulas, and a solver that negotiated
over them would be negotiating over the feel of the game. So the player is never a rigid body: it is
a capsule that Avian sweeps and slides on demand, through
[`physics::Level`](shared/src/physics.rs). Nothing about it persists between ticks, so the entire
rollback state stays at four fields — `position`, `velocity`, `on_ground`, `crouching` — and a
replayed tick is bit-identical to the original.

**Everything else — stateful, and that is now fine.** Crates that can be pushed, doors, vehicles,
ragdolls: all of them want a solver, and `lightyear_avian3d` rolls one back. *(Level collision and
replicated kinematic bodies run on Avian today. Nothing is a dynamic body yet, which is the only
reason the solver has no work to do.)* It snapshots the whole
persistent state — contact graph, constraint graph, islands, sleeping, warm-start impulses, the
collider BVHs — locally, per tick. Only `Position`, `Rotation`, `LinearVelocity` and
`AngularVelocity` ever cross the network; a client re-derives the rest by running the same solver
over the same poses.

That derivation is why the line still exists. Solver state *converges*, it does not match: after a
rollback two peers agree on positions and only approximately on contact impulses. For a crate that
is invisible. For the player it is the stutter this whole project is built to avoid.

**Why Avian and not rapier.** The first version used `rapier3d` directly as a query library, with
`PhysicsPipeline::step()` never called. It worked, and its one limitation ended it: the level BVH is
built once and cannot be refit, so no collider in it can ever move. A lift, a swinging door or a
vehicle is not expressible at all. Avian's collider trees update incrementally as bodies move, keeps
static geometry in a tree of its own, and — through `lightyear_avian3d` — comes with the rollback
integration that made the solver affordable in the first place.

The migration was held to account by a test that asked both engines every query the movement code
makes, kept while both were in the tree. Rays agreed to the last decimal place; capsule sweeps
agreed horizontally and differed vertically by design, because Avian's move-and-slide runs
depenetration passes that hold the capsule a skin width clear of a surface where rapier's let it
rest. Pushed diagonally into a crate's corner rapier squeezed the capsule 7 cm upwards and Avian
does not, which is a way of climbing a box we are glad to lose. rapier is gone and so is the test.

**Ragdolls — client-only, cosmetic.** When a player dies the body falls under its own simulation.
That carries state by definition, and it does not matter: it never touches gameplay, is never
replicated, and is never rolled back. Two clients may see the same corpse land differently and
nothing breaks. These are ordinary Avian dynamic bodies without a prediction marker — the second
physics world the earlier design needed is simply gone.

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
    → Collider::trimesh → an entity with RigidBody::Static
```

Colliders are ECS entities, so the acceleration structure maintains itself: Avian keeps static,
kinematic and dynamic bodies in separate trees and refits them as things move. That is what the
first version could not do at all — its BVH was built once and no collider in it could ever move.

Four things about Avian's API answer *wrongly* rather than loudly when they are got wrong, and each
one cost real time:

- **`MoveAndSlide` only sees colliders attached to a rigid body.** Its collider query is filtered
  `With<ColliderOf>` and used as the predicate for every cast it makes, so a bare `Collider` is
  invisible to sweeps while staying visible to `SpatialQuery::cast_ray`. Level geometry carries
  `RigidBody::Static` for that reason and no other.
- **A collider is placed by `Position`, not `Transform`.** `Transform` reaches `Position` through a
  system, so a collider spawned with only a transform sits at the origin until that has run.
- **Disabling `PhysicsTransformPlugin`, which `lightyear_avian3d` requires, breaks collider
  queries** unless `Transform` is required from `ColliderMarker` by hand. Without a
  `GlobalTransform` present when `ColliderOf` is inserted, a collider is wired into the tree
  wrongly: its shape and pose stay correct — a direct `Collider::cast_ray` still answers — while
  `SpatialQuery` never finds it, so every ray misses and the floor is not there.
- **`Collider::cuboid` takes full side lengths**, where `Hitbox` and the level constants are
  written in half-extents.

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

Testing usually means two or three clients at once, and every one of them opening a window over
whatever you were doing is more than an irritation: an unfocused window releases the cursor, and a
client with a released cursor stops firing, so the windows change the thing being measured.

```bash
NOOB_TUBE_HEADLESS=1 cargo run -p noob_tube_client --features remote
```

builds the client with **no window at all**. Not a hidden one — winit cannot hide a window on
Wayland. `WinitPlugin` is left out, no window is ever created, and a plain loop drives the schedule
instead of an event loop. Rendering is still set up, so meshes, materials and the camera behave
exactly as they do on screen; there is simply nowhere for the frames to go, and everything is driven
over BRP instead. Startup is a few seconds slower, because initialising the GPU is now the longest
thing that happens before the connection. What does *not* work headless is the screenshot harness,
which needs a surface to read back.

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
tick_hz = 64.0             # simulation rate; must match on both sides
meta_port = 5001           # where the server publishes this config      (server only)
ping_ms = 100              # simulated round trip; each end delays half of it
jitter_ms = 10             # random variation on each leg, ± this
loss = 0.02                # packet loss probability, 0.0 to 1.0
send_hz = 32.0             # how often the server replicates           (server only)
cmd_hz = 64.0              # how often the client sends inputs         (client only)
input_redundancy = 5       # consecutive input packet losses survived  (client only)
interp_ratio = 1.7         # interpolation delay, in send intervals    (client only)
interp_min_ms = 5          # floor under that delay                    (client only)
min_client_lead_ticks = 1  # guaranteed lead of the client's clock     (client only)
jitter_safety_multiple = 4 # multiples of measured jitter added to it  (client only)
input_delay_min_ticks = 0  # postpone the tick an input counts for     (client only)
input_delay_max_ticks = 0  # ping covered by delay before predicting   (client only)
max_predicted_ticks = 100  # how far ahead the client may simulate     (client only)
lag_compensation = true    # rewind targets to what the shooter saw           (both)
lag_comp_history_ticks = 35 # how far back the server can rewind        (server only)
```

```bash
NOOB_TUBE_PING_MS=200 cargo run -p noob_tube_client    # try one value, edit nothing
```

Both binaries read the same file and take the fields they need, so one file describes a whole
session.

**The conditioner has to be in effect on every process.** It delays only what a process *receives* —
the server's copy delays inputs coming in, each client's copy delays snapshots coming in — so a file
read by the server alone gives a half-duplex link that behaves like nothing real.

Verified: a 64 Hz server and a 64 Hz client connect, a 128/128 pair connects, and a 64 Hz server
with a 32 Hz client times out instead.

#### Learning the tick rate from the server

A client should not have to be told what the server runs at, so it asks. The server publishes its
`NetConfig` as TOML — the same language the config file speaks, parsed by the same code — over a
small HTTP endpoint on `meta_port`, beside the game's UDP socket:

```
$ curl http://127.0.0.1:5001/
tick_hz = 128.0
ping_ms = 0
...
```

The client fetches it in `main`, before `App::new`. It has to be that early, and that is the whole
reason for a second listener rather than sending it over the game connection: the tick rate goes
into the lightyear plugin group and into `Time<Fixed>` at app-build time, so by the time a
connection exists the app is already built around a number. lightyear cannot help here — it never
puts the tick duration on the wire (`SenderMetadata` carries the send interval *in ticks*, which is
circular), and its `SetTickDuration` trigger is half-finished: the only global observer updates
`Time<Fixed>` and leaves the `TickDuration` resource that every timeline converts with untouched.

**Only the tick rate is adopted.** Everything else in the served config is either the client's own
preference — its simulated link, its input rate, how far in the past it draws other players — or
something lightyear already learns over the wire. A server dictating a client's latency simulation
would be nonsense.

Measured, with the client always starting at 64 Hz:

| server | endpoint | client says | result |
|---|---|---|---|
| 128 Hz | on | *runs 128 Hz, adopting it over our 64* | connects |
| 64 Hz | on | *agrees on 64 Hz* | connects |
| 64 Hz | off | *no metadata, keeping our 64 Hz* | connects |

The first row is the point: before the endpoint, that pair could not connect at all.

This is a convenience, not a safety net. The safety net is below, and stays.

`tick_hz` is the one setting both sides must agree on — the server owns the simulation rate and the
client replays its prediction at it. Two processes reading two files cannot be made to agree, so the
tick rate is mixed into the netcode protocol id instead: peers that disagree fail to connect rather
than connecting and then quietly disagreeing about every tick number after that. The symptom is
`connection request timed out`, and both binaries log their rate at startup.

Nothing else here fails quietly either, which is the point:

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
| the server learning what you did | half the ping, plus the client's lead | `ping_ms`, `min_client_lead_ticks` |
| seeing another player's move | half the ping + interpolation delay | ping, `SEND_HZ`, `INTERP_RATIO` |

The third is the one worth spending time on. At the defaults it is `1.7 / 32 Hz ≈ 53 ms` on top of
the network. Raising `send_hz` shortens it and costs bandwidth; lowering `interp_ratio` shortens it
and starts letting remote players freeze between updates, because the next one has not arrived yet.

Lightyear **clamps rather than extrapolates** when it does run dry, so a too-short delay shows up as
players stuttering to a halt and jumping, not as them sliding through walls.

#### The other direction: how often inputs go out

`send_hz` is the server talking. `cmd_hz` is the client talking back — Source's `cl_cmdrate`, and
the same trap `send_hz` was: lightyear's own default sends inputs every *frame*, which at 200 fps is
three packets per simulated tick, two of which carry no tick the first did not. It now defaults to
64, matching the tick rate as Source does.

Beside it, `input_redundancy`: every input message repeats the last N packets' worth of ticks, so a
lost packet is covered by the next one instead of costing the server a tick of movement. Five by
default. It is the cheapest redundancy in the protocol — inputs are a handful of bytes — and it is
why walking stayed straight at 10 % packet loss in the M4 measurement. Set it to 1 with `loss = 0.1`
to see what it buys.

Both are fixed when the protocol is registered, which is why `ProtocolPlugin` takes the config.

#### When does the server act on my input?

Two different answers, and only one of them costs the player anything. Both are decided entirely on
the client: it stamps each input with the tick it is *meant for*, and the server simply acts on that
tick. Rollback, throughout, means **client-side** rollback — the server never rewinds.

**The cheap answer: hold the client's clock further ahead.** It already runs ahead of the server by
roughly half the ping, exactly so that an input stamped for tick `T` arrives before the server
simulates `T`. `min_client_lead_ticks` is the guaranteed floor under that lead, on top of what ping
and jitter already demand. Local movement is untouched — the client applies your input the moment
you press it either way; only the whole timeline moves further into the future, so inputs land with
more slack. Measured at a 100 ms ping, leads of 1, 6 and 12 ticks all leave the input delay at 0.

Below 1.0 is refused: the server would sometimes simulate a tick before its input arrived.

**The expensive answer: postpone the tick the input counts for.** This is `input_delay_*`, the
fighting-game knob, and it is *not* what Source does. The catch is that the client also waits for
that tick before applying the input to itself — written into the buffer at `T + d`, read back out at
`T`:

```rust
// buffer_action_state                     // get_action_state
let tick = current_tick                    let tick = local_timeline.tick();
    + timeline.input_delay();              if let Some(s) = input_buffer.get(tick)
input_buffer.set(tick, snapshot);
```

It has to. The whole point is that both simulations use the input at the *same* tick; applying it
locally at `T` and on the server at `T + d` is a disagreement by construction, which is a rollback
on every keypress. So the trade is real and one-directional: fewer rollbacks, at the price of your
own movement starting that late every time, even on a perfect link.

Three knobs for it, all on the client:

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

**The hitbox is the movement capsule**, and the placeholder silhouette is drawn to fit inside it. It
did not always: the first version of the heads sat from 1.70 m to 2.04 m, entirely above the 1.70 m
capsule — perfectly visible and impossible to shoot. The conflict that produced it is real, though:
a mesh capsule matching the collision shape exactly *encloses* a head and hides it. The fix is to
draw the body shorter and give the head the room, not to move the head out of the hitbox.

`silhouette_fits_inside_the_hitbox` in `shared/src/movement.rs` holds that to account, and caught a
30 cm gap between body and head while the numbers were being chosen.

Death drops a ragdoll: the animated model is swapped for the rigid-body skeleton described above,
placed at the pose the death animation reached, seeded with the player's velocity plus the shot
impulse. Bone transforms are then read back from the body poses each frame. This runs purely on
each client — the server only replicates that the player died.

**Two players fighting each other on the plane completes the first step.**

Firing travels as a bool in `PlayerInput`, which buys three things at once: it is stamped with the
tick it belongs to, it inherits the input redundancy so a lost packet does not swallow a shot, and
the tick number is exactly what lag compensation will need to rewind to.

The rate of fire is `PlayerState::fire_cooldown`, in the rollback snapshot with everything else. It
has to be predicted — the client's own answer to "can I shoot yet" cannot wait for a round trip, or
the weapon feels disconnected from the trigger. What is *not* predicted is `Health`. A client
guessing that its shot landed would have to un-kill someone on screen when the server disagreed, and
there is no graceful way to do that.

`resolve_shots` runs before `step_players` in `FixedUpdate`, so a shot resolves against the
positions its shooter was looking at rather than the ones a tick of movement later — and so the
cooldown check sees the trigger before `apply_input` consumes it.

Measured with two clients 2 m apart, one holding fire:

```
target health over 2.5 s:
32 -> 100 -> 66 -> 32 -> 100 -> 66 -> 32 -> 100 -> 66 -> 32 -> ...
6 respawns
```

Three shots to kill, respawn at the player's own spawn point. Turned 90° away with the trigger still
held, health stops moving.

#### Seeing a shot

Until now a shot was a number in the server's log. Three things show it now.

A **crosshair**, because the weapon fires down the centre of the screen and nothing said where the
centre was. A **tracer** along the shot's path, gone in a twentieth of a second, which is what makes
fire directional — being shot at *from somewhere* is a different thing from being shot at. And a
**bullet hole** where it met the level, which is what makes a fight leave a mark. A shot that landed
on a player puts a marker on the shooter's crosshair instead: white ticks with dark outlines, since
the players are red and a red marker on the body it just hit would be invisible exactly when it
matters.

The server resolves every shot and sends a `ShotFired` — shooter, muzzle, endpoint, and whether it
stopped in a player — to everyone including the shooter.

**One's own tracer is predicted.** Automatic fire is not split into shots by either side alone:
`fire_cooldown` lives in `PlayerState`, which is predicted, so the client and the server run the
same `fire && cooldown == 0` over the same input and pick out the same ticks. The client already
knew which ticks were shots; it simply did nothing with it. Now it draws the line immediately.

The distinction that matters is between the client *computing* that and the client *deciding* it. A
client that sent discrete "shoot now" events would set its own rate of fire, which is the cheat. A
client that replays a shared rule while the server does the same is prediction, and the invented
shot of a lying client still has no effect on anyone.

Only the tracer, though. The bullet hole and the hit marker still come from the server, and they can
afford to: a hole lasts twelve seconds, so arriving late is invisible, and a *hit* is exactly the
thing a client must never guess — the server rewinds the world to decide it, and a predicted kill it
then denied could not be taken back. The rule is the same one prediction always follows: predict
what has to be instant and is over in a moment, take from the server what is long-lived and
authoritative.

Both are consequences of the same shot, so the gap between them measures what the prediction is
worth:

| ping | own tracer | bullet hole (server) | saved |
|---|---|---|---|
| 100 ms | 22 ms | 178 ms | **157 ms** |
| 300 ms | 13 ms | 379 ms | **366 ms** |

and over the same runs, 16 and 17 holes for about two seconds of fire — 7.3 and 7.1 a second against
the 8.0 the cooldown allows. One tracer per shot, not two.

Finding this needed one fix first. `resolve_shots` aimed with the replicated `Aim`, which
`step_players` writes at the *end* of a tick — so every shot went off with the previous tick's
angles, 15.6 ms of mouse movement stale. Invisible until a client tried to reproduce it, at which
point the two would have drawn different lines. The angles now come from the same tick's input on
both sides, and `shooting::fire` is the one place a held trigger becomes a ray — called by the
server to score the shot and by the shooter's client to draw it, for the same reason `step_players`
is one system and not two.

#### Does the server know where the shooter was?

The obvious worry: the client predicts its own movement, so the server might disagree about where
the shot came from, and then the shot is not scored the way the shooter saw it. The usual answer to
that is to let the client send its own position with every shot.

It turns out not to be a prediction problem at all. The server does not *guess* where the shooter
is — it **simulates** the shooter, from the shooter's own inputs, and is the authority on the
result. The client runs the same code over the same inputs. The two can only disagree when an input
never arrives at all.

Measured by having both sides log the eye position of every shot and matching them by tick, while
running, turning, jumping and firing at once:

| ping | loss | jitter | input redundancy | shots on matching ticks | worst disagreement |
|---|---|---|---|---|---|
| 100 ms | 0 | 0 | 5 | 57 / 57 | **0.000 cm** |
| 300 ms | 15% | 40 ms | 5 | 57 / 57 | **0.000 cm** |
| 300 ms | 50% | 40 ms | 5 | 56 / 56 | **0.000 cm** |
| 300 ms | 50% | 40 ms | **1** | 0 / 56 | — |

Bit-identical, with zero rollbacks, up to half the packets being dropped. `input_redundancy` is what
buys that: every input message repeats the last five packets' worth of ticks, so an input has to be
lost five times running to be lost at all.

The last row is the interesting failure, and it is not the one expected. With the redundancy turned
off, the two never disagree about a *position* — they disagree about which **tick** the shot happens
on. The input carrying the trigger's first press is lost, the server keeps doing the last thing it
was told for two more ticks, and from then on the cooldown keeps both sides firing every nine ticks
but permanently two ticks apart. Every shot still happens; each one is 31 ms out of step. So
`input_redundancy` is not only about movement not stuttering — it is what keeps the trigger itself
in step.

Which leaves the part of the worry that is real, and it is the *other* end of the shot. What the
shooter saw of **everyone else** is off by the whole round trip, and that error was measured at 0.6
to 1.8 m. That is what the shot already reports, as the view bracket above — the client does send
what it saw, for the half of the problem where it matters by two orders of magnitude.

This is also what the genre does. Source (Counter-Strike, TF2), Overwatch, Valorant and Apex are all
server-authoritative with client-side prediction and target rewind; the shooter's own position is
the server's, never the client's. The other family — client-reported hit registration, where the
client says "I hit them" and the server checks whether that was plausible — appears in parts of the
Call of Duty and Battlefield lineages. It feels better on a bad connection and is far more
exploitable, and it buys nothing here that redundancy has not already bought.

It goes **unreliably**, on a channel of its own. A tracer lives for 50 ms, so a retransmitted one
arrives after the moment it belongs to, and drawing it then is worse than not drawing it. The
separate channel also means a burst of effects can never delay a position update, and that the
whole lot can be dropped under bandwidth pressure without losing anything the simulation needs.

The message carries no surface normal, though a decal needs one to lie flat on a wall. Every client
holds the same `CollisionWorld` the server does, built from the same numbers, so it casts the ray
itself. Twelve bytes a shot that never have to be paid for.

Two details that only showed up on screen. A tracer is light, not an object, and left casting
shadows it drew a black stripe across the ground beside every shot — the most obviously wrong thing
in the first screenshot. And one's own tracer starts at the eye, where perspective turns any real
thickness into a wedge across half the screen; it now starts a little right and below, and never
further out than a third of the way to what was hit, so a point-blank shot does not become a blob.

Registering the channel is not enough to make it work: `add_direction` is what wires it into each
connection's transport. Without it the server sent into a `ChannelNotFound`, logged once per shot
and dropped, and every client drew nothing.

#### Things that move and are not players

A crate that rides up and down, to have something moving that is nobody's player. It is the case
lag compensation has to cover for a lift, a swinging door, a train.

The crate is a **kinematic Avian body** whose `Position` the server writes each tick. Clients
receive that pose like any other replicated component and interpolate it; nothing on a client works
out where the crate ought to be. That is deliberate even though the motion is a pure function of the
tick and every client *could* compute it — the moment anything can stop, push or break the crate, a
locally computed one is wrong, and the version that is wrong later is not worth being right now. It
also means a crate goes down exactly the same path as a player, replicated and interpolated and
rewound out of the same `HitboxHistory`, rather than being a second mechanism beside it.

There are two kinds of crate, and the difference is one variant of `RigidBody`. A **bobbing** crate
is kinematic: it goes where the server puts it and nothing pushes back, which is what an animation
is, and it is the moving target lag compensation is measured against. A **loose** crate is dynamic:
it falls, it stacks, and a shot shoves it — the first thing in this game the solver actually does
work for.

Loose crates are 40 kg, which is a number worth stating. Avian derives mass from a collider's volume
and its density, and the default density of 1 makes a cubic-metre box weigh a kilogram; a shot then
launched it at forty metres a second, out of the level. Wood is around 40 kg per cubic metre packed
loosely, and at that weight the same shot shoves the crate a few centimetres.

Loose crates were briefly **predicted by every client**, and are not any more. The reason is worth
writing down, because the obvious objection to prediction is the wrong one.

The obvious objection is that a predicted entity lives in a different time from the server's, so a
shot at one would be resolved against a pose the shooter never saw. That is not true. A client
predicting tick T+k draws the state of tick T+k, and the shot it fires is *stamped* for T+k; the
server resolves it while simulating T+k, against its own state for that same tick. Prediction is not
a different time — it is the same tick, computed earlier. Interpolation is the one that genuinely
draws the past, which is why it, and not prediction, needs lag compensation.

The real objection is information. A client can only predict what it has what it needs to compute,
and what moves a crate is *somebody else's* shot, which it learns about no sooner than the server
tells it. Predicting it is therefore guessing, and the guess is wrong every time anyone else fires:
measured at a median snap of 3.7 cm and up to 79 cm, in a single frame. Worse, a client that has not
yet heard about another player's shove aims at a crate that is no longer there, and misses — where
an interpolated crate is reconstructed exactly by the server and hits what the shooter saw.

So the rule is not "predict what moves" but **predict what you have the information to compute**:
your own player, and the vehicle you are driving. Everything else is interpolated and rewound out of
a `HitboxHistory`, which is what that machinery is for.

Verified live: four crates dropped a little above their resting heights settle at 0.50, 1.50, 2.50
and 3.50 m and drift 0.00 cm over the following second. A burst into the bottom of the stack moves
it by up to 10 cm, and the shove propagates up through the contacts. While they were still
predicted, at 300 ms of ping with 15% packet loss the client's own simulation stayed within 0.0 cm
of the server's across 24 rollbacks and 584 replayed ticks — the solver rollback works; it is simply
not what a crate wants.

**`max_rollback_ticks` has to be raised, and getting it wrong fails silently.** Lightyear keeps two
separate bounds — how far ahead a client may predict, and how far back a rollback may reach — and
takes the smaller. The rollback default is 20 ticks, which is 312 ms at 64 Hz, so at 300 ms of ping
a correction is simply dropped: no rollback, no warning, and a predicted crate that was shoved froze
ten centimetres from where the server had it and stayed there indefinitely. It worked at 60 ms and
silently did not at 300 ms, which is the worst shape a bug can have. `PredictionManager` is now
built with the rollback bound set from the same number that bounds prediction, because there is only
one honest answer: a rollback can never need to reach further back than the client is allowed to run
ahead.

Making all of this possible took two generalisations, both of the same shape. A target used to be a
feet position and a `crouching` flag: a player and nothing else, with the shape hard-coded in the
hit test. It then became an enum with a variant per kind of target, which is the same problem one
level up — a vehicle needs a new variant, a per-bone hitbox needs another, and each brings its own
arm in the ray cast. A `Hitbox` now holds an actual `Collider` and a pose, so anything Avian can
express is a target and the hit test is one call with no cases in it. Rotation came free with it: a
door that swings rewinds to the angle it was at, which the enum could not represent at all.

A crate is **not** terrain, and for a while that meant you walked through it. It sits on
`Layer::Body` and the movement queries only asked about `Layer::Level`. They now ask about both, and
the two filters are what the distinction became:

- **footing** — the map *and* the things in it. What a capsule sweeps against, what a ground probe
  finds, what a crouched player checks for headroom. A crate is something to climb.
- **sight line** — the map alone. What stops a bullet on its way to a target. Deliberately *not*
  widened: a crate is already tested as a hitbox, and counting it twice would let the wall test beat
  the target test at the same distance and turn a hit into a miss.

One trap came with it. A client's copy of a crate carried a bare `Collider` and no `RigidBody`, and
`MoveAndSlide` only sees colliders attached to one — its query is filtered `With<ColliderOf>`. The
crate was solid to a ray and transparent to feet, on the client only. They are `RigidBody::Static`
now: lightyear writes the pose and Avian never integrates a static body, so there is no second
opinion about where the crate is.

The jump reaches 1.116 m, measured. The loose stack's crates are a metre apart, so it can be
climbed a step at a time; the level's own crates are 2 m cubes and cannot be got on top of at all,
which is geometry rather than physics. Riding a *moving* platform is still separate work: an
interpolated crate is drawn in the past, and a predicted player standing on it would be standing on
where it was.

Verified live at 100 ms of ping, one player firing at a crate: the server reports rewinding it by
13 to 14 ticks and the crate having moved 0.12 to 0.43 m since — and the bracket it used came from
the *crate's* history, since there was no second player to take one from.

#### Vehicles

A four-wheeled off-roader, in the shape Half-Life 2 and Halo both use: **the wheels are not
collision shapes**. The whole vehicle is one dynamic box, and at four points on it a ray is cast
straight down. Where the ray finds ground, a spring pushes the chassis up by how far it is
compressed, a damper resists how fast that is changing, and a tyre model at the contact patch
resists sliding sideways much harder than it resists rolling.

That looks like a shortcut and is the opposite of one. Rolling cylinders catch on the seams between
triangles, climb steps they should bounce off, and need a much shorter timestep to stay stable —
and a rollback pays for all of that once per replayed tick. Four rays cost four rays, behave the
same at any speed, and every property worth changing is a number in `VehicleSpec` rather than a
solver setting.

The spec is **not replicated**. `VehicleKind` travels once per entity and the numbers behind it are
a constant both sides already have, in the same way the level's geometry is: client and server agree
by being built from the same source. Nor are the wheels replicated — where a wheel sits is a pure
function of the chassis pose and the ground under it, so a client that has the pose works it out
with one ray each, at frame rate rather than tick rate. Sending four wheel states per vehicle per
update to save four rays per frame would cost bandwidth to save nothing.

Unlike a crate, a vehicle is not a borderline case for prediction: the driver's input goes into it
and the result comes back out under the driver's own camera, so whoever is driving must predict it
and everyone else interpolates — the same split as a player, for the same reason. Until there is a
driver there is no input, so nobody predicts a parked one. That is also why `drive_vehicles` takes a
query filter exactly as `step_players` does: the server steps every vehicle, a client steps only the
one it is driving.

Verified live. Parked, the chassis settles at 1.0661 m with all four struts compressed 0.1839 m and
zero velocity — which is exactly what `mg/4k` predicts, so the spring constants are checked against
the simulation rather than against themselves. Shoved at 12 m/s into a 12° ramp: the front struts
take the transition (0.44 m against the rear pair's 0.19 m), it climbs to 3.46 m, leaves the ramp
with all four wheels off the ground, lands front-first, absorbs it at 0.46–0.63 m of compression and
settles back to 1.06 m. A client interpolating all of that stayed within 2–30 cm — which is the
interpolation delay, not error — and matched the height and the tilt through the jump.

There are two of them, and that is a test rather than scenery. Everything about the seat is written
per vehicle — `use_vehicles` picks the nearest free one within four metres, and the prediction
handover names the vehicle it applies to — so a second one is the cheapest way to find out whether
any of it was quietly written for exactly one. Verified live: climbing into the far vehicle makes
that one `Predicted` on the driver's client and leaves the other `Interpolated` and static, driving
it moves it and nothing else, and getting out hands it back. The two start a quarter of a turn
apart, because two vehicles facing the same way say nothing about whether the spawn honours a
rotation. A test checks they start clear of each other, of the crates and of the ramp; nothing
checks that at spawn time, and being pushed apart on the first tick looks like a bug because it is
one.

**`record_positions` now runs explicitly after the solver.** Avian steps in `FixedPostUpdate` and so
did the history recording, with no ordering between them: a *dynamic* target's history would be
filled with the pose from before the step on some runs and after it on others. A kinematic crate hid
this completely, because nothing but a system of ours ever moved one.

#### Driving it

**E** gets in, and gets out again. The player is hidden rather than despawned — still a replicated
entity with a pose and a hitbox, back on their feet the moment they get out — and the walking step
skips them, which is the whole of "you cannot walk while driving". They are not standing on the
vehicle and there is no cab to sit in; the camera moves seven metres behind it, because a
first-person view from inside a windowless box is a black screen.

Getting in is **not predicted**, and that is a decision rather than an omission. Whether a seat is
free is the server's to settle — two people reaching for the same door on the same tick have to be
resolved somewhere — and a client that guessed would have to be taken back out again. It costs half
a round trip before the camera moves, once a minute.

What *is* predicted is the driving, and the split happens at the moment of getting in: the server
gives the vehicle `PredictionTarget` for that one peer and `InterpolationTarget` for everyone else,
the same split a player gets, for the same reason. A client's copy swaps from `RigidBody::Static` to
`Dynamic` as it changes hands, which is the one place this could fail silently in either direction —
a predicted vehicle left static takes the throttle and does not move; an interpolated one left
dynamic falls through the pose being written on top of it.

Throttle, brakes and steering come from the same `PlayerInput` that walks, and the two sides differ
in exactly one place: the server looks up who is in the seat, a client uses its own input because
the only vehicle it predicts is the one it is driving. Everything downstream reads a `Controls`
component and cannot tell the difference.

**Steering is not a torque.** Turning the front wheels only changes which way their grip points; the
sideways force that grip produces is what swings the vehicle round, through the length of the
wheelbase. Nothing anywhere applies a turning moment.

That grip is capped by the load on each tyre — `friction x load x dt` — and the cap is not a detail.
Without it a rate alone let a tyre take out any amount of sideways speed however lightly it was
loaded, and full lock at 13 m/s scrubbed the vehicle to a standstill in three seconds, measured on a
live server. With it, a wheel in the air holds nothing, the inside wheels of a fast corner hold less
than the outside ones, and cornering too fast understeers instead of stopping dead.

Verified live at 100 ms of ping with 10 % packet loss, from a standing start to nearly 20 m/s and
round a full-lock corner: **zero rollbacks and zero correction**, and the client's vehicle within
0.000 m of the server's once stopped. The picture trails the simulation by exactly one tick
throughout — 18.7 cm at 12.4 m/s, 28.5 cm at 19.5 m/s, against a tick's 19.4 and 30.5 — which is
frame interpolation and nothing else.

One bug on the way, and it was the same one twice. `carry_driver` puts the driver wherever the
vehicle ended up, and it ran in `FixedPostUpdate` alongside Avian with no ordering between them, so
on some runs the driver was placed at the vehicle's pose from *before* the step. A tick of a
vehicle's speed is 20 cm, and it arrived as a correction on every update — the same ambiguity that
had already been fixed for `record_positions`, in the same schedule, for the same reason.

#### Getting back on its wheels

A vehicle on its side is not a hard problem to drive out of, it is an impossible one. The entire
model acts through the wheels, and the wheels find no ground; the only thing still touching the
world is a box that slides. Left alone, the round has one fewer vehicle in it from the first badly
taken ramp onwards.

So a vehicle that has been past 78 degrees of lean for a second and a half stands itself back up.
The delay is the whole difference between helping and interfering — a barrel roll passes through
upside down on its way to landing on its wheels, and righting it there takes the roll away from the
driver who earned it.

What does it is a spring and a damper on the attitude, the same shape as a strut: an angular
acceleration toward upright, proportional to the lean, minus a term against the spin it produces
itself. Deliberately not a snap to an upright pose — a teleport is a rollback's worst case, and two
sides that snap on slightly different ticks disagree by the whole of the flip. It says nothing about
yaw either, so a vehicle that lands facing a wall is stood up still facing the wall.

The first version did nothing at all, and the arithmetic says why. A buggy on its roof lies on a
face, and turning it means lifting 1200 kg over the edge it rests on: 10.6 kN·m, against the
4.9 kN·m a torque gentle enough to look like a vehicle can produce. The fix is not a bigger torque
but a hand underneath — six tenths of a g of lift while it is getting up, which takes most of its
weight off the ground and drops what the turn has to overcome to a third. Below gravity, so it never
leaves the ground; it just goes light on its edge. Measured in the test world, it is back on its
wheels 1.1 seconds after the delay expires and settled at its ride height a second after that.

#### Driving into things

Driving the buggy into the loose crates looked terrible, and it was two separate faults with one
symptom.

**The client was ramming a wall that was not there.** An interpolated crate is `RigidBody::Static`
on a client and stands where the server said it was a round trip ago. The predicted vehicle
therefore hit an immovable box in the past, while the server pushed straight through the real one.
Four seconds of that, at 100 ms of ping with 10 % loss:

| | rollbacks in 4 s | worst correction |
|---|---|---|
| one crate, before | 237 | 173 cm |
| one crate, after | **1** | **15.7 cm** |
| a stack of four, before | 245 | 740 cm |
| a stack of four, after | 238 | 32.5 cm |

The fix is the handover the vehicle already had: while somebody is driving, the server gives the
loose crates `PredictionTarget` for exactly that peer and `InterpolationTarget` for everyone else,
and takes it back when they get out. A client's crate then swaps `Static` for `Dynamic` and both
sides shove the same box on the same tick.

This is not a retraction of "a crate is interpolated". The rule was always *predict what you have
the information to compute*, and behind the wheel that is what a driver has — the crate is moved by
their own bumper. What they lack is somebody else's shot, which arrives as a correction measured
earlier at a median of 3.7 cm, and which the driver is the worst-placed person in the game to care
about, because nobody shoots from the driver's seat. Everyone not driving keeps the interpolated
crate and the rewound hitbox that goes with it. The crate's mass now travels on the wire as
`Density`, because a client that weighs a crate differently from the server pushes it somewhere
else.

The same treatment went to **parked vehicles**, which had the identical fault and a worse version of
it: 208 rollbacks and a 140 cm correction, now none and nothing. Worse despite the gentler impact,
because of mass — a 40 kg crate barely slows a 1200 kg buggy, so even the wrong answer was nearly
right, while two vehicles of the same mass trade half their momentum and the client's "it is a wall"
has nothing in common with the server's "they both move". It showed as the vehicle crawling forward
at half a metre a second and shaking.

That broke an invariant worth naming, because it had been load-bearing: *the only vehicle a client
predicts is the one it drives*. Parked vehicles are predicted now too, so a client had to be told
which one to steer, and `Driven` — the vehicle-side half of `Driving` — says it. Still not an entity
reference across the wire: *predicted and driven* can only ever match one entity, because a vehicle
somebody else is driving is one this client interpolates.

**A vehicle with a driver in it is deliberately not in here**, and that is the harder problem rather
than an oversight. It has an input behind it, that input belongs to a peer this client never hears
from, and no amount of solver work substitutes for not knowing what somebody else is pressing.

The stack is the honest remainder. Four boxes in contact are chaotic, two solvers stepping slightly
different histories diverge every update, and no amount of prediction fixes that — but the
disagreement is now a third of a metre instead of seven.

**The vehicle was braking against contacts it had not reached.** Avian predicts contacts before they
happen and by default lets the prediction reach as far as the body's velocity does. At 24 m/s that
is nearly a metre of guesswork ahead of the bumper, and the solver treats a contact surface as an
infinite plane — Avian's own documentation calls the result *ghost collisions*. Measured against the
identical run with nothing in the way, hitting a 40 kg crate cost the 1200 kg vehicle **3.8 m/s**
where the momentum it hands over accounts for 0.7. Bounding `SpeculativeMargin` to 10 cm brings that
to **0.9**.

Swept CCD was the obvious partner and measurably does nothing: 0.10 m and 0.02 m, with the sweep and
without, all cost the same speed to a tenth. It was never protecting anything — the thinnest thing
in the level is a metre thick and the vehicle covers 38 cm in a tick — so it is not switched on. A
sweep per body per replayed tick is not worth paying for on the strength of the name.

Two things this cost, and both are worth writing down. The crate never outruns the car, which was
the first thing suspected: a clean trace shows it leaving at 22.8 m/s from a 23.5 m/s vehicle and
then travelling with it, and the crates that ended up 200 m away were being dribbled, not launched.
Friction is exact — a crate given 15 m/s slides 22.9 m, against the 22.9 m that µ = 0.5 predicts.
And the speculative-margin effect, flatly reproducible on a running server across seven
configurations, does **not** reproduce in the test world at all: the same staged collision costs the
same to two decimal places with the bound and without. A test that passes either way is worse than
no test, so there is none, and changing that number is a thing to measure live.

#### Should a vehicle be predicted at all?

`predict_vehicles` in the config turns the whole of the above off, and it is a knob rather than a
decision because neither answer is obviously right.

Measured back to back at 100 ms of ping, from the input being set to the vehicle actually moving —
the server's own timing is the control, identical in both runs at ~345 ms of harness overhead:

| | picture moves | against the server |
|---|---|---|
| predicted | 260 ms | 87 ms **ahead** |
| not predicted | 436 ms | 89 ms **behind** |

So turning prediction off costs **176 ms**, not the ~130 ms the arithmetic suggested: half a ping
down plus the interpolation buffer is only the second half of it, and the first half is losing the
head start prediction was already running with. The comparison that makes it survivable anyway is
that the buggy takes 183 ms to reach full lock and two seconds to reach 20 m/s by itself, so this
lands on something already slow — 176 ms on a mouse-aimed shot would be unthinkable.

What it buys is everything prediction has been costing: no rollback storms on contact, no handover
machinery, and vehicles and crates back to being rewound exactly rather than only as accurately as
the prediction was.

**The experiment turned up something better than the number, though.** With the vehicle
interpolated, driving still produced 35 rollbacks a second — and not for any of the reasons guessed.
The driver is a *predicted* entity being carried by an *unpredicted* one: the server moves their
`PlayerState` with the vehicle every tick, and the client cannot, because it does not simulate the
vehicle. Nothing the client writes can agree, and two attempts to make it agree — deriving the seat
from the interpolated pose, then not deriving it at all — left the count unchanged, because neither
addressed the shape of it.

It is the same rule again, one level down: **you cannot predict a passenger of something you do not
predict.** For this mode to be clean the driver has to stop being predicted too while seated, and
that runs into an assumption further in — a client identifies its own player *by* it being the
predicted one. Which is where this stops, deliberately, rather than being hacked past.

`predict_vehicles` is therefore a ladder rather than a switch, and the middle rung is the
interesting one. `"world"` predicts the vehicle against the level and nothing else: the chassis
stops colliding with crates and with other vehicles on the driver's own client, while the server
still collides with all of it. The wheels still find crates — a suspension ray is a query, not a
solver contact, and the server casts the same one — so only the chassis stops noticing them.

Measured at 100 ms of ping with 10 % loss, three seconds of open road and then four seconds ramming
a parked vehicle:

| | open road | into a parked vehicle |
|---|---|---|
| `"full"` | 0 rollbacks | 0 rollbacks, 0 cm |
| `"world"` | 0 rollbacks | 54, worst 87 cm |
| `"off"` | 85 rollbacks | 110 rollbacks |

Which shows the shape of the trade exactly: `"world"` costs nothing at all while driving, which is
nearly all of the time, and pays it in one lump during the second of contact. `"off"` is worst
everywhere, including on an empty road with nothing to hit, for the passenger reason above.

**What it does not show is the case `"world"` exists for.** A parked vehicle is something `"full"`
can predict, so this table is `"world"` paying its cost with none of its benefit; the case it is
meant for is a vehicle *another player* is driving.

#### Predicting a vehicle somebody else is driving

Which `"full"` can now do, and could not before, because `Controls` travels.

The objection was that a client cannot predict another player's vehicle, having none of their input.
That is true of the input and false of the vehicle: the *result* of that input — throttle, handbrake,
where the wheels point and where the driver is asking them to point — is four numbers, and it
arrives every update like any other replicated state. A client that holds them for the length of its
prediction window is guessing how much a driver changed their mind in a round trip, which is very
little. Compare that with the alternative it replaces, which was a frozen box standing in the past.

`wanted_steer` travels beside `steer` for a specific reason. Holding the *angle* freezes a turn
halfway through it; holding the *intent* lets a peer keep easing the wheels exactly as the server is
easing them, so a driver holding full lock is predicted through the whole turn and is wrong only
from the moment they actually let go.

Measured against a vehicle driven by nobody this client can see, at 100 ms of ping with 10 % loss —
full throttle to 16 m/s, then the same again into full lock:

| | rollbacks in 2.5 s |
|---|---|
| accelerating in a straight line | 0 |
| accelerating into a full-lock turn | 0 |

Two bugs surfaced on the way there, and the second was the better find. The server never eased the
wheels of a vehicle with nobody in it — `take_the_wheel` only reaches occupied ones — so every
client predicting a driverless vehicle went on easing while the server did not: a disagreement
invented by the split rather than by the network, worth 78 rollbacks in two and a half seconds. And
stepping out of a vehicle left `Controls` at whatever they last were, so getting out at full
throttle left it accelerating away by itself for the rest of the round. Nothing cleared them,
because nothing had ever needed to before the driving step started running on vehicles nobody was
driving.

Still missing: standing on a vehicle rather than being inside it, passengers, a camera that gets out
of the way of walls, and running people over.

#### Three things a shot was getting wrong

Found by looking for why bullet holes were missing, and each worse than the symptom that led to it.

**Every wall was a target.** `resolve_shots` gathered props with an unfiltered
`Query<(Entity, &Collider, &Position, &Rotation)>`, which is every collider in the world — the
ground, the walls, the ramp. So a shot at a wall came back as a hit: it suppressed the bullet hole,
because a hit needs no decal, and put a hit marker on the shooter's crosshair. The map is already
accounted for by `Level::raycast`, which is what stops the bullet; a second reckoning of it can only
disagree with the first. Targets are now filtered to `Layer::Body`.

**A crate counted as a person.** `ShotFired::hit_player` was `target.is_some()`, and a prop is a
target like any other, so hitting a crate skipped its decal too. It now asks whether the thing hit
was a player.

**Shots were spinning the crates on rails.** `resolve_shots` handed its impulse to anything with a
`Forces`, and the comment beside it asserted that a kinematic body would not match. It matches
perfectly well — it has every component in that query — and Avian integrates a kinematic body's
velocities like any other's, so a corner hit set the bobbing crates turning. Only dynamic bodies
take a shove now, and the bobbing crates carry `LockedAxes::ROTATION_LOCKED` besides, because a
crate on rails does not turn and the entity should say so.

Bullet holes are also **hung on what they hit** rather than pinned in world space, when that thing
can move. A hole left floating where a crate used to be is the same objection that keeps decals off
players, only slower and so easier to miss.

#### Lag compensation

Every shot is tested against the world the shooter was looking at, not the present one.

Without that the server tests against where a target is *now*, while the aim came from where the
target was on the shooter's screen — older by the round trip **plus** the interpolation delay. At
100 ms of ping that is about 200 ms; a player crossing at 6 m/s has moved two thirds of a metre in
it. You would have to lead a running target by most of its own width, at every range, which players
experience as the game being broken rather than as a skill to learn.

The shot travels as a **tick-stamped input**, so the server knows which tick the trigger went down
on rather than when the packet happened to arrive. What it still needs is how far behind that tick
the shooter's view of everyone else was, and there are two ways to know it.

The exact one: **the client sends the two confirmed ticks it was blending between, and how far
between them it was.** A remote player's position on screen was never a position the server
simulated — it was a blend of two received snapshots at some fraction. Sending both ends and the
fraction lets the server rebuild that blend from its own history and land on the identical point.
The ticks are the *confirmed* ones, the ones replication actually delivered, which at a send rate
below the tick rate are two ticks apart rather than one — which is exactly why a single instant
would not do.

The fallback: lightyear's own `InputConfig::lag_compensation`, which has the client report its
interpolation *delay* with every input message. The server turns that into a moment and blends the
two ticks either side of it. On the same shots, measured against the bracket, the two land **0.3 to
2.1 cm apart** on a target running at 6 m/s — the delay is sampled once per message rather than at
the frame the trigger went down, and quantised on the way.

A shot with neither resolves against the present, which is the old behaviour. Each rung down is
logged rather than silent.

The server keeps the rest: `HitboxHistory`, a short ring buffer of what each target *was*, written
in `FixedPostUpdate` so it holds the value at the *end* of a tick — the one replication sends and
therefore the one a client interpolates towards. `resolve_shots` then rebuilds each target at the
moment the shot names, and passes those shapes rather than the present ones to the hit test.

It stores a whole `Hitbox` — collider and pose — and not just a position, because the shape changes
too. Crouching was already an example before any prop existed: someone who ducked half a round trip
ago must still be standing in the past the shooter aimed at. Rewinding a shape to the right place
but the wrong size is only half a rewind. Carrying the shape costs nothing to speak of: a collider
is shared, so an entry is two atomic increments rather than a copy of any geometry.

Per entity, not by world snapshot: a hitscan ray asks about each target separately anyway, and per
entity means a player who joined a moment ago simply has a short history instead of a hole in a
shared structure. The level is not rewound — geometry does not move, so a shot blocked by a crate
now was blocked by it then.

The client sends **one** bracket, not one per target, and that is enough: anything that is moving
produces an update every send interval, so everything moving shares the same bracket, and something
that is not moving produces no updates at all, over which any bracket gives the same answer. It is
read across every interpolated history, players *and* props — a lone player shooting at a moving
crate has no other player to take a bracket from, and would otherwise report none at all.

The bracket is read *before* interpolation runs again in the frame, on purpose. A player reacts to
what is on the screen, and what is on the screen is the blend interpolation produced last frame.

**Found while measuring this:** above roughly 150 ms of ping the client has no bracket to send,
because its interpolation timeline has caught up with the newest sample that has arrived — at
300 ms it sits four ticks past it. Lightyear is then clamping to the last received position rather
than blending two, which means remote players are stepping, not moving. That is a problem with the
interpolation buffer rather than with shooting, it is now logged in as many words, and lag
compensation degrades to the delay rung instead of breaking. `interp_ratio` is the knob; 1.7 send
intervals is not enough once the network delay dominates.

Measured with a single shot, at 300 ms of simulated ping. The shooter aims at a standing target;
the target starts running perpendicular; a quarter of a second later the shooter fires, still aimed
at the old spot:

| | shot | server log |
|---|---|---|
| `lag_compensation = true` | **hit**, target 0.77 m past the aim point | rewound 20 ticks (313 ms) |
| `lag_compensation = false` | **miss** | — |

At 100 ms of ping the same rewind is 13 ticks, 203 ms.

What it costs is what lag compensation always costs: **you can be shot after stepping behind a
wall**, because on the shooter's screen you had not stepped behind it yet. Every shooter makes this
trade, and the alternative — needing to lead every target — is worse.

Both halves are settings, in separate processes, so either can be on while the other is off and the
result looks like nothing happening. Both cases name themselves on the first shot:

```
first shot reports view ticks 544..546 at 0.86                        (client)
lag compensation live: first shot rewound 13 ticks (203ms), ticks 546..548 at 0.26
first shot reports no view bracket: interpolation is at tick 544 but the newest confirmed
  sample is 540 — it is clamping, not blending. The shot falls back to the coarser rewind.
lag compensation is on, but peer 178811… reports no view delay: its shots resolve against the
  present. Is lag_compensation off on that client?
lag_comp_history_ticks is too short: peer 178811… asked to rewind to tick 1241, oldest kept is 1257
```

The last one falls back to the present for that shot rather than testing against a position nobody
was ever in. A history shorter than the buffer is *not* warned about: that is a player who joined
moments ago, which is normal and passes.

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

**Visual correction**, and how its size was settled. Nothing used to be smoothed at all — neither
between fixed ticks nor after a rollback. `place_camera` wrote the predicted position raw, so the
eye moved at 64 Hz while the view turned at frame rate, and a rollback landed on the next frame as a
jump.

How big a jump is measured rather than guessed, by `client/src/corrections.rs`: two systems inside
lightyear's rollback, one either side of the replay, plus a per-frame watch on the camera itself.
The per-frame figure subtracts `speed x frame time`, because at 5.5 m/s and 30 fps ordinary walking
is 18 cm in a frame and a 15 cm jump would hide inside it.

| link | rollbacks | camera jump in one frame |
| --- | --- | --- |
| 100 ms, 10 % loss, walking | 4 per minute | 0.5 cm |
| 300 ms, 30 % loss, walking | none in 70 s | — |
| 300 ms, 30 % loss, **no input redundancy** | 18 per second | 85 cm |
| 300 ms, 10 % loss, shooting a crate stack | 9 per second | 0.0 cm, but the crate snapped up to 79 cm |

The result is not what the frequency suggested. **The camera needs almost no smoothing yet, and the
reason is not that the link is good.** The only thing driving the local player is the local player's
own input, and `input_redundancy = 5` means six consecutive packets must drop before the server
misses one — so the server simulates from exactly the input the client predicted from, and the two
agree to within half a centimetre. Setting redundancy to 0 shows what a genuinely missed input
costs, and that is the amplitude to expect the moment the *simulation* gains a way to diverge —
another player pushing you, a vehicle under your feet — rather than the moment the network gets
worse.

Two things are now switched on.

**Frame interpolation** (lightyear's `FrameInterpolationPlugin`, plus `FrameInterpolate` on the
predicted player) draws the player blended between the last two ticks rather than on them. It costs
one tick of delay by design — 8.6 cm at running speed, and that figure shows up in the measurements
exactly where it should.

**View smoothing**, ours rather than lightyear's. `add_linear_correction` does the same job and was
tried first; its `CorrectionPolicy` has private fields and no constructor besides the default, and
the decay constant is the entire design. Measured with lightyear's 200 ms half-life under a storm of
corrections, the view trailed the simulation by **1.9 m**: the filter never released, so the camera
simply ran a fifth of a second behind everything. Thirty lines in `local_player.rs` own that number
instead — a 110 ms time constant, and a hard leash at 25 cm.

The leash is the whole trade in one number, and it does not go away by tuning. Smoothing a
correction means drawing the player where they are not, so **the size of error that can be hidden is
exactly how far behind the view is allowed to get**. A view a metre back puts the crosshair
somewhere the player is not, which is worse than the jump it was hiding. So errors up to 25 cm — 45
ms of running — are smoothed away, and anything larger shows, on purpose.

Measured after, under the same storm: the trail is bounded at 21 cm where it had been 1.9 m, and
what still jumps is the part above the leash. On a sane link the errors are half a centimetre, so
everything is smoothed, the jump is 0.0 cm and the trail is 0.0 cm.

The figure is on screen while playing, bottom left, and not only in a log: how far the drawn player
was from the simulated one, worst frame of the last second, amber past half the leash and red on it.
A number that exists only in a log after the fact cannot be compared with "that felt wrong just
then". F3 hides it.

Two figures side by side, neither claimed to be part of the other — which was the mistake the first
version made. It read "X behind, Y of that smoothing" and reported a share *larger* than the whole,
because the smoothing offset and frame interpolation's one tick of delay point in different
directions the moment the player turns, and vectors at an angle do not add like numbers.

They answer different questions, and only one of them is a fault. **Behind** is the whole distance
between the drawn player and the simulated one, and on a link with no corrections at all it is not
zero — which is the thing that surprises people. It is one tick of movement, what frame
interpolation costs by design. Measured on a perfect link: walking at 5.50 m/s gives 8.5 cm against
a tick's 8.59, crouching at 2.60 m/s gives 3.9 against 4.06, and standing still gives 0.00. It
scales with speed and vanishes when you stop, because it is a delay rather than an error — and it is
0.00 even while walking into a wall at full velocity, which is how the first attempt at this
measurement managed to report nothing at all. **Correction** is the part left over from a rollback,
and it stays at zero until the client guesses wrong.

None of this touches the simulation. Both the frame blend and the smoothing write into the live
`PlayerState` in `PostUpdate`, and `RunFixedMainLoop` restores the simulated value before the next
tick — so a shot still leaves from where the simulation says the player is, because the shooting
code runs in `Update`, after that restore and before either of them.



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
- **Per-bone hitboxes** — the hitbox is the movement capsule, so a head shot and a shin shot are
  the same shot. Waiting on the real models in M2.
