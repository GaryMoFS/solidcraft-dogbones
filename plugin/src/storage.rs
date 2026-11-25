use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::math::Vec3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredDoc {
    pub version: u32,
    pub recipes: Vec<DogboneRecipe>,
}

impl Default for StoredDoc {
    fn default() -> Self {
        Self {
            version: 1,
            recipes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DogboneRecipe {
    pub id: u64,
    pub selection_type: String,
    #[serde(default)]
    pub bodies: Vec<String>,
    #[serde(default)]
    pub upper_faces: Vec<StoredFaceTarget>,
    #[serde(default)]
    pub edges: Vec<StoredEdgeTarget>,
    pub tool_diameter: f64,
    pub additional_clearance: f64,
    pub dogbone_type: String,
    pub corner_interference: f64,
    pub corner_angle_tolerance_deg: f64,
    pub feature_name: String,
    #[serde(default)]
    pub feature_ids: Vec<u64>,
    #[serde(default)]
    pub sketch_ids: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredFaceTarget {
    pub body: String,
    pub point: Vec3,
    pub normal: Vec3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEdgeTarget {
    pub body: String,
    pub point: Vec3,
    #[serde(default)]
    pub index: Option<usize>,
}

pub fn load_doc<F>(mut run_cmd: F) -> Result<StoredDoc, String>
where
    F: FnMut(&str, &Value) -> Result<Value, String>,
{
    let val = run_cmd("storage.get", &json!({}))?;
    let Some(doc_val) = val.get("document") else {
        return Ok(StoredDoc::default());
    };
    if doc_val.is_null() {
        return Ok(StoredDoc::default());
    }
    serde_json::from_value(doc_val.clone()).map_err(|e| alloc::format!("parse stored dogbones: {e}"))
}

pub fn save_doc<F>(doc: &StoredDoc, mut run_cmd: F) -> Result<(), String>
where
    F: FnMut(&str, &Value) -> Result<Value, String>,
{
    let doc_val = serde_json::to_value(doc).map_err(|e| alloc::format!("serialize dogbones: {e}"))?;
    run_cmd("storage.set", &json!({ "document": doc_val }))?;
    Ok(())
}

/// Delete previously generated extrude features & sketches for a recipe.
/// Silently ignores features that the user may have already deleted manually.
pub fn delete_generated<F>(feature_ids: &[u64], sketch_ids: &[u64], mut run_cmd: F)
where
    F: FnMut(&str, &Value) -> Result<Value, String>,
{
    let mut items: Vec<Value> = Vec::new();
    // Delete extrude features first, then sketches.
    for &fid in feature_ids {
        items.push(json!({ "type": "feature", "id": fid }));
    }
    for &sid in sketch_ids {
        items.push(json!({ "type": "feature", "id": sid }));
    }
    if !items.is_empty() {
        let _ = run_cmd("selection.delete", &json!({ "items": items }));
    }
}
