//! Reading a `.glb` down to bare triangles in chassis space.
//!
//! Deliberately the smallest thing that works: node names, node transforms and `POSITION`
//! accessors. Materials, textures, skins and animations are all skipped, which is also why the
//! `gltf` crate is taken without its `import` feature — that one exists to decode textures.

use parry3d::math::Vector;

/// A 4x4 transform, column-major, in the layout glTF stores a node matrix in.
type Matrix = [[f32; 4]; 4];

const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn multiply(outer: &Matrix, inner: &Matrix) -> Matrix {
    let mut out = [[0.0; 4]; 4];
    for (column, source) in out.iter_mut().zip(inner) {
        for (row, cell) in column.iter_mut().enumerate() {
            *cell = (0..4).map(|k| outer[k][row] * source[k]).sum();
        }
    }
    out
}

fn transform(matrix: &Matrix, point: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (row, cell) in out.iter_mut().enumerate() {
        *cell = matrix[0][row] * point[0]
            + matrix[1][row] * point[1]
            + matrix[2][row] * point[2]
            + matrix[3][row];
    }
    out
}

/// What the model calls the three parts of a wheel assembly, as the prefix of a node's name.
///
/// The same three strings `client/src/vehicle.rs` uses to find the wheels and drive them. They are
/// left out of the chassis shape because a wheel is not part of the body: it turns, it steers, it
/// moves up and down on its strut, and a shape baked with the wheels in it would be a vehicle
/// permanently sitting on four blocks.
pub const WHEEL_PARTS: [&str; 3] = ["Tire", "Axel", "Suspension"];

/// A triangle soup: vertices and the triples that index them.
pub struct Mesh {
    pub vertices: Vec<Vector>,
    pub triangles: Vec<[u32; 3]>,
}

/// One drawable piece of the file, kept apart only so `list` can report what is in there.
pub struct Piece {
    pub node: String,
    pub vertices: usize,
    pub triangles: usize,
    pub wheel: bool,
    pub low: Vector,
    pub high: Vector,
}

/// How the model's own axes and units become the chassis's.
///
/// The vehicle model is a Sketchfab export normalised into a 2 x 0.893 x 0.994 box that faces −X,
/// and the game's chassis is 3.8 m long facing −Z with its origin at the middle of the body. Both
/// halves of that — the quarter turn and the scale — are what `client/src/vehicle.rs` already
/// applies to the *visible* model, and this has to be the same map or the shape would sit somewhere
/// the bodywork is not. A test in the client holds the two together.
#[derive(Clone, Copy)]
pub struct ToChassis {
    /// Model units to metres.
    pub scale: f32,
    /// Metres the origin moves up, once scaled: the model measures from between its tyres, the
    /// chassis from its own centre.
    pub lift: f32,
}

impl ToChassis {
    /// A quarter turn about Y, then the scale, then the lift.
    ///
    /// The turn is written out rather than built from a quaternion because it is exactly the axis
    /// swap `(x, y, z) -> (-z, y, x)`, and spelling it that way is the thing that can be checked by
    /// eye against `MODEL_YAW`.
    pub fn apply(&self, point: [f32; 3]) -> Vector {
        Vector::new(
            -point[2] * self.scale,
            point[1] * self.scale + self.lift,
            point[0] * self.scale,
        )
    }
}

/// Read every triangle of the file, minus the wheels, into one mesh in chassis space.
pub fn load(path: &str, to_chassis: ToChassis) -> Result<(Mesh, Vec<Piece>), String> {
    let bytes = std::fs::read(path).map_err(|why| format!("{path}: {why}"))?;
    let file = gltf::Gltf::from_slice(&bytes).map_err(|why| format!("{path}: {why}"))?;
    let blob = file
        .blob
        .as_deref()
        .ok_or_else(|| format!("{path}: no binary chunk — is it a .glb?"))?;

    let mut mesh = Mesh { vertices: Vec::new(), triangles: Vec::new() };
    let mut pieces = Vec::new();
    let scene = file
        .default_scene()
        .or_else(|| file.scenes().next())
        .ok_or_else(|| format!("{path}: no scene"))?;
    for node in scene.nodes() {
        walk(&node, IDENTITY, false, blob, to_chassis, &mut mesh, &mut pieces);
    }
    if mesh.triangles.is_empty() {
        return Err(format!("{path}: no body triangles — are the node names still the same?"));
    }
    Ok((mesh, pieces))
}

fn walk(
    node: &gltf::Node,
    parent: Matrix,
    inherited_wheel: bool,
    blob: &[u8],
    to_chassis: ToChassis,
    mesh: &mut Mesh,
    pieces: &mut Vec<Piece>,
) {
    let name = node.name().unwrap_or("");
    let wheel = inherited_wheel || WHEEL_PARTS.iter().any(|part| name.starts_with(part));
    let here = multiply(&parent, &node.transform().matrix());

    if let Some(drawable) = node.mesh() {
        for primitive in drawable.primitives() {
            let reader = primitive.reader(|_| Some(blob));
            let Some(positions) = reader.read_positions() else { continue };
            let points: Vec<Vector> =
                positions.map(|p| to_chassis.apply(transform(&here, p))).collect();
            let indices: Vec<u32> = match reader.read_indices() {
                Some(indices) => indices.into_u32().collect(),
                // A primitive with no index buffer is a plain run of triangles.
                None => (0..points.len() as u32).collect(),
            };
            let (mut low, mut high) = (Vector::splat(f32::MAX), Vector::splat(f32::MIN));
            for point in &points {
                low = low.min(*point);
                high = high.max(*point);
            }
            pieces.push(Piece {
                node: name.to_string(),
                vertices: points.len(),
                triangles: indices.len() / 3,
                wheel,
                low,
                high,
            });
            if wheel {
                continue;
            }
            let base = mesh.vertices.len() as u32;
            mesh.vertices.extend_from_slice(&points);
            mesh.triangles
                .extend(indices.chunks_exact(3).map(|t| [base + t[0], base + t[1], base + t[2]]));
        }
    }

    for child in node.children() {
        walk(&child, here, wheel, blob, to_chassis, mesh, pieces);
    }
}
