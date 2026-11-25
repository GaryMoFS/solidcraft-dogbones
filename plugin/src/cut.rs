use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{json, Value};

use crate::detect::DetectedCorner;
use crate::math::{add, basis_from_normal, dogbone_center, dot, scale, sub, DogboneType, Vec3};

#[derive(Clone, Debug)]
pub struct CutSettings {
    pub tool_diameter: f64,
    pub additional_clearance: f64,
    pub dogbone_type: DogboneType,
    pub corner_interference: f64,
    pub feature_name: String,
}

#[derive(Clone, Debug)]
pub struct CutOutcome {
    pub corners_cut: usize,
    pub feature_ids: Vec<u64>,
    pub sketch_ids: Vec<u64>,
}

struct CornerGroup {
    body: String,
    axis: Vec3,
    z_top: f64,
    z_bottom: f64,
    top_open: bool,
    bottom_open: bool,
    circles: Vec<Vec3>,
}

fn collect_existing_names<F>(run_cmd: &mut F) -> Vec<String>
where
    F: FnMut(&str, &Value) -> Result<Value, String>,
{
    let mut names = Vec::new();
    let Ok(doc) = run_cmd("document.inspect", &json!({})) else {
        return names;
    };
    for key in ["features", "sketches", "planes", "bodies"] {
        if let Some(arr) = doc.get(key).and_then(Value::as_array) {
            for item in arr {
                if let Some(n) = item.get("name").and_then(Value::as_str) {
                    if !n.is_empty() && !names.iter().any(|x| x == n) {
                        names.push(n.to_string());
                    }
                }
            }
        }
    }
    names
}

fn unique_name(base: &str, used: &mut Vec<String>) -> String {
    if !used.iter().any(|x| x == base) {
        used.push(base.to_string());
        return base.to_string();
    }
    let mut n = 2usize;
    loop {
        let candidate = format!("{base} {n}");
        if !used.iter().any(|x| x == &candidate) {
            used.push(candidate.clone());
            return candidate;
        }
        n += 1;
    }
}

/// Group corners by `(body, axis, z_top, z_bottom, top_open, bottom_open)` so all dogbones
/// sharing the same machining orientation and cut depth on a body become ONE Sketch + ONE Extrude Cut.
pub fn cut_dogbones<F>(
    corners: &[DetectedCorner],
    settings: &CutSettings,
    mut run_cmd: F,
) -> Result<CutOutcome, String>
where
    F: FnMut(&str, &Value) -> Result<Value, String>,
{
    let radius = (settings.tool_diameter + settings.additional_clearance) * 0.5;
    if radius <= 0.0 {
        return Err("effective tool radius must be positive".to_string());
    }

    let mut groups: Vec<CornerGroup> = Vec::new();
    for c in corners {
        let Some(center_3d) = dogbone_center(
            c.top_point,
            c.n1,
            c.n2,
            c.len1,
            c.len2,
            c.axis,
            radius,
            settings.dogbone_type,
            settings.corner_interference,
        ) else {
            continue;
        };

        if let Some(g) = groups.iter_mut().find(|g| {
            g.body == c.body
                && dot(g.axis, c.axis) > 0.999
                && (g.z_top - c.z_top).abs() < 0.02
                && (g.z_bottom - c.z_bottom).abs() < 0.02
                && g.top_open == c.top_open
                && g.bottom_open == c.bottom_open
        }) {
            g.circles.push(center_3d);
        } else {
            groups.push(CornerGroup {
                body: c.body.clone(),
                axis: c.axis,
                z_top: c.z_top,
                z_bottom: c.z_bottom,
                top_open: c.top_open,
                bottom_open: c.bottom_open,
                circles: alloc::vec![center_3d],
            });
        }
    }

    let mut feature_ids = Vec::new();
    let mut sketch_ids = Vec::new();
    let mut corners_cut = 0usize;

    let base_name = if settings.feature_name.trim().is_empty() {
        "Dogbone"
    } else {
        settings.feature_name.trim()
    };

    let mut used_names = collect_existing_names(&mut run_cmd);

    for g in groups {
        let depth = g.z_top - g.z_bottom;
        if depth <= 1e-4 || g.circles.is_empty() {
            continue;
        }

        // Small overshoot on open ends prevents coplanar boolean artifacts while preserving
        // exact flush floors on blind pockets.
        let top_pad = if g.top_open { 0.05 } else { 0.0 };
        let bottom_pad = if g.bottom_open { 0.05 } else { 0.0 };
        let cut_distance = depth + top_pad + bottom_pad;

        // Sketch plane normal points INTO the cut (`-g.axis`) so positive `distance` extrudes downward.
        let cut_dir = scale(g.axis, -1.0);
        let (x_dir, y_dir) = basis_from_normal(cut_dir);

        // Anchor origin at first circle center shifted up by `top_pad` along `+g.axis`.
        let origin = add(g.circles[0], scale(g.axis, top_pad));

        let sketch_name = unique_name(&format!("{base_name} Sketch"), &mut used_names);
        let extrude_name = unique_name(&format!("{base_name} Cut"), &mut used_names);

        let sk_res = run_cmd(
            "sketch.create",
            &json!({
                "plane": {
                    "origin": origin,
                    "x_dir": x_dir,
                    "y_dir": y_dir
                },
                "name": sketch_name
            }),
        )?;
        let sketch_id = sk_res
            .get("sketch")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("sketch.create missing sketch id: {sk_res}"))?;

        for center_3d in &g.circles {
            let rel = sub(*center_3d, origin);
            let u = dot(rel, x_dir);
            let v = dot(rel, y_dir);
            run_cmd(
                "sketch.circle.center",
                &json!({
                    "center": [u, v],
                    "radius": radius
                }),
            )?;
        }

        run_cmd("sketch.finish", &json!({}))?;

        let ext_res = run_cmd(
            "solid.extrude",
            &json!({
                "sketch": sketch_id,
                "distance": cut_distance,
                "operation": "cut",
                "targets": [g.body],
                "name": extrude_name
            }),
        )?;

        if let Some(fid) = ext_res.get("feature").and_then(Value::as_u64) {
            feature_ids.push(fid);
        }
        sketch_ids.push(sketch_id);
        corners_cut += g.circles.len();
    }

    Ok(CutOutcome {
        corners_cut,
        feature_ids,
        sketch_ids,
    })
}
