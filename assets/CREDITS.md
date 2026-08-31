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
