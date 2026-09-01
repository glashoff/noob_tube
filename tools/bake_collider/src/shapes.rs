//! The candidate shapes, and the settings the baked one is made with.

use parry3d::math::Vector;
use parry3d::transformation::vhacd::{VHACD, VHACDParameters};
use parry3d::transformation::voxelization::FillMode;

use crate::model::Mesh;

/// Whether the voxelizer fills the inside of the model or keeps only its skin.
#[derive(Clone, Copy)]
pub enum Fill {
    /// Flood-fill: everything enclosed by the panels counts as vehicle.
    Solid,
    /// Surface only: the parts follow the panels and the cabin stays open.
    Skin,
}

/// One way of running the decomposition.
#[derive(Clone, Copy)]
pub struct Settings {
    pub name: &'static str,
    /// Roughly how many voxels the model is cut into. The single biggest knob: it sets how fine a
    /// feature — a wing mirror, a cage tube — can survive as its own part.
    pub resolution: u32,
    /// How much concavity a part may keep before it is split again. Lower splits more.
    pub concavity: f32,
    /// A ceiling, so that a bad setting fails loudly rather than by baking a thousand hulls.
    pub max_hulls: u32,
    pub fill: Fill,
}

/// What `bake` writes out.
///
/// Chosen by running `check` and reading the table it prints — see the README.
///
/// Solid rather than skin, on two counts. It measures better at every part count, which was not
/// the guess: a shell follows the panels but each of its convex parts still bridges the concavity
/// behind them, and the parts are thin enough that a grazing shot finds the gap between two of
/// them. And a shell is *hollow* — a player or a crate could end up inside the cabin, which is a
/// new kind of stuck for a fix that was only ever about decals.
///
/// `concavity` is what actually buys the accuracy here, not `resolution`: at 256 voxels the model
/// is already resolved finer than the error being chased, and it is the willingness to split a
/// part again that closes the last few centimetres. Past 0.002 the part count runs away for
/// tenths of a centimetre — see the `0.001` row.
pub const CHOSEN: Settings = Settings {
    name: "solid 256, 96, 0.002",
    resolution: 256,
    concavity: 0.002,
    max_hulls: 96,
    fill: Fill::Solid,
};

/// A candidate to score, with a name to print it under.
pub struct Candidate {
    pub name: String,
    pub hulls: Vec<Vec<Vector>>,
}

/// The shapes worth comparing: the two this replaces, and a sweep across the settings that matter.
pub fn candidates(mesh: &Mesh) -> Vec<Candidate> {
    let mut out = vec![
        Candidate { name: "the nominal box".into(), hulls: vec![box_at(Vector::ZERO, NOMINAL)] },
        Candidate { name: "sixteen slices".into(), hulls: slices() },
    ];
    for settings in SWEEP {
        out.push(Candidate {
            name: settings.name.into(),
            hulls: decompose(mesh, settings),
        });
    }
    out
}

/// The settings `check` sweeps: fill against skin, and resolution up until it stops paying.
const SWEEP: [Settings; 8] = [
    Settings { name: "solid 128, 24 hulls", resolution: 128, concavity: 0.01, max_hulls: 24, fill: Fill::Solid },
    Settings { name: "solid 256, 48 hulls", resolution: 256, concavity: 0.01, max_hulls: 48, fill: Fill::Solid },
    Settings { name: "solid 256, 96, 0.005", resolution: 256, concavity: 0.005, max_hulls: 96, fill: Fill::Solid },
    CHOSEN,
    Settings { name: "solid 256, 96, 0.001", resolution: 256, concavity: 0.001, max_hulls: 96, fill: Fill::Solid },
    Settings { name: "skin 256, 48 hulls", resolution: 256, concavity: 0.01, max_hulls: 48, fill: Fill::Skin },
    Settings { name: "skin 256, 96, 0.005", resolution: 256, concavity: 0.005, max_hulls: 96, fill: Fill::Skin },
    Settings { name: "skin 256, 96, 0.002", resolution: 256, concavity: 0.002, max_hulls: 96, fill: Fill::Skin },
];

/// Run the decomposition and keep the hulls' vertices.
pub fn decompose(mesh: &Mesh, settings: Settings) -> Vec<Vec<Vector>> {
    let parameters = VHACDParameters {
        resolution: settings.resolution,
        concavity: settings.concavity,
        max_convex_hulls: settings.max_hulls,
        fill_mode: match settings.fill {
            Fill::Solid => FillMode::FloodFill { detect_cavities: false },
            Fill::Skin => FillMode::SurfaceOnly,
        },
        ..Default::default()
    };
    let parts = VHACD::decompose(&parameters, &mesh.vertices, &mesh.triangles, true);
    parts
        .compute_exact_convex_hulls(&mesh.vertices, &mesh.triangles)
        .into_iter()
        .map(|(vertices, _)| vertices)
        // Fewer than four points is not a solid, and Avian would refuse it.
        .filter(|vertices| vertices.len() >= 4)
        .collect()
}

/// Half the box a vehicle was reckoned to be, from `BUGGY.half_extents`.
const NOMINAL: Vector = Vector::new(0.9, 0.4, 1.9);

fn box_at(centre: Vector, half: Vector) -> Vec<Vector> {
    let mut corners = Vec::with_capacity(8);
    for x in [-half.x, half.x] {
        for y in [-half.y, half.y] {
            for z in [-half.z, half.z] {
                corners.push(centre + Vector::new(x, y, z));
            }
        }
    }
    corners
}

/// The shape this replaces: sixteen boxes along the length, measured off the model by hand.
///
/// Kept only so that `check` can still print the row the README quotes. Nothing in the game uses
/// it any more, and if a second vehicle ever needs its own table this is not the way to give it one.
/// The columns are: from, to, half width, bottom, top.
const SLICES: [[f32; 5]; 16] = [
    [-1.900, -1.662, 0.519, -0.447, 0.086],
    [-1.662, -1.425, 0.685, -0.424, 0.166],
    [-1.425, -1.188, 0.751, -0.342, 0.230],
    [-1.188, -0.950, 0.829, -0.368, 0.307],
    [-0.950, -0.712, 0.864, -0.480, 0.202],
    [-0.712, -0.475, 0.906, -0.556, 0.235],
    [-0.475, -0.238, 0.792, -0.366, 0.175],
    [-0.238, 0.000, 0.628, -0.366, 0.045],
    [0.000, 0.237, 0.790, -0.366, -0.006],
    [0.237, 0.475, 0.906, -0.456, 0.154],
    [0.475, 0.712, 0.837, -0.554, 0.155],
    [0.712, 0.950, 0.839, -0.365, 0.111],
    [0.950, 1.188, 0.669, -0.312, 0.192],
    [1.188, 1.425, 0.828, -0.316, 0.112],
    [1.425, 1.663, 0.824, -0.309, 0.260],
    [1.663, 1.900, 0.714, -0.221, 0.278],
];

fn slices() -> Vec<Vec<Vector>> {
    SLICES
        .iter()
        .map(|[from, to, half_width, bottom, top]| {
            box_at(
                Vector::new(0.0, (bottom + top) / 2.0, (from + to) / 2.0),
                Vector::new(*half_width, (top - bottom) / 2.0, (to - from) / 2.0),
            )
        })
        .collect()
}
