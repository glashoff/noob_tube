//! Measuring a candidate shape against the triangles it stands in for.
//!
//! The question a bullet hole asks is simple: the shot stopped *here*, is there bodywork here? So
//! a ray that is known to hit the model is fired at the candidate, and what is measured is the
//! distance from where the candidate stopped it to the **nearest point on the real triangles** —
//! which is what the eye judges, a decal sitting a hand's width off the panel.
//!
//! Not the distance along the ray, which was the first thing tried and is a different and much
//! more flattering-or-damning quantity: a shot grazing the wing can stop two metres early along
//! its own line while sitting three centimetres off the paint. Nobody sees along the ray.
//!
//! A shape that does not stop the ray at all lets a shot through a vehicle that was visibly hit.
//! That is worse than any distance, so it is counted separately rather than averaged in.

use parry3d::math::Vector;
use parry3d::query::{PointQuery, Ray, RayCast};
use parry3d::shape::{ConvexPolyhedron, TriMesh};

use crate::model::Mesh;

/// How many rays that actually hit the model each score is made of.
///
/// Large enough that the ninetieth percentile is steady to a millimetre between seeds, which was
/// checked by running it at several; small enough that a sweep of a dozen candidates is a coffee
/// rather than a lunch.
pub const RAYS: usize = 20_000;

/// The seed. Fixed, so that two runs of `check` can be compared to each other at all.
const SEED: u64 = 0x1d7c_93fd_6760_425e;

/// What a shape scores against the truth. Distances in metres, fractions in 0..1.
pub struct Score {
    /// How far short of the bodywork a shot stops, at the middle ray.
    pub median: f32,
    pub ninetieth: f32,
    pub worst: f32,
    /// The share of hits that stop more than 5 cm clear of the panel — the ones that read as
    /// floating rather than as a hole in the paint.
    pub hanging: f32,
    /// The share that miss the shape entirely although the model was hit.
    pub through: f32,
}

/// The model's own triangles, and the rays to fire at them.
pub struct Truth {
    mesh: TriMesh,
    rays: Vec<Ray>,
}

impl Truth {
    pub fn new(mesh: &Mesh) -> Result<Self, String> {
        let triangles = TriMesh::new(mesh.vertices.clone(), mesh.triangles.clone())
            .map_err(|why| format!("the model does not make a mesh: {why}"))?;
        let (mut low, mut high) = (Vector::splat(f32::MAX), Vector::splat(f32::MIN));
        for vertex in &mesh.vertices {
            low = low.min(*vertex);
            high = high.max(*vertex);
        }
        let reach = (high - low).length();

        let mut random = Random(SEED);
        let mut rays = Vec::with_capacity(RAYS);
        // Bounded rather than `while`: a shape whose rays almost never connect would otherwise
        // spin here for ever instead of saying so.
        for _ in 0..RAYS * 100 {
            if rays.len() == RAYS {
                break;
            }
            let direction = random.direction();
            let at = low + (high - low) * random.unit_cube();
            let ray = Ray::new(at - direction * reach, direction);
            if triangles.cast_local_ray(&ray, reach * 3.0, true).is_some() {
                rays.push(ray);
            }
        }
        if rays.len() < RAYS {
            return Err("could not find enough rays that hit the model".into());
        }
        Ok(Self { mesh: triangles, rays })
    }

    pub fn score(&self, hulls: &[Vec<Vector>]) -> Score {
        let shapes: Vec<ConvexPolyhedron> = hulls
            .iter()
            .filter_map(|hull| ConvexPolyhedron::from_convex_hull(hull))
            .collect();

        let mut gaps = Vec::with_capacity(self.rays.len());
        let mut through = 0usize;
        for ray in &self.rays {
            let hit = shapes
                .iter()
                .filter_map(|shape| shape.cast_local_ray(ray, f32::MAX, true))
                .fold(f32::MAX, f32::min);
            if hit == f32::MAX {
                through += 1;
                continue;
            }
            let stopped = ray.point_at(hit);
            // `solid: false` on purpose. A point inside the bodywork still has a nearest panel,
            // and a hole sunk a centimetre into the wing is as good as one on it — what is being
            // measured is how far from the metal the decal ends up, in either direction.
            let nearest = self.mesh.project_local_point(stopped, false).point;
            gaps.push(stopped.distance(nearest));
        }
        gaps.sort_by(f32::total_cmp);

        let at = |fraction: f32| gaps[((gaps.len() - 1) as f32 * fraction) as usize];
        Score {
            median: at(0.5),
            ninetieth: at(0.9),
            worst: *gaps.last().unwrap_or(&0.0),
            hanging: gaps.iter().filter(|gap| **gap > 0.05).count() as f32 / gaps.len() as f32,
            through: through as f32 / self.rays.len() as f32,
        }
    }
}

/// xorshift64*. Here because a comparison is only a comparison if it repeats, and pulling in a
/// random-number crate to fire the same twenty thousand rays twice would be a strange trade.
struct Random(u64);

impl Random {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u32 << 24) as f32
    }

    fn unit_cube(&mut self) -> Vector {
        Vector::new(self.next(), self.next(), self.next())
    }

    /// Uniform on the sphere, by the usual z-then-angle construction rather than by rejecting
    /// points in a cube — no loop, and no bias towards the corners.
    fn direction(&mut self) -> Vector {
        let z = self.next() * 2.0 - 1.0;
        let angle = self.next() * core::f32::consts::TAU;
        let ring = (1.0 - z * z).max(0.0).sqrt();
        Vector::new(ring * angle.cos(), ring * angle.sin(), z)
    }
}
