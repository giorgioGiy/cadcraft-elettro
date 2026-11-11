use std::sync::Arc;

use cadcraft_color::Color;
use cadcraft_engine::Session;
use cadcraft_engine::doc::{Block, EntityKind};
use cadcraft_geom::Vec2;
use serde_json::json;

#[test]
fn a0_one_to_one_hundred_in_metres_matches_eplus_dimensions() {
    let mut s = Session::new();
    let value = s.execute("electrical.sheet.setup", &json!({"paper":"A0", "scaleDenominator":100.0, "units":"m", "landscape":true})).unwrap();
    assert_eq!(value["drawingWidth"], json!(118.9));
    assert_eq!(value["drawingHeight"], json!(84.1));
    assert_eq!(value["symbolScale"], json!(0.1));
    assert_eq!(value["plotMillimetresPerDrawingUnit"], json!(10.0));
    let d = s.doc().unwrap();
    assert_eq!(d.header.i64("INSUNITS", 0), 6);
    assert_eq!(d.header.f64("DIMSCALE", 0.0), 0.1);
    assert_eq!(d.header.f64("LTSCALE", 0.0), 30.0);
    assert_eq!(d.header.point("LIMMAX").unwrap().xy(), cadcraft_geom::Vec2::new(118.9, 84.1));
}

#[test]
fn explicit_symbol_scale_is_kept_and_hostile_values_are_rejected() {
    let mut s = Session::new();
    s.execute("electrical.sheet.setup", &json!({"paper":"A3", "scaleDenominator":50.0, "units":"cm", "landscape":false, "symbolScale":2.5})).unwrap();
    assert_eq!(s.doc().unwrap().header.f64("DIMSCALE", 0.0), 2.5);
    assert_eq!(s.doc().unwrap().header.f64("LTSCALE", 0.0), 750.0);
    for bad in [
        json!({"paper":"A9", "scaleDenominator":100.0, "units":"m"}),
        json!({"paper":"A3", "scaleDenominator":0.0, "units":"m"}),
        json!({"paper":"A3", "scaleDenominator":100.0, "units":"parsec"}),
        json!({"paper":"A3", "scaleDenominator":100.0, "units":"m", "symbolScale":-1.0}),
    ] {
        assert!(s.execute("electrical.sheet.setup", &bad).is_err());
    }
}

#[test]
fn create_builds_iso_frame_electrical_layers_and_native_title_block() {
    let mut s = Session::new();
    let value = s.execute("electrical.sheet.create", &json!({"paper":"A3", "scaleDenominator":100.0, "units":"m", "landscape":true})).unwrap();

    assert_eq!(value["drawingWidth"], json!(42.0));
    assert_eq!(value["drawingHeight"], json!(29.7));
    assert_eq!(value["frameHandles"].as_array().unwrap().len(), 1);
    assert_eq!(value["markHandles"].as_array().unwrap().len(), 12);
    assert_eq!(value["standardProfile"], json!(true));
    assert_eq!(value["nativeTitleBlock"], json!(true));
    assert_eq!(value["titleBlockValidation"]["valid"], json!(true));
    assert!(value["titleBlockHandle"].as_str().is_some());
    let d = s.doc().unwrap();
    assert_eq!(d.layer("Terra").unwrap().color, Color::Index(3));
    assert_eq!(d.layer("BORDO").unwrap().color, Color::Index(7));
    assert!(d.layer("-INFOMLT").unwrap().frozen);
    let frames = d
        .model
        .iter()
        .filter_map(|entity| match &entity.kind {
            EntityKind::LwPolyline(poly) if entity.common.layer == "SQUADRATURA_ISO" => Some((poly, entity.common.color)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0.vertices[0].p, Vec2::new(2.0, 1.0));
    assert_eq!(frames[0].1, Color::ByLayer);
    let title = d
        .model
        .iter()
        .find_map(|entity| match &entity.kind {
            EntityKind::Insert(insert) if insert.block == "CADCRAFT_ISO7200" => Some((entity, insert)),
            _ => None,
        })
        .unwrap();
    assert_eq!(title.0.common.layer, "BORDO");
    assert_eq!(title.1.insert.xy(), Vec2::new(41.0, 1.0));
    assert_eq!(title.1.scale.x, 0.1);
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "SCALA" && attribute.text.value == "1:100"));
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "FORMATO" && attribute.text.value == "A3"));
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "TITOLO" && attribute.text.value == "Schema d'installazione impianto elettrico"));
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "CREATO_DA" && attribute.text.value == "Dott. Giorgio Sardei"));
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "ID_DOCUMENTO" && attribute.text.value == "Sardei01"));
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "TIPO_DOCUMENTO" && attribute.text.value == "Esecutivo"));
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "REVISIONE" && attribute.text.value == "0"));
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "APPROVATO" && attribute.text.value.is_empty()));
    assert!(title.1.attribs.iter().any(|attribute| attribute.tag == "COMMITTENTE" && attribute.text.value.is_empty()));
    assert_eq!(d.header.str("CLAYER", ""), "0");
}

#[test]
fn compatibility_profile_keeps_the_eplus_double_frame() {
    let mut s = Session::new();
    s.doc_mut().unwrap().blocks.insert("Cart_pl".into(), Arc::new(Block::new("Cart_pl")));
    let value = s
        .execute(
            "electrical.sheet.create",
            &json!({"paper":"A3", "scaleDenominator":100.0, "units":"m", "standardProfile":false, "nativeTitleBlock":false}),
        )
        .unwrap();
    assert_eq!(value["standardProfile"], json!(false));
    assert_eq!(value["frameHandles"].as_array().unwrap().len(), 2);
    assert!(value["markHandles"].as_array().unwrap().is_empty());
    let starts = s
        .doc()
        .unwrap()
        .model
        .iter()
        .filter_map(|entity| match &entity.kind {
            EntityKind::LwPolyline(poly) if entity.common.layer == "BORDO" => poly.vertices.first().map(|vertex| vertex.p),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(starts, [Vec2::new(0.5, 0.5), Vec2::new(1.0, 1.0)]);
}

#[test]
fn iso_profile_forces_a4_portrait_but_eplus_can_use_landscape() {
    let mut iso = Session::new();
    let value = iso
        .execute("electrical.sheet.setup", &json!({"paper":"A4", "scaleDenominator":100.0, "units":"m", "landscape":true, "standardProfile":true}))
        .unwrap();
    assert_eq!((value["drawingWidth"].as_f64(), value["drawingHeight"].as_f64()), (Some(21.0), Some(29.7)));

    let mut eplus = Session::new();
    let value = eplus
        .execute("electrical.sheet.setup", &json!({"paper":"A4", "scaleDenominator":100.0, "units":"m", "landscape":true, "standardProfile":false}))
        .unwrap();
    assert_eq!((value["drawingWidth"].as_f64(), value["drawingHeight"].as_f64()), (Some(29.7), Some(21.0)));
}

#[test]
fn create_without_a_title_block_is_safe_and_duplicate_sheet_is_rejected() {
    let mut s = Session::new();
    let params = json!({
        "paper":"A4",
        "scaleDenominator":1.0,
        "units":"mm",
        "landscape":false,
        "insertTitleBlock":false
    });
    s.execute("electrical.sheet.create", &params).unwrap();
    let count = s.doc().unwrap().model.len();
    assert!(s.execute("electrical.sheet.create", &params).is_err());
    assert_eq!(s.doc().unwrap().model.len(), count);

    let mut missing = Session::new();
    let before = missing.doc().unwrap().layers.len();
    assert!(
        missing
            .execute(
                "electrical.sheet.create",
                &json!({"paper":"A3", "scaleDenominator":1.0, "units":"mm", "standardProfile":false, "nativeTitleBlock":false}),
            )
            .is_err()
    );
    assert_eq!(missing.doc().unwrap().layers.len(), before);
}

#[test]
fn replacing_a_sheet_preserves_all_title_block_values_including_duplicate_tags() {
    let mut s = Session::new();
    s.execute("attdef", &json!({"tag":"PROGETTO1", "default":"Oggetto", "at":[0, 0]})).unwrap();
    s.execute("attdef", &json!({"tag":"PROGETTO1", "default":"Esecutivo", "at":[0, 1]})).unwrap();
    let handles = s.doc().unwrap().model.iter().map(|entity| entity.handle.hex()).collect::<Vec<_>>();
    s.execute("block", &json!({"name":"Cart_pl", "base":[0, 0], "handles":handles, "keep":"delete"})).unwrap();
    s.execute("electrical.sheet.create", &json!({"paper":"A3", "scaleDenominator":100.0, "units":"m", "nativeTitleBlock":false})).unwrap();
    let old = s
        .doc()
        .unwrap()
        .model
        .iter()
        .find_map(|entity| matches!(&entity.kind, EntityKind::Insert(insert) if insert.block == "Cart_pl").then_some(entity.handle))
        .unwrap();
    s.doc_mut()
        .unwrap()
        .modify_entity(old, |entity| {
            if let EntityKind::Insert(insert) = &mut entity.kind {
                if let Some(attribute) = insert.attribs.first_mut() {
                    attribute.text.value = "Centrale termica".into();
                }
                if let Some(attribute) = insert.attribs.get_mut(1) {
                    attribute.text.value = "Progetto esecutivo".into();
                }
            }
        })
        .unwrap();

    s.execute(
        "electrical.sheet.create",
        &json!({"paper":"A3", "scaleDenominator":1000.0, "units":"m", "replaceExisting":true, "nativeTitleBlock":false}),
    )
    .unwrap();
    let insert = s
        .doc()
        .unwrap()
        .model
        .iter()
        .find_map(|entity| match &entity.kind {
            EntityKind::Insert(insert) if insert.block == "Cart_pl" => Some(insert),
            _ => None,
        })
        .unwrap();
    assert_eq!(insert.attribs.iter().map(|attribute| attribute.text.value.as_str()).collect::<Vec<_>>(), ["Centrale termica", "Progetto esecutivo"]);
    assert_eq!(insert.scale.x, 1.0);
    assert_eq!(insert.insert.xy(), Vec2::new(410.0, 10.0));
}

#[test]
fn large_iso_sheets_receive_modular_folding_marks_but_a3_does_not() {
    let mut a1 = Session::new();
    let value =
        a1.execute("electrical.sheet.create", &json!({"paper":"A1", "scaleDenominator":1.0, "units":"mm", "insertTitleBlock":false})).unwrap();
    assert_eq!(value["foldMarkHandles"].as_array().unwrap().len(), 8);

    let mut a3 = Session::new();
    let value =
        a3.execute("electrical.sheet.create", &json!({"paper":"A3", "scaleDenominator":1.0, "units":"mm", "insertTitleBlock":false})).unwrap();
    assert!(value["foldMarkHandles"].as_array().unwrap().is_empty());
}

#[test]
fn title_block_validator_reports_native_and_incomplete_external_blocks() {
    let mut native = Session::new();
    native.execute("electrical.sheet.create", &json!({"paper":"A2", "scaleDenominator":50.0, "units":"mm"})).unwrap();
    let value = native.execute("electrical.titleblock.validate", &json!({})).unwrap();
    assert_eq!(value["valid"], json!(true));
    assert_eq!(value["block"], json!("CADCRAFT_ISO7200"));

    let mut external = Session::new();
    external.doc_mut().unwrap().blocks.insert("Cart_pl".into(), Arc::new(Block::new("Cart_pl")));
    let value = external.execute("electrical.titleblock.validate", &json!({"titleBlock":"Cart_pl"})).unwrap();
    assert_eq!(value["valid"], json!(false));
    assert!(!value["missingFields"].as_array().unwrap().is_empty());
}
