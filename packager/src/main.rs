use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use wit_component::{ComponentEncoder, StringEncoding};
use wit_parser::Resolve;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

const ICONS: &[&str] = &[
    "icons/dogbones.svg",
    "icons/update_dogbones.svg",
    "icons/select_body.svg",
    "icons/select_face.svg",
    "icons/select_edge.svg",
    "icons/type_corner.svg",
    "icons/type_minimal.svg",
    "icons/type_long.svg",
    "icons/type_short.svg",
];

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("missing parent")?
        .to_path_buf();
    let core_wasm = root.join("plugin/target/wasm32-unknown-unknown/release/dogbones_plugin.wasm");
    let wit_path = root.join("wit/plugin.wit");
    let manifest_path = root.join("plugin.json");

    let mut wasm_bytes =
        fs::read(&core_wasm).with_context(|| format!("read {}", core_wasm.display()))?;
    let mut resolve = Resolve::default();
    let (pkg_id, _) = resolve
        .push_path(&wit_path)
        .with_context(|| format!("parse {}", wit_path.display()))?;
    let world_id = resolve.select_world(&[pkg_id], Some("command-dialog-plugin"))?;

    wit_component::embed_component_metadata(
        &mut wasm_bytes,
        &resolve,
        world_id,
        StringEncoding::UTF8,
    )?;
    let component = ComponentEncoder::default()
        .module(&wasm_bytes)?
        .validate(true)
        .encode()?;

    let dist_dir = root.join("dist/dogbones");
    fs::create_dir_all(dist_dir.join("icons"))?;
    let out_wasm = dist_dir.join("dogbones.wasm");
    let out_manifest = dist_dir.join("plugin.json");
    let manifest_bytes = fs::read(&manifest_path)?;

    fs::write(&out_wasm, &component)?;
    fs::write(&out_manifest, &manifest_bytes)?;

    let zip_path = root.join("dist/dogbones.zip");
    let file = File::create(&zip_path)?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("dogbones/plugin.json", opts)?;
    zip.write_all(&manifest_bytes)?;
    zip.start_file("dogbones/dogbones.wasm", opts)?;
    zip.write_all(&component)?;

    for rel in ICONS {
        let src = root.join(rel);
        let bytes = fs::read(&src).with_context(|| format!("read {}", src.display()))?;
        fs::write(dist_dir.join(rel), &bytes)?;
        zip.start_file(format!("dogbones/{rel}"), opts)?;
        zip.write_all(&bytes)?;
    }

    zip.finish()?;

    println!(
        "Built component: {} ({} bytes)",
        out_wasm.display(),
        component.len()
    );
    println!("Packaged zip:    {}", zip_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use serde_json::{json, Value};
    use solidcraft_engine::Session;

    fn dist_and_approval(session: &mut Session) -> (PathBuf, String) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let pkg_dir = root.join("dist/dogbones");
        let zip_path = root.join("dist/dogbones.zip");
        assert!(pkg_dir.join("plugin.json").exists(), "run packager first");
        assert!(zip_path.exists(), "run packager first");

        let inspect = session
            .execute(
                "plugin.inspect",
                &json!({ "path": pkg_dir.to_string_lossy() }),
            )
            .expect("plugin.inspect should succeed");
        assert_eq!(inspect["within_host_limits"], json!(true));
        let approval = inspect["approval"]
            .as_str()
            .expect("approval hash")
            .to_string();
        (pkg_dir, approval)
    }

    /// Create a 100 x 60 x 18 mm panel with a 30 x 10 mm through-slot centered at (50, 30).
    fn make_slotted_panel(session: &mut Session, body_name: &str) {
        session
            .execute(
                "solid.box",
                &json!({ "length": 100.0, "width": 60.0, "height": 18.0, "body_name": body_name }),
            )
            .expect("box");
        let sk = session
            .execute(
                "sketch.create",
                &json!({ "plane": "XY", "offset": 18.0, "name": format!("{body_name}_SlotSketch") }),
            )
            .expect("slot sketch");
        let sk_id = sk["sketch"].as_u64().unwrap();
        session
            .execute(
                "sketch.rectangle.center",
                &json!({ "center": [50.0, 30.0], "corner": [65.0, 35.0] }),
            )
            .expect("slot rect");
        session.execute("sketch.finish", &json!({})).expect("finish");
        session
            .execute(
                "solid.extrude",
                &json!({
                    "sketch": sk_id,
                    "distance": -18.0,
                    "operation": "cut",
                    "targets": [body_name],
                    "name": format!("{body_name}_SlotCut")
                }),
            )
            .expect("slot cut");
    }

    #[test]
    fn body_select_corner_dogbones_on_through_slot_and_update() {
        let mut session = Session::default();
        let (pkg_dir, approval) = dist_and_approval(&mut session);
        make_slotted_panel(&mut session, "Panel");

        // 1. Create Corner dogbones with 6 mm tool + 0.025 mm clearance.
        let res = session
            .execute(
                "plugin.run",
                &json!({
                    "path": pkg_dir.to_string_lossy(),
                    "approval": approval,
                    "entry": "dogbones",
                    "params": {
                        "selection_type": "body",
                        "bodies": [{ "type": "body", "name": "Panel" }],
                        "tool_diameter": 6.0,
                        "additional_clearance": 0.025,
                        "dogbone_type": "corner"
                    }
                }),
            )
            .expect("dogbones should succeed");

        assert_eq!(res["result"]["corners_cut"], json!(4));
        assert_eq!(res["result"]["feature_ids"].as_array().map(Vec::len), Some(1));
        assert_eq!(res["result"]["sketch_ids"].as_array().map(Vec::len), Some(1));

        // Verify the 4 sharp corners of the 30x10 slot now have material removed slightly past
        // each wall (e.g. corner (35, 25) -> dogbone cuts into (34.5, 25.5)).
        let check = session
            .execute(
                "inspect.classify_points",
                &json!({
                    "body": "Panel",
                    "points": [
                        [35.0, 25.0, 9.0],
                        [65.0, 25.0, 9.0],
                        [65.0, 35.0, 9.0],
                        [35.0, 35.0, 9.0]
                    ]
                }),
            )
            .expect("classify");
        let results = check["results"].as_array().unwrap();
        for (i, r) in results.iter().enumerate() {
            assert_ne!(
                r["class"],
                json!("inside"),
                "corner {i} should be relieved (not inside solid): {r}"
            );
        }

        // 2. Create a second body and run Dogbones again with the SAME default feature_name
        //    to verify auto-numbered unique sketch/feature names never collide.
        make_slotted_panel(&mut session, "Panel2");
        let res2 = session
            .execute(
                "plugin.run",
                &json!({
                    "path": pkg_dir.to_string_lossy(),
                    "approval": approval,
                    "entry": "dogbones",
                    "params": {
                        "selection_type": "body",
                        "bodies": [{ "type": "body", "name": "Panel2" }],
                        "tool_diameter": 3.0,
                        "additional_clearance": 0.025,
                        "dogbone_type": "corner"
                    }
                }),
            )
            .expect("second dogbones run with same default feature_name should succeed");
        assert_eq!(res2["result"]["corners_cut"], json!(4));

        // 3. Run Update Dogbones and verify it cleanly deletes + regenerates both recipes (8 corners).
        let upd = session
            .execute(
                "plugin.run",
                &json!({
                    "path": pkg_dir.to_string_lossy(),
                    "approval": approval,
                    "entry": "update-dogbones",
                    "params": {}
                }),
            )
            .expect("update-dogbones should succeed");
        assert_eq!(upd["result"]["updated_recipes"], json!(2));
        assert_eq!(upd["result"]["corners_cut"], json!(8));
    }

    #[test]
    fn minimal_corner_and_t_bone_long_and_short_sides() {
        for kind in ["minimal_corner", "along_long_side", "along_short_side"] {
            let mut session = Session::default();
            let (pkg_dir, approval) = dist_and_approval(&mut session);
            make_slotted_panel(&mut session, "Panel");

            let res = session
                .execute(
                    "plugin.run",
                    &json!({
                        "path": pkg_dir.to_string_lossy(),
                        "approval": approval,
                        "entry": "dogbones",
                        "params": {
                            "selection_type": "body",
                            "bodies": [{ "type": "body", "name": "Panel" }],
                            "tool_diameter": 6.0,
                            "additional_clearance": 0.025,
                            "dogbone_type": kind,
                            "corner_interference": 0.25
                        }
                    }),
                )
                .unwrap_or_else(|e| panic!("dogbone type {kind} failed: {e:?}"));

            assert_eq!(
                res["result"]["corners_cut"],
                json!(4),
                "expected 4 corners cut for {kind}"
            );
        }
    }

    #[test]
    fn blind_pocket_preserves_floor_and_edge_select_picks_single_corner() {
        let mut session = Session::default();
        let (pkg_dir, approval) = dist_and_approval(&mut session);

        // 80 x 60 x 20 mm block with a 30 x 20 x 10 mm blind pocket from Z=20 down to Z=10.
        session
            .execute(
                "solid.box",
                &json!({ "length": 80.0, "width": 60.0, "height": 20.0, "body_name": "Block" }),
            )
            .expect("box");
        let sk = session
            .execute(
                "sketch.create",
                &json!({ "plane": "XY", "offset": 20.0, "name": "PocketSketch" }),
            )
            .expect("pocket sketch");
        let sk_id = sk["sketch"].as_u64().unwrap();
        session
            .execute(
                "sketch.rectangle.center",
                &json!({ "center": [40.0, 30.0], "corner": [55.0, 40.0] }),
            )
            .expect("pocket rect");
        session.execute("sketch.finish", &json!({})).expect("finish");
        session
            .execute(
                "solid.extrude",
                &json!({
                    "sketch": sk_id,
                    "distance": -10.0,
                    "operation": "cut",
                    "targets": ["Block"],
                    "name": "BlindPocket"
                }),
            )
            .expect("pocket cut");

        // Find the single vertical corner edge at (25, 20, 15).
        let listed = session
            .execute("model.edges", &json!({ "body": "Block" }))
            .expect("model.edges");
        let edges = listed["edges"].as_array().unwrap();
        let corner_edge = edges
            .iter()
            .find(|e| {
                let m = e["mid"].as_array().unwrap();
                (m[0].as_f64().unwrap() - 25.0).abs() < 0.2
                    && (m[1].as_f64().unwrap() - 20.0).abs() < 0.2
                    && (m[2].as_f64().unwrap() - 15.0).abs() < 0.2
            })
            .expect("vertical pocket corner edge");

        let res = session
            .execute(
                "plugin.run",
                &json!({
                    "path": pkg_dir.to_string_lossy(),
                    "approval": approval,
                    "entry": "dogbones",
                    "params": {
                        "selection_type": "edge",
                        "edges": [{
                            "type": "edge",
                            "body": "Block",
                            "index": corner_edge["index"],
                            "point": corner_edge["mid"]
                        }],
                        "tool_diameter": 6.0,
                        "dogbone_type": "corner"
                    }
                }),
            )
            .expect("edge select dogbone should succeed");

        assert_eq!(res["result"]["corners_cut"], json!(1));

        // Verify the blind pocket floor at Z=5 (below Z=10) is STILL SOLID inside the block!
        let floor_check = session
            .execute(
                "inspect.classify_points",
                &json!({
                    "body": "Block",
                    "points": [[25.0, 20.0, 5.0]]
                }),
            )
            .expect("classify floor");
        assert_eq!(
            floor_check["results"][0]["class"],
            json!("inside"),
            "blind pocket floor below Z=10 must remain untouched"
        );
    }

    #[test]
    fn upper_face_select_targets_only_chosen_orientation() {
        let mut session = Session::default();
        let (pkg_dir, approval) = dist_and_approval(&mut session);

        // Board with a blind pocket on top (+Z) AND a mortise on the front edge (-Y).
        session
            .execute(
                "solid.box",
                &json!({ "length": 100.0, "width": 40.0, "height": 20.0, "body_name": "Board" }),
            )
            .expect("box");

        // Top pocket (+Z): 20 x 14 x 8 mm
        let sk1 = session
            .execute("sketch.create", &json!({ "plane": "XY", "offset": 20.0 }))
            .expect("sk1");
        session
            .execute(
                "sketch.rectangle.center",
                &json!({ "center": [50.0, 20.0], "corner": [60.0, 27.0] }),
            )
            .expect("rect1");
        session.execute("sketch.finish", &json!({})).expect("fin1");
        session
            .execute(
                "solid.extrude",
                &json!({
                    "sketch": sk1["sketch"],
                    "distance": -8.0,
                    "operation": "cut",
                    "targets": ["Board"]
                }),
            )
            .expect("cut1");

        // Front-edge mortise (-Y face at y=0): 16 x 8 x 10 mm deep into +Y
        let sk2 = session
            .execute(
                "sketch.create",
                &json!({
                    "plane": {
                        "origin": [0.0, 0.0, 0.0],
                        "x_dir": [1.0, 0.0, 0.0],
                        "y_dir": [0.0, 0.0, 1.0]
                    }
                }),
            )
            .expect("sk2");
        session
            .execute(
                "sketch.rectangle.center",
                &json!({ "center": [50.0, 10.0], "corner": [58.0, 14.0] }),
            )
            .expect("rect2");
        session.execute("sketch.finish", &json!({})).expect("fin2");
        session
            .execute(
                "solid.extrude",
                &json!({
                    "sketch": sk2["sketch"],
                    "distance": -10.0,
                    "operation": "cut",
                    "targets": ["Board"]
                }),
            )
            .expect("cut2");

        // Select the front (-Y) face as Upper Face: should cut ONLY the 4 corners of the mortise!
        let faces = session
            .execute("model.faces", &json!({ "body": "Board" }))
            .expect("faces");
        let front_face = faces["faces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| {
                f["plane_normal"]
                    .as_array()
                    .is_some_and(|n| n[1].as_f64().unwrap() < -0.99)
            })
            .expect("front -Y face");

        let res = session
            .execute(
                "plugin.run",
                &json!({
                    "path": pkg_dir.to_string_lossy(),
                    "approval": approval,
                    "entry": "dogbones",
                    "params": {
                        "selection_type": "upper_face",
                        "upper_faces": [{
                            "type": "face",
                            "body": "Board",
                            "index": front_face["index"],
                            "point": front_face["point"].clone().as_array().cloned().map(Value::Array).unwrap_or_else(|| front_face["centroid"].clone())
                        }],
                        "tool_diameter": 4.0,
                        "dogbone_type": "corner"
                    }
                }),
            )
            .expect("upper_face dogbone should succeed");

        assert_eq!(
            res["result"]["corners_cut"],
            json!(4),
            "should cut only the 4 mortise corners on the chosen front face"
        );
    }
}
