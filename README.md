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

### After a crash, a green test run can be a lie

Worse than the link errors below, because it looks like success. A hard power loss can leave the
machine's clock adrift, and it came back an hour behind after one of these. Every build artifact
from before the reboot then carries a timestamp **in the future**, cargo compares them against
freshly edited sources and concludes there is nothing to do — so `cargo test` reruns the previous
binary and reports it passing. New tests do not appear; changed tests still pass under their old
bodies. That happened here, and the run said `19 passed` for a file whose twelve new tests had
never been compiled.

`touch` does not fix it: it sets a source to *now*, which is still older than an artifact from the
future. Nor does the count of tests give it away unless somebody happens to know what it should be.

```bash
find target -type f -newermt "@$(date +%s)" | wc -l    # artifacts dated in the future
cargo clean -p noob_tube_client -p noob_tube_shared -p noob_tube_server -p bake_collider
```

Cleaning by package is enough. A dependency that has not changed is fine however it is dated; the
damage is only to crates whose sources are being edited. It costs a few minutes rather than the
twenty a full rebuild of the Bevy tree takes.

### After a crash, the build stops linking

Twice now a hard power loss has left the workspace unable to link, with pages of

```text
rust-lld: error: undefined hidden symbol: anon.52766...llvm.30694...
```

That is not a code error and no amount of reading the diff will find it. Incremental compilation
keeps its codegen units in `target/debug/incremental`, a crash truncates some of them to zero
bytes, and the linker then looks for symbols in a file that no longer contains anything. The fix is
to throw that cache away — everything else in `target/` survives, so this costs seconds rather than
the twenty minutes a full rebuild of the Bevy tree takes:

```bash
rm -rf target/debug/incremental && cargo build
```

Worth checking the same way whenever something is inexplicable after a crash:

```bash
find target -type f -size 0 | sed 's|.*/||' | sort | uniq -c | sort -rn | head
```

Empty `.lock`, `stderr` and `output` files are normal. Empty `.rmeta`, `.rlib` or `.o` files are
not, and `cargo clean -p <crate>` clears those for one crate without touching its dependencies.

**`.git` is worth checking at the same time**, because it has been damaged by both of these
crashes in the same way — a zero-length object where the last commit should be, and every git
command answering `fatal: bad object HEAD`. `git fsck --full` says so, the empty objects are
findable with the same `find`, and they have been orphans both times: resetting the branch to the
last intact commit, deleting them, and re-committing the working tree has lost nothing.

### Which graphics card draws it

On a laptop with two GPUs this now draws on the **integrated** one, and that is a change from
Bevy's default rather than from the system's.

The system was never the problem. `switcherooctl list` on the machine this was written on reports
the Intel UHD as `Default: yes` and the GeForce as `Default: no` — GNOME and the kernel had already
picked the integrated card. What overrode them was wgpu, which asks for `HighPerformance` by
default, and on a hybrid laptop that means the discrete one. So the fix is not to tell the system
something it already knew; it is to stop this game contradicting it.

Why bother: nothing here needs a discrete GPU — a few thousand triangles, no post-processing — and
the discrete driver on that machine is Mesa's **NVK**, which took the whole machine down twice in
an afternoon of running two clients side by side. A card that is not worth using is not worth
crashing for.

```bash
WGPU_POWER_PREF=high   ./target/debug/noob_tube_client   # back to the discrete card
```

Setting that variable at all hands the choice back to wgpu, because the code only fills in a
preference nobody expressed. On a machine with a solid discrete driver it is the right thing to
reach for.

If the trouble is the driver being *loaded* rather than used, two heavier hammers, both environment
rather than code because they are about a machine rather than about this game:

```bash
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/intel_icd.json   # the only Vulkan driver in sight
WGPU_BACKEND=gl                                          # skip Vulkan altogether
```

And the heaviest, which is a decision about the whole machine and not about this repository:
blacklisting the `nouveau` kernel module removes the card from everything, and needs root and a
reboot.

Which adapter was actually chosen is printed at startup — `AdapterInfo { name: ... }` — and that
line is the only thing worth believing, because every layer above it can be overridden by a layer
below.

### Looking at an asset

A `.glb` is a JSON document with a blob of numbers stapled to it, and the JSON says almost
everything a decision about the asset depends on: who made it and under what licence, how many
triangles it costs, what the skeleton is called, which animations are in there and how long they
run. None of that needs a renderer. `tools/glb` is a dependency-free reader for it, in the same
spirit as `tools/brp`:

```bash
tools/glb info a.glb b.glb        # licence, geometry, skins, clips — several files to compare
tools/glb nodes <file>            # the node tree and mesh sizes
tools/glb skeleton <file>         # the joint tree, in bind pose
tools/glb animations <file>       # per clip: what moves, and whether it loops
tools/glb rigs <file>...          # do these skeletons match — whose clips drive whose model
tools/glb strip <file>...         # throw the character back out of a clip (rewrites in place)
```

Both `.glb` and `.gltf` are read; a `.gltf` finds its `.bin` beside it.

`info` takes several files on purpose. A Sketchfab download usually offers two quality levels, and
the useful question is whether they differ in geometry or only in texture size — if the geometry
lines match, keep the small one. Both the Warthog and the mounted gun turned out that way.

The author and licence come out of `asset.extras`, which is what Sketchfab writes on export. That is
where every entry in `assets/CREDITS.md` came from: it can be read back out of the file at any time
rather than trusted to a browser tab.

`rigs` answers the question a mixed pile of downloads keeps asking, and the one the [Risks](#risks)
section calls the significant one. Bevy binds an animation to a skeleton by the *path* of bone
names, so a clip drives a model only if that model has a bone at the same path — names alone are
not enough, since two rigs can use the same names in a different hierarchy. `rigs` compares full
paths and says plainly whether clips interchange, drive a model in part, or need retargeting.

`strip` is the one command that writes, and it exists because a downloaded clip is usually not
only a clip. Mixamo hands out "Driving" with the whole soldier attached — mesh, four textures — and
Bevy's glTF loader builds *every* sub-asset of a file it opens, so asking for `Animation(0)` still
decodes those textures and uploads them. The clip is worth 130 KB and the file cost 4.6 MB and 13 MB
of graphics memory. `strip` drops the meshes, skins, materials and images and keeps every node, so
the bone paths — and therefore `rigs` — come out unchanged. `tools/setup-assets` runs it over every
clip it converts, and the whole `assets/anims` directory went from 12 MB to 3 MB the first time.

What `tools/glb` cannot do is show you the model. For that:

- **[gltf-viewer.donmccurdy.com](https://gltf-viewer.donmccurdy.com/)** — drag the file in. Nothing
  to install, it lists the animation clips and plays them. The quickest way to answer "what does
  this clip actually look like".
- **Blender** — `File ▸ Import ▸ glTF 2.0`. The Dope Sheet's *Action Editor* lists every clip and
  the Outliner shows the armature. This is also the only one of the three that can *change*
  anything: retargeting, splitting one long take into clips, deleting a skeleton's unused half.
- **The game itself**, once a model is wired up. Slowest to reach, and the only one that answers
  whether it looks right at the size and framing it will actually be seen at.

### Baking a collision shape

`tools/bake_collider` turns a vehicle's `.glb` into the convex hulls the game collides and shoots
with, so that the shape travels as numbers and neither the server nor the repository needs the
model. It is an offline step; nothing at play time runs it.

```bash
cargo run -p bake_collider -- list  assets/models/warthog.glb   # what is in the file
cargo run -p bake_collider -- check assets/models/warthog.glb   # score candidate shapes
cargo run -p bake_collider -- bake  assets/models/warthog.glb shared/src/vehicle_shape.rs
```

`shared/src/vehicle_shape.rs` is generated and committed — do not edit it by hand. Run `check` in
release; a debug build of the decomposition is a coffee rather than a keystroke. See
[Bullet holes hanging beside the bodywork](#bullet-holes-hanging-beside-the-bodywork) for what the
scores mean and why the chosen settings are the chosen settings.

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

A player is a person now rather than a capsule with a box for a head — the **Mixamo soldier**, the
same one `webgame` used, with its Rifle 8-Way Locomotion Pack. The plan this replaces was built
around exactly that and then went round the houses; both the detour and why it ended here are under
[Character assets](#character-assets-settled).

**Two kits, and the difference is licensing rather than taste.** The soldier may be used and not
redistributed, so it is not in this repository and `tools/setup-assets` is what brings it in. A
checkout without it falls back to the Quaternius character, which is CC0 and committed. Nothing may
*require* an asset a fresh checkout does not have — the same rule the vehicle model follows — and
the client says at startup which one it found. What being without the soldier costs is stated
plainly: the fallback can only walk forward.

```bash
tools/setup-assets                 # defaults to ../webgame/assets
tools/glb rigs assets/characters/swat.glb assets/anims/idle.glb
```

**The model and the clips come from different files**, which works only because they share a
skeleton exactly. Bevy binds an animation to a bone by the *path* of names from the scene root
down, so `RootNode/mixamorig:Hips/mixamorig:Spine` has to be spelt the same in both. Measured, not
hoped for: all 70 bone paths shared, the only three unmatched being the character's own mesh nodes,
which no clip has a curve for. That was this milestone's named risk, and it is now a one-second
check.

**The model is scaled to the hitbox, not to taste.** The collision capsule *is* the hitbox and the
soldier is 1.78 m in its own file against a 1.70 m capsule. Drawn at its own size, 8 cm of head
would be visible, aimable and unhittable — the mistake the placeholder head box made once already.
The scale is derived rather than typed, with a test on each end, for both kits.

What no uniform scale fixes is **width**. A character with an arm out reaches past a 35 cm capsule.
That is the honest cost of a real model over a capsule, and it makes the per-bone hitboxes under
[Still to settle](#still-to-settle) a real gap rather than a theoretical one.

#### The plumbing the file did not come with

The figure appeared, correctly posed, and did not move — and the reason is worth writing down
because nothing about it is visible from the outside. Bevy's glTF loader builds its list of
animation roots **while walking a file's animations**. A character model has none: it is a mesh and
a skeleton. So it comes out with no `AnimationPlayer` on it and no `AnimationTargetId` on any bone.
Nothing is missing from the file and nothing is wrong with the loader; the plumbing simply had
nothing to be built from.

It is laid by hand the same way the loader lays it, and **the root is found rather than assumed**.
The first attempt took the spawned scene's top entity, and every path came out
`Scene/Armature/root/...` against a library spelling `Armature/root/...` — not one bone of
seventy-three recognised. Bevy's wrapper is Bevy's business; what cannot change is that the right
root is the one whose paths the library knows. Every candidate is tried and the best wins, and the
count is reported:

```text
52 of 76 bones under 780v0 are animated by the library
```

Which turned a silent failure into a one-line diagnosis, and is the reason to prefer a check that
can fail loudly over a comment saying it should work.

#### Choosing a clip

From `PlayerState` and `Aim`, not from watching the transform move. The simulation already knows
whether a player is on the ground and whether they are crouching, and differencing positions would
turn interpolation's smoothing into a flicker in the choice of clip. Horizontal speed only: a
player dropping off a ledge at 12 m/s should not have their legs sprint.

**Eight directions.** The travel is taken into the player's own frame first, because a body strafes
relative to its own head rather than relative to the world, and then rounded to the nearest
eighth-turn. A player strafing right while looking at you is the commonest silhouette in a shooter
and the one the fallback kit cannot draw at all.

**Foot lock** is playing the clip at `actual speed / the speed its stride was authored for`. Those
speeds are measured rather than guessed — `tools/glb animations` prints how far the hips travel
over a clip:

| | soldier | fallback |
|---|---|---|
| walk | 1.84 m/s | 0.97 m/s |
| run | **4.61 m/s** | 5.36 m/s |
| crouch-walk | **1.96 m/s** | 0.75 m/s |

The two bold figures are `webgame`'s 4.606 and 1.956 to three significant figures, measured
independently here from the converted GLBs. At full speed the soldier's run is stretched 1.19×,
which is `webgame`'s number as well.

**The crossover between walk and run is derived, not chosen**: the geometric mean of the two
authored speeds, which is what keeps the ratio either is stretched by as small as it can be — at
it, both are stretched alike. 2.91 m/s for the soldier, 2.28 for the fallback, and a test holds
both to the property rather than to the number.

The difference between the kits shows up exactly there. The soldier is never stretched more than
1.3× at any speed this game produces. The fallback's crouch is authored for 0.75 m/s against a
crouch speed of 2.6, runs into the 2× clamp, and skates. Both facts are tests, so replacing either
pack says which side of the line it lands on.

#### What is not done

- **The head does not follow the aim.** The placeholder had a head box on a stick and pitching it
  was a quaternion; the head is a bone inside an animated skeleton now, its local axes are the
  rig's rather than the world's, and the animation rewrites its rotation every frame. Doing it
  right means measuring the bone's rest orientation and post-multiplying after the animation
  systems — worth doing, not worth guessing at.
- **The aiming idles are unused.** `idle_aiming` and `idle_crouching_aiming` are in the pack, and
  nothing yet knows whether a player has their weapon up.
- **Deaths and turns are unused.** Six death clips and four turn-in-place clips are sitting there.
- **A seated driver has no clip for sitting.** The soldier's pack has 49 of them and not one is a
  person sitting down, so a crouched idle stands in — knees bent, hands forward, which reads far
  better behind a steering wheel than a figure standing to attention. Mixamo has "Driving" and
  "Sitting Idle" for the asking; dropping either into `assets/anims/` is all it needs. The
  fallback kit has `Driving_Loop` and uses it.
- **On foot our own body is still not drawn**, because the camera is inside it. First-person arms
  and a third-person view on foot are both waiting on that. In a seat it *is* drawn, because the
  chase camera has the driver's seat in shot.
- **Per-bone hitboxes**, as above. The pack's own skeleton is what `webgame` generated
  `skeleton.json` from, so the data exists.

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
first-person view from inside a windowless box is a black screen. Where it goes from there is
[the view from the driver's seat](#the-view-from-the-drivers-seat), below.

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

#### The view from the driver's seat

The camera behind a vehicle answers to two things now.

**The wheel winds it in and out**, between three metres and twenty, seven tenths of a metre a notch.
Only while driving: on foot the view is from the eyes, and there is nothing to wind. It reads
`AccumulatedMouseScroll` rather than the raw events, which is also where the one wrinkle is — a
wheel and a touchpad arrive as the same event in different units, and treating a touchpad's pixels
as notches would wind the camera to its far end in a single flick.

What the zoom cost was a constant. The camera used to ride a fixed 1.2 m above the driver's eye,
which is a tenth of the picture at twenty metres and a quarter of it at three; wound all the way in,
the vehicle slid off the bottom of the screen. It is a **ratio** now — 0.17 of however far back the
camera is — so winding the wheel changes how far away the vehicle is and nothing else. At the
default seven metres that is 1.19 m against the old 1.2, so the view nobody asked to change did not.

**The right mouse button takes the driver's weapon out, and puts it away again**, and the camera
follows from that. It was `V` at first, and the button is better for a reason worth stating: this is
the switch between two ways of driving, gunner or passenger looking around, and it is used while
steering — a hand already on the mouse should not have to leave it. The wheel is the zoom and stays
that.

It only listens while the cursor is grabbed, which the key deliberately did not. The argument for
not gating the key was that a mode which could not be left after pressing Escape would be a trap,
and that does not survive the move: the click that takes the grab back is right there. Ungrabbed,
the pointer belongs to the inspector and to whatever is behind the window.

The switch is over the weapon rather than over the camera because the two cannot both be had. A
camera that pulls itself back behind the vehicle is pulling the crosshair with it — the yaw is one
number, and it is both where you are looking and where you are aiming. So: weapon stowed, the camera
is steered for you and the trigger does nothing; weapon out, you aim, and the camera is yours to
hold.

Halo could have this both ways, and it is worth being clear about why. Its Warthog driver has no
weapon at all — the chain gun is a second seat and a second player's view — so there was never a
crosshair for its camera to drag. We do not have passengers, so we have the conflict, and this is
the cheap way out of it: never have the camera and the aim disagree about who owns them.

**Stowed, the camera is sprung back behind the vehicle, harder the faster it is going.** An
exponential approach — `1 - exp(-rate * dt)`, so it behaves the same at 30 fps and at 300 — with the
rate scaled by speed up to 6 m/s and no further. That scaling is what leaves a parked vehicle alone:
standing still there is nothing behind to be pulled towards, and a driver looking around their own
car should be able to. Measured, from a standing start:

| speed | off the vehicle's nose |
|---|---|
| parked, two seconds | pushed 147.3 degrees aside and **still 147.3** |
| 2.5 m/s | 89.1 |
| 5.3 m/s | 26.2 |
| 7.6 m/s | 4.6 |
| 9.7 m/s | 0.8 |
| 13.2 m/s and up | **0.0** |

It is a spring and not a lock, so the mouse still wins while it is pulling: a 500 px sweep at speed
threw the view 29.7 degrees aside, and it slid back through 5.6, 1.1 and 0.2 to nothing over a
second and a half. With the weapon drawn there is no spring at all — pushed 80 degrees aside at
16.6 m/s, the camera had not moved a hundredth of a degree a second and a half later.

Yaw only, and not because it is easier. A camera given the vehicle's whole attitude would put the
horizon on its side every time the buggy leaned into a corner and would stare at the sky for the
length of a jump. Mid-barrel-roll there is no heading to take at all — the nose points straight up —
and there the camera keeps the one it had, which is the only answer that does not spin.

Not firing is its own system rather than a term inside the input sampling, because it is its own
rule: what the player asked for is one thing, and what a stowed weapon is capable of is another. It
clears the view bracket with the shot, since a bracket is the evidence for a shot and one that is
not taken has nothing to prove. **None of it needs the server.** Whether my weapon is out changes
nothing for anybody else, the driver is not drawn while driving, and "not firing" is exactly what
the server already sees when a trigger is not pulled — there is no new state on the wire and nothing
to arbitrate.

**With the weapon out the camera drops onto the shot's own line.** Any height above the muzzle is
parallax: the camera looks along the same direction the bullet travels, but from `rise x chase`
above it, so the two are *parallel lines* and the shot lands that far below the crosshair — at every
distance, which is what makes it read as "always a bit low" rather than as a ranging error. Measured
at the default zoom: **119.0 cm** off the line with the weapon stowed.

The usual fix is to cast a ray from the camera, find what the crosshair is actually over, and aim
the muzzle at that point — what every third-person shooter does, and a real piece of machinery,
since the ray has to be cast against the same targets the shot will meet or it converges on the wall
behind the person you were aiming at. Setting the rise to zero costs nothing and is exact at *every*
distance rather than at one: the camera then sits at `eye - direction x chase`, which is a point on
the shot's own ray, so the crosshair marks where the bullet goes by construction. Measured, with the
weapon drawn: **0.0 cm** off the line, level, 20 degrees up and 20 degrees down alike.

It is affordable only because the driver's head is above the bodywork — the eye rides 1.19 m over
the chassis centre and the model's highest point is 0.63 m, so the sight line clears the whole
vehicle by half a metre. Checked at both ends of the zoom: the crosshair is clear of the vehicle
level, 20 up and 20 down, at seven metres and at three. Past about 20 degrees up the roll bar comes
into the line behind the eye, and past about 27 down the bonnet does, and those are the limits.

That also gives the two modes two cameras with two jobs: stowed it rides high and is steered for
you, drawn it drops to the sight line and is yours.

**Your own vehicle is not a target.** A driver fires from the seat, so their own bodywork is at zero
distance in every direction: without this, every shot from a vehicle would stop against the inside
of its own panels, and — since a hit shoves what it lands on — shove the vehicle it was fired from.
The shooter was already excluded from their own shot for exactly this reason, one body further in.

It is written as a rule rather than as a special case for the geometry. "The vehicle you are sitting
in cannot be hit by you" is something a player can rely on and a mapmaker can reason about; "the ray
happens to start inside it" stops being true the moment anyone leans out of a window. `Driven`
travels with the target rather than being looked up separately, because it is a fact about the
target and is only ever asked while deciding whether to consider one.

Both sides do it, and they have to. The client resolves its own tracer locally so the line appears
on the trigger rather than half a round trip later; if the two disagreed about what is not a target,
the tracer would stop against a bonnet the server shot straight through.

The crosshair goes with the weapon. A crosshair over a trigger that does nothing is a small lie told
sixty-four times a second, at the moment somebody is deciding whether to shoot; it is also the only
thing on the screen that says which mode is on. `Visibility` rather than despawning, so the hit
markers hanging off it survive the switch. On foot none of this applies — the flag means nothing
there, and the crosshair and the trigger behave as they always did.

Still missing: the chase camera cannot get out of the way of walls, and the wheel has made that
easier to arrange. Passengers would dissolve the whole trade — a gunner's seat aims wherever it
likes while the driver's camera steers itself, which is what Halo actually does.

#### Getting back on its wheels

A vehicle on its side is not a hard problem to drive out of, it is an impossible one. The entire
model acts through the wheels, and the wheels find no ground; the only thing still touching the
world is a box that slides. Left alone, the round has one fewer vehicle in it from the first badly
taken ramp onwards.

So a driver past 78 degrees of lean who **holds the trigger for a second** is stood back up. It
used to happen on its own, on a timer, and that was fine while the ground was a plane: past 78
degrees the vehicle was on its side, and there was no way back from it. Terrain ended that. The
walls of a ravine are 66 degrees, and a vehicle working its way along one is past the threshold for
seconds at a time while its driver is doing something quite deliberate about it — at which point a
hand reaches in and stands the car up. Nobody wanted that hand.

A driver on their roof knows they are on their roof, so they can say so. It is the left button
because that is the one already under the finger and there is nothing else to do with it there: the
gun cannot bear at that attitude, so the trigger is not a trigger. It travels as its own input
field for exactly that reason — what a driver upside down is asking for is not a shot, and the two
are suppressed by different rules. The hold is a second, and letting go starts it again, so a
trigger pulled in a panic is not a request. Nobody in the seat means nobody asking, which is the
one thing this gives up: an abandoned vehicle stays where it fell until somebody walks over and
gets in.

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
wheels 1.1 seconds after the hold completes and settled at its ride height a second after that.

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

#### The buggy that fell out of the sky

Getting into a vehicle launched it. Measured on a client that had just started: the chassis left
the ground at the moment the driver got in, reached **5.9 m**, and fell back over two seconds.

Two things made it hard to see. It happened only on a client's **first** handover of that vehicle —
enter, get out, enter again, and the second time was clean — and its size looked as though it
depended on where the player was standing, which sent the first hour of the hunt after the player's
capsule. It did not: a fresh client, standing 1.9 m away on flat ground, produced the biggest jump
of all.

`predict_vehicles = "off"` settled where to look. The vehicle never moves; the server's copy stays
at its ride height throughout. So it was the client's own simulation, and specifically the moment
the chassis stops being `RigidBody::Static` and starts being `Dynamic`.

**A body with no `ColliderDensity` is given the default of 1.** For this hull that is about two and
a half kilograms against twelve hundred. The client was inserting the density in the *same frame*
as `RigidBody::Dynamic`, so for one tick the suspension pushed with forces sized for a 1200 kg
vehicle against a body that weighed as much as a cat. A static body does not care what it weighs,
which is why nothing showed until the handover; and after the first one the component stayed
behind, which is why the second was correct.

The fix is one line moved. Density and centre of mass are facts about what a buggy *is*, not about
who is predicting it, so they belong where the chassis is built. The server had them there all
along, which is why it never jumped. After: **1.09 m peak** against 1.066 at rest — 2.7 cm, which is
the suspension settling.

`a_vehicle_without_its_own_density_weighs_nothing_like_enough` keeps the reason written down: it
asserts the default density is off by more than a hundredfold, so that a future spec whose density
happened to be near 1 would say out loud that the ordering had stopped mattering.

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

#### A second player, on demand

```
NOOB_TUBE_BOT=1 NOOB_TUBE_HEADLESS=1 cargo run -p noob_tube_client
```

A bot walks to the nearest free vehicle, gets in, and drives a fixed beat back and forth. It exists
because half the questions here need **two** clients and one keyboard cannot answer them — two
predicted vehicles meeting each other has no measurement anywhere above, and every attempt to stand
a second player in measured something adjacent instead.

It drives through `ScriptedInput`, the same door the test harness uses, so it goes through
prediction, rollback, input redundancy and the link conditioner exactly as a person does. It is not
an AI and is not trying to be: it is a fixed errand in a loop. What it must not do is write the yaw
into `PlayerInput` directly, because `sample_input` overwrites that from `LocalPlayer` whether the
input is scripted or not — the look angles being the one thing a client is authoritative over. So
the bot turns by setting the field a mouse would.

Three things it does that a straight line would not, each of them a measurement rather than
foresight:

It **picks its beat by sensing**, casting a ray along each of the four directions the vehicle could
set off in and taking the longest clear one. Driving along whatever heading the vehicle was parked
on pointed the first buggy straight at the ramp; it wedged itself under the lip with its suspension
fully compressed and sat there for forty seconds. Nothing tells it where the ramp is.

It **drives forwards both ways**, U-turning at each end rather than reversing back down its own line.
Reversing needs the steering sign inverted, and getting that wrong does not look like a wrong sign —
it looks like a bot calmly driving 500 m off the edge of the level, reported as a vehicle doing
several hundred metres a second because by then it was falling. Reverse is now used in exactly one
place: full lock, for two and a half seconds, to free itself when it has been going nowhere while
asking to move.

And it is **on a leash**: 120 m from where it first sat down, past which the only target is home.
That is there because the patrol has run away twice, the second time by re-anchoring its beat after
every escape until the anchor itself had walked a kilometre. An anchor that moves whenever the bot
gets stuck is not an anchor. Whatever else turns out to be wrong with it, that cannot be.

Measured over 75 seconds: it stays inside about 90 by 130 metres, reaches 21 m/s, and is standing
still in 2 samples out of 50.

#### What it looks like

A Warthog, by pinto36, CC BY 4.0 — the credit and the source link are in `assets/CREDITS.md`, and
none of it was typed off a web page: the glTF carries it in `asset.extras`, which is what Sketchfab
writes on export.

Three numbers are read out of the file rather than guessed, because it is a Sketchfab export
normalised into a 2 x 0.893 x 0.994 box: how long it is along its own X, how far its origin sits
above where its tyres touch, and which way it faces. Its windscreen and steering wheel are at −X and
its antenna at +X, so it wants a quarter turn to point along −Z like everything else here. It is
then scaled by its length, so the model and the shape a shot is tested against agree along the axis
a driver notices most.

**Nothing requires it.** `/assets/` is ignored file by file and this one is let through because CC
BY allows it; the mounted gun beside it is not, because CC BY-**ND** does not. A client with no
model on disk gets the box it
always had, with its four cylinders visible — checked, because that is the state every other
developer's first checkout is in. The choice is made from the filesystem rather than from the asset
server, which would answer asynchronously, some frames after the vehicle already needed a body.

The model brings its own wheels, so ours are spawned and placed as before but hidden.

#### A gun on the cross-beam

A mounted machine gun by bonk.iopro77, bolted to the rear hoop of the roll cage — the cross-beam
that stands behind the seat backs.

Two things had to be found rather than eyeballed, and both came out of the geometry. **Where the
beam is**: the roll cage is the only part of the body above y = 0.28 in the model's own units, and
it is two full-width hoops joined by a pair of thin rails, so the rear hoop is the run of full-width
geometry at x = 0.146 to 0.307 — the seat backs end at 0.24, which puts it right behind them. Its top
face is at y = 0.360. **Where the gun's foot is**: the bottom two units of the gun are a single
2.1-wide post, and the centre of its underside is the one point that has to land on that face. The
gun is then turned about that point rather than about its own origin, which is somewhere in the
middle of its receiver.

Its own scene is not normalised the way a Sketchfab export usually is — it is 63.7 units long and
carries a chain of node transforms that compose to a plain scale — so the length is measured and
everything else is a ratio against it. Scaled to 1.2 m, about a third of the vehicle.

It hangs off the *vehicle model* rather than off the chassis, and is placed in the model's own
units. Where the beam is depends on the model and on nothing else, so the two stay glued together:
the vehicle could be respecified tomorrow and the gun would still be on its beam. It also means the
gun is absent exactly when the beam is — on a client with no vehicle model there is nothing to bolt
it to, and the box gets no gun.

**It turns with the driver, and the tracer comes out of its barrel.** Where a shot goes has not
changed — it is still cast from the seat, by the driver, with the same angles as ever — but the
picture now agrees with it. The gun's barrel is laid along the aim ray and the tracer starts at the
end of that barrel instead of beside the driver's head.

Both turns are worked out in the *vehicle model's* own frame rather than the world's, which is what
makes the gun follow a car that is cornering, leaning on its springs or parked on a slope: the whole
chain from the chassis down is already in the transform, so subtracting it once leaves two angles
that mean the same thing at any attitude.

Both of them are about the foot of the post — the point measured onto the beam — so the gun stays
bolted to it however it is aimed. A real pintle elevates about a trunnion higher up instead, and
that is not available here: the post and the gun are a single mesh in the file, and turning about
the trunnion lifts the foot 9 cm out of its socket at full elevation. Turning about the socket is
the other kind of mount, and the only one this geometry can be.

The traverse is not clamped — a pintle behind the seats can be swung all the way round, which is
what it is for — and the two ends of the elevation are clamped by two different things.

**Up** is the stock: 23.3° is where its bottom corner swings down onto the plane of the beam.
**Down** is the roll cage in front of the gun. It stands on the rear hoop and fires forward over a
single centre rail and then over the front hoop, clearing that by 27 cm at rest; swept against the
model's own silhouette on the barrel's centreline, the barrel reaches it at **18.4°**.

That second number is the one place here where the measurement is not the answer, and the limit is
set past it on purpose, at **30°**:

| depression | nearest ground | barrel inside the hoop |
|---|---|---|
| 18.4° | 5.1 m | — |
| 23° | 4.0 m | 7 cm |
| **30°** | **2.9 m** | **13 cm** |
| 35° | 2.4 m | 21 cm |

The muzzle stands 1.70 m up, so the reach is `1.70 / tan φ`. Stopping where the geometry says leaves
a dead ring five metres wide around the vehicle that is felt on every pass; thirty degrees brings it
inside three, and pays for it with a barrel that visibly enters one tube of the cage when aimed
steeply forward and down. A clip seen occasionally against a hole in the weapon felt constantly. It
is written down here because it is a choice, and should not later read as a measurement.

The alternative was to raise the mount — 13 cm of extra post buys the same 30° with nothing
touching — and it was turned down because the post and the gun are a single mesh, so the foot would
float that far off the beam it is supposed to be bolted to.

**The trigger stops where the barrel stops.** Aiming below the arc does not fire — held fire pauses
and picks up again the moment the aim comes back up. Otherwise the shot would leave a gun that is
visibly pointing somewhere else, which is worse than no shot. Only the *depression* does this: a gun
that cannot be raised far enough is still pointing roughly where the driver is looking, and a weapon
that went dead for looking at the sky would be a rule nobody could guess. A test walks the aim down
until the barrel gives up and checks the trigger gave up at the same angle, in four directions,
because two numbers for one limit is exactly how a shot comes to leave a gun pointing elsewhere.

**The crosshair goes red there**, because a weapon that quietly stops firing gives the player
nothing to reason about: the barrel is the only other clue and it is behind the camera's subject
rather than in front of it. The answer is written once, where the trigger is decided, and read by
the crosshair — two spellings of one rule is how a red crosshair comes to appear over a shot that
fires anyway. Only the four white arms change; the black outlines stay, because a red arm needs
them more than a white one does.

The rule lives with the input, beside the one that empties the trigger when the weapon is stowed,
and it is a pure function of the chassis pose and the look angles rather than a question about the
gun *entity*. A client with no vehicle model has no gun to ask, and must not thereby be allowed to
shoot where nobody else can.

The first version of all this looked like it worked and did not, in the way that is hardest to see.
The gun held one direction in the *world* and corrected for the chassis turning underneath it —
smoothly, exactly, and to an aim that never changed. `step_players` skips a seated player, because
a driver's pose comes from the vehicle and two things must not write it; the line that copies the
look angles into the replicated `Aim` was inside that step. **So a player's aim froze the moment
they got in.** The gun followed it faithfully, and so did their head on every other screen.

Measuring it the obvious way confirmed the bug instead of finding it. The gun agreed with `Aim` to
0.04° with the chassis yawed 8.8° underneath — which says the two are consistent, not that either is
right. Turning your head is now its own system, `look_around`, filtered by nothing: getting into a
vehicle takes away your legs, not your head.

It still does not fire on its own, and it has no gunner. Making it a second seat that aims
independently is the thing that would dissolve the camera trade described below, and it is its own
piece of work.

**The licence is a third answer again, and the first one that is uncomfortable.** The Warthog is
CC BY, so it may travel with the repository. The animation library is licensed for use but not
redistribution, so it may not. This gun is CC BY-**ND** — NoDerivatives — which permits sharing the
work verbatim but forbids distributing adapted material, and whether putting an unmodified model
into a game counts as adapting it is genuinely unsettled. The file itself is untouched, which is the
strongest argument that it is not. Because the answer is uncertain rather than clearly yes, it is
not let through `.gitignore`; the reasoning is written down in `assets/CREDITS.md` so the decision
is not lost.

#### Giving the suspension back its travel

Hiding our cylinders took the suspension off the screen: the springs still worked, and nothing
showed it. The model's own wheels are moved instead — not ours redrawn with its tyre, because its
wheels sit 14 cm inboard and 12 cm closer together than our struts, and putting a tyre where the
strut is would stand it outside the arch.

That turns out to want nothing new. The model is drawn at the pose the artist modelled, and that
pose *is* the vehicle standing on its springs under its own weight, so a wheel does not need its
height — it needs the difference between its strut's compression and the compression it has parked.
That difference is zero at rest, which is why a parked vehicle looks exactly as it did before any of
this existed, and it is checked: on a parked vehicle every one of the twelve parts asks for a
movement of 0.0000, and the transform each of them ends up with matches the strut arithmetic to
1e-8.

Three parts per corner, and each does something different. The **tyre** rises and falls, steers if
it is a front one, and rolls. The **stub axle** goes with it and does not roll — it is what the
wheel turns on, and it is inside the hub. The **suspension arm** is bolted to the body at one end,
so it does not travel at all: it swings, by exactly the angle that keeps its far end on the wheel,
about the end the file says is bolted down.

Which strut a part belongs to is read out of the geometry, never off a list of node names. The
bounding boxes the glTF loader has already measured say where each part sits; turning that through
the model's yaw says which corner it is. A table of names against corners would be silently wrong
the first time somebody re-exported the model, and wrong in the way that is hardest to see — three
wheels right and one crossed over is invisible standing still and only shows when the vehicle leans.
There is a test for exactly that, against the four tyre positions as measured out of this file.

The tyre is trimmed to the radius the struts assume, which is 2 cm smaller than the model's own once
scaled; without it the tread would sit that far under the floor for as long as the vehicle is on the
ground. Rolling is measured from how far the chassis has actually moved rather than from its
velocity, because for a vehicle this client does not simulate those are two different numbers — an
interpolated one is *placed* each frame, and the distance between two placements is the only speed
it really has. A step longer than the vehicle is not driving but the jump from where an entity
spawned to where replication says it belongs, and is ignored; without that every vehicle spun its
wheels a dozen times on the spot in its first frame, which is measured — 30 radians for a 12 m jump.

What it does not yet do is turn the front arms with the steering, or spin a wheel that is locked
under braking rather than rolling.

Two things had to change around it. Bevy's asset root defaults to `assets/` beside the executable,
which for a cargo build is `target/debug/assets` — a directory `cargo clean` deletes; it is
anchored to the source tree instead. And `jpeg` is not in Bevy's default features, so the geometry
loaded and the textures did not, which shows up as one line in the log and an untextured model.

Still missing: standing on a vehicle rather than being inside it, passengers, a camera that gets out
of the way of walls, and running people over.

#### The wheel that dug itself in

Reversing on full lock buried the front outside wheel in the road after a second or two, and the
driving went to pieces with it. The suggestion was more solver iterations; the measurement says
otherwise, and the measurement is worth more than the argument.

The chain is this. The body rolls into the corner and reaches 25 degrees. The outer strut runs out
of travel and pegs at its 58 cm. The roll carries on, because at that point there is nothing left to
push with — the model has no more spring. The tyre ends 57 cm below the road and the chassis
collider 37 cm below it, a full metre lower than where it rests. A buried body drags: the vehicle
falls from 9 m/s to 2, pops out, accelerates, and digs in again 2.6 seconds later. That cycle is
what a driver feels as juddering.

**More solver iterations do nothing, and it is worth being clear why.** The wheels are ray casts —
one per wheel per tick, no iteration to raise. Only the chassis is a solver contact, and it is
buried as a *consequence* rather than as a cause. Measured at 6, 12, 24 and 48 substeps: 0.481,
0.527, 0.487 and 0.536 m of tyre under the road, which is noise. Stiffer contacts
(`contact_damping_ratio` 10 → 100) make it slightly worse.

What was missing is a part a real vehicle has: an **anti-roll bar**. A torsion bar across an axle,
which pushes up on whichever side is more compressed by the difference between the two. It is
20 000 N per metre of difference here, against a spring rate of 27 000, and it is one line inside
the strut's load.

The property that makes it the right part is that it is *zero when both sides are level*. It
stiffens the vehicle in roll and leaves it exactly as soft over a bump, so a parked vehicle sits at
the same ride height it always did and a landing is unchanged. That is what separates it from
simply raising the spring rate, and from the bump stop that was tried first — which worked on paper
at 8 to 48 times the spring rate, and left a vehicle on its side unable to get back up.

| | before | after |
|---|---|---|
| reverse full lock: tyre below the road | 57 cm | **0.5 cm** |
| chassis collider | 37 cm under | 29 cm clear |
| lean | 25° | 8.4° |
| the old hard corner at 19.3 m/s | 7.8 cm, 50 ticks on the stop | **0.3 cm, none** |

What it does not fix, and what is honest to write down: a hard *landing* still bottoms out, because
that is both sides at once and a bar has nothing to say about it. A 1 m drop is clean, 2 m puts
11 cm of tyre through the road for a moment, 3 m puts 22 cm, and past 4 m the chassis touches. That
wants suspension travel, not a bar.

One tempting fix was tried and thrown away: applying the strut's force at the ground the ray found
rather than under the wheel centre, so that a bottomed strut never pushes up from a point below the
surface. It is more defensible on paper and it measures as nothing at all — identical to three
decimal places on both the corner and the drop — so it is not in the code.

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

Which is exactly what made the next one so odd. **Shooting the ground beside the buggy left holes on
the ground that then drove off with the buggy.** The message says where a shot ended and not what it
ended *on*, so each client casts its own ray to find the surface and its normal — and that ray was
cast from the shooter's eye, along the whole flight. The server's shot ignores the vehicle its
shooter is sitting in; a ray does not, and it stopped against that vehicle's own bodywork. So the
hole went to the right place, took its normal from the wrong surface, and was hung on the buggy.

The ray is now cast over the last 15 cm before the endpoint instead of the whole flight, and the
answer is rejected unless it lands on that endpoint — which it does not when the probe itself begins
inside something. Asking about the neighbourhood of the point the server reported is the question
that was actually meant; nothing the shot passed through can answer it, and now nothing the shot
passed through is asked.

#### Bullet holes hanging beside the bodywork

A vehicle was a box to a bullet: 1.8 × 0.8 × 3.8 m, which is 38 cm wider than the Warthog's nose on
each side, 27 cm wider than its waist, and *shorter* than the body is tall — so shots at the nose
stopped out in the open air and shots at the roll cage sailed straight over.

Sixteen hand-measured slices along the length were the second answer, and better, and still not good
enough. The third answer is the one that should have been first: **use the mesh**. The model is run
through an approximate convex decomposition offline — parry's own VHACD, the same one Avian would
run — and the resulting hulls are written out as a table of vertices by `tools/bake_collider`.

The reason it was not the first answer was a real constraint read too broadly. The server loads no
assets at all — no renderer, no glTF loader — and at the time the model was gitignored as well; from
that the conclusion was that the shape had to be numbers, and then the numbers were measured by
hand. But "the shape must be numbers" does not imply "the numbers must come from a ruler". Baking
the mesh into a table is still numbers. It needs nothing on the server, it survives a model that may
not be redistributed, and replacing a model becomes one command rather than an afternoon.

##### What it measures

The tool can score any candidate shape against the model's own triangles, which is what makes the
choice a measurement rather than a preference:

```text
cargo run -p bake_collider -- check assets/models/warthog.glb
```

Twenty thousand rays that are known to hit the model are fired at the candidate, and what is
recorded is the distance from where the candidate stopped the ray to the **nearest point on the real
triangles**. That is what the eye judges — a decal sitting a hand's width off the paint. It is not
the distance along the ray, which was the first thing tried and is a much more flattering-or-damning
number: a shot grazing the wing can stop two metres early along its own line while sitting three
centimetres off the panel. Nobody sees along the ray.

| shape | parts | median | 90th | worst | > 5 cm out | through |
|---|---|---|---|---|---|---|
| the nominal box | 1 | 12.4 cm | 30.4 cm | 51.9 cm | 82 % | 0.8 % |
| sixteen slices | 16 | 5.8 cm | 18.1 cm | 38.4 cm | 54 % | 2.9 % |
| solid 128, 24 hulls | 22 | 1.7 cm | 6.8 cm | 23.0 cm | 17 % | 0 % |
| solid 256, 48 hulls | 27 | 1.2 cm | 5.3 cm | 23.1 cm | 11 % | 0 % |
| solid 256, 96, 0.005 | 40 | 0.9 cm | 4.2 cm | 14.7 cm | 6 % | 0 % |
| **solid 256, 96, 0.002** | **55** | **0.7 cm** | **3.5 cm** | **14.4 cm** | **4 %** | **0 %** |

Every row is at the 3.80 m vehicle it was measured on. The buggy is 4.90 m now, and the same
bake on it reads 0.9 cm, 4.4 cm, 18.6 cm and 7 % — the same shape scaled by 1.29, not a worse one.
| solid 256, 96, 0.001 | 69 | 0.6 cm | 3.1 cm | 14.4 cm | 3 % | 0 % |
| skin 256, 96, 0.002 | 89 | 0.9 cm | 4.7 cm | 18.0 cm | 9 % | 0 % |

The last column is the one the slices could not fix at all: 2.9 % of shots went through a vehicle
they visibly hit, because the slices had to leave the roll cage out. The bake does not, and that
column is now zero.

##### The three things the table settled

**Solid, not a shell.** The guess was that a surface-only decomposition would win, because a Warthog
is a vehicle you sit *in* and filling the cabin is exactly what the boxes got wrong. It loses at
every part count — each thin shell part still bridges the concavity behind the panel it follows, and
a grazing shot finds the gap between two of them. It is also hollow, which would let a player or a
crate end up inside the cabin: a new kind of stuck, for a fix that was only ever about decals.

**Concavity, not resolution.** At 256 voxels the model is already resolved finer than the error being
chased. What buys the last few centimetres is the willingness to split a part again — and past
`0.002` the part count runs away for tenths of a centimetre.

**Fifty-five parts.** Where the curve flattens — and, it turns out, comfortably inside the budget.
Collision work happens every substep and rollback replays those substeps, so the part count was the
one real risk; measured, eight vehicles piled into contact cost 0.29 ms a tick against a single
box's 0.14, which is two per cent of a 15.6 ms tick at 64 Hz. The measurement is kept as an ignored
test, `what_a_tick_costs_with_this_shape`, for when the number is next up for debate.

##### The two ends that have to agree

The bake places the model in chassis space by the same quarter turn, scale and lift the client
applies to the model a player looks at — and the two live in different crates, one of which cannot
see the other's constants. Drift between them would put every hole where the bodywork is not: the
same bug, with a subtler cause. So the generated file records the scale and lift it was baked with,
and a client test checks them against the numbers the visible model is actually drawn with. If it
ever fails, the fix is to re-bake, not to edit either number.

The mass comes off the volume the *collider* reports rather than one worked out by hand: Avian adds a
compound's parts up, a decomposition's parts touch and overlap a little at their seams, and asking
the shape what it weighs at unit density is the only sum guaranteed to be the sum Avian will do. A
test weighs the shape and holds it to the 1200 kg the spec claims.

#### A vehicle stepped out of stood still with its wheels turning

Getting out of a moving buggy left it beside you, stationary, wheels spinning — and you could not
get back in unless you walked to where it was really rolling. Pressing the key at the visible one
snapped it to the truth for a moment, then it froze again.

Lightyear puts `FrameInterpolate` on what it predicts and leaves it there when prediction ends. That
marker is not inert: frame interpolation writes a blend of the last two **fixed** ticks into the
live component every frame, so on an entity nothing steps any more it writes the same two dead
values for the rest of the round, on top of the replicated pose that has replaced them.

Measured, stepping out at 12.5 m/s and then only watching:

| | released, before | released, after |
|---|---|---|
| `Position` over 12 s | still, jittering 4 cm between two dead endpoints | rolls 60 m and stops |
| `LinearVelocity` | 12.46 m/s, unchanged to the last digit | 16.7 → 8.4 m/s, coasting |
| where the server had it | 90 m away, stopped | the same place |

Removing the marker by hand from the running client moved the vehicle 90 m in a single frame, which
is what turned a suspicion into a cause. The wheels were the tell and the red herring at once: they
are driven by how far the chassis has moved, and the dead blend kept handing them a few centimetres
of it, so the one part of the vehicle that looked alive was the part reading a corpse's pulse.

It is one system rather than a line in the vehicle's handover and another in the crate's, because it
is true of anything this client stops predicting — the crates a driver predicts had it too, where
nothing has wheels to give it away. Boarding again makes the vehicle predicted, and lightyear puts
the marker back itself, so nothing is smoothed less than before.

#### A seat comes back when its driver does not

A player who disconnected while driving took their `Driver` and `Driven` with them into nothing.
The components stayed on the vehicle, pointing at an entity that no longer existed: `use_vehicles`
read the seat as taken, nobody could get in, the throttle stayed wherever it had been left, and
`the_world_follows_the_drivers` went on handing that vehicle to a peer that had gone. For the rest
of the round.

It was known and unfixed for a while, on the grounds that disconnecting mid-drive is rare. Then a
test bot's connection timed out mid-drive and cost a server restart, which is the usual way a rare
case establishes its rate.

Giving the seat back is now one function that both ways out of it call — the key and the lost
connection — because the whole of the bug was that it had been written out once, in the branch that
handles the key. The disconnect side is an observer on the *player* entity rather than on the
connection, since the player is the thing the seat points at; lightyear despawns it for us, so it
covers a clean disconnect, a timeout and a crash alike.

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

### M5 — Terrain

The 500 m ground plane is gone. The ground is a **height field** — one height per sample on a
regular grid — that the server owns, that clients are sent, and that will later be sculpted from
inside the running game. The design it implements is `terrain.md`; what follows is what got built
and what it cost, in the order it was found.

#### The two traps, and why they had to be measured

Both are silent, and both were settled against the installed crate rather than against anybody's
documentation.

**Which index is which axis.** Avian's own doc comment says the number of rows is the subdivisions
along X. Underneath, parry's `Array2` is *column-major* — `flat_index(i, j) = i + j * nrows` — while
Avian flattens the nested `Vec<Vec<f32>>` row-major, and parry's accessors read `j` as x and `i` as
z. Get it wrong and the terrain is transposed about its diagonal with no error of any kind: a ramp
authored along +X comes out running along +Z. Avian's wrapper is only correct for square grids,
which is why the test grid is 5×3 and asymmetric — a square probe cannot catch a transpose.

**Centring.** A parry height field is centred on its own origin, so the body belongs at the field's
*centre*, not at the min corner.

What did *not* have to change is everything downstream: `Level`'s rays and sweeps already went
through Avian, so ground probing, sliding, crouch clearance and hit detection work against a height
field without a line of change. That was the plan's own test of itself, and it passed.

#### Drawing it cost two thirds of the frame rate, and the first two suspects were wrong

Drawing the ground at all took 62 frames a second to 22. Tiling it into 64 pieces changed nothing —
from ground level you can see most of a 512 m map, so there is nothing to cull — and turning off
the sun's shadows changed nothing either. It is triangle *density*: half a million triangles on
screen are about two pixels each, and a GPU shades in 2×2 quads, so sub-pixel triangles cost four
times what they cover.

| samples drawn | triangles | fps |
|---|---|---|
| every one | 524 288 | 22.0 |
| **every second** | 131 072 | **45.1** |
| every fourth | 32 768 | 46.8 |

So the picture skips every other sample and the collider keeps them all. That is ordinary level of
detail rather than the mismatch it replaced — a plane the size of the terrain was a *different
shape*; this is the same shape at fewer points. It is not free, and the number is written down
where it can be seen: on the sharpest lip of a ravine the drawn ground is 62 cm from the ground
underfoot, and centimetres everywhere gentler. The fix when it comes is level of detail by distance,
not a finer mesh everywhere — that is exactly what cost the frame rate.

#### The wire: the server owns the map, and a client is never allowed to read one

A joining client is sent the whole height field on a reliable ordered channel of its own, and the
**absence** of that map is what "not ready" means — there is no flag, the resource simply is not
there, and every system that needs ground is gated on it.

A client must never load the map itself, and the reason arrives with sculpting rather than now:
once terrain is editable the file on disk is the *last saved* state, so a client that read it would
collide against different ground from everyone else for as long as anybody held an unsaved edit.

It does not travel as a component, which is the one place the obvious Bevy answer is the wrong one.
Terrain is resource-shaped: there is one authoritative field, no timeline on which a second version
of it means anything, and half a megabyte of it — 526 338 bytes for 513² samples — which has no
business going through a path that diffs components tick by tick.

Measured against the configured link, which is a deliberately unkind one at 40 ms of ping and 2 %
loss: the map lands **1.0 to 1.2 seconds** after the connection does. That is long enough to matter,
and it is why the walking step and the vehicle step both wait for it — the player entity can easily
arrive first, and walking before the ground has would be a second of falling through an empty world.

One thing worth knowing before it is mistaken for a bug. Lightyear hands the whole fragmented
message to the transport at once, and netcode's replay protection is a 256-packet window; a burst of
some four hundred fragments plus jitter therefore pushes packets out the back of that window, which
is logged, once per packet, as `sequence ... already received`. Between 0 and 80 packets per join in
the runs above. It is not lost data — the channel is reliable and resends — but it is wasted
bandwidth and alarming log noise, and a bigger map makes it worse rather than better.

#### Still to come

Steps 6 to 11 of the plan: maps that can be made, loaded and saved; sculpting; placement;
undo; water; and texturing derived from slope and height rather than painted. The rendering step is
half done — the shape is visible, and `ExtendedMaterial` with layers chosen by slope is not.

## Risks

**Bone path matching — settled.** Bevy binds animations by bone *path*, and a clip library that
disagrees with a model's skeleton is a rewrite discovered late. The five character and animation
files in `assets/` were checked against each other on full bone paths and are bit-identical:

```bash
tools/glb rigs assets/animations/*.glb assets/characters/*.glb assets/characters/*.gltf
```

Re-run it after any re-download. It is a five-second check for the failure this section used to
call the significant one.

**lightyear's learning curve.** Capable, but the API shifts noticeably between minor versions and
the documentation has gaps. Budget time for reading the examples.

**Asset licensing.** Settled for what is here — the character kits are CC0 and the vehicle is
CC BY, both credited in `assets/CREDITS.md` and both in the repository. The mounted gun is CC BY-ND
and stays out; a build that ships it is a question to answer before cutting releases, not now.

---

## Character assets: settled

The player is the **Mixamo soldier**, which is where `webgame` started and where this ended up
after a detour worth recording, because the detour is what produced the tooling.

| | |
|---|---|
| Body | `characters/swat.glb`, 19 450 triangles, 1024² textures — 13 MB of GPU memory |
| Clips | 49, including 8-way walk, run, sprint and crouch-walk, aiming idles, six deaths |
| Rig | 70 bone paths, Mixamo's own (`mixamorig:Hips`) |
| Scale | metres already — no model scale and no correction |
| Licence | free with an Adobe account; **use yes, redistribution no** |

So it is not in this repository. `tools/setup-assets` converts it from the two downloaded packs
with `FBX2glTF`, and `assets/CREDITS.md` says what the packs are and where they come from.

### The detour, and what it was worth

Quaternius' CC0 kit was evaluated first, on the strength of being redistributable — and it is still
here, as the fallback a checkout without Mixamo gets. What it could not do was the job:

- **Forward locomotion only.** No strafe, no backpedal. The 8-way set is behind the paid tier.
- **Clips paced for a different game.** Its crouch cycle is authored for 0.75 m/s against this
  game's 2.6, so it plays at the clamp and the feet skate. Its walk and jog are 0.97 and 5.36 with
  nothing between.
- **The bodies are superheroes**, which is a look and not a soldier.

What the detour bought is `tools/glb`, and that turned out to matter more than the assets. Deciding
between four character packs by eye is guesswork; `tools/glb rigs` compares skeletons on full bone
paths and answers "these interchange" or "this needs retargeting" in a second, which is the check
this milestone's [named risk](#risks) was about. `tools/glb info` prints what a model costs in GPU
memory, which is not the size of the file and which had already taken this machine down twice.
Three candidate models were rejected on measurements from it rather than on taste:

| | triangles | verdict |
|---|---|---|
| Mira (Sketchfab) | 77 178 | own rig, one 13-second idle, four arms |
| Silver Soldier | 261 035 | Reallusion rig, one clip |
| Stylized Sci-Fi Soldier | 510 358 | Reallusion rig, one clip |
| **Mixamo Swat** | **19 450** | 49 clips on a matching rig |

The last row is the whole argument. It is the smallest of them and the only one that brought
animation with it.

### Dressing a character

Moot for the soldier, which arrives dressed. It is written down for the fallback and for whatever
comes next, because the question comes back with every new body.

**Paint it into the texture.** A smooth full-body model takes a uniform as a base-colour map and
nothing else — no rig work, no new geometry, nothing at run time. Kenney's character packs are
built entirely on this and ship editable SVGs.

**Attach a garment on the same rig.** A separate glTF carrying the same skin, drawn on the same
skeleton and animated by the same clips. Whether one from elsewhere fits is a measurement, not a
guess:

```bash
tools/glb rigs assets/characters/swat.glb <the candidate>
```

`identical` means it drops in; a handful of differing names means a bone rename in Blender, which
is much cheaper than a retarget because the hierarchy already agrees; `nothing in common` means
retargeting.

**Take it from the body itself.** Duplicate the part of the body mesh the garment covers, push it
out slightly, and it inherits the body's own vertex weights — a rig match by construction rather
than by luck, and no skinning work at all.

## Still to settle

- **Per-bone hitboxes** — the hitbox is the movement capsule, so a head shot and a shin shot are
  the same shot. Waiting on the real models in M2.
