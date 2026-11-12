//! Electrical plan drawing setup: paper, drawing scale, units and symbol scale.

use cadcraft_color::Color;
use std::sync::Arc;

use cadcraft_doc::{Attrib, Block, Common, Drawing, Entity, EntityKind, HVal, Layer, Line, LwPolyline, Space, Text};
use cadcraft_geom::{PolyVertex, Vec2, Vec3};
use serde_json::{Value, json};

use super::{CommandSpec, Result, Session, bad, bool_or, f64_req, str_param};

const CMD: &str = "electrical.sheet.setup";
const CREATE_CMD: &str = "electrical.sheet.create";
const VALIDATE_CMD: &str = "electrical.titleblock.validate";
const ISO_SHEET_LAYER: &str = "SQUADRATURA_ISO";
const NATIVE_TITLE_BLOCK: &str = "CADCRAFT_ISO7200";
const DEFAULT_DESIGNER: &str = "";
const DEFAULT_DOCUMENT_ID: &str = "DOC-001";
const DEFAULT_DOCUMENT_TYPE: &str = "Esecutivo";
const DEFAULT_DOCUMENT_TITLE: &str = "Schema d'installazione impianto elettrico";

const ELECTRICAL_LAYERS: &[(&str, u8, bool)] = &[
    ("Defpoints", 7, false),
    ("Fm", 5, false),
    ("Luce", 4, false),
    ("Tv_Cit_Tel", 4, false),
    ("Antintrusione", 1, false),
    ("Terra", 3, false),
    ("Condotti", 5, false),
    ("Quadri", 40, false),
    ("Luce_Notturna", 41, false),
    ("Luce_Sicurezza", 1, false),
    ("Blindoluce", 4, false),
    ("CondottiSbarra", 40, false),
    ("Canali", 5, false),
    ("Rete dati", 4, false),
    ("Termico", 6, false),
    ("VVF", 1, false),
    ("Interrato FM", 1, false),
    ("Interrato Dati", 4, false),
    ("Fotovoltaico", 3, false),
    ("BORDO", 7, false),
    ("DRAW", 3, false),
    ("-D_ITA", 7, false),
    ("-INFOMLT", 7, true),
    (ISO_SHEET_LAYER, 7, false),
];

#[derive(Clone, Copy)]
struct Paper {
    name: &'static str,
    width_mm: f64,
    height_mm: f64,
}

const PAPERS: &[Paper] = &[
    Paper { name: "A0", width_mm: 841.0, height_mm: 1189.0 },
    Paper { name: "A1", width_mm: 594.0, height_mm: 841.0 },
    Paper { name: "A2", width_mm: 420.0, height_mm: 594.0 },
    Paper { name: "A3", width_mm: 297.0, height_mm: 420.0 },
    Paper { name: "A4", width_mm: 210.0, height_mm: 297.0 },
];

fn paper(name: &str) -> Option<Paper> {
    PAPERS.iter().copied().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
}

/// `(DXF INSUNITS, millimetres represented by one drawing unit, label)`.
fn unit(name: &str) -> Option<(i64, f64, &'static str)> {
    match name.trim().to_ascii_lowercase().as_str() {
        "m" | "meter" | "meters" | "metri" => Some((6, 1000.0, "m")),
        "cm" | "centimeter" | "centimeters" | "centimetri" => Some((5, 10.0, "cm")),
        "mm" | "millimeter" | "millimeters" | "millimetri" => Some((4, 1.0, "mm")),
        _ => None,
    }
}

fn output(paper: Paper, denominator: f64, unit_mm: f64, units: &str, landscape: bool, symbol_scale: f64) -> Value {
    let (paper_width, paper_height) = if landscape {
        (paper.width_mm.max(paper.height_mm), paper.width_mm.min(paper.height_mm))
    } else {
        (paper.width_mm.min(paper.height_mm), paper.width_mm.max(paper.height_mm))
    };
    let width = paper_width * denominator / unit_mm;
    let height = paper_height * denominator / unit_mm;
    json!({
        "paper": paper.name,
        "landscape": landscape,
        "scaleDenominator": denominator,
        "units": units,
        "symbolScale": symbol_scale,
        "paperWidthMm": paper_width,
        "paperHeightMm": paper_height,
        "drawingWidth": width,
        "drawingHeight": height,
        "plotMillimetresPerDrawingUnit": unit_mm / denominator,
    })
}

fn setup(s: &mut Session, p: &Value) -> Result<Value> {
    let paper_name = str_param(p, "paper").ok_or_else(|| bad(CMD, "`paper` deve essere A0, A1, A2, A3 o A4"))?;
    let paper = paper(paper_name).ok_or_else(|| bad(CMD, "`paper` deve essere A0, A1, A2, A3 o A4"))?;
    let denominator = f64_req(CMD, p, "scaleDenominator")?;
    if !(1.0..=1_000_000.0).contains(&denominator) {
        return Err(bad(CMD, "`scaleDenominator` deve essere compreso fra 1 e 1000000"));
    }
    let unit_name = str_param(p, "units").ok_or_else(|| bad(CMD, "`units` deve essere m, cm o mm"))?;
    let (insunits, unit_mm, units) = unit(unit_name).ok_or_else(|| bad(CMD, "`units` deve essere m, cm o mm"))?;
    let standard_profile = bool_or(p, "standardProfile", true);
    // ISO 5457 uses A4 in the upright arrangement.  Compatibility mode keeps the
    // historical Eplus freedom to create an A4 landscape sheet.
    let landscape = if standard_profile && paper.name == "A4" { false } else { bool_or(p, "landscape", true) };
    let automatic_symbol_scale = denominator / unit_mm;
    let symbol_scale = p.get("symbolScale").and_then(Value::as_f64).unwrap_or(automatic_symbol_scale);
    if !symbol_scale.is_finite() || !(1e-9..=1_000_000.0).contains(&symbol_scale) {
        return Err(bad(CMD, "`symbolScale` deve essere un numero positivo valido"));
    }
    let result = output(paper, denominator, unit_mm, units, landscape, symbol_scale);
    let width = result.get("drawingWidth").and_then(Value::as_f64).ok_or_else(|| bad(CMD, "dimensione foglio non valida"))?;
    let height = result.get("drawingHeight").and_then(Value::as_f64).ok_or_else(|| bad(CMD, "dimensione foglio non valida"))?;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(bad(CMD, "dimensione reale del foglio non valida"));
    }

    let d = s.doc_mut()?;
    // Standard DXF/DWG values. Eplus drawings use DIMSCALE as the insertion factor for their
    // electrical symbols, so the result needs no private CADCraft metadata.
    d.header.set_i64("MEASUREMENT", 1);
    d.header.set_i64("INSUNITS", insunits);
    d.header.set_f64("DIMSCALE", symbol_scale);
    // The two Eplus reference drawings use 300 times the symbol factor for line types:
    // 1:100 in metres -> symbols 0.1, LTSCALE 30; 1:1 in millimetres -> 1 and 300.
    let ltscale = (symbol_scale * 300.0).clamp(1e-9, 1_000_000_000.0);
    d.header.set_f64("LTSCALE", ltscale);
    d.header.set("LIMMIN", HVal::Point(Vec3::ZERO));
    d.header.set("LIMMAX", HVal::Point(Vec3::new(width, height, 0.0)));
    Ok(result)
}

fn frame(width: f64, height: f64, margin: f64) -> EntityKind {
    frame_with_margins(width, height, margin, margin, margin, margin)
}

fn frame_with_margins(width: f64, height: f64, left: f64, right: f64, bottom: f64, top: f64) -> EntityKind {
    let vertices = [Vec2::new(left, bottom), Vec2::new(width - right, bottom), Vec2::new(width - right, height - top), Vec2::new(left, height - top)]
        .into_iter()
        .map(PolyVertex::new)
        .collect();
    EntityKind::LwPolyline(LwPolyline { vertices, closed: true, const_width: 0.0, elevation: 0.0, plinegen: false })
}

fn line(a: Vec2, b: Vec2) -> EntityKind {
    EntityKind::Line(Line { a: Vec3::new(a.x, a.y, 0.0), b: Vec3::new(b.x, b.y, 0.0) })
}

/// ISO 5457 sheet marks expressed in model units.  The drawing frame has a 20 mm
/// filing margin on the left and 10 mm on the other sides.  Trimming and centring
/// marks stay inside the nominal paper rectangle, so they never enlarge its limits.
fn iso_marks(width: f64, height: f64, mm: f64) -> Vec<EntityKind> {
    let short = 5.0 * mm;
    let mid_x = width / 2.0;
    let mid_y = height / 2.0;
    let mut marks = vec![
        line(Vec2::new(0.0, 0.0), Vec2::new(short, 0.0)),
        line(Vec2::new(0.0, 0.0), Vec2::new(0.0, short)),
        line(Vec2::new(width, 0.0), Vec2::new(width - short, 0.0)),
        line(Vec2::new(width, 0.0), Vec2::new(width, short)),
        line(Vec2::new(0.0, height), Vec2::new(short, height)),
        line(Vec2::new(0.0, height), Vec2::new(0.0, height - short)),
        line(Vec2::new(width, height), Vec2::new(width - short, height)),
        line(Vec2::new(width, height), Vec2::new(width, height - short)),
    ];
    marks.extend([
        line(Vec2::new(mid_x, 0.0), Vec2::new(mid_x, short)),
        line(Vec2::new(mid_x, height), Vec2::new(mid_x, height - short)),
        line(Vec2::new(0.0, mid_y), Vec2::new(short, mid_y)),
        line(Vec2::new(width, mid_y), Vec2::new(width - short, mid_y)),
    ]);
    marks
}

/// Folding references for large landscape sheets, measured from the lower-right
/// presentation corner in A4 modules (210 mm wide, 297 mm high).  They are short
/// edge marks only: no construction line crosses the drawing area.
fn iso_fold_marks(width: f64, height: f64, mm: f64) -> Vec<EntityKind> {
    let short = 5.0 * mm;
    let module_x = 210.0 * mm;
    let module_y = 297.0 * mm;
    let left_limit = 20.0 * mm;
    let right_limit = 10.0 * mm;
    let top_limit = 10.0 * mm;
    let mut marks = Vec::new();

    let mut x = width - module_x;
    while x > left_limit + short && marks.len() < 64 {
        marks.push(line(Vec2::new(x, 0.0), Vec2::new(x, short)));
        marks.push(line(Vec2::new(x, height), Vec2::new(x, height - short)));
        x -= module_x;
    }

    let mut y = module_y;
    while y < height - top_limit - short && marks.len() < 128 {
        marks.push(line(Vec2::new(0.0, y), Vec2::new(short, y)));
        marks.push(line(Vec2::new(width, y), Vec2::new(width - right_limit.min(short), y)));
        y += module_y;
    }
    marks
}

fn block_entity(drawing: &mut Drawing, block: &mut Block, kind: EntityKind) {
    block.entities.push(Entity::new(drawing.new_handle(), kind));
}

fn block_line(drawing: &mut Drawing, block: &mut Block, a: Vec2, b: Vec2) {
    block_entity(drawing, block, line(a, b));
}

fn block_text(drawing: &mut Drawing, block: &mut Block, at: Vec2, height: f64, value: &str) {
    block_entity(
        drawing,
        block,
        EntityKind::Text(Text {
            insert: Vec3::new(at.x, at.y, 0.0),
            align_pt: None,
            height,
            value: value.into(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign: Default::default(),
            valign: Default::default(),
        }),
    );
}

fn block_attribute(drawing: &mut Drawing, block: &mut Block, tag: &str, prompt: &str, at: Vec2, height: f64) {
    let text = Text {
        insert: Vec3::new(at.x, at.y, 0.0),
        align_pt: None,
        height,
        value: String::new(),
        rotation: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".into(),
        halign: Default::default(),
        valign: Default::default(),
    };
    block_entity(
        drawing,
        block,
        EntityKind::AttDef(Attrib {
            tag: tag.into(),
            text,
            invisible: false,
            constant: false,
            prompt: prompt.into(),
            props: Default::default(),
            multiline: None,
            modes: Default::default(),
        }),
    );
}

fn title_cell(drawing: &mut Drawing, block: &mut Block, x: f64, y: f64, label: &str, tag: &str) {
    block_text(drawing, block, Vec2::new(x + 2.0, y + 7.0), 1.8, label);
    block_attribute(drawing, block, tag, label, Vec2::new(x + 2.0, y + 2.0), 2.5);
}

/// Original CADCraft title block: 180 x 55 mm, base point at the lower-right corner.
/// Its field tags follow the ISO 7200 document-management concepts and remain ordinary
/// DXF attributes, so other CAD applications can edit them without CADCraft metadata.
fn ensure_native_title_block(drawing: &mut Drawing) {
    if drawing.block(NATIVE_TITLE_BLOCK).is_some() {
        return;
    }
    let mut block = Block::new(NATIVE_TITLE_BLOCK);
    block.description = "Cartiglio CADCraft con campi ISO 7200".into();
    block.base = Vec3::ZERO;

    for (a, b) in [
        (Vec2::new(-180.0, 0.0), Vec2::new(0.0, 0.0)),
        (Vec2::new(0.0, 0.0), Vec2::new(0.0, 55.0)),
        (Vec2::new(0.0, 55.0), Vec2::new(-180.0, 55.0)),
        (Vec2::new(-180.0, 55.0), Vec2::new(-180.0, 0.0)),
    ] {
        block_line(drawing, &mut block, a, b);
    }
    for y in [10.0, 20.0, 30.0, 40.0] {
        block_line(drawing, &mut block, Vec2::new(-180.0, y), Vec2::new(0.0, y));
    }
    for x in [-120.0, -60.0] {
        block_line(drawing, &mut block, Vec2::new(x, 0.0), Vec2::new(x, 40.0));
    }
    block_line(drawing, &mut block, Vec2::new(-60.0, 40.0), Vec2::new(-60.0, 55.0));

    block_text(drawing, &mut block, Vec2::new(-178.0, 52.0), 1.8, "TITOLO DEL DOCUMENTO");
    // The 120 mm title cell must also accommodate Cart_pl's longer electrical description.
    block_attribute(drawing, &mut block, "TITOLO", "Titolo del documento", Vec2::new(-178.0, 43.0), 3.2);
    title_cell(drawing, &mut block, -60.0, 40.0, "PROPRIETARIO", "PROPRIETARIO");
    title_cell(drawing, &mut block, -180.0, 30.0, "IDENTIFICATIVO DOCUMENTO", "ID_DOCUMENTO");
    title_cell(drawing, &mut block, -120.0, 30.0, "TIPO DOCUMENTO", "TIPO_DOCUMENTO");
    title_cell(drawing, &mut block, -60.0, 30.0, "REVISIONE", "REVISIONE");
    title_cell(drawing, &mut block, -180.0, 20.0, "CREATO DA", "CREATO_DA");
    title_cell(drawing, &mut block, -120.0, 20.0, "APPROVATO DA", "APPROVATO");
    title_cell(drawing, &mut block, -60.0, 20.0, "DATA EMISSIONE", "DATA_EMISSIONE");
    title_cell(drawing, &mut block, -180.0, 10.0, "SCALA", "SCALA");
    title_cell(drawing, &mut block, -120.0, 10.0, "FOGLIO", "FOGLIO");
    title_cell(drawing, &mut block, -60.0, 10.0, "TOTALE FOGLI", "FOGLI_TOTALI");
    title_cell(drawing, &mut block, -180.0, 0.0, "PROGETTO / IMPIANTO", "PROGETTO");
    title_cell(drawing, &mut block, -120.0, 0.0, "COMMITTENTE", "COMMITTENTE");
    title_cell(drawing, &mut block, -60.0, 0.0, "FORMATO", "FORMATO");

    drawing.blocks.insert(NATIVE_TITLE_BLOCK.into(), Arc::new(block));
}

const ISO_TITLE_FIELDS: &[(&str, &[&str])] = &[
    ("Proprietario", &["PROPRIETARIO", "LEGAL_OWNER", "COMMITTENTE", "CLIENTE"]),
    ("Identificativo documento", &["ID_DOCUMENTO", "DOCUMENT_ID", "FILE-MATR.", "TAVOLA"]),
    ("Revisione", &["REVISIONE", "REVISION", "REV", "REV1"]),
    ("Data di emissione", &["DATA_EMISSIONE", "ISSUE_DATE", "DATA"]),
    ("Numero foglio", &["FOGLIO", "SHEET", "TAVOLA"]),
    ("Titolo", &["TITOLO", "TITLE", "PROGETTO1", "DENOMINAZIONE1"]),
    ("Approvazione", &["APPROVATO", "APPROVED_BY", "VISTO", "TECNICO"]),
    ("Creatore", &["CREATO_DA", "CREATED_BY", "DISEGNATO", "DISEG", "TECNICO"]),
    ("Tipo documento", &["TIPO_DOCUMENTO", "DOCUMENT_TYPE", "ESECUTIVO"]),
];

fn validate_title_block(drawing: &Drawing, name: &str) -> Value {
    let Some(block) = drawing.block(name) else {
        return json!({"valid":false,"block":name,"missingFields":["Definizione del cartiglio"]});
    };
    let tags = block
        .entities
        .iter()
        .filter_map(|entity| match &entity.kind {
            EntityKind::AttDef(attribute) => Some(attribute.tag.to_ascii_uppercase()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let missing = ISO_TITLE_FIELDS
        .iter()
        .filter(|(_, aliases)| !aliases.iter().any(|alias| tags.iter().any(|tag| tag.eq_ignore_ascii_case(alias))))
        .map(|(label, _)| *label)
        .collect::<Vec<_>>();
    json!({"valid":missing.is_empty(),"block":name,"missingFields":missing,"attributeCount":tags.len()})
}

fn validate(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param(p, "titleBlock").unwrap_or(NATIVE_TITLE_BLOCK);
    Ok(validate_title_block(s.doc()?, name))
}

fn create(s: &mut Session, p: &Value) -> Result<Value> {
    let create_frame = bool_or(p, "createFrame", true);
    let insert_title = bool_or(p, "insertTitleBlock", true);
    let replace = bool_or(p, "replaceExisting", false);
    let standard_profile = bool_or(p, "standardProfile", true);
    let sheet_marks = bool_or(p, "sheetMarks", standard_profile);
    let native_title = bool_or(p, "nativeTitleBlock", standard_profile);
    let title = if native_title { NATIVE_TITLE_BLOCK.to_string() } else { str_param(p, "titleBlock").unwrap_or("Cart_pl").to_string() };

    if native_title {
        ensure_native_title_block(s.doc_mut()?);
    }

    if s.doc()?.block(&title).is_none()
        && let Some(path) = str_param(p, "titleBlockFile").filter(|path| !path.trim().is_empty())
    {
        super::blocks::insert_file::import_named(s, path, &title)?;
    }
    if insert_title && s.doc()?.block(&title).is_none() {
        return Err(bad(CREATE_CMD, format!("blocco cartiglio `{title}` non trovato; scegli un disegno sorgente che lo contenga")));
    }
    let has_sheet = s.doc()?.model.iter().any(|entity| {
        entity.common.layer.eq_ignore_ascii_case(ISO_SHEET_LAYER)
            || (entity.common.layer.eq_ignore_ascii_case("BORDO")
                && matches!(&entity.kind, EntityKind::LwPolyline(poly) if poly.closed && poly.vertices.len() == 4))
            || matches!(&entity.kind, EntityKind::Insert(insert) if insert.block.eq_ignore_ascii_case(&title) || insert.block.eq_ignore_ascii_case("Cart_pl") || insert.block.eq_ignore_ascii_case(NATIVE_TITLE_BLOCK))
    });
    if has_sheet && !replace {
        return Err(bad(CREATE_CMD, "nel disegno esistono gia una squadratura o il cartiglio; usa `replaceExisting: true` per sostituirli"));
    }

    let result = setup(s, p)?;
    let width = result.get("drawingWidth").and_then(Value::as_f64).ok_or_else(|| bad(CREATE_CMD, "larghezza foglio non valida"))?;
    let height = result.get("drawingHeight").and_then(Value::as_f64).ok_or_else(|| bad(CREATE_CMD, "altezza foglio non valida"))?;
    let symbol_scale = result.get("symbolScale").and_then(Value::as_f64).ok_or_else(|| bad(CREATE_CMD, "scala simboli non valida"))?;
    let paper_name = result.get("paper").and_then(Value::as_str).unwrap_or("A3");
    let fold_marks = bool_or(p, "foldMarks", standard_profile && matches!(paper_name, "A0" | "A1" | "A2"));

    // Preserve every value in definition order, including repeated tags such as the two
    // PROGETTO1 fields in Cart_pl. Replacing a frame must never erase title-block data.
    let title_values = if replace {
        s.doc()?.model.iter().find_map(|entity| match &entity.kind {
            EntityKind::Insert(insert) if insert.block.eq_ignore_ascii_case(&title) => {
                Some(insert.attribs.iter().map(|attribute| attribute.text.value.clone()).collect::<Vec<_>>())
            }
            _ => None,
        })
    } else {
        None
    };

    let removed = if replace {
        s.doc()?
            .model
            .iter()
            .filter(|entity| {
                entity.common.layer.eq_ignore_ascii_case(ISO_SHEET_LAYER)
                    || (entity.common.layer.eq_ignore_ascii_case("BORDO")
                        && matches!(&entity.kind, EntityKind::LwPolyline(poly) if poly.closed && poly.vertices.len() == 4))
                    || matches!(&entity.kind, EntityKind::Insert(insert) if insert.block.eq_ignore_ascii_case(&title) || insert.block.eq_ignore_ascii_case("Cart_pl") || insert.block.eq_ignore_ascii_case(NATIVE_TITLE_BLOCK))
            })
            .map(|entity| entity.handle)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let mut frame_handles = Vec::new();
    let mut mark_handles = Vec::new();
    let mut fold_mark_handles = Vec::new();
    {
        let drawing = s.doc_mut()?;
        for (name, color, frozen) in ELECTRICAL_LAYERS {
            if drawing.layer(name).is_none() {
                let mut layer = Layer::new(name);
                layer.color = Color::Index(*color);
                layer.frozen = *frozen;
                drawing.layers.push(layer);
            }
        }
        for handle in removed {
            drawing.remove_entity(handle);
        }
        if create_frame {
            if standard_profile {
                let common = Common { layer: ISO_SHEET_LAYER.into(), color: Color::ByLayer, ..Common::default() };
                frame_handles.push(drawing.add(
                    &Space::Model,
                    common.clone(),
                    frame_with_margins(width, height, 20.0 * symbol_scale, 10.0 * symbol_scale, 10.0 * symbol_scale, 10.0 * symbol_scale),
                )?);
                if sheet_marks {
                    for mark in iso_marks(width, height, symbol_scale) {
                        mark_handles.push(drawing.add(&Space::Model, common.clone(), mark)?);
                    }
                }
                if fold_marks {
                    for mark in iso_fold_marks(width, height, symbol_scale) {
                        fold_mark_handles.push(drawing.add(&Space::Model, common.clone(), mark)?);
                    }
                }
            } else {
                let outer = Common { layer: "BORDO".into(), color: Color::Index(2), ..Common::default() };
                let inner = Common { layer: "BORDO".into(), color: Color::Index(5), ..Common::default() };
                frame_handles.push(drawing.add(&Space::Model, outer, frame(width, height, 5.0 * symbol_scale))?);
                frame_handles.push(drawing.add(&Space::Model, inner, frame(width, height, 10.0 * symbol_scale))?);
            }
        }
    }

    let title_handle = if insert_title {
        let previous_layer = s.doc()?.header.str("CLAYER", "0");
        s.doc_mut()?.header.set_str("CLAYER", "BORDO");
        let at = Vec2::new(width - 10.0 * symbol_scale, 10.0 * symbol_scale);
        let scale = Vec2::new(symbol_scale, symbol_scale);
        let inserted = match title_values.as_deref() {
            Some(values) => super::blocks::insert_with_ordered(s, &title, at, scale, 0.0, None, values),
            None if native_title => {
                let mut values = serde_json::Map::new();
                // Neutral defaults keep the native title block reusable. Client, project,
                // designer, issue date and approval belong to the individual job and remain
                // blank until the user fills them in.
                values.insert("TITOLO".into(), json!(DEFAULT_DOCUMENT_TITLE));
                values.insert("PROPRIETARIO".into(), json!(DEFAULT_DESIGNER));
                values.insert("ID_DOCUMENTO".into(), json!(DEFAULT_DOCUMENT_ID));
                values.insert("TIPO_DOCUMENTO".into(), json!(DEFAULT_DOCUMENT_TYPE));
                values.insert("REVISIONE".into(), json!("0"));
                values.insert("CREATO_DA".into(), json!(DEFAULT_DESIGNER));
                values.insert("SCALA".into(), json!(format!("1:{}", result["scaleDenominator"].as_f64().unwrap_or(1.0))));
                values.insert("FORMATO".into(), json!(paper_name));
                values.insert("FOGLIO".into(), json!("1"));
                values.insert("FOGLI_TOTALI".into(), json!("1"));
                super::blocks::insert_with(s, &title, at, scale, 0.0, None, &values)
            }
            None => super::blocks::insert_with(s, &title, at, scale, 0.0, None, &serde_json::Map::new()),
        };
        s.doc_mut()?.header.set_str("CLAYER", &previous_layer);
        Some(inserted?)
    } else {
        None
    };

    let mut result = result;
    if let Some(object) = result.as_object_mut() {
        object.insert("frameHandles".into(), json!(frame_handles.iter().map(|handle| handle.hex()).collect::<Vec<_>>()));
        object.insert("markHandles".into(), json!(mark_handles.iter().map(|handle| handle.hex()).collect::<Vec<_>>()));
        object.insert("foldMarkHandles".into(), json!(fold_mark_handles.iter().map(|handle| handle.hex()).collect::<Vec<_>>()));
        object.insert("standardProfile".into(), json!(standard_profile));
        object.insert("nativeTitleBlock".into(), json!(native_title));
        object.insert("titleBlockValidation".into(), validate_title_block(s.doc()?, &title));
        object.insert("titleBlockHandle".into(), title_handle.map(|handle| Value::String(handle.hex())).unwrap_or(Value::Null));
    }
    Ok(result)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(CMD, "Imposta foglio planimetrico", setup)
            .params("{paper: A0|A1|A2|A3|A4, scaleDenominator: number, units: m|cm|mm, landscape?: bool, symbolScale?: number} → dimensions"),
        CommandSpec::new(CREATE_CMD, "Crea foglio planimetrico", create).params(
            "{paper, scaleDenominator, units, landscape?, symbolScale?, standardProfile?: bool, sheetMarks?: bool, foldMarks?: bool, nativeTitleBlock?: bool, createFrame?: bool, insertTitleBlock?: bool, titleBlock?: string, titleBlockFile?: DXF/DWG path, replaceExisting?: bool} → dimensions and handles",
        ),
        CommandSpec::new(VALIDATE_CMD, "Verifica campi ISO 7200 del cartiglio", validate)
            .params("{titleBlock?: string} → {valid, block, missingFields, attributeCount}"),
    ]
}
