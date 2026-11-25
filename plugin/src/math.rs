use libm::{acos, sin, sqrt};

pub type Vec3 = [f64; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DogboneType {
    Corner,
    MinimalCorner,
    AlongLongSide,
    AlongShortSide,
}

impl DogboneType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "corner" => Some(Self::Corner),
            "minimal_corner" => Some(Self::MinimalCorner),
            "along_long_side" => Some(Self::AlongLongSide),
            "along_short_side" => Some(Self::AlongShortSide),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Corner => "corner",
            Self::MinimalCorner => "minimal_corner",
            Self::AlongLongSide => "along_long_side",
            Self::AlongShortSide => "along_short_side",
        }
    }
}

pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn scale(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn len(a: Vec3) -> f64 {
    sqrt(dot(a, a))
}

pub fn normalize(a: Vec3) -> Option<Vec3> {
    let l = len(a);
    if l < 1e-12 {
        None
    } else {
        Some(scale(a, 1.0 / l))
    }
}

pub fn dist(a: Vec3, b: Vec3) -> f64 {
    len(sub(a, b))
}

/// Build an orthonormal basis `(x_dir, y_dir)` perpendicular to unit vector `z_dir`
/// such that `cross(x_dir, y_dir) == z_dir`.
pub fn basis_from_normal(z_dir: Vec3) -> (Vec3, Vec3) {
    let z = normalize(z_dir).unwrap_or([0.0, 0.0, 1.0]);
    let ref_vec = if z[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let x_dir = normalize(cross(ref_vec, z)).unwrap_or([1.0, 0.0, 0.0]);
    let y_dir = normalize(cross(z, x_dir)).unwrap_or([0.0, 1.0, 0.0]);
    (x_dir, y_dir)
}

/// Given two outward wall normals `n1`, `n2` (perpendicular to edge axis `z`, pointing into air/pocket),
/// compute:
/// - `t1`, `t2`: unit tangents along each wall pointing away from the corner vertex
/// - `bisector`: unit vector pointing into the pocket along the angle bisector
/// - `interior_angle_deg`: interior angle of the pocket corner in degrees (0..180)
pub fn corner_frame(n1: Vec3, n2: Vec3, z: Vec3) -> Option<(Vec3, Vec3, Vec3, f64)> {
    let mut t1 = normalize(cross(z, n1))?;
    if dot(t1, n2) < 0.0 {
        t1 = scale(t1, -1.0);
    }
    let mut t2 = normalize(cross(z, n2))?;
    if dot(t2, n1) < 0.0 {
        t2 = scale(t2, -1.0);
    }
    let bisector = normalize(add(t1, t2))?;
    let c = dot(t1, t2).clamp(-1.0, 1.0);
    let angle_deg = acos(c).to_degrees();
    Some((t1, t2, bisector, angle_deg))
}

/// Compute the 3D dogbone circle center (in the plane of `vertex`) for a concave corner.
///
/// - `vertex`: corner point in the cutting plane
/// - `n1`, `n2`: outward unit normals of the two walls (pointing into the pocket/air)
/// - `len1`, `len2`: wall lengths along wall 1 and wall 2 (used for AlongLongSide / AlongShortSide)
/// - `z`: unit edge direction
/// - `radius`: effective tool radius `(tool_diameter + additional_clearance) / 2`
/// - `kind`: dogbone type
/// - `corner_interference`: radial interference for `MinimalCorner`
pub fn dogbone_center(
    vertex: Vec3,
    n1: Vec3,
    n2: Vec3,
    len1: f64,
    len2: f64,
    z: Vec3,
    radius: f64,
    kind: DogboneType,
    corner_interference: f64,
) -> Option<Vec3> {
    let (_t1, _t2, bisector, angle_deg) = corner_frame(n1, n2, z)?;
    let half_rad = (angle_deg * 0.5).to_radians();
    let sin_half = sin(half_rad).max(1e-6);

    let offset = match kind {
        DogboneType::Corner => scale(bisector, radius),
        DogboneType::MinimalCorner => {
            // Maximum meaningful interference is when the circle becomes a tangent fillet:
            // center distance from vertex = radius / sin(alpha).
            let max_interference = (radius / sin_half - radius).max(0.0);
            let clamped = corner_interference.clamp(0.0, max_interference);
            scale(bisector, radius + clamped)
        }
        DogboneType::AlongLongSide => {
            // Tangent to the longer wall, cutting a semicircular relief past the shorter wall.
            let n_long = if len1 >= len2 { n1 } else { n2 };
            scale(n_long, radius)
        }
        DogboneType::AlongShortSide => {
            // Tangent to the shorter wall, cutting a semicircular relief past the longer wall.
            let n_short = if len1 <= len2 { n1 } else { n2 };
            scale(n_short, radius)
        }
    };

    Some(add(vertex, offset))
}

/// Compute the visible gap outside a 90-degree mating corner for UI display.
pub fn visible_gap_90(tool_diameter: f64, additional_clearance: f64, kind: DogboneType, corner_interference: f64) -> f64 {
    let r = (tool_diameter + additional_clearance) * 0.5;
    if r <= 0.0 {
        return 0.0;
    }
    let inv_sqrt2 = 0.7071067811865476;
    match kind {
        DogboneType::Corner => r * (1.0 - inv_sqrt2),
        DogboneType::MinimalCorner => {
            let max_i = r * (sqrt(2.0) - 1.0);
            let i = corner_interference.clamp(0.0, max_i);
            (r - (r + i) * inv_sqrt2).max(0.0)
        }
        DogboneType::AlongLongSide | DogboneType::AlongShortSide => r,
    }
}
