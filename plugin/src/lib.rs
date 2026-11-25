#![no_std]

extern crate alloc;

#[global_allocator]
static ALLOC: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

mod cut;
mod detect;
mod math;
mod storage;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{json, Value};

use crate::cut::{cut_dogbones, CutSettings};
use crate::detect::{detect_corners, DetectedCorner, ModelEdge, ModelFace};
use crate::math::{dist, dot, normalize, visible_gap_90, DogboneType, Vec3};
use crate::storage::{
    delete_generated, load_doc, save_doc, DogboneRecipe, StoredEdgeTarget, StoredFaceTarget,
};

wit_bindgen::generate!({
    path: "../wit",
    world: "command-dialog-plugin",
});

use craft::plugin::commands;
use craft::plugin::types::{FieldFailure, PluginError};

struct DogbonesPlugin;

export!(DogbonesPlugin);

impl exports::craft::plugin::entry::Guest for DogbonesPlugin {
    fn execute(entry: String, params: String) -> Result<String, PluginError> {
        let args: Value = serde_json::from_str(&params).map_err(|e| {
            PluginError::InvalidInput(FieldFailure {
                field: "params".to_string(),
                reason: format!("invalid JSON: {e}"),
            })
        })?;

        let result = match entry.as_str() {
            "dogbones" => run_create_dogbones(&args)?,
            "update-dogbones" => run_update_dogbones(&args)?,
            other => {
                return Err(PluginError::Unsupported(format!(
                    "unknown entry `{other}`"
                )));
            }
        };

        serde_json::to_string(&result).map_err(|e| PluginError::Trap(e.to_string()))
    }
}

impl exports::craft::plugin::dialog_provider::Guest for DogbonesPlugin {
    fn update_fields(entry: String, values: String) -> Result<String, PluginError> {
        if entry != "dogbones" {
            return Ok("{}".to_string());
        }
        let v: Value = serde_json::from_str(&values).unwrap_or(Value::Null);
        let tool_d = v.get("tool_diameter").and_then(Value::as_f64).unwrap_or(6.0);
        let clearance = v
            .get("additional_clearance")
            .and_then(Value::as_f64)
            .unwrap_or(0.025);
        let kind_str = v
            .get("dogbone_type")
            .and_then(Value::as_str)
            .unwrap_or("corner");
        let kind = DogboneType::from_str(kind_str).unwrap_or(DogboneType::Corner);
        let interference = v
            .get("corner_interference")
            .and_then(Value::as_f64)
            .unwrap_or(0.25);

        let eff_d = tool_d + clearance;
        let gap = visible_gap_90(tool_d, clearance, kind, interference);

        let gap_text = match kind {
            DogboneType::Corner => format!("{gap:.3} mm"),
            DogboneType::MinimalCorner => {
                format!("{gap:.3} mm (interference {interference:.3} mm)")
            }
            DogboneType::AlongLongSide | DogboneType::AlongShortSide => {
                format!("{gap:.3} mm (hidden on tangent side)")
            }
        };

        let reply = json!({
            "visible_gap": {
                "default": gap_text,
                "hint": format!("Effective cutter diameter: {eff_d:.3} mm")
            }
        });
        serde_json::to_string(&reply).map_err(|e| PluginError::Trap(e.to_string()))
    }
}

fn call_cmd(cmd: &str, args: &Value) -> Result<Value, String> {
    let payload = serde_json::to_string(args).map_err(|e| e.to_string())?;
    let raw = commands::run(cmd, &payload).map_err(|e| format!("{cmd} failed: {e:?}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("{cmd} returned invalid JSON: {e}"))
}

fn invalid(field: &str, reason: impl Into<String>) -> PluginError {
    PluginError::InvalidInput(FieldFailure {
        field: field.to_string(),
        reason: reason.into(),
    })
}

fn cmd_err(reason: String) -> PluginError {
    PluginError::CommandFailed(craft::plugin::types::CommandFailure {
        command: "dogbones".to_string(),
        reason,
    })
}

fn parse_vec3(v: &Value) -> Option<Vec3> {
    let arr = v.as_array()?;
    if arr.len() != 3 {
        return None;
    }
    Some([arr[0].as_f64()?, arr[1].as_f64()?, arr[2].as_f64()?])
}

fn extract_body_names(arr: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for item in arr {
        let name = item
            .as_str()
            .or_else(|| item.get("name").and_then(Value::as_str))
            .or_else(|| item.get("body").and_then(Value::as_str));
        if let Some(n) = name {
            if !n.is_empty() && !out.iter().any(|x| x == n) {
                out.push(n.to_string());
            }
        }
    }
    out
}

fn resolve_upper_face(item: &Value) -> Result<StoredFaceTarget, PluginError> {
    let body = item
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("upper_faces", "selected face is missing `body`"))?
        .to_string();
    let pt = item
        .get("point")
        .and_then(parse_vec3)
        .ok_or_else(|| invalid("upper_faces", "selected face is missing `point`"))?;
    let face_idx = item.get("index").and_then(Value::as_u64).map(|x| x as usize);

    let faces_val = call_cmd("model.faces", &json!({ "body": body })).map_err(cmd_err)?;
    let faces: Vec<ModelFace> = serde_json::from_value(
        faces_val
            .get("faces")
            .cloned()
            .ok_or_else(|| cmd_err("model.faces missing faces".to_string()))?,
    )
    .map_err(|e| cmd_err(format!("parse model.faces: {e}")))?;

    let chosen = if let Some(idx) = face_idx {
        faces.iter().find(|f| f.index == idx)
    } else {
        None
    }
    .or_else(|| {
        faces
            .iter()
            .filter(|f| f.plane_normal.is_some())
            .min_by(|a, b| {
                let da = dist(a.point.unwrap_or(a.centroid), pt);
                let db = dist(b.point.unwrap_or(b.centroid), pt);
                da.total_cmp(&db)
            })
    })
    .ok_or_else(|| invalid("upper_faces", format!("no planar face found on body `{body}`")))?;

    let normal = chosen
        .plane_normal
        .and_then(normalize)
        .ok_or_else(|| invalid("upper_faces", "selected upper face must be planar"))?;

    Ok(StoredFaceTarget {
        body,
        point: chosen.point.unwrap_or(chosen.centroid),
        normal,
    })
}

fn resolve_edges(items: &[Value]) -> Result<Vec<StoredEdgeTarget>, PluginError> {
    let mut out = Vec::new();
    for item in items {
        let body = item
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("edges", "selected edge is missing `body`"))?
            .to_string();
        let point = item
            .get("point")
            .and_then(parse_vec3)
            .ok_or_else(|| invalid("edges", "selected edge is missing `point`"))?;
        let index = item.get("index").and_then(Value::as_u64).map(|x| x as usize);
        out.push(StoredEdgeTarget { body, point, index });
    }
    Ok(out)
}

fn resolve_edge_indices_for_body(body: &str, targets: &[&StoredEdgeTarget]) -> Result<Vec<usize>, String> {
    let edges_val = call_cmd("model.edges", &json!({ "body": body }))?;
    let edges: Vec<ModelEdge> = serde_json::from_value(
        edges_val
            .get("edges")
            .cloned()
            .ok_or_else(|| "model.edges missing edges".to_string())?,
    )
    .map_err(|e| format!("parse model.edges: {e}"))?;

    let mut indices = Vec::new();
    for t in targets {
        // Prefer exact index if its midpoint is still close, else match nearest edge midpoint.
        let matched = t
            .index
            .and_then(|idx| edges.iter().find(|e| e.index == idx && dist(e.mid, t.point) < 1.0))
            .or_else(|| {
                edges
                    .iter()
                    .min_by(|a, b| dist(a.mid, t.point).total_cmp(&dist(b.mid, t.point)))
            });
        if let Some(e) = matched {
            if !indices.contains(&e.index) {
                indices.push(e.index);
            }
        }
    }
    Ok(indices)
}

fn execute_recipe_corners(
    selection_type: &str,
    bodies: &[String],
    upper_faces: &[StoredFaceTarget],
    edges: &[StoredEdgeTarget],
    angle_tol_deg: f64,
) -> Result<Vec<DetectedCorner>, String> {
    let mut all_corners = Vec::new();
    match selection_type {
        "body" => {
            for body in bodies {
                let mut cs = detect_corners(body, None, None, None, angle_tol_deg, call_cmd)?;
                all_corners.append(&mut cs);
            }
        }
        "upper_face" => {
            for uf in upper_faces {
                let mut cs = detect_corners(
                    &uf.body,
                    Some(uf.normal),
                    Some((uf.point, uf.normal)),
                    None,
                    angle_tol_deg,
                    call_cmd,
                )?;
                all_corners.append(&mut cs);
            }
        }
        "edge" => {
            // Group edge targets by body.
            let mut body_list: Vec<&str> = Vec::new();
            for e in edges {
                if !body_list.contains(&e.body.as_str()) {
                    body_list.push(e.body.as_str());
                }
            }
            for body in body_list {
                let group: Vec<&StoredEdgeTarget> =
                    edges.iter().filter(|e| e.body == body).collect();
                let indices = resolve_edge_indices_for_body(body, &group)?;
                if !indices.is_empty() {
                    let mut cs =
                        detect_corners(body, None, None, Some(&indices), angle_tol_deg, call_cmd)?;
                    all_corners.append(&mut cs);
                }
            }
        }
        other => return Err(format!("unsupported selection_type `{other}`")),
    }
    Ok(all_corners)
}

fn run_create_dogbones(args: &Value) -> Result<Value, PluginError> {
    let selection_type = args
        .get("selection_type")
        .and_then(Value::as_str)
        .unwrap_or("body");
    let tool_diameter = args
        .get("tool_diameter")
        .and_then(Value::as_f64)
        .unwrap_or(6.0);
    if tool_diameter <= 0.0 {
        return Err(invalid("tool_diameter", "tool diameter must be positive"));
    }
    let additional_clearance = args
        .get("additional_clearance")
        .and_then(Value::as_f64)
        .unwrap_or(0.025);
    if additional_clearance < 0.0 {
        return Err(invalid(
            "additional_clearance",
            "additional clearance cannot be negative",
        ));
    }
    let dogbone_type_str = args
        .get("dogbone_type")
        .and_then(Value::as_str)
        .unwrap_or("corner");
    let dogbone_type = DogboneType::from_str(dogbone_type_str)
        .ok_or_else(|| invalid("dogbone_type", format!("invalid type `{dogbone_type_str}`")))?;
    let corner_interference = args
        .get("corner_interference")
        .and_then(Value::as_f64)
        .unwrap_or(0.25);
    let corner_angle_tolerance_deg = args
        .get("corner_angle_tolerance_deg")
        .and_then(Value::as_f64)
        .unwrap_or(5.0);
    let feature_name = args
        .get("feature_name")
        .and_then(Value::as_str)
        .unwrap_or("Dogbone")
        .to_string();

    let mut bodies = Vec::new();
    let mut upper_faces = Vec::new();
    let mut edges = Vec::new();

    match selection_type {
        "body" => {
            let arr = args
                .get("bodies")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("bodies", "select at least one body"))?;
            bodies = extract_body_names(arr);
            if bodies.is_empty() {
                return Err(invalid("bodies", "select at least one body"));
            }
        }
        "upper_face" => {
            let arr = args
                .get("upper_faces")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("upper_faces", "select at least one planar upper face"))?;
            if arr.is_empty() {
                return Err(invalid(
                    "upper_faces",
                    "select at least one planar upper face",
                ));
            }
            for item in arr {
                upper_faces.push(resolve_upper_face(item)?);
            }
        }
        "edge" => {
            let arr = args
                .get("edges")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("edges", "select at least one internal corner edge"))?;
            if arr.is_empty() {
                return Err(invalid(
                    "edges",
                    "select at least one internal corner edge",
                ));
            }
            edges = resolve_edges(arr)?;
        }
        other => {
            return Err(invalid(
                "selection_type",
                format!("unknown selection type `{other}`"),
            ));
        }
    }

    let corners = execute_recipe_corners(
        selection_type,
        &bodies,
        &upper_faces,
        &edges,
        corner_angle_tolerance_deg,
    )
    .map_err(cmd_err)?;

    if corners.is_empty() {
        return Err(PluginError::Unsupported(
            "no matching internal concave corners were found on the selected geometry".to_string(),
        ));
    }

    let settings = CutSettings {
        tool_diameter,
        additional_clearance,
        dogbone_type,
        corner_interference,
        feature_name: feature_name.clone(),
    };

    let outcome = cut_dogbones(&corners, &settings, call_cmd).map_err(cmd_err)?;

    // Persist recipe for Update Dogbones (best-effort so preview sessions still succeed cleanly).
    if let Ok(mut doc) = load_doc(call_cmd) {
        let next_id = doc.recipes.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        doc.recipes.push(DogboneRecipe {
            id: next_id,
            selection_type: selection_type.to_string(),
            bodies,
            upper_faces,
            edges,
            tool_diameter,
            additional_clearance,
            dogbone_type: dogbone_type.as_str().to_string(),
            corner_interference,
            corner_angle_tolerance_deg,
            feature_name,
            feature_ids: outcome.feature_ids.clone(),
            sketch_ids: outcome.sketch_ids.clone(),
        });
        let _ = save_doc(&doc, call_cmd);
    }

    let gap_90 = visible_gap_90(
        tool_diameter,
        additional_clearance,
        dogbone_type,
        corner_interference,
    );

    Ok(json!({
        "selection_type": selection_type,
        "dogbone_type": dogbone_type.as_str(),
        "effective_diameter_mm": tool_diameter + additional_clearance,
        "visible_gap_90_mm": gap_90,
        "corners_cut": outcome.corners_cut,
        "feature_ids": outcome.feature_ids,
        "sketch_ids": outcome.sketch_ids
    }))
}

fn run_update_dogbones(args: &Value) -> Result<Value, PluginError> {
    let remove_missing = args
        .get("remove_missing")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut doc = load_doc(call_cmd).map_err(cmd_err)?;
    if doc.recipes.is_empty() {
        return Ok(json!({
            "updated_recipes": 0,
            "corners_cut": 0,
            "message": "No stored dogbone recipes found in this design."
        }));
    }

    let mut updated_recipes = 0usize;
    let mut total_corners = 0usize;
    let mut kept_recipes = Vec::new();

    for mut recipe in doc.recipes.drain(..) {
        // 1. Delete old dogbone features/sketches so we re-detect on clean updated geometry.
        delete_generated(&recipe.feature_ids, &recipe.sketch_ids, call_cmd);
        recipe.feature_ids.clear();
        recipe.sketch_ids.clear();

        // 2. Re-detect corners.
        let corners_res = execute_recipe_corners(
            &recipe.selection_type,
            &recipe.bodies,
            &recipe.upper_faces,
            &recipe.edges,
            recipe.corner_angle_tolerance_deg,
        );

        match corners_res {
            Ok(corners) if !corners.is_empty() => {
                let kind =
                    DogboneType::from_str(&recipe.dogbone_type).unwrap_or(DogboneType::Corner);
                let settings = CutSettings {
                    tool_diameter: recipe.tool_diameter,
                    additional_clearance: recipe.additional_clearance,
                    dogbone_type: kind,
                    corner_interference: recipe.corner_interference,
                    feature_name: recipe.feature_name.clone(),
                };
                let outcome = cut_dogbones(&corners, &settings, call_cmd).map_err(cmd_err)?;
                recipe.feature_ids = outcome.feature_ids;
                recipe.sketch_ids = outcome.sketch_ids;
                total_corners += outcome.corners_cut;
                updated_recipes += 1;
                kept_recipes.push(recipe);
            }
            Ok(_) => {
                if !remove_missing {
                    kept_recipes.push(recipe);
                }
            }
            Err(_) => {
                if !remove_missing {
                    kept_recipes.push(recipe);
                }
            }
        }
    }

    doc.recipes = kept_recipes;
    save_doc(&doc, call_cmd).map_err(cmd_err)?;

    // Suppress unused import warning on dot if not directly referenced in lib.rs.
    let _ = dot;

    Ok(json!({
        "updated_recipes": updated_recipes,
        "corners_cut": total_corners,
        "stored_recipes": doc.recipes.len()
    }))
}
