use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::math::{
    add, basis_from_normal, corner_frame, cross, dist, dot, len, normalize, scale, sub, Vec3,
};

#[derive(Clone, Debug, Deserialize)]
pub struct ModelEdge {
    pub index: usize,
    #[serde(default)]
    pub name: Option<String>,
    pub mid: Vec3,
    pub length: f64,
    #[serde(default)]
    pub start: Option<Vec3>,
    #[serde(default)]
    pub end: Option<Vec3>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelFace {
    pub index: usize,
    #[serde(default)]
    pub name: Option<String>,
    pub area: f64,
    pub centroid: Vec3,
    #[serde(default)]
    pub plane_normal: Option<Vec3>,
    #[serde(default)]
    pub point: Option<Vec3>,
}

#[derive(Clone, Debug)]
pub struct DetectedCorner {
    pub body: String,
    pub edge_index: usize,
    /// Unit machining normal pointing from bottom (`z_bottom`) toward top (`z_top`).
    pub axis: Vec3,
    /// Corner vertex at the top of the edge (`z_top`).
    pub top_point: Vec3,
    /// Corner vertex at the bottom of the edge (`z_bottom`).
    pub bottom_point: Vec3,
    /// Projection of `top_point` along `axis`.
    pub z_top: f64,
    /// Projection of `bottom_point` along `axis`.
    pub z_bottom: f64,
    /// Outward normal of adjacent planar wall 1 (points into air/pocket).
    pub n1: Vec3,
    /// Outward normal of adjacent planar wall 2 (points into air/pocket).
    pub n2: Vec3,
    /// Length of wall 1 (in the cutting plane).
    pub len1: f64,
    /// Length of wall 2 (in the cutting plane).
    pub len2: f64,
    /// Interior corner angle in degrees.
    pub angle_deg: f64,
    /// Whether the region just above `top_point` along `+axis` is open air.
    pub top_open: bool,
    /// Whether the region just below `bottom_point` along `-axis` is open air (through-cut vs blind pocket).
    pub bottom_open: bool,
}

#[derive(Clone, Copy, Debug)]
struct CandidateEdge<'a> {
    edge: &'a ModelEdge,
    start: Vec3,
    end: Vec3,
    axis: Vec3,
    /// Indices of planar faces whose plane contains this straight edge.
    plane_faces: [usize; 8],
    plane_face_count: usize,
}

fn is_straight_edge(edge: &ModelEdge) -> Option<(Vec3, Vec3, Vec3)> {
    let start = edge.start?;
    let end = edge.end?;
    let chord = dist(start, end);
    if chord < 1e-4 {
        return None;
    }
    // Straight edge: arc length matches chord length within 0.5%.
    if (edge.length - chord).abs() > (chord * 0.005).max(1e-3) {
        return None;
    }
    // Also verify midpoint lies on segment start..end.
    let dir = normalize(sub(end, start))?;
    let expected_mid = scale(add(start, end), 0.5);
    if dist(edge.mid, expected_mid) > (chord * 0.01).max(1e-3) {
        return None;
    }
    Some((start, end, dir))
}

/// Infer the primary CNC panel normal for a body by summing planar face areas grouped by
/// parallel direction (so a board with cutouts on its large faces still picks the sheet normal).
pub fn infer_panel_normal(faces: &[ModelFace]) -> Option<Vec3> {
    let mut groups: Vec<(Vec3, f64)> = Vec::new();
    for f in faces {
        let Some(n_raw) = f.plane_normal else {
            continue;
        };
        let Some(n) = normalize(n_raw) else {
            continue;
        };
        if let Some(g) = groups.iter_mut().find(|(gn, _)| dot(*gn, n).abs() > 0.999) {
            g.1 += f.area;
        } else {
            // Canonicalize sign so +Z/+Y/+X is preferred for stability.
            let canon = canonical_axis(n);
            groups.push((canon, f.area));
        }
    }
    groups
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(n, _)| n)
}

pub fn canonical_axis(n: Vec3) -> Vec3 {
    if n[2].abs() > 1e-6 {
        if n[2] < 0.0 { scale(n, -1.0) } else { n }
    } else if n[1].abs() > 1e-6 {
        if n[1] < 0.0 { scale(n, -1.0) } else { n }
    } else if n[0] < 0.0 {
        scale(n, -1.0)
    } else {
        n
    }
}

/// Estimate the wall length along direction `tangent` starting from `start` or `end`
/// by scanning straight edges on the body that share an endpoint and run along `tangent`.
fn estimate_wall_length(edges: &[ModelEdge], start: Vec3, end: Vec3, tangent: Vec3) -> f64 {
    let mut best = 0.0_f64;
    for e in edges {
        let Some((es, ee, edir)) = is_straight_edge(e) else {
            continue;
        };
        if dot(edir, tangent).abs() < 0.99 {
            continue;
        }
        let near_start = dist(es, start) < 0.05 || dist(ee, start) < 0.05;
        let near_end = dist(es, end) < 0.05 || dist(ee, end) < 0.05;
        if near_start || near_end {
            // Make sure the edge actually extends in `+tangent` from the corner vertex.
            let other = if dist(es, start) < 0.05 || dist(es, end) < 0.05 {
                ee
            } else {
                es
            };
            let anchor = if near_start { start } else { end };
            if dot(sub(other, anchor), tangent) > 0.0 {
                if e.length > best {
                    best = e.length;
                }
            }
        }
    }
    best
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointClass {
    Inside,
    Outside,
    On,
}

fn parse_point_classes(reply: &Value) -> Result<Vec<PointClass>, String> {
    let arr = reply
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "inspect.classify_points missing results array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(s) = item.as_str() {
            match s {
                "inside" | "Inside" => out.push(PointClass::Inside),
                "outside" | "Outside" => out.push(PointClass::Outside),
                _ => out.push(PointClass::On),
            }
        } else if let Some(obj) = item.as_object() {
            if let Some(kind) = obj.get("kind").or_else(|| obj.get("status")).or_else(|| obj.get("class")).and_then(Value::as_str) {
                match kind {
                    "inside" | "Inside" => out.push(PointClass::Inside),
                    "outside" | "Outside" => out.push(PointClass::Outside),
                    _ => out.push(PointClass::On),
                }
            } else if obj.contains_key("On") || obj.contains_key("on") || obj.contains_key("face") {
                out.push(PointClass::On);
            } else if obj.contains_key("Inside") || obj.contains_key("inside") {
                out.push(PointClass::Inside);
            } else {
                out.push(PointClass::Outside);
            }
        } else {
            out.push(PointClass::Outside);
        }
    }
    Ok(out)
}

/// Detect internal concave corners on `body` using `model.faces`, `model.edges`, and one
/// batched `inspect.classify_points` call.
///
/// - If `target_edge_indices` is `Some(indices)`, only those edges are inspected (Edge Select mode),
///   and `corner_angle_tolerance_deg` is ignored (all concave corners on picked edges are accepted).
/// - If `required_axis` is `Some(axis)`, only straight edges parallel to `axis` are considered
///   (Body Select / Upper Face Select).
/// - If `upper_face_plane` is `Some((face_point, face_normal))`, we ensure the corner is accessible
///   from the `+face_normal` direction (i.e. `top_open == true` when oriented along `face_normal`).
pub fn detect_corners<F>(
    body: &str,
    required_axis: Option<Vec3>,
    upper_face_plane: Option<(Vec3, Vec3)>,
    target_edge_indices: Option<&[usize]>,
    corner_angle_tolerance_deg: f64,
    mut run_cmd: F,
) -> Result<Vec<DetectedCorner>, String>
where
    F: FnMut(&str, &Value) -> Result<Value, String>,
{
    let faces_val = run_cmd("model.faces", &json!({ "body": body }))?;
    let faces: Vec<ModelFace> = serde_json::from_value(
        faces_val
            .get("faces")
            .cloned()
            .ok_or_else(|| "model.faces missing faces".to_string())?,
    )
    .map_err(|e| alloc::format!("parse model.faces: {e}"))?;

    let edges_val = run_cmd("model.edges", &json!({ "body": body }))?;
    let edges: Vec<ModelEdge> = serde_json::from_value(
        edges_val
            .get("edges")
            .cloned()
            .ok_or_else(|| "model.edges missing edges".to_string())?,
    )
    .map_err(|e| alloc::format!("parse model.edges: {e}"))?;

    let axis_filter = match (required_axis, target_edge_indices) {
        (Some(ax), _) => Some(normalize(ax).ok_or_else(|| "invalid machining axis".to_string())?),
        (None, None) => Some(
            infer_panel_normal(&faces)
                .ok_or_else(|| alloc::format!("body `{body}` has no planar faces to infer machining direction"))?,
        ),
        (None, Some(_)) => None,
    };

    // Build candidate straight edges and find planar faces whose plane contains each edge.
    let mut candidates: Vec<CandidateEdge<'_>> = Vec::new();
    for edge in &edges {
        if let Some(filter_ids) = target_edge_indices {
            if !filter_ids.contains(&edge.index) {
                continue;
            }
        }
        let Some((mut start, mut end, mut dir)) = is_straight_edge(edge) else {
            continue;
        };
        if let Some(ax) = axis_filter {
            let alignment = dot(dir, ax);
            if alignment.abs() < 0.995 {
                continue;
            }
            // Orient edge from bottom to top along `+ax`.
            if alignment < 0.0 {
                core::mem::swap(&mut start, &mut end);
                dir = scale(dir, -1.0);
            }
        } else {
            dir = canonical_axis(dir);
            if dot(sub(end, start), dir) < 0.0 {
                core::mem::swap(&mut start, &mut end);
            }
        }

        let mut plane_faces = [0usize; 8];
        let mut count = 0usize;
        for (fi, f) in faces.iter().enumerate() {
            let Some(n_raw) = f.plane_normal else {
                continue;
            };
            let Some(n) = normalize(n_raw) else {
                continue;
            };
            // Face normal must be perpendicular to the edge direction.
            if dot(n, dir).abs() > 0.02 {
                continue;
            }
            let fp = f.point.unwrap_or(f.centroid);
            let d_start = dot(sub(start, fp), n).abs();
            let d_end = dot(sub(end, fp), n).abs();
            if d_start < 0.02 && d_end < 0.02 {
                if count < plane_faces.len() {
                    plane_faces[count] = fi;
                    count += 1;
                }
            }
        }
        if count >= 2 {
            candidates.push(CandidateEdge {
                edge,
                start,
                end,
                axis: dir,
                plane_faces,
                plane_face_count: count,
            });
        }
    }

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // For each candidate edge, evaluate every unordered pair of incident planar faces
    // with distinct normals. We batch ALL test points into ONE `inspect.classify_points` call!
    //
    // For a valid concave corner formed by walls (n1, n2):
    // - `p_air   = mid + eps * bisector`            must be Outside (pocket air)
    // - `p_solid = mid - eps * bisector`            must be Inside  (solid material behind corner)
    // - `p_w1_in = mid + eps * t1 - eps2 * n1`      must be Inside  (material behind wall 1)
    // - `p_w2_in = mid + eps * t2 - eps2 * n2`      must be Inside  (material behind wall 2)
    // - `p_top   = end + eps_z * axis + eps * bisector`  checks if top is open air
    // - `p_bot   = start - eps_z * axis + eps * bisector` checks if bottom is open air (through)
    struct PairProbe {
        cand_idx: usize,
        n1: Vec3,
        n2: Vec3,
        t1: Vec3,
        t2: Vec3,
        angle_deg: f64,
        point_offset: usize,
    }

    let eps = 0.05_f64;
    let eps2 = 0.02_f64;
    let eps_z = 0.05_f64;
    let mut probes: Vec<PairProbe> = Vec::new();
    let mut query_points: Vec<Vec3> = Vec::new();

    for (ci, cand) in candidates.iter().enumerate() {
        for a in 0..cand.plane_face_count {
            let fa = &faces[cand.plane_faces[a]];
            let Some(n1) = fa.plane_normal.and_then(normalize) else {
                continue;
            };
            for b in (a + 1)..cand.plane_face_count {
                let fb = &faces[cand.plane_faces[b]];
                let Some(n2) = fb.plane_normal.and_then(normalize) else {
                    continue;
                };
                // Skip parallel/anti-parallel face pairs (must form a real corner).
                if dot(n1, n2).abs() > 0.998 {
                    continue;
                }
                let Some((t1, t2, bisector, angle_deg)) = corner_frame(n1, n2, cand.axis) else {
                    continue;
                };
                // Filter by corner angle range in Body / Upper Face mode.
                if target_edge_indices.is_none()
                    && (angle_deg - 90.0).abs() > corner_angle_tolerance_deg + 1e-3
                {
                    continue;
                }
                let mid = cand.edge.mid;
                let point_offset = query_points.len();
                // 0: air in pocket
                query_points.push(add(mid, scale(bisector, eps)));
                // 1: solid behind corner
                query_points.push(sub(mid, scale(bisector, eps)));
                // 2: solid just behind wall 1 near edge
                query_points.push(sub(add(mid, scale(t1, eps)), scale(n1, eps2)));
                // 3: solid just behind wall 2 near edge
                query_points.push(sub(add(mid, scale(t2, eps)), scale(n2, eps2)));
                // 4: air just above top of edge (slightly into pocket)
                query_points.push(add(add(cand.end, scale(cand.axis, eps_z)), scale(bisector, eps)));
                // 5: air just below bottom of edge (slightly into pocket)
                query_points.push(add(sub(cand.start, scale(cand.axis, eps_z)), scale(bisector, eps)));

                probes.push(PairProbe {
                    cand_idx: ci,
                    n1,
                    n2,
                    t1,
                    t2,
                    angle_deg,
                    point_offset,
                });
            }
        }
    }

    if probes.is_empty() {
        return Ok(Vec::new());
    }

    let class_val = run_cmd(
        "inspect.classify_points",
        &json!({
            "body": body,
            "points": query_points
        }),
    )?;
    let classes = parse_point_classes(&class_val)?;
    if classes.len() != query_points.len() {
        return Err("inspect.classify_points returned mismatched result count".to_string());
    }

    let mut matched_edges: Vec<usize> = Vec::new();
    let mut detected: Vec<DetectedCorner> = Vec::new();

    for probe in probes {
        let cand = &candidates[probe.cand_idx];
        if matched_edges.contains(&cand.edge.index) {
            continue;
        }
        let o = probe.point_offset;
        let p_air = classes[o];
        let p_solid = classes[o + 1];
        let p_w1_in = classes[o + 2];
        let p_w2_in = classes[o + 3];
        let p_top = classes[o + 4];
        let p_bot = classes[o + 5];

        if p_air == PointClass::Outside
            && p_solid == PointClass::Inside
            && p_w1_in == PointClass::Inside
            && p_w2_in == PointClass::Inside
        {
            let top_open = p_top == PointClass::Outside;
            let bottom_open = p_bot == PointClass::Outside;

            // In Upper Face mode, only include corners accessible from the upper face side
            // (not blind pockets opening solely on the underside).
            if let Some((_uf_pt, _uf_n)) = upper_face_plane {
                if !top_open {
                    continue;
                }
            }

            let len1 = estimate_wall_length(&edges, cand.start, cand.end, probe.t1);
            let len2 = estimate_wall_length(&edges, cand.start, cand.end, probe.t2);

            matched_edges.push(cand.edge.index);
            detected.push(DetectedCorner {
                body: body.to_string(),
                edge_index: cand.edge.index,
                axis: cand.axis,
                top_point: cand.end,
                bottom_point: cand.start,
                z_top: dot(cand.end, cand.axis),
                z_bottom: dot(cand.start, cand.axis),
                n1: probe.n1,
                n2: probe.n2,
                len1,
                len2,
                angle_deg: probe.angle_deg,
                top_open,
                bottom_open,
            });
        }
    }

    // Suppress unused warning on basis_from_normal / cross / len if not called here.
    let _ = (basis_from_normal, cross, len);

    Ok(detected)
}
