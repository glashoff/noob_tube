# Assets

Everything in this directory came from somewhere else. This file says where, because most of it is
licensed on the condition that it does.

## Mixamo Swat — the player character

| | |
|---|---|
| Files | `characters/swat.glb` and `anims/*.glb` — **not in this repository** |
| Source | https://www.mixamo.com — character "Swat", plus the "Rifle 8-Way Locomotion Pack" |
| Author | Mixamo, an Adobe company |
| Licence | Free with an Adobe account. Use is permitted; **redistribution is not.** |
| Changes | Converted from FBX to glTF with `FBX2glTF`. Mixamo's own file names are kept, because the clip names are taken from them. |

**This is why `tools/setup-assets` exists.** Everything else under `assets/` is here; this cannot be,
and a glTF conversion is still the same animation data, so converting it changes nothing about the
licence. Each developer downloads the two packs and runs:

```bash
tools/setup-assets                 # defaults to ../webgame/assets
tools/setup-assets ~/mixamo        # or wherever the packs are
```

Nothing in the game requires the result — see `client/src/character.rs`, where a checkout without
it falls back to the Quaternius character below. What it costs to be without it is 8-way
locomotion: the fallback can only walk forward.

The two skeletons agree on all 70 bone paths, which is the failure that is otherwise silent and
is worth re-checking after any re-download:

```bash
tools/glb rigs assets/characters/swat.glb assets/anims/idle.glb
```

The three paths it reports unmatched are the character's own mesh nodes, which no clip animates.

## Universal Base Characters — the player bodies

| | |
|---|---|
| Files | `characters/Superhero_{Male,Female}_FullBody.gltf` and the `.bin` and `T_*.png` beside them, plus `characters/Hair_*.gltf`, `characters/Eyebrows_*.gltf` |
| Title | Universal Base Characters (Standard) |
| Author | Quaternius — https://quaternius.com |
| Licence | CC0 1.0 Universal, per `License_Standard.txt` in the download |
| Changes | Only the `Godot - UE` glTF variant is here; the FBX and the duplicate "Origin at 0" hairstyles are not. Two image URIs were repointed — see below. The light-skin texture was renamed from `T_Superhero_Male_Ligh.png`. |

**Two references in the archive point at files it does not contain.** Both bodies name
`T_Eye_Normal_png.png` and `T_Hair_1_Normal_png.png`; what ships is `T_Eye_Normal.png` and
`T_Hair_1_Normal.png`. The `.gltf` files here were edited to name the files that exist. Without
that, Bevy fails to load the eye and hair normal maps and says so at run time rather than at build
time. If the kit is ever re-downloaded, check whether this is still needed:

```bash
tools/glb info assets/characters/Superhero_Male_FullBody.gltf
```

Two skin tones ship for each body — `_Dark` and `_Light` — and only the dark one is wired into the
material. The other is a texture swap away; see the README under "Dressing a character".

## Universal Animation Library 1 and 2 — the clips

| | |
|---|---|
| Files | `animations/universal_animation_library_1.glb`, `animations/universal_animation_library_2.glb`, `characters/Mannequin_F.glb` |
| Title | Universal Animation Library / Universal Animation Library 2 (Standard) |
| Author | Quaternius — https://quaternius.com |
| Licence | CC0 1.0 Universal, per `License.txt` in each download |
| Changes | Renamed from `UAL1_Standard.glb` and `UAL2_Standard.glb`. The `_RM` variants, which have root motion baked in, are deliberately not here — world position is server-driven. The Unity FBX variants are not here either. |

43 clips each, and both carry the male `Mannequin` mesh as well, so the first file is a body and a
clip library at once. `Mannequin_F.glb` is the female body from the second kit's own folder.

**All five files share a bit-identical 65-joint rig**, matched on full bone paths rather than on
names — which is what Bevy binds on. That is checkable, and worth re-checking after any
re-download:

```bash
tools/glb rigs assets/animations/*.glb assets/characters/*.glb assets/characters/*.gltf
```

The naming is Unreal Engine's mannequin skeleton (`root`, `pelvis`, `spine_01`, `hand_l`), which is
a de-facto standard well beyond Quaternius — so a garment or a body built for that rig by anybody
drops in without retargeting.

**On the licence.** The README records that Quaternius replaced CC0 with the Quaternius Asset
License v1.0 on 28 August 2026, which forbids redistributing the assets themselves. The archives
downloaded here were built on 17 June 2026 (both animation libraries) and 18 August 2025 (the base
characters), and each carries a CC0 notice in writing; QAL §7 says the version in force at download
time governs. That is the basis on which these files are in the repository, and it is a reading of
the terms rather than advice — it is written down here so that it can be revisited rather than
rediscovered.

## Halo Warthog — the vehicle model

| | |
|---|---|
| File | `models/warthog.glb` |
| Title | Halo Warthog |
| Author | pinto36 — https://sketchfab.com/pinto36 |
| Source | https://sketchfab.com/3d-models/halo-warthog-bd3403bc06884260ac31d0e98eed81e4 |
| Licence | CC BY 4.0 — http://creativecommons.org/licenses/by/4.0/ |
| Changes | Renamed from `halo_warthog_lowres.glb`. Not otherwise modified; it is rotated and scaled at load time rather than in the file. |

None of that was typed from a web page: the glTF carries it in `asset.extras`, which is what
Sketchfab writes on export, and it can be read back out of the file at any time.

CC BY 4.0 allows redistribution, including here, provided the credit above travels with it. What it
does not do is settle the Warthog *design*, which is Microsoft's — pinto36 can license the mesh they
built and cannot license what it depicts. That is the usual position of every fan model and is worth
knowing rather than discovering.

The high-resolution version of the same model exists (74 MB against 13 MB) and differs only in
texture size. The smaller one is here because a debug build reloads it on every run.

## Mounted machine gun — the gun on the cross-beam

| | |
|---|---|
| File | `models/machine_gun.glb` |
| Title | Mounted machine gun. |
| Author | bonk.iopro77 — https://sketchfab.com/bonk.iopro77 |
| Source | https://sketchfab.com/3d-models/mounted-machine-gun-ae283e09f10f406fa6e6f6e37cfb3cc4 |
| Licence | **CC BY-ND 4.0** — http://creativecommons.org/licenses/by-nd/4.0/ |
| Changes | Renamed from `mounted_machine_gun.(1).glb`. Not otherwise modified; it is rotated, scaled and placed at load time rather than in the file. |

Read out of `asset.extras` in the file, like the Warthog's.

**This one is not ours to pass on, and that is a different answer from the Warthog's.** The `ND` is
NoDerivatives: the work may be shared verbatim with credit, but adapted material may not be
distributed at all. Whether putting an unmodified model into a game is "adapting" it is genuinely
unsettled — the model file itself is untouched, which is the strongest argument that it is not, and
Creative Commons' own guidance is that including an ND work in a larger work can create an
adaptation depending on how it is used.

Because the answer is uncertain rather than clearly yes, this file is **not** let through
`.gitignore`. Each developer brings their own copy from the source link above, and nothing in the
game requires it — see `client/src/vehicle.rs`, where a vehicle without it simply has no gun. If it
is ever wanted in the repository, that is a decision to make deliberately and with the licence in
front of you, not by adding a line.

The higher-resolution version (8.4 MB against 4.4 MB) is the same geometry to the vertex and differs
only in texture size.

---

## Ground textures — `assets/textures/`

| | |
|---|---|
| Source | **ambientCG** by Lennart Demes — https://ambientcg.com |
| Packs | `Grass001`, `Ground048`, `Rock020`, each `1K-PNG` |
| Licence | **CC0 1.0 Universal** — https://creativecommons.org/publicdomain/zero/1.0/ |
| Changes | None. The colour map of each pack, under ambientCG's own file name; the other maps of each pack were not taken. |

The original page for any pack is `https://ambientcg.com/view?id=<pack>` — for the first of them,
<https://ambientcg.com/view?id=Grass001>.

CC0 is a public domain dedication rather than a licence with conditions: the material may be used,
modified, redistributed and sold, for any purpose, without permission or attribution. This entry
exists because saying where something came from is decent practice, not because CC0 asks for it. It
is also why these three files are let through `.gitignore` where the mounted gun above is not — the
question the `ND` raises does not arise here at all.

**These are the three `webgame` uses on its own hills map**, at the same 4 m tile scale, so that the
ground of the two games reads as the same place. Which of them shows at a point is decided by slope
alone — see `default_layers` in `shared/src/terrain.rs`.

**Colour maps only.** Each pack also ships normal, roughness, displacement and ambient-occlusion
maps; the normal map alone is six megabytes, and there is nothing to hang one on until the ground
mesh carries tangents. Roughness is a constant per layer for now. Both are additions rather than
corrections when they come.
