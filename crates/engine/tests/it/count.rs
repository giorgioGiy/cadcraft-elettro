//! COUNT across model space, layouts and nested blocks; objects like a picked one; COUNTLIST and
//! COUNTTABLE (#563).

use std::sync::Arc;
use std::time::Instant;

use cadcraft_engine::doc::geom::Vec3;
use cadcraft_engine::doc::{Block, Circle, Common, Entity, EntityKind, Handle, Insert, Layer, Space};
use cadcraft_engine::{Input, Session};
use serde_json::{Value, json};

fn insert(block: &str, x: f64, cols: u32, rows: u32) -> EntityKind {
    EntityKind::Insert(Insert {
        block: block.into(),
        insert: Vec3::new(x, 0.0, 0.0),
        scale: Vec3::new(1.0, 1.0, 1.0),
        rotation: 0.0,
        attribs: Vec::new(),
        cols,
        rows,
        col_spacing: 5.0,
        row_spacing: 5.0,
        clip: None,
    })
}

fn circle(x: f64, r: f64) -> EntityKind {
    EntityKind::Circle(Circle { center: Vec3::new(x, 0.0, 0.0), radius: r })
}

/// Define block `name` holding `kinds`.
fn define(s: &mut Session, name: &str, kinds: Vec<EntityKind>) {
    let d = s.doc_mut().unwrap();
    let mut b = Block::new(name);
    for k in kinds {
        let h = d.new_handle();
        b.entities.push(Entity::new(h, k));
    }
    d.blocks.insert(name.into(), Arc::new(b));
}

fn put(s: &mut Session, space: &Space, k: EntityKind) -> Handle {
    s.doc_mut().unwrap().add(space, Common::default(), k).unwrap()
}

fn layout() -> Space {
    Space::Paper("Layout1".into())
}

/// BOLT is a circle; PLATE holds four BOLTs. Model space: two PLATEs and one BOLT; Layout1: one
/// PLATE.
fn plates() -> Session {
    let mut s = Session::new();
    define(&mut s, "BOLT", vec![circle(0.0, 1.0)]);
    define(&mut s, "PLATE", (0..4).map(|i| insert("BOLT", f64::from(i), 1, 1)).collect());
    put(&mut s, &Space::Model, insert("PLATE", 0.0, 1, 1));
    put(&mut s, &Space::Model, insert("PLATE", 50.0, 1, 1));
    put(&mut s, &Space::Model, insert("BOLT", 100.0, 1, 1));
    put(&mut s, &layout(), insert("PLATE", 0.0, 1, 1));
    s
}

#[test]
fn counts_nested_references_in_the_current_space() {
    let mut s = plates();
    let r = s.execute("count", &json!({})).unwrap();
    assert_eq!(r["blocks"], json!({"BOLT": 9, "PLATE": 2}), "{r}");
    assert_eq!(r["nested"], json!({"BOLT": 8}), "{r}");
    assert_eq!(r["message"], "Block references in model space:\n  BOLT: 9 (8 nested)\n  PLATE: 2\n  Total: 11");
    let r = s.execute("count", &json!({"block": "bolt"})).unwrap();
    assert_eq!((r["count"].clone(), r["nested"].clone()), (json!(9), json!(8)), "{r}");
    assert_eq!(r["handles"].as_array().map(Vec::len), Some(3), "both plates and the bolt hold a BOLT: {r}");
    assert_eq!(r["message"], "Block BOLT: 9 in model space (8 nested).");
    // Top-level references only.
    let r = s.execute("count", &json!({"nested": false})).unwrap();
    assert_eq!(r["blocks"], json!({"BOLT": 1, "PLATE": 2}), "{r}");
}

#[test]
fn counts_layouts_and_all_spaces() {
    let mut s = plates();
    let r = s.execute("count", &json!({"space": "layout1"})).unwrap();
    assert_eq!(r["blocks"], json!({"BOLT": 4, "PLATE": 1}), "{r}");
    assert!(r["message"].as_str().unwrap().starts_with("Block references in layout Layout1:"), "{r}");
    let r = s.execute("count", &json!({"space": "all"})).unwrap();
    assert_eq!(r["blocks"], json!({"BOLT": 13, "PLATE": 3}), "{r}");
    assert_eq!(r["spaces"]["Model"], json!({"BOLT": 9, "PLATE": 2}), "{r}");
    assert_eq!(r["spaces"]["Layout1"], json!({"BOLT": 4, "PLATE": 1}), "{r}");
    // COUNTLIST lists every space by default.
    let r = s.execute("countlist", &json!({})).unwrap();
    assert_eq!(r["total"], 16, "{r}");
    assert!(s.execute("count", &json!({"space": "NoSuchLayout"})).is_err());
    assert!(s.execute("count", &json!({"space": 7})).is_err());
}

#[test]
fn counts_minsert_copies_and_selections() {
    let mut s = plates();
    let grid = put(&mut s, &Space::Model, insert("BOLT", 200.0, 3, 2));
    let r = s.execute("count", &json!({"block": "BOLT"})).unwrap();
    assert_eq!(r["count"], 15, "a 3 × 2 MINSERT is six bolts: {r}");
    let r = s.execute("count", &json!({"handles": [grid.hex()]})).unwrap();
    assert_eq!(r["blocks"], json!({"BOLT": 6}), "{r}");
    assert!(r["message"].as_str().unwrap().contains("the selection"), "{r}");
}

#[test]
fn counts_objects_like_a_picked_one_and_highlights_them() {
    let mut s = Session::new();
    let a = put(&mut s, &Space::Model, circle(0.0, 1.0));
    put(&mut s, &Space::Model, circle(10.0, 1.0));
    put(&mut s, &Space::Model, circle(20.0, 1.0));
    put(&mut s, &Space::Model, circle(30.0, 2.0));
    let r = s.execute("count", &json!({"object": a.hex(), "highlight": true})).unwrap();
    assert_eq!(r["count"], 3, "the larger circle is not the same shape: {r}");
    assert_eq!(s.selection().len(), 3);
    assert!(s.execute("count", &json!({"object": "zz"})).is_err());
    assert!(s.execute("count", &json!({"object": "FFFFFF"})).is_err());
}

#[test]
fn count_at_the_command_line() {
    let mut s = plates();
    let plate = s.doc().unwrap().model.iter().find(|e| matches!(&e.kind, EntityKind::Insert(i) if i.block == "PLATE")).unwrap().handle;
    s.cmdline("count").unwrap();
    s.input(Input::Pick(vec![plate])).unwrap();
    assert_eq!(s.log.last().map(String::as_str), Some("Block PLATE: 2 in model space."));
    assert_eq!(s.selection().len(), 2, "both plates are highlighted");
    s.cmdline("count").unwrap();
    s.cmdline("s").unwrap();
    assert_eq!(s.log.last().map(String::as_str), Some("  Total: 16"));
    assert!(s.running.is_none());
}

#[test]
fn counttable_inserts_a_table() {
    let mut s = plates();
    let r = s.execute("counttable", &json!({"at": [0, -20]})).unwrap();
    let h = Handle::parse_hex(r["handle"].as_str().unwrap()).unwrap();
    let EntityKind::Table(t) = &s.doc().unwrap().entity(h).unwrap().kind else { panic!("not a table") };
    let text: Vec<Vec<&str>> = t.cells.iter().map(|row| row.iter().map(|c| c.text.as_str()).collect()).collect();
    assert!(text.contains(&vec!["BOLT", "9"]) && text.contains(&vec!["PLATE", "2"]), "{text:?}");
    s.execute("undo", &json!({})).unwrap();
    assert!(s.doc().unwrap().entity(h).is_none(), "one undo step removes the table");
    assert!(Session::new().execute("counttable", &json!({"at": [0, 0]})).is_err(), "nothing to count");
}

#[test]
fn symbol_legend_uses_a_layer_all_layers_or_a_window_selection() {
    let mut s = Session::new();
    define(&mut s, "LAMP", vec![circle(0.0, 1.0)]);
    define(&mut s, "SOCKET", vec![circle(0.0, 2.0)]);
    define(&mut s, "WALL", vec![circle(0.0, 4.0)]);
    define(&mut s, "Cart_pl", vec![circle(0.0, 3.0)]);
    s.execute("blocks.description", &json!({"name":"LAMP", "description":"Lampada a soffitto"})).unwrap();
    s.execute("blocks.description", &json!({"name":"SOCKET", "description":"Presa elettrica"})).unwrap();
    s.execute("blocks.description", &json!({"name":"WALL", "description":"Parete edilizia"})).unwrap();
    s.doc_mut().unwrap().layers.push(Layer { name: "IMPIANTO LUCE".into(), ..Layer::default() });
    s.doc_mut().unwrap().layers.push(Layer { name: "IMPIANTO PRESE".into(), ..Layer::default() });
    s.doc_mut().unwrap().layers.push(Layer { name: "ARCH|MURI".into(), ..Layer::default() });
    s.doc_mut().unwrap().header.set_str("CLAYER", "IMPIANTO LUCE");
    let lamp_a = put(&mut s, &Space::Model, insert("LAMP", 0.0, 1, 1));
    let lamp_b = put(&mut s, &Space::Model, insert("LAMP", 10.0, 1, 1));
    let socket = put(&mut s, &Space::Model, insert("SOCKET", 20.0, 1, 1));
    let wall = put(&mut s, &Space::Model, insert("WALL", 25.0, 1, 1));
    let title = put(&mut s, &Space::Model, insert("Cart_pl", 30.0, 1, 1));
    for handle in [lamp_a, lamp_b] {
        s.doc_mut().unwrap().modify_entity(handle, |entity| entity.common.layer = "IMPIANTO LUCE".into()).unwrap();
    }
    s.doc_mut().unwrap().modify_entity(socket, |entity| entity.common.layer = "IMPIANTO PRESE".into()).unwrap();
    s.doc_mut().unwrap().modify_entity(wall, |entity| entity.common.layer = "ARCH|MURI".into()).unwrap();
    s.doc_mut().unwrap().modify_entity(title, |entity| entity.common.layer = "IMPIANTO LUCE".into()).unwrap();

    // With no explicit scope, the current layer is used even when its name is not numeric.
    let result = s.execute("symbollegend", &json!({"at":[0,-20]})).unwrap();
    assert_eq!(result["symbols"], 1);
    let table_handle = Handle::parse_hex(result["handle"].as_str().unwrap()).unwrap();
    let EntityKind::Table(table) = &s.doc().unwrap().entity(table_handle).unwrap().kind else { panic!("not a table") };
    assert_eq!(table.cells[0][0].text, "LEGENDA SEGNI GRAFICI");
    assert_eq!(table.cells[1][0].text, "SEGNO");
    assert!(table.cells[2][1].text_height.is_some_and(|height| height < table.text_height));
    assert_eq!(table.row_heights[0], table.text_height * 3.2, "the title is a distinct compact band");
    assert_eq!(table.row_heights[1], table.text_height * 1.8, "the column heading is compact");
    assert_eq!(table.row_heights[2], table.text_height * 2.0, "symbol rows follow the compact reference layout");
    assert_eq!(table.cells[2][2].style, "Header", "quantities are centred in their narrow column");
    assert!(table.col_widths[1] > table.col_widths[0] && table.col_widths[1] > table.col_widths[2]);
    assert!(
        table
            .cells
            .iter()
            .any(|row| row.get(1).is_some_and(|cell| cell.text == "Lampada a soffitto") && row.get(2).is_some_and(|cell| cell.text == "2"))
    );
    assert_eq!(s.doc().unwrap().groups.last().map(|group| group.members.len()), Some(2), "table plus one fitted symbol graphic");

    // A named layer may be chosen explicitly.
    let second = s.execute("symbollegend", &json!({"at":[100,-20], "layer":"impianto prese"})).unwrap();
    let second_handle = Handle::parse_hex(second["handle"].as_str().unwrap()).unwrap();
    let EntityKind::Table(second_table) = &s.doc().unwrap().entity(second_handle).unwrap().kind else { panic!("not a table") };
    assert!(
        second_table
            .cells
            .iter()
            .any(|row| row.get(1).is_some_and(|cell| cell.text == "Presa elettrica") && row.get(2).is_some_and(|cell| cell.text == "1"))
    );

    // All layers includes both systems and still excludes graphics created by older legends.
    let all = s.execute("symbollegend", &json!({"at":[150,-20], "allLayers":true})).unwrap();
    let all_handle = Handle::parse_hex(all["handle"].as_str().unwrap()).unwrap();
    let EntityKind::Table(all_table) = &s.doc().unwrap().entity(all_handle).unwrap().kind else { panic!("not a table") };
    assert!(
        all_table
            .cells
            .iter()
            .any(|row| row.get(1).is_some_and(|cell| cell.text == "Lampada a soffitto") && row.get(2).is_some_and(|cell| cell.text == "2"))
    );
    assert!(
        all_table
            .cells
            .iter()
            .any(|row| row.get(1).is_some_and(|cell| cell.text == "Presa elettrica") && row.get(2).is_some_and(|cell| cell.text == "1"))
    );
    assert!(all_table.cells.iter().flatten().all(|cell| cell.text != "Cart_pl"), "title blocks do not belong in a symbol legend");
    assert!(
        all_table.cells.iter().flatten().all(|cell| cell.text != "Parete edilizia"),
        "architectural blocks do not belong in an electrical legend"
    );
    let base_height = all_table.text_height;
    let base_description_height = all_table.cells[2][1].text_height.unwrap();
    let unfiltered = s.execute("symbollegend", &json!({"at":[160,-20], "allLayers":true, "electricalOnly":false})).unwrap();
    let unfiltered_handle = Handle::parse_hex(unfiltered["handle"].as_str().unwrap()).unwrap();
    let EntityKind::Table(unfiltered_table) = &s.doc().unwrap().entity(unfiltered_handle).unwrap().kind else { panic!("not a table") };
    assert!(
        unfiltered_table.cells.iter().flatten().any(|cell| cell.text == "Parete edilizia"),
        "scripts can explicitly request a general-purpose unfiltered legend"
    );
    let scaled = s.execute("symbollegend", &json!({"at":[175,-20], "allLayers":true, "scale":2.0})).unwrap();
    let scaled_handle = Handle::parse_hex(scaled["handle"].as_str().unwrap()).unwrap();
    let EntityKind::Table(scaled_table) = &s.doc().unwrap().entity(scaled_handle).unwrap().kind else { panic!("not a table") };
    assert!((scaled_table.text_height - base_height * 2.0).abs() < 1e-9, "the whole legend follows its scale");
    assert!(
        (scaled_table.cells[2][1].text_height.unwrap() - base_description_height * 2.0).abs() < 1e-9,
        "description text follows the legend scale too"
    );

    // A window/pick selection is represented by handles and can omit the quantity column.
    let selected = s.execute("symbollegend", &json!({"at":[200,-20], "handles":[socket.hex()], "quantity":false})).unwrap();
    let selected_handle = Handle::parse_hex(selected["handle"].as_str().unwrap()).unwrap();
    let EntityKind::Table(selected_table) = &s.doc().unwrap().entity(selected_handle).unwrap().kind else { panic!("not a table") };
    assert_eq!(selected_table.col_widths.len(), 2);
    assert_eq!(selected_table.cells.len(), 3, "title, header and one selected symbol");
    assert_eq!(selected_table.cells[2][1].text, "Presa elettrica");

    // The toolbar/command-line path reaches and accepts the final insertion point too.
    s.cmdline("legenda").unwrap();
    s.input(Input::Text("1".into())).unwrap();
    s.input(Input::Keyword("No".into())).unwrap();
    s.input(Input::Keyword("Layer".into())).unwrap();
    assert!(s.current_prompt().is_some_and(|prompt| prompt.accept.text));
    s.input(Input::Text("IMPIANTO PRESE".into())).unwrap();
    assert!(s.current_prompt().is_some_and(|prompt| prompt.accept.point));
    let preview = s.preview(cadcraft_engine::doc::geom::Vec2::new(300.0, -20.0));
    assert!(preview.iter().any(|entity| matches!(entity.kind, EntityKind::Table(_))), "the table follows the cursor");
    s.input(Input::Point(cadcraft_engine::doc::geom::Vec2::new(300.0, -20.0))).unwrap();
    assert!(s.running.is_none(), "the point completes the interactive command");

    // Right-click/Enter also confirms the point under the cursor.
    s.cmdline("legenda").unwrap();
    s.input(Input::Text("1".into())).unwrap();
    s.input(Input::Keyword("No".into())).unwrap();
    s.input(Input::Keyword("All".into())).unwrap();
    s.cursor = cadcraft_engine::doc::geom::Vec2::new(400.0, -20.0);
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none(), "Enter confirms the cursor instead of leaving the command stuck");
}

#[test]
fn symbol_legend_keeps_notec_visible_and_separates_different_values() {
    let mut s = Session::new();
    s.doc_mut().unwrap().layers.push(Layer { name: "Fm".into(), ..Layer::default() });
    s.doc_mut().unwrap().header.set_str("CLAYER", "Fm");
    s.execute(
        "attdef",
        &json!({
            "tag": "NOTEC",
            "default": "",
            "at": [0.0, 1.5],
            "height": 0.5,
            "invisible": true
        }),
    )
    .unwrap();
    s.execute("circle", &json!({"center": [0.0, 0.0], "radius": 1.0})).unwrap();
    let handles: Vec<_> = s.doc().unwrap().model.iter().map(|entity| entity.handle.hex()).collect();
    s.execute("block", &json!({"name": "BREAKER", "base": [0.0, 0.0], "handles": handles, "keep": "delete"})).unwrap();
    s.execute("blocks.description", &json!({"name": "BREAKER", "description": "Interruttore automatico"})).unwrap();
    s.execute("insert", &json!({"name": "BREAKER", "at": [10.0, 5.0], "scale": 2.0, "rotation": 30.0, "attribs": {"NOTEC": "10 A"}})).unwrap();
    s.execute("insert", &json!({"name": "BREAKER", "at": [20.0, 5.0], "scale": 0.5, "rotation": 90.0, "attribs": {"NOTEC": "16 A"}})).unwrap();

    let result = s.execute("symbollegend", &json!({"at": [0.0, -10.0], "allLayers": true})).unwrap();
    assert_eq!(result["symbols"], 2, "different NOTEC ratings need separate legend rows");
    let values: Vec<_> = s
        .doc()
        .unwrap()
        .groups
        .last()
        .unwrap()
        .members
        .iter()
        .skip(1)
        .filter_map(|handle| s.doc().unwrap().entity(*handle))
        .filter_map(|entity| match &entity.kind {
            EntityKind::Insert(insert) => insert.attribs.iter().find(|attribute| attribute.tag.eq_ignore_ascii_case("NOTEC")),
            _ => None,
        })
        .map(|attribute| {
            assert!(!attribute.invisible, "NOTEC must be visible in the fitted legend symbol");
            attribute.text.value.clone()
        })
        .collect();
    assert_eq!(values, vec!["10 A", "16 A"]);
}

/// Self-referencing blocks and a ten-way fan-out twenty levels deep finish at once and never
/// overflow.
#[test]
fn hostile_nesting_is_bounded() {
    let mut s = Session::new();
    define(&mut s, "LOOP", vec![insert("LOOP", 0.0, 1, 1)]);
    for level in 0..20 {
        define(&mut s, &format!("L{level}"), (0..10).map(|i| insert(&format!("L{}", level + 1), f64::from(i), 1000, 1000)).collect());
    }
    put(&mut s, &Space::Model, insert("LOOP", 0.0, 1, 1));
    put(&mut s, &Space::Model, insert("L0", 0.0, u32::MAX, u32::MAX));
    let t = Instant::now();
    let r: Value = s.execute("count", &json!({"space": "all"})).unwrap();
    assert!(t.elapsed().as_secs() < 5);
    assert_eq!(r["blocks"]["L15"], json!(u64::MAX), "counts saturate: {r}");
    assert!(r["blocks"]["LOOP"].as_u64().unwrap() >= 1, "{r}");
    assert!(r["blocks"].get("L16").is_none(), "nesting stops at MAX_BLOCK_DEPTH: {r}");
}

/// #1440: COUNT says which objects it scanned, which it counted, and why the rest were skipped.
#[test]
fn self_check_says_what_count_covered() {
    let mut s = plates();
    put(&mut s, &Space::Model, circle(300.0, 1.0));
    let r = s.execute("count", &json!({})).unwrap();
    let sc = &r["selfCheck"];
    assert_eq!(sc["schema"], 1);
    assert_eq!(sc["found"], 4, "three block references and one circle: {r}");
    assert_eq!(sc["analyzed"], 3);
    assert_eq!(sc["skipped"]["Circle"], json!({ "count": 1, "reason": "not a block reference" }));
    assert_eq!(sc["caps"], json!({}));
    assert_eq!(sc["implausible"], json!([]));

    // Counting the circles compares only circles, and says so.
    let h = put(&mut s, &Space::Model, circle(310.0, 1.0));
    let r = s.execute("count", &json!({"object": h.hex()})).unwrap();
    let sc = &r["selfCheck"];
    assert_eq!((sc["found"].clone(), sc["analyzed"].clone()), (json!(5), json!(2)), "{r}");
    assert_eq!(sc["skipped"]["Block Reference"]["reason"], "a different type");

    // When nested expansion hits the work cap, the cap and its value are reported.
    let mut s = Session::new();
    define(&mut s, "LOOP", vec![insert("LOOP", 0.0, 1, 1)]);
    put(&mut s, &Space::Model, insert("LOOP", 0.0, 1, 1));
    let r: Value = s.execute("count", &json!({"space": "all"})).unwrap();
    assert_eq!(r["selfCheck"]["caps"], json!({ "blockDepth": 16 }), "the infinite LOOP hits the depth cap: {r}");
}
