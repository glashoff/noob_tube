# Terrain plan — Noob Tube

Design notes for replacing the flat ground plane with sculptable outdoor terrain. Written
2026-09-01. **Nothing here is built.** This is a plan.

Today the world is [`level.rs`](shared/src/level.rs): a 500 m square plane, three crates and a
ramp, written as constants that both binaries build the same geometry from. Terrain replaces the
plane. It is a height field — one height per sample on a regular grid — that the server owns, that
clients receive and collide against, and that players sculpt from inside the running game.

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
nx, nz        sample counts per axis  — chosen when the map is created
spacing       metres between samples  — chosen when the map is created (default 1.0)
origin        world x/z of sample (0,0)
min_y, max_y  the range the quantised heights map onto
heights       nx*nz samples, u16, row-major (x fastest)
```

`u16` over an explicit `[min_y, max_y]` range gives 2 mm precision over a 128 m range, at half the
size of `f32`. Terrain never needs more than that.

**The quantisation is not a storage trick, it is the determinism mechanism.** Brushes round to the
`u16` lattice *inside* the brush, not on save (§5), so every machine lands on the same lattice
after every stroke and repeated small strokes cannot accumulate apart. Storing `f32` and rounding
on the way to disk would give up the property that makes it safe to replicate a sculpt as a gesture
rather than as a list of samples.

`nx` and `nz` are independent, so a map can be a strip rather than a square. With `spacing` they
fix the extent: `(nx-1) * spacing` by `(nz-1) * spacing` metres.

### Surface appearance is derived, and there is no splat map

The usual way to texture terrain is a *splat map*: a second grid, RGBA, one byte per layer weight,
painted by hand and stored alongside the heights. This design has none. Which texture appears at a
point is a function of that point's height, its slope, and its distance to any path drawn on the
map (§8) — evaluated per pixel in the fragment shader (§4).

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

**And the way back is not a splat.** Paths and roads are wanted eventually (§8), and they arrive as
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
it — a slope range, a height range, or a path kind. A layer names an ordinary material asset, so
texture loading and packaging are whatever the rest of the game already does.

### File format

One binary file per map, with a leading version byte:

```
u8   version
f32  spacing, origin_x, origin_z, min_y, max_y
u32  nx, nz
u8   layer_count      then per layer: str texture, f32 tile_scale, rule parameters
     heights          — nx*nz u16
```

Not RON and not JSON: 66k heights as text is some 400 KB of digits and a slow parse, and nobody
hand-edits a height field in a text editor, so the authoring source has no reason to be readable.

**The same bytes are the file and the wire baseline** (§2). One encoder, one decoder, serving disk
and network alike.

---

## 2. Ownership and wire

The server owns the authoritative height field. A joining client receives it once as a baseline;
every subsequent change arrives as an edit gesture (§5).

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

## 3. Physics

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

## 4. Rendering

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

Because the weights come from per-pixel values rather than a stored grid, transitions follow the
geometry exactly and can never be coarser than the surface itself. The cost is the mirror image:
transitions are *uniform*, and a shoreline has no hand-placed variation anywhere along it. Noise in
the rule is what buys that back.

---

## 5. In-game sculpting

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

## 6. Terrain edits and rollback

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

## 7. The one derivation that has to exist twice

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

**Paths are exempt from all of this** (§8). "Am I standing on a road?" is a pure function of the
path list, which lives in `shared` and is the same data on both sides — one call, no second
implementation, no approximation. The duplication above is the price of deriving from *height and
slope* specifically, and it applies only to the ground rules.

---

## 8. Paths and roads — later, and vector

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
  shader as §4's rules. The server never evaluates it, nothing can diverge, and it costs nothing.
- **The height half is a terrain gesture.** Cutting a level corridor changes the height field,
  which changes the collider, which changes where players can stand — so it falls under §5's
  determinism discipline and §6's `commit_tick + max_rollback_ticks` rule like any other sculpt. It
  is genuinely a different brush from flatten: flatten levels to one height, this levels to a
  *profile* sampled and smoothed along the path.

That the cut is required is not a nuisance, it is what makes the ribbon work: a corridor that is
level across and near-linear along is exactly what a coarse LOD reproduces without a visible gap.

The ribbon itself needs **no collider**, because it lies on ground that already collides. Bridges
and tunnels are out of scope for the same reason — the moment a ribbon leaves the surface it needs
a collider of its own, and that is a different feature wearing the same word.

---

## 9. Vegetation — sketch only

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

## 10. Work steps

1. **Data model and codec.** `TerrainHeights` in `shared`, `u16` quantisation, encode and decode,
   unit tests on round-tripping. No rendering, no physics.
2. **Collider.** One height field, spawned by server and client from the same shared data,
   replacing the ground plane in `level.rs`. Verify axis order and centring with the asymmetric
   ramp probe. This is where the design proves itself: if `Level` needs changes, something is
   wrong.
3. **The slope limit.** Add the normal test to `is_grounded`, retune against the existing crates
   and ramp. Before there are hills to be surprised by.
4. **Rendering.** `ExtendedMaterial` with one layer and no triplanar; then the derived weights,
   then triplanar, then tile break — in that order, measuring each.
5. **Wire.** Terrain channel, baseline on join, no edits yet. A joining client sees the server's
   terrain.
6. **Sculpting.** Raise/lower first, then flatten, smooth, ramp. Gestures, `commit_tick +
   max_rollback_ticks`, per-tile rebuild.
7. **Save.** The server writes the file; unsaved-state tracking.

Steps 1–3 are the ones that can invalidate the plan. Everything after them is addition.

---

## 11. Open questions

- **Map size.** `HALF_EXTENT` is 250 m, so a 512 m terrain covers the current playable area
  exactly. At `f32` and 512 m from the origin, positional precision is still far under a
  millimetre, so the prediction scheme is indifferent — this is a content question, not a technical
  one.
- **Where does a height field come from initially?** Sculpting a whole map from flat is tedious. An
  import path from a heightmap image is a tool rather than a runtime feature — `bevy_heightmap`
  0.19 is current and does exactly this, or it is ~150 lines against the `image` crate.
- **Is there water?** Several of the texturing rules want a water level to key off, and a sea that
  is real geometry rather than a painted band is its own piece of design. If there is no water,
  one of the four layers loses its rule and the shoreline case disappears.
- **Does built geometry stay `level.rs` constants?** Terrain does not care, but a sculpt mode will
  eventually want to sit beside a build mode that does not exist yet.
- **LOD.** Tiles of 64×64 give it a unit to operate on, and nothing else here depends on when it
  arrives. It becomes urgent at the same time as a road ribbon, which is what a decimated tile
  visibly disagrees with.
