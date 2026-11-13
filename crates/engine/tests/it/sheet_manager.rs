use cadcraft_engine::{Session, doc::EntityKind};
use serde_json::{Value, json};

fn drawing() -> Session {
    let mut s = Session::new();
    for (tag, value) in [("IMPIANTO", "Schema"), ("IMPIANTO2", "Locale"), ("NUMFOG", "old"), ("NUMFOGSEG", "old")] {
        s.execute("attdef", &json!({"tag":tag,"default":value,"at":[0,0]})).unwrap();
    }
    let handles: Vec<_> = s.doc().unwrap().model.handles().iter().map(|h| h.hex()).collect();
    s.execute("block", &json!({"name":"att_tav","base":[0,0],"handles":handles,"keep":"delete"})).unwrap();
    s.execute("rectang", &json!({"p1":[0,0],"p2":[420,297]})).unwrap();
    let handles: Vec<_> = s.doc().unwrap().model.handles().iter().map(|h| h.hex()).collect();
    s.execute("block", &json!({"name":"FG_SCHEMA","base":[0,0],"handles":handles,"keep":"delete"})).unwrap();
    for n in 1..=3 {
        s.execute("layer.new", &json!({"name":n.to_string(),"current":true})).unwrap();
        s.execute("insert", &json!({"name":"att_tav","at":[0,0],"attribs":{"IMPIANTO":format!("Pagina {n}")}})).unwrap();
        s.execute("insert", &json!({"name":"FG_SCHEMA","at":[0,0]})).unwrap();
        s.execute("line", &json!({"points":[[n*10,0],[n*10,5]]})).unwrap();
    }
    s.execute("layer.new", &json!({"name":"COMUNE"})).unwrap();
    s
}

#[test]
fn print_selection_is_numeric_order_and_does_not_modify_drawing() {
    use cadcraft_engine::cmd::file::{IoHooks, set_io};
    set_io(IoHooks {
        read: |_, _| Err("unused".into()),
        write: |_, _| Err("unused".into()),
        plot: Some(|d, space, params| {
            assert_eq!(*space, cadcraft_engine::doc::Space::Model);
            assert_eq!(params["sheetLayers"], json!(["1", "3"]));
            assert_eq!(params["paperBlackLayers"], json!(["CARTIGLIO_ATTRIBUTI"]));
            assert!(d.layer("COMUNE").unwrap().visible());
            assert_eq!(params["paper"], "A3");
            assert_eq!(params["fit"], true);
            assert_eq!(params["landscape"], false);
            let bytes = cadcraft_io::plot(d, space, params).map_err(|e| e.to_string())?;
            assert!(bytes.starts_with(b"%PDF-"));
            Ok(bytes)
        }),
    });
    let mut s = drawing();
    let before = s.doc().unwrap().clone();
    let selection = s.selection().to_vec();
    let result = s.execute("sheet.plot", &json!({"pages":[3,1],"paper":"A3","landscape":false})).unwrap();
    assert_eq!(result["sheetNumbers"], json!(["1", "3"]));
    assert_eq!(result["pages"], 2);
    assert!(result["bytes"].as_u64().unwrap() > 100);
    assert!(result["data"].as_str().unwrap().starts_with("JVBER"));
    assert_eq!(*s.doc().unwrap(), before);
    assert_eq!(s.selection(), selection);
    for params in [json!({"pages":[]}), json!({"pages":[99]}), json!({"pages":[1],"paper":"invalid"}), json!({"pages":[1],"path":"wrong.dxf"})] {
        assert!(s.execute("sheet.plot", &params).is_err());
        assert_eq!(*s.doc().unwrap(), before);
    }
}
fn list(s: &mut Session) -> Value {
    s.execute("sheet.list", &json!({})).unwrap()
}

fn move_title_to_shared_layer(s: &mut Session, page: &str, name: &str, layer: &str, sheet: &str) {
    s.execute("layer.new", &json!({"name":layer})).unwrap();
    let mut definition = (**s.doc().unwrap().blocks.get("att_tav").unwrap()).clone();
    definition.name = name.into();
    s.doc_mut().unwrap().blocks.insert(name.into(), std::sync::Arc::new(definition));
    let handle = s
        .doc()
        .unwrap()
        .model
        .iter()
        .find(|entity| entity.common.layer == page && matches!(&entity.kind, EntityKind::Insert(insert) if insert.block == "att_tav"))
        .map(|entity| entity.handle)
        .unwrap();
    s.doc_mut()
        .unwrap()
        .modify_entity(handle, |entity| {
            entity.common.layer = layer.into();
            let EntityKind::Insert(insert) = &mut entity.kind else { return };
            insert.block = name.into();
            for attribute in &mut insert.attribs {
                match attribute.tag.as_str() {
                    "IMPIANTO" => {
                        attribute.tag = "OGGETTO1".into();
                        attribute.text.value = "Schema fotovoltaico".into();
                    }
                    "IMPIANTO2" => {
                        attribute.tag = "OGGETTO2".into();
                        attribute.text.value = "Piano copertura".into();
                    }
                    "NUMFOG" => {
                        attribute.tag = "SHEET".into();
                        attribute.text.value = sheet.into();
                    }
                    "NUMFOGSEG" => {
                        attribute.tag = "NSHEET".into();
                        attribute.text.value = String::new();
                    }
                    _ => {}
                }
            }
            let Some(template) = insert.attribs.first().cloned() else { return };
            for (tag, value) in [("SHEETS", "3"), ("COMUNE", "Sarego"), ("DATA", "2026-11-12")] {
                let mut attribute = template.clone();
                attribute.tag = tag.into();
                attribute.text.value = value.into();
                insert.attribs.push(attribute);
            }
        })
        .unwrap();
}

fn block_value(s: &Session, block: &str, tag: &str) -> Option<String> {
    s.doc().ok()?.model.iter().find_map(|entity| {
        let EntityKind::Insert(insert) = &entity.kind else { return None };
        (insert.block == block)
            .then(|| insert.attribs.iter().find(|attribute| attribute.tag == tag).map(|attribute| attribute.text.value.clone()))
            .flatten()
    })
}

#[test]
fn sg2018_on_logo_layer_uses_sheet_and_exposes_title_fields() {
    let mut s = drawing();
    move_title_to_shared_layer(&mut s, "2", "SG2018", "-LOGO", "02");

    let rows = list(&mut s);
    assert_eq!(rows["pages"][1]["titleBlocks"], json!(["SG2018"]));
    assert_eq!(rows["pages"][1]["description1"], "Schema fotovoltaico");
    assert_eq!(rows["pages"][1]["description2"], "Piano copertura");
    assert_eq!(rows["pages"][1]["values"]["SHEET"], "02");
    assert_eq!(rows["pages"][1]["values"]["COMUNE"], "Sarego");

    s.execute("sheet.attributes", &json!({"pages":[2],"values":{"OGGETTO1":"Schema FV aggiornato","COMUNE":"Vicenza"}})).unwrap();
    assert_eq!(block_value(&s, "SG2018", "OGGETTO1").as_deref(), Some("Schema FV aggiornato"));
    assert_eq!(block_value(&s, "SG2018", "COMUNE").as_deref(), Some("Vicenza"));

    s.execute("sheet.renumber", &json!({"pages":[2],"start":7})).unwrap();
    assert_eq!(block_value(&s, "SG2018", "SHEET").as_deref(), Some("7"));
    assert!(
        s.doc()
            .unwrap()
            .model
            .iter()
            .any(|entity| { entity.common.layer == "-LOGO" && matches!(&entity.kind, EntityKind::Insert(insert) if insert.block == "SG2018") })
    );
}

#[test]
fn equivalent_title_block_on_cartiglio_layer_is_detected_by_attributes() {
    let mut s = drawing();
    move_title_to_shared_layer(&mut s, "3", "PROJECT_FRAME", "-CARTIGLIO", "3");

    let rows = list(&mut s);
    assert_eq!(rows["pages"][2]["titleBlocks"], json!(["PROJECT_FRAME"]));
    assert_eq!(rows["pages"][2]["values"]["DATA"], "2026-11-12");

    s.execute("sheet.delete", &json!({"pages":[3]})).unwrap();
    assert!(block_value(&s, "PROJECT_FRAME", "SHEET").is_none());
    assert!(s.doc().unwrap().layer("-CARTIGLIO").is_some());
}

#[test]
fn combined_title_formats_map_metadata_without_editing_column_attributes() {
    let mut s = drawing();
    for name in ["0UNIA3", "00A3SCHEMI"] {
        let mut definition = (**s.doc().unwrap().blocks.get("att_tav").unwrap()).clone();
        definition.name = name.into();
        s.doc_mut().unwrap().blocks.insert(name.into(), std::sync::Arc::new(definition));
    }
    let handles = s.doc().unwrap().model.handles();
    for h in handles {
        s.doc_mut()
            .unwrap()
            .modify_entity(h, |e| {
                let EntityKind::Insert(i) = &mut e.kind else { return };
                if i.block != "att_tav" {
                    return;
                }
                i.block = if e.common.layer == "3" { "00A3SCHEMI".into() } else { "0UNIA3".into() };
                for a in &mut i.attribs {
                    a.tag = match a.tag.as_str() {
                        "IMPIANTO" => "OGGETTO1",
                        "IMPIANTO2" => "OGGETTO2",
                        "NUMFOG" => "SHEET",
                        "NUMFOGSEG" => "NSHEET",
                        other => other,
                    }
                    .into();
                }
                let mut column = i.attribs[0].clone();
                column.tag = "U_DEN".into();
                column.text.value = "Motore".into();
                i.attribs.push(column.clone());
                i.attribs.push(column.clone());
                column.tag = "SHEETS".into();
                column.text.value = "3".into();
                i.attribs.push(column);
            })
            .unwrap();
    }
    let before = s.doc().unwrap().clone();
    let rows = list(&mut s);
    assert_eq!(rows["pages"][0]["description1"], "Pagina 1");
    assert_eq!(rows["pages"][2]["titleBlocks"], json!(["00A3SCHEMI"]));
    assert!(rows["pages"][0]["values"].get("U_DEN").is_none());
    s.execute("sheet.attributes", &json!({"pages":[1,3],"values":{"IMPIANTO":"Comune"}})).unwrap();
    assert_eq!(value(&s, "1", "OGGETTO1"), "Comune");
    assert_eq!(value(&s, "3", "OGGETTO1"), "Comune");
    assert_eq!(value(&s, "1", "U_DEN"), "Motore");
    s.undo().unwrap();
    assert_eq!(*s.doc().unwrap(), before);
    s.execute("sheet.renumber", &json!({"pages":[1,2,3],"start":11})).unwrap();
    assert_eq!(value(&s, "11", "SHEET"), "11");
    assert_eq!(value(&s, "11", "NSHEET"), "12");
    assert_eq!(value(&s, "13", "NSHEET"), "");
    assert_eq!(value(&s, "13", "SHEETS"), "3");
    s.undo().unwrap();
    assert_eq!(*s.doc().unwrap(), before);
}
fn value(s: &Session, page: &str, tag: &str) -> String {
    s.doc()
        .unwrap()
        .model
        .iter()
        .filter(|e| e.common.layer == page)
        .find_map(|e| if let EntityKind::Insert(i) = &e.kind { i.attribs.iter().find(|a| a.tag == tag).map(|a| a.text.value.clone()) } else { None })
        .unwrap()
}
#[test]
fn reorder_updates_layers_and_title_numbers_without_translating_geometry() {
    let mut s = drawing();
    let before = s.doc().unwrap().clone();
    let line = s.doc().unwrap().model.iter().find(|e| e.common.layer == "3" && matches!(e.kind, EntityKind::Line(_))).unwrap().clone();
    s.execute("sheet.move", &json!({"pages":[3],"position":1})).unwrap();
    assert_eq!(value(&s, "1", "IMPIANTO"), "Pagina 3");
    assert_eq!(value(&s, "2", "IMPIANTO"), "Pagina 1");
    assert_eq!(value(&s, "1", "NUMFOG"), "1");
    assert_eq!(value(&s, "1", "NUMFOGSEG"), "2");
    assert_eq!(value(&s, "3", "NUMFOGSEG"), "");
    let after = s.doc().unwrap().entity(line.handle).unwrap();
    assert_eq!(after.common.layer, "1");
    assert_eq!(after.kind, line.kind);
    s.undo().unwrap();
    assert_eq!(*s.doc().unwrap(), before);
}
#[test]
fn insert_copies_only_frame_and_title_then_undo_restores_everything() {
    let mut s = drawing();
    let before = s.doc().unwrap().clone();
    s.execute("sheet.insert", &json!({"template":"2","position":2,"blocks":["FG_SCHEMA","att_tav"]})).unwrap();
    assert_eq!(list(&mut s)["pages"].as_array().unwrap().len(), 4);
    let entities: Vec<_> = s.doc().unwrap().model.iter().filter(|e| e.common.layer == "2").collect();
    assert_eq!(entities.len(), 2);
    assert!(entities.iter().all(|e| matches!(e.kind, EntityKind::Insert(_))));
    assert_eq!(value(&s, "2", "NUMFOG"), "2");
    assert_eq!(value(&s, "3", "IMPIANTO"), "Pagina 2");
    s.undo().unwrap();
    assert_eq!(*s.doc().unwrap(), before);
}
#[test]
fn delete_selected_pages_keeps_common_layers_and_last_page_uses_layer_zero() {
    let mut s = drawing();
    s.execute("sheet.delete", &json!({"pages":[2]})).unwrap();
    assert!(s.doc().unwrap().layer("2").is_none());
    assert!(s.doc().unwrap().layer("COMUNE").is_some());
    assert_eq!(value(&s, "1", "NUMFOGSEG"), "3");
    s.execute("sheet.delete", &json!({"pages":[1,3]})).unwrap();
    assert!(list(&mut s)["pages"].as_array().unwrap().is_empty());
    assert_eq!(s.doc().unwrap().header.str("CLAYER", ""), "0");
}
#[test]
fn bulk_attribute_edits_are_scoped_to_the_selection_and_undoable() {
    let mut s = drawing();
    let before = s.doc().unwrap().clone();
    s.execute("sheet.attributes", &json!({"pages":[1,3],"values":{"IMPIANTO":"Nuovo","IMPIANTO2":""}})).unwrap();
    assert_eq!(value(&s, "1", "IMPIANTO"), "Nuovo");
    assert_eq!(value(&s, "3", "IMPIANTO2"), "");
    assert_eq!(value(&s, "2", "IMPIANTO"), "Pagina 2");
    s.undo().unwrap();
    assert_eq!(*s.doc().unwrap(), before);
}
#[test]
fn collisions_missing_pages_locked_layers_and_overflow_are_atomic() {
    let mut s = drawing();
    for (command, params) in [
        ("sheet.renumber", json!({"pages":[1],"start":2})),
        ("sheet.renumber", json!({"pages":[1,2],"start":u64::MAX})),
        ("sheet.move", json!({"pages":[1],"position":99})),
        ("sheet.delete", json!({"pages":[1,99]})),
        ("sheet.insert", json!({"template":"1","position":2,"blocks":["missing"]})),
    ] {
        let before = s.doc().unwrap().clone();
        assert!(s.execute(command, &params).is_err(), "{command}");
        assert_eq!(*s.doc().unwrap(), before);
    }
    s.execute("layer.set", &json!({"name":"2","locked":true})).unwrap();
    let before = s.doc().unwrap().clone();
    assert!(s.execute("sheet.move", &json!({"pages":[3],"position":1})).is_err());
    assert_eq!(*s.doc().unwrap(), before);
}

#[test]
fn renumbered_pages_and_title_values_survive_dxf_save_and_reopen() {
    let mut s = drawing();
    s.execute("sheet.renumber", &json!({"pages":[1,2,3],"start":10})).unwrap();
    let bytes = cadcraft_io::write_dxf(s.doc().unwrap());
    let reopened = cadcraft_io::read_dxf(bytes.as_bytes()).unwrap();
    let mut next = Session::empty();
    next.open_drawing(reopened, "roundtrip", None);
    assert_eq!(value(&next, "10", "NUMFOG"), "10");
    assert_eq!(value(&next, "10", "NUMFOGSEG"), "11");
    assert_eq!(list(&mut next)["pages"][2]["number"], "12");
}

#[test]
fn layer_metadata_and_paper_viewport_references_follow_page_changes() {
    use cadcraft_engine::doc::{Entity, Handle, Viewport};
    use std::sync::Arc;
    let mut s = drawing();
    let h = {
        let d = s.doc_mut().unwrap();
        let h = d.new_handle();
        let layout = d.layouts.first_mut().unwrap();
        layout.entities.push(Entity::new(h, EntityKind::Viewport(Viewport { frozen_layers: vec!["1".into(), "3".into()], ..Default::default() })));
        let foreign = Arc::make_mut(&mut d.foreign);
        foreign.records.insert(("LAYER".into(), "3".into()), Default::default());
        foreign.targets.insert(Handle(999999), ("LAYER".into(), "3".into()));
        h
    };
    s.execute("sheet.move", &json!({"pages":[3],"position":1})).unwrap();
    let d = s.doc().unwrap();
    assert!(d.foreign.records.contains_key(&("LAYER".into(), "1".into())));
    let EntityKind::Viewport(v) = &d.entity(h).unwrap().kind else { panic!("viewport missing") };
    assert_eq!(v.frozen_layers, vec!["2", "1"]);
    s.execute("sheet.insert", &json!({"template":"1","position":2,"blocks":["FG_SCHEMA","att_tav"]})).unwrap();
    let EntityKind::Viewport(v) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!("viewport missing") };
    assert!(v.frozen_layers.iter().any(|n| n == "2"), "new sheets stay hidden in existing paper viewports");
    s.execute("sheet.delete", &json!({"pages":[2]})).unwrap();
    let EntityKind::Viewport(v) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!("viewport missing") };
    assert!(!v.frozen_layers.iter().any(|n| n == "2"));
}
