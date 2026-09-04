# Terrain plan — Noob Tube

Design notes for replacing the flat ground plane with sculptable outdoor terrain. Written
2026-09-01. **Nothing here is built.** This is a plan.

Today the world is [`level.rs`](shared/src/level.rs): a 500 m square plane, three crates and a
ramp, written as constants that both binaries build the same geometry from. Terrain replaces the
plane. It is a height field — one height per sample on a regular grid — that the server owns, that
clients receive and collide against, and that players sculpt from inside the running game. With it
comes the map management that implies: any client can create a map on the server, load an existing
one, and save what it has changed, through a menu (§2).

Two properties decide nearly everything downstream, and they are worth stating before any of the
detail:

> **The ground is a single-valued surface.** `y = f(x, z)`, one height per column. No caves, no
> overhangs, no arches.
>
> **Nothing is ever painted by hand.** Every surface property — which texture, which footstep
> sound — is derived from the shape of the ground and from the paths drawn on it.

The first buys the collider, the wire format and the LOD story. The second removes an entire
authored data structure that a terrain system would otherwise need, and it is the reason this plan
is smaller than it looks.

### Prior art

A predecessor project, [`webgame`](../webgame), implements this same system in TypeScript against
three.js and Rapier; its `terrain.md` is a long document that records what its implementation
actually settled, as opposed to what it first proposed. Findings taken from it are marked where
they appear, because a measured result from a working implementation is worth more than an
argument. **Nothing in this document depends on having read it.** Where it goes further than this
plan does — vegetation, roads, water, LOD decimation — the sections below say so.

---

## The split: terrain is the ground, everything else is built

There is exactly one rule, and it is what keeps terrain independent of every decision this project
has not made yet:

> **Terrain is the ground. Everything built is somebody else's geometry.** There is no boolean
> operation between them. A crate below the surface is hidden inside it; a crate above floats on
> it. Neither system ever asks the other what it contains.

Accepting the single-valued limitation is the whole point rather than a regrettable compromise. A
cave, an overhang, a bridge deck, a building interior — those belong to built geometry, which can
be arbitrary because it is not trying to be a terrain.

Today "built geometry" means the crates and the ramp in `level.rs`, and terrain has to coexist with
exactly that. Whether it later becomes a voxel grid, a set of placed meshes, or something else
makes no difference to anything below, because of the rule. **That is why terrain can be built
before that question is answered.** The one change that would invalidate this plan is a built layer
that has to ask terrain what is inside it — if that ever comes up, the cost estimates here stop
holding.

### Why not voxels for the ground

A voxel grid fine enough to make a hillside look like a hillside is the wrong data model, and the
numbers are not close. Take 16³ chunks of 16-bit cells — 8 KB per chunk — at a 0.25 m cell size. A
256×256 m outdoor area is 1024×1024 columns, and a surface with relief passes through two to four
chunk layers, so roughly 8–16k chunks: **64–128 MB**, plus a mesh of millions of quads with no
practical LOD. The same area as a height field at 1 m spacing is 257×257 samples — **132 KB** as
`u16`. Three orders of magnitude.

The visual argument is as strong and needs no arithmetic: at 0.25 m quantisation a natural slope
reads as a staircase. The model is wrong for the subject, not merely expensive.

### Why not a mesh authored in Blender

Static terrain could be a mesh asset and a `Collider::trimesh_from_mesh` today, with almost no new
code. Rejected for three reasons, in increasing order of weight:

1. It cannot be edited in-game, which is most of the point.
2. A trimesh collider is strictly worse than a height field — larger, slower to query, and it has
   to be rebuilt wholesale where a height field is rebuilt per tile.
3. **The server would need the mesh.** `trimesh_from_mesh` takes a `Mesh`, which means asset
   loading and `bevy_render` types on a binary whose entire `Cargo.toml` is arranged to keep wgpu
   out of it. `Collider::heightfield` takes raw floats and nothing else. This is the decisive one.

---

## 1. Data model

### Height field

```
nx, nz        sample counts per axis  — derived from extent and spacing (§2)
spacing       metres between samples  — chosen when the map is created
origin        world x/z of sample (0,0)
min_y, max_y  the range the quantised heights map onto
water_y       world y of the sea surface, or none for a dry map
heights       nx*nz samples, u16, row-major (x fastest)
```

`u16` over an explicit `[min_y, max_y]` range gives 2 mm precision over a 128 m range, at half the
size of `f32`. Terrain never needs more than that.

**The quantisation is not a storage trick, it is the determinism mechanism.** Brushes round to the
`u16` lattice *inside* the brush, not on save (§6), so every machine lands on the same lattice
after every stroke and repeated small strokes cannot accumulate apart. Storing `f32` and rounding
on the way to disk would give up the property that makes it safe to replicate a sculpt as a gesture
rather than as a list of samples.

`nx` and `nz` are independent, so a map can be a strip rather than a square. With `spacing` they
fix the extent: `(nx-1) * spacing` by `(nz-1) * spacing` metres.

### Surface appearance is derived, and there is no splat map

The usual way to texture terrain is a *splat map*: a second grid, RGBA, one byte per layer weight,
painted by hand and stored alongside the heights. This design has none. Which texture appears at a
point is a function of that point's height, its slope, and its distance to any path drawn on the
map (§11) — evaluated per pixel in the fragment shader (§5).

The case for storing a painted splat instead is real, and it is worth stating at its strongest
before dismissing it. From the predecessor's design notes, which chose that way:

> The splat is content, exactly like the heights — never derived at load time. […] This is the
> decisive property: painting is then the *normal case*, not an exception layered over a
> generator. There is no rule that a painted terrain has to contradict, no override list to keep
> in sync with a derivation, and no derivation running outside the editor that could disagree
> between two machines.

**Every clause of that is about painting, and here nobody paints.** A derivation cannot contradict
authored data that does not exist. There is no override list because there are no overrides. And
the derivation cannot disagree between two machines because it runs in a fragment shader on the
client — the server never evaluates it at all, and cannot, since it has no renderer.

What this means in practice is that a whole layer of the system never gets built: no second grid on
disk or on the wire, no packing scheme to make that grid affordable, no paint brush, no per-layer
weight bookkeeping, no rule that painted weights must sum to a constant, and no "regenerate the
texturing from rules" action that destroys hand-painted work. The baseline a joining client
receives is the heights and nothing else.

**What it costs**, on the record: a map can never have a patch of moss or a worn bare spot that the
ground's own shape does not explain. Every visual distinction has to be expressible as a rule.

**And the way back is not a splat.** Paths and roads are wanted eventually (§11), and they arrive as
*2D vector paths* somebody places — an ordered handful of control points, evaluated per pixel — not
as a raster somebody paints. That keeps the invariant whole: a road is still derived, from a curve
instead of from height and slope.

The two are not merely compatible; they are in conflict, and the predecessor found the conflict the
hard way:

> the splat is a destructive canvas. Painting a road onto it cannot be undone without remembering
> what was underneath, so a road that keeps repainting as it is dragged needs the splat to be
> *derived* rather than *edited* […] A map wanting both hand-painted ground and draggable roads is
> the case that does not work.

Wanting paths you can drag is therefore a reason to have no splat, not a reason to keep one.

### Layers

Up to four texture layers, each `{ texture: String, tile_scale: f32 }` plus the rule that selects
it. A layer names an ordinary material asset, so texture loading and packaging are whatever the rest
of the game already does.

**The rule is a band on each of four axes, multiplied together**: how steep the ground is, how high
it is, how far it sits below the ground around it, and how far it stands above the waterline. Only
the first two are properties of the point alone — the third is a question about the neighbourhood,
and the fourth is a question about the *map*, since the same ground is a lake bed or a meadow
depending on where the author last put the sea. That is also why a shore rule cannot be a height
band: a beach written as one is a beach nailed to a world y, left behind the moment the water
moves. A path kind will be the fifth (§11).

### Water

There is water, its level is one number, and an author sets it in the editor — so `water_y` lives
in the terrain file beside the heights and is edited the same way. A map with no water stores
nothing.

**Water is everywhere at `water_y`; the height field only says where the bottom is close enough to
matter.** Stating it that way round rather than "water exists where the terrain is low" is what
makes the model simple: the shore texture rule, the underwater tint and the splash test all become
one comparison against a plane, with no "is there terrain here" case in front of them, and swimming
past the edge of the field does not switch the sea off.

The rendered surface is then a flat mesh at `water_y` carrying, per vertex, the **depth** of the
ground beneath it. Depth is what makes water read as water — it drives the colour from clear
shallows to dark deeps — and it is a subtraction against the height field the client already holds.
Quads are worth emitting only where at least one corner is submerged, so a pond in one valley costs
a pond rather than a map-sized plane.

`water_y` is also a rule input for §5: the shore layer is "below the water line", which is why a
dry map loses one of its four layers rather than needing a different rule set.

### File format: two files, and only one of them is binary

A map is a readable manifest plus one blob of samples.

```
<name>.json       everything human-scale — serde, and readable
  version
  grid      { nx, nz, spacing, origin_x, origin_z, min_y, max_y }
  water_y   number | null
  layers    [ { texture, tile_scale, rule parameters } ]
  markers   [ { kind, x, z, y, yaw | rotation } ]   (§7)

<name>.heights    nx*nz u16, row-major, x fastest — and nothing else
```

**The heights are binary because they have to be.** 66k samples as text is some 400 KB of digits
and a slow parse, and nobody hand-edits a height field in an editor, so that file gains nothing by
being readable. Everything else is the opposite case: a few dozen markers and a handful of grid
numbers, changed constantly while the design is young.

**Everything else is JSON to begin with, and binary only if that ever stops working.** The reason
is not tidiness, it is the rate of change. A binary manifest with a leading version byte means
every new field is a codec change, a version bump and a migration for maps that already exist —
paid on every field, including the ones that turn out to be wrong a week later. With `serde` and
`#[serde(default)]` an added field costs nothing and old maps keep loading. Being able to read a
map in a diff, and to fix one by hand when a bug writes something impossible, is worth more right
now than the bytes are.

JSON specifically, over the two obvious alternatives: RON is more Bevy-idiomatic and buys nothing
here; `toml` is already a workspace dependency but is a poor fit for an array of structs, which is
what `markers` is. `serde_json` is the least surprising thing for a file an external tool might one
day write.

**Loading must not trust the pair.** The manifest states `nx` and `nz`; the blob is however many
bytes it is. Check that `len == nx * nz * 2` on load and reject the map otherwise — the two files
can be separated, edited, or half-written, and a mismatch that is not caught reads the height field
off the end of itself.

The heights blob is still both the file and the bulk of the wire baseline (§3); the manifest travels
beside it as a few kilobytes of text. That is one encoder and one decoder for the part where it
matters, and no hand-written codec at all for the part that changes.

---

## 2. Maps: creating, loading and saving

Terrain is the first thing in this project that is *content* rather than constants. `level.rs` is
compiled in; a height field is a file, and the entire point of sculpting is that it changes. So
terrain arrives together with the map management this game does not have yet, and the two cannot
sensibly be separated — a sculpt you cannot save is a demo.

**Any connected client may create a map, load one, and save changes.** No ownership and no
permissions, which is the same stance the rest of the game takes; the protections are against
accident and abuse of *size*, not against the player.

### A menu, not a HUD

This is modal and opened deliberately: it stops the world rather than annotating it, because every
action in it is disruptive to everyone. It holds:

- **the map list**, with the current one marked and unsaved changes shown — a sculpt that exists
  only in memory is one disconnect from gone, and the menu is the only place that can say so;
- **load**, which switches everybody;
- **new…**, which opens the creation dialog below;
- **save** and **save as…**.

### Creating a map: extent and spacing, not sample counts

Two numbers, and they are the two an author actually thinks in:

- **extent** — how many metres across the map is, per axis;
- **spacing** — how many metres between samples.

`nx` and `nz` are derived, not typed: `nx = round(extent_x / spacing) + 1`. The dialog shows the
derived sample count and the resulting baseline size as the fields change, because those are the
numbers the caps are enforced against and the author should watch them move.

**The `+ 1` is not an off-by-one.** Heights are grid *points*, not cells, so an n-metre terrain at
1 m spacing needs n+1 of them. It is also why heightmap tools export 2ⁿ+1 sizes — 513, 1025, 2049 —
and why a sample cap written as a round 512² would reject a canonical 513² import by exactly one
sample per axis, for no reason at all.

`min_y` and `max_y` are the third creation decision: they set how much vertical range there is to
spend, and they cannot be changed later without requantising every height.

### A new map starts at half height

The height field of a fresh map is **`(min_y + max_y) / 2` everywhere** — not flat at zero, and not
flat at the bottom of its range.

Sculpting is two-directional, and a field that starts at its floor can only be raised. The first
attempt to carve a riverbed or a hollow clamps, and the author has to raise the entire map before
they can dig anything at all. Starting at the midpoint gives the same range in both directions and
makes the first stroke work whichever way it goes.

The two spawn points sit on that plane, so a freshly created map is immediately playable.

### Clamping is mandatory, not tidy

Extent and spacing arrive from a client over a reliable message, and their quotient drives a
server-side allocation and the size of every future baseline. Unclamped, that is a memory and
bandwidth denial of service with a one-line exploit — a client asking for a 10 km map at 5 cm
spacing.

The caps belong in `shared`, so the menu can grey out over-cap inputs against **the same constants
the server enforces** rather than a second copy that drifts:

- `spacing` in `[0.25, 8]` m. Finer buys nothing a sculpt brush can express; coarser is unusable.
- a per-axis sample cap, **and** a separate cap on `nx * nz` — not the product of the axis caps, or
  an extreme aspect ratio walks straight through both.
- a per-client rate limit on creation specifically, because that is the action that writes files.

Size the total-sample cap from what a baseline may cost on join, not from what fits in memory. For
scale: 513×513 is a 512 m map at 1 m spacing and about 526 KB of `u16` heights.

### Names, and the one guard that matters

A map name from a client is sanitised on arrival, and **a load only ever accepts a name that is
literally one of the directories the server itself discovered.** That is the path-traversal guard;
it is not a separate check bolted on beside the load, it *is* how the load resolves a name, and
writing it any other way is how the check gets forgotten on the second code path.

### Load and save

The file I/O is the easy half. The two parts that need deciding:

- **A load is a map switch**, and everybody has to end up on the same map. The simplest correct
  thing is for the server to apply it between ticks and then re-send the baseline to every client,
  which is the join path it already has rather than a second one beside it.
- **Save is explicit, never automatic.** The file on disk is what the next joiner starts from
  (§3), and an accidental save over a good map with a half-finished experiment is unrecoverable
  without versioning this project does not have. The menu shows unsaved state; it does not resolve
  it on its own.

---

## 3. Ownership and wire

The server owns the authoritative height field. A joining client receives it once as a baseline;
every subsequent change arrives as an edit gesture (§6).

**The client never loads the terrain file itself, and it must not.** Once terrain is editable, the
file on disk is the *last saved* state rather than the current one. A client that loaded it
directly would see — and physically collide with — different ground than everyone else, for as
long as anybody is holding unsaved edits.

### Not through component replication

This is the part where the obvious Bevy answer is the wrong one.

Terrain is resource-shaped, not entity-shaped. A ~130 KB baseline has no business travelling
through a path that diffs components tick by tick, and terrain must be neither `Predicted` nor
`Interpolated`: there is one authoritative height field, and no timeline on which a second version
of it means anything.

Use a dedicated channel instead — the same move [`protocol.rs`](shared/src/protocol.rs) already
makes for `EffectsChannel`, there so that a burst of cosmetics can never delay a position update,
here so that bulk terrain data cannot either:

- `ChannelMode::OrderedReliable(ReliableSettings)`
  (`lightyear_transport-0.29.0/src/channel/builder.rs:553`)
- a `TerrainBaseline` message, sent to a client on join
- a `TerrainEdit` message per committed gesture, broadcast to everyone

Ordered matters, because edits do not commute: raise-then-smooth and smooth-then-raise are
different terrains.

Client-side the height field lives in a resource, and it needs a small queue for edits that arrive
before the baseline has been applied — a reliable stream can deliver an edit for a terrain the
client has not finished installing.

---

## 4. Physics

One `Collider::heightfield` per collider tile.

```rust
// avian3d-0.7.0/src/collision/collider/parry/mod.rs:1128
pub fn heightfield(heights: Vec<Vec<Scalar>>, scale: Vector) -> Self
```

Avian takes a nested `Vec<Vec<Scalar>>` and derives the grid dimensions from the structure, even
asserting that every row is the same length. That is worth noticing because the equivalent Rapier
call takes flat data plus explicit row and column counts — which are *segment* counts, one less
than the sample counts, and getting that wrong panics somewhere deep inside the physics library
rather than at the call site. Rust's type system eats that entire class of mistake here.

**Two traps survive, and both fail silently rather than loudly.**

- **Axis order.** Avian documents it: "The number of rows indicates the number of subdivisions
  along the `X` axis, while the number of columns indicates the number of subdivisions along the
  `Z` axis." So a height field stored `iz * nx + ix` must be handed over as `heights[ix][iz]`.
  Getting it wrong transposes the terrain about its diagonal with no error of any kind — a ramp
  authored along +X comes out running along +Z.
- **Centring.** parry's height field is centred on its origin, so the collider body belongs at the
  tile's *centre* and not at its min corner, which is where `origin` points. Avian does not
  document this; it wraps `SharedShape::heightfield` directly, so parry's convention is what
  applies.

Both are inherited from parry rather than reasoned from first principles, so they should be
**verified against the installed crate rather than read off the documentation**. The predecessor
project hit both against Rapier — the transpose silently, costing an afternoon — and the probe that
caught them transfers directly: build a live physics world containing an intentionally asymmetric
ramp, flat along one axis and sloped along the other. A symmetric test shape cannot catch a
transpose, which is exactly why the bug survives casual testing.

### The two traps this repository already documents

From [`physics.rs`](shared/src/physics.rs), both of which apply to the terrain collider:

- It needs `RigidBody::Static`, or `MoveAndSlide` cannot see it. That query is filtered
  `With<ColliderOf>`, so a bare `Collider` is invisible to sweeps while staying visible to
  `cast_ray` — terrain you can shoot but walk through is the failure mode.
- It needs `CollisionLayers::new(Layer::Level, LayerMask::ALL)`, or `Level`'s queries, which ask
  only about `Layer::Level`, will not find the ground at all.

Everything downstream is then free, and that is the single biggest reason this design is
affordable: `Level`'s ray casts and capsule sweeps already go through Avian, so ground probing,
sliding, crouch clearance and hit detection work on terrain without a line of change.

### The gap terrain opens: there is no slope limit

`Level::is_grounded` is a downward ray within `GROUND_SNAP_DIST` and **nothing else**
([`physics.rs:174-195`](shared/src/physics.rs#L174-L195)). Nothing tests the surface normal.

On a flat plane with cuboid crates that is invisible, and correct: every walkable surface is
horizontal and every wall is vertical, so a ray that hits at all has hit a floor by construction.
Terrain breaks the assumption immediately — a 70° cliff face returns a hit within snap distance,
and the player strolls up it.

The fix is a normal test in `is_grounded`, and the threshold is an authoring decision as much as a
physical one: it sets what counts as a cliff. The predecessor uses `normal.y > 0.7`, about 45.6°,
and reports the consequence — terrain steeper than the limit is ground players slide off, which is
good for framing a map and bad by accident.

**This belongs in the terrain work rather than after it**, and it has to land before the first
sculpted hill, because it changes movement on every surface that already exists and wants retuning
against the ramp. The sculpt tools should then tint anything past the limit, so unclimbable ground
is visible while it is being made instead of discovered during a match.

---

## 5. Rendering

`ExtendedMaterial<StandardMaterial, TerrainMaterial>`
(`bevy_pbr-0.19.1/src/extended_material.rs:145`) rather than a `Material` written from scratch:
lights, shadows, fog and the whole PBR path keep working, and only the surface appearance is
replaced. A bare custom material would give all of that up to gain nothing terrain needs.

**Mesh:** a regular grid split into tiles of 64×64 samples, so frustum culling has a unit to work
with and a later LOD has something to operate on. Normals from central differences of the height
field — exact and cheap, and never taken from the triangle mesh.

**Fragment shader**, per pixel, with no splat texture to sample:

1. Derive the layer weights from world height and surface normal — *steeper than 35° → rock, below
   water level → sand, otherwise grass*, with smooth transitions and noise on the thresholds.
2. Sample each contributing layer's colour, normal and roughness at its own `tile_scale`, and
   blend.
3. **Triplanar projection, blended in above a slope threshold.** Every height field stretches its
   texture to mush on steep faces; flat ground does not need triplanar, so it costs only where it
   pays for itself.
4. **Tile break** — a second, much larger-scale sample multiplied over the result, to kill the
   visible repetition on open ground.

Deriving the weights costs some ALU per pixel and saves a texture fetch, so it is *cheaper* than
sampling a stored splat rather than merely simpler. The worst case is still around fifteen texture
samples for one surface, which needs measuring; a reduced variant — two layers, no triplanar —
belongs with the other quality settings.

**Water** is a second, much simpler material: a flat mesh at `water_y` whose vertices carry the
depth of the ground below them (§1). Colour from depth, and the shoreline falls out of the same
number the sand rule uses, so the wet edge and the sand edge cannot disagree.

Because the weights come from per-pixel values rather than a stored grid, transitions follow the
geometry exactly and can never be coarser than the surface itself. The cost is the mirror image:
transitions are *uniform*, and a shoreline has no hand-placed variation anywhere along it. Noise in
the rule is what buys that back.

---

## 6. In-game sculpting

A sculpt mode, with the tools in the order they earn their place:

1. **Flatten** — level the ground under the brush to a picked height, with a height eyedropper.
   Mandatory rather than optional: built structures have flat footprints and unsculpted ground does
   not, so without it every building gets gaps under one corner and buries another.
2. **Raise / lower** — with distance falloff for soft edges.
3. **Smooth** — pull each sample toward the mean of its neighbours.
4. **Ramp** — two points, linear interpolation between them.

There is no paint tool, because there is nothing to paint (§1).

### Network shape

- `TerrainEdit { op, x, z, radius, strength, height, apply_at_tick }` — a **gesture**, not a list
  of samples. Both sides recompute the same result deterministically. This is the principle
  [`simulation.rs`](shared/src/simulation.rs) already rests on, one level up: send the input, not
  the outcome, and keep one implementation of the step that turns one into the other.
- Server-validated before broadcast: radius cap, coordinate range, finite strength.
- A token bucket over touched samples, charging an **upper bound computed before anything is
  applied** and rounded up on both ends of each axis. Undercharging would let a client buy a bigger
  stroke than it pays for, and the commit is broadcast before it is applied, so an oversized stroke
  stalls every machine in the game rather than only the sender's.

### Determinism is bought by avoiding transcendental functions, not by hoping

The brushes use only `+ - * /`, `round`, `min`/`max` and comparisons. Every one of those is exactly
specified by IEEE-754 and identical everywhere. The radial falloff is therefore a polynomial in the
**squared** distance and never takes a square root: `powf`, `exp`, `sin`/`cos` and `hypot` go
through `libm`, which is not required to be correctly rounded and may differ in the last ulp
between platforms.

Do not reach for `f32::mul_add` either. It is a *different* result from `a * b + c`, exactly and
deliberately, and whether it lowers to one instruction or two is a property of the target.

**Smooth needs a snapshot.** Reading neighbours while writing in place makes the result depend on
iteration order, and a sample near the rim would be smoothed against neighbours that were already
smoothed. Copy the affected box first; samples outside it are read live, which is correct precisely
because nothing outside the box is written.

An edit dirties tiles, and a dirty tile rebuilds its mesh and its collider. Tile the collider on
the same grid as the mesh, so one brush stroke rebuilds one height field instead of the whole
terrain.

---

## 7. Placing things

One mechanism places everything that stands on the ground: spawn points, vehicles, crates, trees,
whatever comes next. They differ in what they spawn and in nothing else, so they share a record, a
hotbar, a set of gestures and a validation path.

The first three already exist as constants — `CRATES`, `VEHICLE_STARTS` and `spawn_point` in
[`level.rs`](shared/src/level.rs) — and that file already says what is wrong with them:

> Real spawn points come with real levels. […] Harmless on an empty plane, wrong on a real map, and
> fixed by picking a free spawn point rather than counting.

So this starts as moving three constants into the map file and letting somebody drag them around,
and the generalisation costs nothing: a marker's `kind` is a palette id rather than a closed enum
of three, and adding a placeable becomes adding an asset instead of a code change.

**Trees are the case worth being careful about.** Placing them individually is right for the ones
that matter — a landmark, cover at a chokepoint, something an author aimed at. It is not how a
forest gets made; ten thousand of those come from a seed and the height field (§12), cost no
bandwidth, and are not markers at all. Both can be true on one map. What must not happen is the
placement system growing a scatter brush and quietly becoming the forest — that is the point at
which the marker list stops being a few dozen readable lines.

### A marker is not the thing it spawns

The distinction that keeps this cheap:

- A **marker** is map content — a kind, an x/z, a yaw. No collider, no hitbox, invisible outside
  edit mode, and it travels with the map rather than per tick.
- The **entity** it produces — a crate, a vehicle, a player — is ordinary gameplay state, created
  at round start and replicated exactly as it is today.

Three things follow. Markers never enter prediction, because nothing predicts them. A round reset
is a re-read of the markers rather than a special case. And **placement is exempt from §9's
rollback rule**: a marker has no collider, so nothing it does can change where a player may stand,
and there is no per-tile rebuild to make idempotent. Only spawning the live entity immediately
would need the tick discipline, and then it would need it for the entity rather than the marker.

### The height is relative to the ground

A marker stores `x`, `z`, `yaw` and a `y` that is an **offset above the ground**, not a world
height. Where the entity appears is `ground_height(x, z) + y`, resolved when it spawns.

That is a correctness rule rather than a saving. Terrain is editable, so an absolute `y` is wrong
the moment somebody sculpts underneath it — a spawn buried in a new hill, or a vehicle dropped four
metres onto a valley floor that used to be a ridge. An offset moves with the ground, so every
marker survives every sculpt with no fix-up pass and no way to forget one.

It also keeps the cases a bare "sit on the ground" rule would lose. `y = 0` is the ordinary one and
what placement defaults to; a positive offset stacks a crate spawn on top of another crate, or puts
one on a ledge of built geometry; and the author sets it by the same gesture that sets everything
else rather than by editing a file.

### Where markers live

In the map manifest (§1), which is the JSON file, alongside the grid parameters and the layers —
and explicitly *not* in the heights blob. By this document's own rule (see The split) a spawn point
is a thing placed on the ground rather than part of it, and the file split follows the same line:
the blob is samples and nothing else, so a heightmap import can overwrite it wholesale without
touching the spawns you want to keep.

It is also the part of a map most worth reading in a diff. A marker list is exactly the content
where "why did this move" is a question somebody asks.

A marker is `{ kind, x, z, y, rotation }`, with `y` relative to the ground and `rotation` a
quaternion. Around thirty bytes, a few dozen per map — the size question does not arise, which is
why the split can be decided on tidiness alone.

### Rotation is a quaternion, not a yaw

`VEHICLE_STARTS` stores a yaw today, which is enough for a vehicle standing on a flat plane and
not enough for anything else. A crate on a hillside wants to lie the way the hillside does; a
vehicle parked facing down a ramp is pitched; a prop is not always upright. So a marker carries a
full `Quat`, which is also exactly what `Transform.rotation` wants, so nothing has to convert it on
the way to spawning an entity.

Not Euler angles as the *representation*, for one reason that has nothing to do with gimbal lock:
**a quaternion has no convention to disagree about.** Three angles need an axis order and a
handedness, both sides have to pick the same ones, and the failure when they do not is a marker
that is subtly turned rather than an error anybody sees.

### But the file may write a yaw

The manifest is JSON precisely so it can be read, and `[0, 0.383, 0, 0.924]` is not readable. Since
the format is not a fixed-width record any more, a marker's rotation may be given either way:

```json
{ "kind": "vehicle", "x": 14, "z": -8, "yaw": 45 }
{ "kind": "crate",   "x": -5, "z": -12, "rotation": [0.0, 0.383, 0.0, 0.924] }
```

Both parse to a `Quat`, and nothing downstream ever sees the difference. Three rules make that safe
rather than merely convenient:

- **Writing is canonical, not remembered.** When a rotation is a pure yaw — its x and z components
  are zero within a small epsilon — the serialiser writes `yaw`, whatever the file said before.
  This is what stops the shorthand being write-only: an "accept either form" reader paired with a
  "always write the general form" writer turns every hand-written `yaw: 45` into a quaternion on
  the first save, and the readability lasts exactly until the editor touches the file.
- **Both fields present is an error, not a precedence rule.** Reject the map. A precedence rule is
  a thing somebody has to remember correctly at 2 a.m.
- **`yaw` is in degrees** in the file and radians everywhere in code, converted at the parse
  boundary. Readability is the entire point of the shorthand, and `"yaw": 45` is readable where
  `"yaw": 0.785` is not. The mixed units are a real hazard — `RAMP_ANGLE` is already `0.21` with a
  comment saying "about 12°" — so the conversion belongs in exactly one place, in the parser.

The convention the shorthand needs is the one `level.rs` already states and uses: a yaw in radians
about +Y, applied to the −Z that is forward everywhere here.

**Where this generalises and where it stops.** A shorthand is safe when it has an exact expansion
*and* an exact contraction, so the canonical writer can detect it and give it back. `yaw` qualifies.
A shorthand that loses information on the way in — anything the writer could not reconstruct — does
not, and would leave the file quietly disagreeing with what the editor holds.

An **align-to-ground** helper belongs beside the free rotation rather than replacing it — take the
surface normal under the marker and rotate the up axis onto it, then let the author turn it from
there. That is the gesture somebody actually wants when dropping a crate on a slope, and it is one
call against a height field the client already holds.

### The hotbar

Ten slots on keys **1–9 and 0**, in that order.

The structure worth copying is that **a slot holds a thing, and the key you press chooses the
verb** — place, delete, rotate. One slot therefore does all three, and the slot count is not
multiplied by the number of actions.

What goes in a slot is chosen from a **palette dialog** — the full list of placeables the server
offers, with the slot picked first and the entry second. The hotbar is the fast path for the ten
things you are using right now; the dialog is where the other hundred live. That split is what lets
the placeable set grow without the hotbar having to.

The palette itself comes from the server, alongside the map. An author on a server with more
assets sees more entries, and a marker whose `kind` names something the server does not have is a
load error rather than a silently missing crate.

### Placement is a gesture, like a sculpt

Same machinery as §6, over the same ordered reliable channel, for the same reason: send what was
asked for, not what it produced.

- `MarkerEdit { op, kind, x, z, y, rotation, id }`, server-validated before broadcast.
- **Rotate is absolute, not a delta.** The sender reads the current rotation, applies its step, and
  sends the *result*. Two authors turning the same vehicle marker in the same moment then land on
  one of the two orientations instead of on their composition — which for rotations is worse than
  for a yaw, since composing two deltas in the other order gives a third answer again. The same
  reasoning is why place carries a position rather than an offset.
- **Delete names an id, not a place.** A position is ambiguous as soon as two markers are close,
  and a delete that quietly removed the wrong one is worse than a delete that misses.

What the server owes on validation: a known kind, `x`/`z` inside the terrain footprint, a bounded
`y` offset — it is a relative height, so it needs a cap in both directions rather than only a
finiteness check — a cap on markers per map, and a rate limit.

The rotation needs its own check: **finite and normalised.** An unnormalised quaternion off the
wire does not error, it scales and skews whatever it is applied to, so the server renormalises or
rejects rather than trusting the sender. That involves a square root, which is fine here and worth
saying out loud given §6's rules — marker data never enters prediction, so nothing about it has to
be bit-identical across machines. Plus one rule that is not about abuse — **at least
one player spawn has to survive.** A map with none is unplayable, and the delete handler is the
cheapest place in the system to know that.

---

## 8. Undo

One step deep, the author's own last edit, and only when nothing has happened since.

That sounds like a limitation and is mostly a design: a shared world with several editors turns
general undo into a merge problem — undoing edit N when N+1 was built on top of it has no
answer that is right in every case. Refusing that case outright costs almost nothing, because it
is not what anybody is asking for. What an author wants, essentially always, is *I just did that,
take it back*.

### The condition, exactly

The server keeps one monotonically increasing **edit sequence number** per map. Every accepted
edit of any kind — sculpt, place, delete, rotate — increments it, and the server remembers who
made the last one.

An undo request carries the sequence number the client believes is current. The server accepts
only if that number still matches, and if the last edit was the requester's own. Otherwise it
refuses, and the client says so rather than doing something approximate.

Both halves are needed and for different reasons. **The sequence check** is what makes this safe
under concurrency: it is a compare-and-swap, so two authors racing to undo cannot both win, and an
undo can never land on top of an edit it did not account for. **The ownership check** is a matter
of taste rather than safety — undoing someone else's stroke is mechanically fine and socially
surprising. It is one condition, and worth having.

### A sculpt undo is a before-image, not an inverse

This is the part that decides the implementation.

For a placement the inverse is trivial and needs nothing stored: undoing a place is a delete of a
known id, undoing a rotate is the previous rotation, undoing a delete is re-creating the marker
the server still has.

For a sculpt there **is no inverse gesture.** Raising by half a metre and then lowering by half a
metre does not restore the field: heights clamp at `min_y` and `max_y`, and every stroke rounds to
the `u16` lattice (§1), so both ends of the round trip lose information exactly where the stroke
was most extreme. An undo built from an inverse brush would leave the terrain almost right, which
is worse than either alternative.

So a sculpt keeps a **before-image**: the affected box of samples as it was. Its size is bounded by
the same cap that bounds a stroke (§6), the editor holds exactly one of them, and it is memory
rather than anything on disk.

**Undo is therefore the one gesture that carries samples rather than a gesture**, which §6 makes a
rule of not doing. The exception is justified and bounded: it is the only exact answer, it is
capped at one stroke's footprint, and it happens at human speed rather than per tick. Everything
else about it is ordinary — it travels the same ordered reliable channel, and a sculpt undo changes
heights, so it lands at `commit_tick + max_rollback_ticks` like any other (§9).

### What it does not do

Undo is per-session and in memory. It is not a history, it does not survive a restart, and saving
does not clear it — a saved edit can still be undone, and then saved again. A map wanting real
history wants versioning, which is a different feature and not this one wearing a smaller name.

---

## 9. Terrain edits and rollback

This is the one piece of design here with no precedent to copy, because it follows from prediction
machinery the predecessor does not have in this form.

`Level` is a `SystemParam` that reads Avian's *current* spatial state. There is no seam anywhere in
it where a replay could be handed historical terrain. But lightyear's rollback re-runs `FixedMain`
for ticks N−k through N, so if a terrain edit was applied inside that window, every replayed tick —
including the ones that happened before the edit — is walked on the new ground.

The failure is small (a player is corrected by however far the ground moved under them) and rare
(edits are rare next to ticks), so *ignoring it* is defensible. It is not what this document
recommends, because the fix costs almost nothing:

> **Apply every terrain edit at `commit_tick + max_rollback_ticks`.**

Then no rollback window can straddle an edit, by construction rather than by luck. The server
stamps the tick, the client applies it at the same one, and the sculptor waits a fraction of a
second to see their own stroke — which in an editor nobody notices. Choosing the tick deliberately
is the same move [`lag_compensation.rs`](shared/src/lag_compensation.rs) already makes for shots.

The per-tile collider rebuild must also be idempotent, since a rollback can re-run the frame that
triggers it.

---

## 10. The one derivation that has to exist twice

Surface classification — *what am I standing on* — is needed on the CPU for footstep sounds, impact
decals and anything else that cares about the material under a player. The shader's copy is WGSL
and runs per pixel; the gameplay copy is Rust and runs per event.

That is exactly the hazard [`simulation.rs`](shared/src/simulation.rs) warns about — "two copies of
the same six lines is precisely how that stops being true" — and here it cannot be designed away,
because one of the copies has to be a shader.

Two mitigations, and the second is the one that actually settles it:

- Keep the rule small enough that two implementations cannot plausibly drift: thresholds on
  `normal.y` and world `y`, and nothing else. The noise that breaks up transitions visually lives
  only in the shader copy, where a disagreement is invisible by definition.
- **Accept that the CPU copy is approximate.** Its worst case is a footstep that sounds like grass
  half a metre into the sand. Collision never consults it, so a divergence cannot reach the
  simulation — which is the test that matters, and the same one that makes the visual half safe.

**Paths are exempt from all of this** (§11). "Am I standing on a road?" is a pure function of the
path list, which lives in `shared` and is the same data on both sides — one call, no second
implementation, no approximation. The duplication above is the price of deriving from *height and
slope* specifically, and it applies only to the ground rules.

---

## 11. Paths and roads — later, and vector

Wanted, not designed here. This section records the shape and the one constraint specific to this
engine; the predecessor works the rest out in detail, including LOD-gap measurements for a road
ribbon over decimated terrain.

**The data is a 2D path**: an ordered list of control points, a width, and a kind. Somebody places
it, so it is authored — but it is *vector* authored data, tens of bytes on the wire rather than a
megabyte of raster, replicated alongside the terrain rather than baked into it. A path is
evaluated, never stamped.

Two grades, which are different features sharing a word:

| | Track | Road |
|---|---|---|
| Cross-section | follows the ground | **level** — both edges at the station's height |
| Geometry | the terrain's own triangles, redrawn with a mask | its own ribbon of quads |
| Terrain changed | no | **yes, and that is the point** |
| Cost | almost nothing | a new brush, and it touches the collider |

The track is the one to build first, because it is nearly free: redraw the terrain triangles the
path crosses with a second material and an alpha mask from the path. It cannot disagree with the
ground, because it *is* the ground.

### The split that matters here

A road has two halves, and they land on opposite sides of the line this whole document is
organised around:

- **The texture half is client-only.** Distance to the nearest path segment feeds the same fragment
  shader as §5's rules. The server never evaluates it, nothing can diverge, and it costs nothing.
- **The height half is a terrain gesture.** Cutting a level corridor changes the height field,
  which changes the collider, which changes where players can stand — so it falls under §6's
  determinism discipline and §9's `commit_tick + max_rollback_ticks` rule like any other sculpt. It
  is genuinely a different brush from flatten: flatten levels to one height, this levels to a
  *profile* sampled and smoothed along the path.

That the cut is required is not a nuisance, it is what makes the ribbon work: a corridor that is
level across and near-linear along is exactly what a coarse LOD reproduces without a visible gap.

The ribbon itself needs **no collider**, because it lies on ground that already collides. Bridges
and tunnels are out of scope for the same reason — the moment a ribbon leaves the surface it needs
a collider of its own, and that is a different feature wearing the same word.

---

## 12. Vegetation — sketch only

Not designed here beyond the property that makes it affordable.

**Trees are derived, not replicated.** A seed plus the height field generates the same forest on
every machine, the same way `MOVING_CRATES` and `CRATES` are shared constants from which both sides
build identical entities. Ten thousand trees cost zero bandwidth and no replication machinery.
Refusing hand-placement here is the same decision as refusing hand-painting in §1, and it buys the
same thing: no authored bulk data, and no way for two machines to hold different versions of it.

Trunks are static colliders on `Layer::Level`, so `Level`'s existing queries stop bullets and feet
against them with no new code. Thousands of static colliders sit in the broadphase and never wake;
the cost is memory, not simulation.

Rendering: Bevy batches identical mesh-and-material pairs, so a few thousand tree entities are
already instanced and the cost is ECS iteration and culling rather than draw calls. `VisibilityRange`
(`bevy_camera-0.19.1/src/visibility/range.rs:80`) does LOD with cross-fading, which is what carries
a tree down to a billboard at distance. Grass-scale counts — 100k and up — need a custom pipeline
with a per-instance buffer, and there is no maintained Bevy plugin to borrow: every candidate is
four to seven versions behind (`warbler_grass` on 0.13, `bevy_procedural_grass` and `frosty_grass`
on 0.12).

---

## 13. Work steps

1. **Data model and codec.** `TerrainHeights` in `shared`: `u16` quantisation, `water_y`, the
   extent-and-spacing derivation, the midpoint fill, encode and decode. Unit tests on
   round-tripping and on the caps. No rendering, no physics.
2. **Collider.** One height field, spawned by server and client from the same shared data,
   replacing the ground plane in `level.rs`. Verify axis order and centring with the asymmetric
   ramp probe. This is where the design proves itself: if `Level` needs changes, something is
   wrong.
3. **The slope limit.** Add the normal test to `is_grounded`, retune against the existing crates
   and ramp. Before there are hills to be surprised by.
4. **Rendering.** `ExtendedMaterial` with one layer and no triplanar — enough to see the shape.
5. **Wire.** Terrain channel, baseline on join. A joining client sees the server's terrain.
6. **Maps.** Create, load and save on the server, with the caps and the name guard, and the menu in
   front of them. At the end of this step a client can make a map, everyone lands on it, and it
   survives a restart — with no sculpting in it yet.
7. **Sculpting.** Raise/lower first, then flatten, smooth, ramp. Gestures,
   `commit_tick + max_rollback_ticks`, per-tile rebuild, unsaved-state tracking in the menu.
8. **Placement.** Markers, the hotbar and its palette dialog, place/delete/rotate as gestures, and
   round start reading markers instead of `CRATES`, `VEHICLE_STARTS` and `spawn_point`.
9. **Undo.** The edit sequence number, the compare-and-swap, and the sculpt before-image.
10. **Water.** The surface mesh with its depth attribute, and an editor control for `water_y`.
11. **Derived texturing.** The full rule set, then triplanar, then tile break — in that order,
   measuring each.

Steps 1–3 are the ones that can invalidate the plan. Everything after them is addition.

---

## 14. Open questions

- **What the new-map dialog offers by default.** `HALF_EXTENT` is 250 m today, so a 512 m map at
  1 m spacing covers the current playable area and lands on the canonical 513² grid. At `f32` and
  512 m from the origin, positional precision is still far under a millimetre, so the prediction
  scheme is indifferent to the choice — this is a content question, not a technical one. The
  vertical range needs a default too, and it decides how deep a valley can go.
- **Importing a height field.** Sculpting a whole map from the midpoint plane is tedious, and an
  import from a heightmap image is the obvious shortcut. It is a tool rather than a runtime
  feature — `bevy_heightmap` 0.19 is current and does exactly this, or it is ~150 lines against
  the `image` crate — but it needs a decision about who may run it, since it overwrites everything.
- **What water does to the play area.** Water everywhere at `water_y` means the sea does not stop
  where the height field does, which is what makes the horizon work. It also means a player can
  swim off the edge of the map into ground that no longer exists. Either the field extends well
  past anywhere reachable, or deep water is itself a boundary, or something pushes back — doing
  nothing means swimming into invisible glass.
- **What does built geometry become?** Terrain is the ground; everything constructed is a separate
  system this project has not designed. Today it is three crates and a ramp as constants in
  `level.rs`, and §7 turns two of those into markers — but a marker only says *where a crate
  starts*, not how a wall or a building gets made. Whether that ends up as a voxel grid, placed
  meshes, or something else is open. Terrain is deliberately indifferent to the answer, which is
  the whole point of the rule under The split; what is *not* indifferent is the editor, since a
  third edit mode would want to share the hotbar, the gesture channel and the menu that §2 and §7
  build.
- **LOD.** Tiles of 64×64 give it a unit to operate on, and nothing else here depends on when it
  arrives. It becomes urgent at the same time as a road ribbon, which is what a decimated tile
  visibly disagrees with.
