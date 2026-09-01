# Assets

Everything in this directory came from somewhere else. This file says where, because most of it is
licensed on the condition that it does.

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
