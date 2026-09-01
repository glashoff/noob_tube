# Terrain plan — Noob Tube

Design notes for adding natural outdoor terrain as a second geometry layer. Written 2026-09-01.
**Nothing here is built.** This is a plan.

It is a port of [`webgame/terrain.md`](../webgame/terrain.md), where the same system is designed
*and implemented* in TypeScript against three.js and Rapier. That document is the source of every
decision this one does not argue from scratch, and it is worth reading first: it records what the
implementation settled, which is the part no plan can predict.

**The deviations are the interesting part, and there are three.** Two follow from the engine — Bevy
and Avian make different things easy than three.js and Rapier — and one follows from a single fact
about how this game's maps get made:

> **Nothing is ever painted by hand. Every surface property is derived.**

That removes the splat map entirely, and with it about 85% of the wire format, the paint tool, the
texture hotbar, the weight-renormalisation invariant, and one of the two places webgame's design
still has to worry about determinism. It is the largest simplification available and it is bought
with a single sentence of scope.

---

## The split

Two systems, two jobs, no overlap in responsibility:

| | Built geometry | Terrain |
|---|---|---|
| Represents | everything **built** — buildings, walls, ramps, props | the **ground** — hills, slopes, valleys |
| Topology | arbitrary 3D (caves, overhangs, floors) | single-valued surface `y = f(x,z)` |
| Editable in-game | (undecided — see below) | yes, by sculpt brush |

**The rule that avoids every ambiguity: terrain is the ground, blocks are everything built.** There
is no boolean operation between them. A block below the terrain surface is hidden inside it; a
block above floats on it. Neither system ever asks the other what it contains.

Accepting the single-valued limitation is the whole point — no caves, no overhangs, no natural
arches. Those are what built geometry is for.

**This is why terrain can start now.** How this project handles the built layer is not yet
decided, and it does not need to be: the rule above is what makes the two independent. If a future
design for the built layer *breaks* that rule — if anything ever has to ask terrain what is inside
it — this document's cost estimates stop holding, and that is the one change that would force a
re-read.

Today the built layer is [`level.rs`](shared/src/level.rs): a flat plane, three crates and a ramp,
written as constants. Terrain replaces the plane. Nothing else in that file has to move.

### Why not voxels for the ground

Run the numbers from webgame's format. Chunks of 16³ cells at `Int16` are 8 KB each. A 256×256 m
area at 0.25 m cells is 1024×1024 columns; a surface with relief passes through 2–4 chunk layers,
so roughly 8–16k chunks = **64–128 MB**, plus a mesh of millions of quads with no practical LOD.
The same area as a heightmap at 1 m spacing is 257×257 samples — **132 KB** as `u16`. Three orders
of magnitude.

The visual argument is as strong: at 0.25 m quantisation a natural hillside reads as a staircase.
The data model is wrong for the subject, not merely expensive.

### Why not a mesh authored in Blender

Static terrain could be a mesh and a `Collider::trimesh_from_mesh` today with almost no new code.
Rejected for three reasons, in increasing order of weight:

1. It cannot be edited in-game.
2. A trimesh collider is strictly worse than a heightfield — larger, slower to query, and it has
   to be rebuilt wholesale where a heightfield can be rebuilt per tile.
3. **The server would need the mesh.** `trimesh_from_mesh` takes a `Mesh`, which means asset
   loading and `bevy_render` types on a binary whose whole `Cargo.toml` is arranged to keep wgpu
   out. `Collider::heightfield` takes raw floats and nothing else. This is the decisive one.

---

## 1. Data model

### Height field

```
nx, nz      sample counts per axis  — chosen when the map is created
spacing     metres between samples  — chosen when the map is created (default 1.0)
origin      world x/z of sample (0,0)
min_y, max_y  the range the quantised heights map onto
heights     nx*nz samples, u16, row-major (x fastest)
```

`u16` over an explicit `[min_y, max_y]` range gives 2 mm precision over a 128 m range at half the
size of `f32`. Terrain never needs more.

**The quantisation is not a storage trick, it is the determinism mechanism.** Brushes round to the
`u16` lattice *inside* the brush, not on save (§5), so every machine lands on the same lattice
after every stroke and small repeated strokes cannot accumulate apart. Storing `f32` and rounding
later would give up the property that makes gesture replication safe.

`nx` and `nz` are independent, so a map can be a strip rather than a square. Together with
`spacing` they fix the extent: `(nx-1) * spacing` by `(nz-1) * spacing` metres.

### No splat map

webgame stores a four-layer RGBA splat map on its own grid, at twice the height resolution, as
authored content. Its argument for doing so is explicit and good:

> The splat is content, exactly like the heights — never derived at load time. […] This is the
> decisive property: painting is then the *normal case*, not an exception layered over a
> generator. There is no rule that a painted terrain has to contradict, no override list to keep
> in sync with a derivation, and no derivation running outside the editor that could disagree
> between two machines.

**Every clause of that argument is about painting, and here nobody paints.** A derivation cannot
contradict authored data that does not exist; there is no override list because there are no
overrides; and the derivation cannot disagree between two machines because — see §4 — it runs in a
fragment shader on the client and the server never evaluates it at all.

So the surface appearance is a function of height, slope and position, evaluated per pixel. What
this deletes, in one list, because the savings are the justification:

- 1.03 MB of raw splat per 256 m map, and the lossless packing written to pay for it
- the paint brush, its layer hotbar, and the "weights always sum to 255" invariant
- the `generate splat` editor action and its `acos`, webgame's one remaining determinism exception
- the splat resolution header field, and the 4× multiplier on the edit token bucket that a
  higher-resolution splat grid forces
- `MAX_TERRAIN_EDIT_SAMPLES`, which exists in webgame only because a legal 64 m brush reaches four
  million *splat* texels

The baseline drops from 196 KB to the heights alone.

**What it costs**, stated plainly so the trade is on the record: a map can never have a footpath, a
patch of moss, or a bare spot that the height and slope do not explain. Every visual distinction
must be expressible as a rule. If that turns out to be too little, the way back is to add a splat
as an *optional override layer* over the derivation — absent by default, present only on maps that
have one. That is a strictly larger design than webgame's and should not be attempted before the
rules have actually proved insufficient.

### Layers

Up to four texture layers, each `{ texture: String, tile_scale: f32 }`, plus the rule that selects
between them. A layer is the same kind of id an ordinary material asset uses, so texture
discovery and packaging are whatever the rest of the game already does.

### File format

One binary file per map, written with a leading version byte:

```
u8   version
f32  spacing, origin_x, origin_z, min_y, max_y
u32  nx, nz
u8   layer_count      then per layer: str texture, f32 tile_scale, rule parameters
     heights          — nx*nz u16, packed
```

Not RON and not JSON: 66k heights as text is ~400 KB of digits and a slow parse. Nobody hand-edits
terrain in a text editor, so the authoring source stays binary too.

**The same bytes are both the file and the wire baseline** (§2). One encoder, one decoder, serving
disk and network alike.

---

## 2. Runtime entity and wire

The server owns the authoritative height field. A joining client receives it once as a baseline;
subsequent changes arrive as an edit stream (§5).

**The client never loads the `.terrain` file itself, and it must not.** The moment terrain is
editable, the file on disk is the *last saved* state, not the current one — a client loading it
directly would see, and physically collide with, different ground than everyone else for as long
as anybody holds unsaved edits.

### Not through component replication

This is the deviation from the obvious Bevy approach, and it matters.

Terrain is resource-shaped, not entity-shaped, and a 196 KB — now ~130 KB — baseline has no
business going through a path that diffs components per tick. It also must not be `Predicted` or
`Interpolated`: there is exactly one authoritative height field and no timeline on which a second
version of it makes sense.

Use a dedicated channel instead, exactly as [`protocol.rs`](shared/src/protocol.rs) already splits
`EffectsChannel` off from replication for the opposite reason:

- `ChannelMode::OrderedReliable(ReliableSettings)`
  (`lightyear_transport-0.29.0/src/channel/builder.rs:553`)
- a `TerrainBaseline` message sent to a client on join
- a `TerrainEdit` message per committed gesture, broadcast to everyone

Ordered matters: edits are not commutative. Raise-then-smooth and smooth-then-raise are different
terrains.

Client-side the height field lives in a resource, not a component, and the client needs a small
queue for edits that arrive before the baseline has been applied. webgame needed the same thing
(`pendingVoxelRpcs`) and the reasoning carries over unchanged.

---

## 3. Physics

One `Collider::heightfield` per collider tile.

```rust
// avian3d-0.7.0/src/collision/collider/parry/mod.rs:1128
pub fn heightfield(heights: Vec<Vec<Scalar>>, scale: Vector) -> Self
```

**One of webgame's three Rapier traps does not exist here.** Avian takes a nested `Vec<Vec<Scalar>>`
and derives the counts from the structure — it even asserts that every row has the same length — so
the "`nrows`/`ncols` are segment counts, and passing sample counts panics inside the WASM" trap is
gone. Rust's type system ate it.

**The other two are still live, and both still fail silently.**

- **Axis order.** Avian documents it: "The number of rows indicates the number of subdivisions
  along the `X` axis, while the number of columns indicates the number of subdivisions along the
  `Z` axis." So the storage layout `iz * nx + ix` must be fed in as `heights[ix][iz]`. Getting this
  wrong transposes the terrain about its diagonal, with no error of any kind — a ramp authored
  along +X comes out running along +Z.
- **Centring.** parry's heightfield is centred on its origin, so the body goes at the tile's
  *centre*, not at its min corner, which is where `origin` points. Avian does not document this;
  it wraps `SharedShape::heightfield` directly, so parry's convention is what applies.

Both are inherited from parry rather than reasoned from first principles, which means they should
be **verified against the installed crate, not read off the docs** — webgame's method transfers
verbatim: probe a live physics world with an intentionally asymmetric ramp, flat along one axis and
sloped along the other. It is the only shape that catches a transpose.

### The two traps this repository already knows about

From [`physics.rs`](shared/src/physics.rs), both of which apply to the terrain collider:

- It needs `RigidBody::Static`, or `MoveAndSlide` cannot see it — its collider query is filtered
  `With<ColliderOf>`, so a bare `Collider` is invisible to sweeps while staying visible to
  `cast_ray`. A terrain you can shoot but walk through is the failure mode.
- It needs `CollisionLayers::new(Layer::Level, LayerMask::ALL)`, or `Level`'s queries — which ask
  only about `Layer::Level` — will not find the ground.

Everything downstream is then free, and that is the single biggest reason this design is
affordable: `Level`'s ray casts and capsule sweeps go through Avian already, so ground probing,
sliding, crouch clearance and hit detection work on terrain without a line of change.

### The gap terrain opens: there is no slope limit

`Level::is_grounded` is a downward ray within `GROUND_SNAP_DIST` and **nothing else**
([`physics.rs:174-195`](shared/src/physics.rs#L174-L195)). There is no test on the surface normal.

On a flat plane with cuboid crates that is invisible — every walkable surface is horizontal and
every wall is vertical, so a ray that hits is a floor by construction. Terrain breaks the
assumption: a 70° cliff face returns a hit within snap distance and the player walks up it.

webgame has the test (`normal1.y > 0.7`, so about 45.6°) and the authoring consequence that follows
from it — terrain steeper than the limit is a cliff players slide off, which is good for framing a
map and bad by accident. **Adding the normal test is part of this work, not a follow-up**, and it
has to land in `is_grounded` before the first sculpted hill, because it changes movement on every
existing surface too. The sculpt overlay should then tint anything past the limit, so unclimbable
ground is visible while it is being made rather than discovered during play.

---

## 4. Rendering

`ExtendedMaterial<StandardMaterial, TerrainMaterial>`
(`bevy_pbr-0.19.1/src/extended_material.rs:145`) is the direct counterpart of the
`onBeforeCompile` patch webgame uses on three.js's `MeshStandardMaterial`, and it is chosen for the
same reason: lights, shadows, fog and the whole PBR path keep working, and only the surface
appearance is replaced. Writing a bare `Material` from scratch would give all of that up.

**Mesh:** a regular grid split into tiles of 64×64 samples, so frustum culling has a unit to work
with and a later LOD has something to operate on. Normals from central differences of the height
field — exact and cheap, never from the triangle mesh.

**Fragment shader**, per pixel, with no splat texture to sample:

1. Derive layer weights from world height and the surface normal. The whole rule set is something
   like *steeper than 35° → rock; below water level → sand; else grass*, with smooth transitions.
2. Sample each contributing layer's colour/normal/roughness at its own `tile_scale` and blend.
3. **Triplanar projection blended in above a slope threshold.** Every heightmap stretches its
   texture to mush on steep faces; flat ground does not need triplanar, so it costs only where it
   pays.
4. **Tile break** — a second, much larger-scale sample multiplied over the result, to kill visible
   repetition on open ground.

Deriving the weights costs a handful of ALU per pixel and saves a texture fetch, so this is
*cheaper* than sampling a stored splat, not merely simpler. Worst case is still ~15 texture samples
for one surface, which needs measuring; a reduced variant (two layers, no triplanar) belongs with
the other quality settings.

Because the weights come from interpolated per-pixel values rather than a stored grid, transitions
follow the geometry exactly and cannot be lower-resolution than the surface. The visible cost is
the opposite one: transitions are *uniform*, and a shoreline has no hand-placed variation. Noise in
the rule is what buys that back.

---

## 5. In-game sculpting

A sculpt mode with the tools, in the order they earn their place:

1. **Flatten** — level the terrain under the brush to a picked height, with a height eyedropper.
   Mandatory, not optional: built structures have flat footprints and unsculpted ground does not,
   so without flatten every building gets gaps or buried corners.
2. **Raise / lower** — with distance falloff for soft edges.
3. **Smooth** — pull each sample toward its neighbours' mean.
4. **Ramp** — two points, linear interpolation between them.

There is no paint tool and no generate-splat action. That is the whole of §1's simplification
showing up as four tools instead of six.

### Network shape

- `TerrainEdit { op, x, z, radius, strength, height, apply_at_tick }` — a **gesture**, not a list
  of samples. Both sides recompute the same result deterministically. This is the same principle
  `step_players` rests on, one level up: send the input, not the outcome.
- Server-validated before broadcast — radius cap, coordinate range, finite strength.
- Token bucket over touched samples, charging an **upper bound computed before anything is
  applied**, rounded up on both ends of each axis. Undercharging would let a client buy a larger
  stroke than it pays for.

### Determinism is bought by avoiding transcendental functions, not by hoping

The brushes use only `+ - * /`, `round`, `min`/`max` and comparisons. Every one of those is exactly
specified by IEEE-754. The radial falloff is a polynomial in the **squared** distance and never
takes a square root; `powf`, `exp`, `sin`/`cos` and `hypot` go through `libm`, which is not
required to be correctly rounded and may differ in the last ulp between platforms.

Rust is slightly safer here than JavaScript — there is no second engine with its own maths library
in the same process — but the hazard is the same one and so is the discipline. Do not reach for
`f32::mul_add` either: it is a *different* result from `a * b + c`, exactly and deliberately, and
whether it compiles to one instruction or two is a platform property.

**Smooth needs a snapshot.** Reading neighbours while writing in place makes the result depend on
iteration order, and a sample near the rim would be smoothed against already-smoothed neighbours.
Copy the affected box first; samples outside it are read live, which is correct precisely because
nothing outside the box is written.

An edit dirties tiles, and a dirty tile rebuilds its mesh and its collider. Tile the collider on
the same grid as the mesh, so one brush stroke rebuilds one heightfield rather than the whole
terrain.

---

## 6. Terrain edits and rollback

**This problem does not exist in webgame and is the one genuinely new piece of design work.**

`Level` is a `SystemParam` that reads Avian's *current* spatial state. There is no seam where a
replay could be handed historical terrain. But lightyear's rollback re-runs `FixedMain` for ticks
N−k through N, and if a terrain edit was applied inside that window, every replayed tick — including
the ones that happened before the edit — walks on the new ground.

The failure is small (a player is corrected by however much the ground moved under them) and rare
(edits are rare relative to ticks), so *ignoring it* is a defensible position. It is not the one
this document recommends, because the fix is nearly free:

> **Apply every terrain edit at `commit_tick + max_rollback_ticks`.**

Then no rollback window can ever straddle an edit, by construction. The server stamps the tick, the
client applies it at the same one, and the sculptor waits a fraction of a second to see their own
stroke — which in an editor nobody notices, and which is the same trick as choosing the tick
deliberately in `lag_compensation.rs`.

The collider rebuild must also be idempotent per tile, since a rollback can re-run the frame that
triggers it.

---

## 7. The one derivation that has to exist twice

Surface classification — *what am I standing on* — is needed on the CPU for footstep sounds, impact
decals and anything else that cares about the material under a player. The shader's copy is WGSL
and runs per pixel; the gameplay copy is Rust and runs per event.

That is precisely the hazard [`simulation.rs`](shared/src/simulation.rs) warns about — "two copies
of the same six lines is precisely how that stops being true" — and here it cannot be avoided,
because one copy has to be a shader.

Two mitigations, and the second is the real one:

- Keep the rule small enough that two implementations cannot plausibly drift: thresholds on
  `normal.y` and world `y`, and nothing else. The noise that breaks up transitions visually belongs
  only in the shader copy, where a disagreement is invisible.
- **Accept that the CPU copy is approximate.** Its worst case is a footstep that sounds like grass
  half a metre into the sand. Collision never consults it, so a divergence cannot affect the
  simulation. This is the same reasoning webgame applies to its `generate splat` exception, and it
  is why that exception was acceptable there too.

---

## 8. Vegetation — sketch only

webgame's terrain.md has a 1400-line design for this (§9). None of it is ported here yet, and this
section is deliberately three paragraphs rather than an outline of that one.

The load-bearing property, and the reason vegetation is cheap in a game shaped like this one:
**trees are derived, not replicated.** A seed plus the height field generates the same forest on
every machine, the same way `MOVING_CRATES` is a shared constant from which both sides build the
same entities. Ten thousand trees cost zero bandwidth. Rejecting hand-placement here is the same
decision as rejecting hand-painting in §1, and it buys the same thing.

Trunks are static colliders on `Layer::Level`, so `Level`'s existing queries stop bullets and feet
against them without new code. Thousands of static colliders sit in the broadphase BVH and never
wake; the cost is memory, not simulation.

Rendering: Bevy batches identical mesh+material pairs, so a few thousand tree entities are already
instanced and the cost is ECS iteration and culling rather than draw calls. `VisibilityRange`
(`bevy_camera-0.19.1/src/visibility/range.rs:80`) does LOD with cross-fading, which is what carries
a tree down to a billboard at distance. Grass-scale counts — 100k+ — need a custom pipeline with a
per-instance buffer; there is no maintained Bevy plugin for that (every candidate is four to seven
versions behind: `warbler_grass` 0.13, `bevy_procedural_grass` 0.12, `frosty_grass` 0.12).

---

## 9. Work steps

1. **Data model and codec.** `TerrainHeights` in `shared`, `u16` quantisation, encode/decode, unit
   tests on round-tripping. No rendering, no physics.
2. **Collider.** One heightfield, spawned by the server and the client from the same shared data,
   replacing the ground plane in `level.rs`. Verify axis order and centring with the asymmetric
   ramp probe. This is where the design proves itself: if `Level` needs changes, something is
   wrong.
3. **The slope limit.** Add the normal test to `is_grounded` and retune against the existing crates
   and ramp. Do this before there are hills to be surprised by.
4. **Rendering.** `ExtendedMaterial`, one layer, no triplanar. Then the derived weights, then
   triplanar, then tile break — in that order, measuring each.
5. **Wire.** Terrain channel, baseline on join, no edits yet. A client that joins sees the server's
   terrain.
6. **Sculpting.** Raise/lower first, then flatten, smooth, ramp. Gestures, `commit_tick +
   max_rollback_ticks`, per-tile rebuild.
7. **Save.** Server writes the file; unsaved-state tracking.

Steps 1–3 are the ones that can invalidate the plan. Everything after them is addition.

---

## 10. Open questions

- **Map size.** 256 m was webgame's working figure. `HALF_EXTENT` here is 250 m, so a 512 m terrain
  would cover the existing playable area. At `f32` and 512 m from the origin the precision is still
  well under a millimetre, so the prediction scheme is unaffected either way; this is a content
  question, not a technical one.
- **Does the built layer stay `level.rs` constants?** Terrain does not care, but the sculpt-mode UI
  will want to sit beside a build mode that does not exist yet.
- **Water.** webgame's §10 makes the sea real geometry rather than a painted band, and the terrain
  rules reference a water level. If there is no water here, one of the four layers loses its rule.
- **Where does the height field come from initially?** Sculpting from flat is tedious for a whole
  map. An import path from a heightmap PNG is a tool, not a runtime feature — `bevy_heightmap` 0.19
  is current and does exactly this, or it is ~150 lines with the `image` crate.
