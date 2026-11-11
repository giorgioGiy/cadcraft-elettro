//! Electrical-plan layer groups. The command changes only standard layer visibility; drawing
//! geometry and block data remain untouched and interoperable with other CAD applications.

use std::collections::{BTreeMap, BTreeSet};

use cadcraft_doc::{Drawing, EntityKind, Handle, Space};
use cadcraft_geom::{Mat3, Vec2};
use serde_json::{Value, json};

use super::{CommandSpec, Result, Session, bad, bool_or, f64_req, str_param};

const ANALYZE: &str = "electrical.layers.analyze";
const APPLY: &str = "electrical.layers.apply";
const SYMBOL_SCALE: &str = "electrical.symbolscale";
const MAX_LAYERS: usize = 20_000;
const MAX_SYMBOL_REFS: usize = 1_000_000;
const MAX_VISIBLE_GROUPS: usize = 32;
const MAX_REPORTED_NAMES: usize = 256;

#[derive(Clone, Copy)]
struct Group {
    id: &'static str,
    label: &'static str,
}

const GROUPS: &[Group] = &[
    Group { id: "common", label: "Squadratura, cartiglio e livelli comuni" },
    Group { id: "architecture", label: "Planimetria architettonica" },
    Group { id: "power", label: "Forza motrice e quadri" },
    Group { id: "lighting", label: "Illuminazione ordinaria" },
    Group { id: "emergency", label: "Illuminazione di emergenza" },
    Group { id: "fire", label: "Rivelazione e protezione incendio" },
    Group { id: "photovoltaic", label: "Fotovoltaico" },
    Group { id: "data", label: "Rete dati, TV e videocitofono" },
    Group { id: "intrusion", label: "Antintrusione e allarme" },
    Group { id: "earth", label: "Impianto di terra" },
    Group { id: "distribution", label: "Canali e condotti" },
];

fn normalized(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect()
}

fn token(name: &str, wanted: &str) -> bool {
    name.split('_').any(|part| part == wanted)
}

fn any(name: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| name.contains(needle))
}

pub(crate) fn group_id(name: &str) -> Option<&'static str> {
    let n = normalized(name);
    if matches!(n.as_str(), "0" | "DEFPOINTS" | "BORDO" | "DRAW" | "_D_ITA" | "_INFOMLT") || any(&n, &["SQUADRATURA", "CARTIGLIO"]) {
        return Some("common");
    }
    if any(&n, &["ARCH", "MUR", "PIANTA", "ARRED", "EDILE", "BASE_ARCH", "RIF_ARCH"])
        || name.split_once('|').is_some_and(|(xref, _)| {
            let xref = normalized(xref);
            any(&xref, &["ARCH", "EDILE", "BASE", "PIANTA"])
        })
    {
        return Some("architecture");
    }
    // More specific groups precede generic light, alarm and distribution words.
    if any(&n, &["EMERGEN", "SICUREZZA"]) {
        return Some("emergency");
    }
    if any(&n, &["FOTOVOL", "SOLARE"]) || token(&n, "FV") {
        return Some("photovoltaic");
    }
    if any(&n, &["RIVEL", "INCEND", "ANTINCEND"]) || token(&n, "VVF") {
        return Some("fire");
    }
    if any(&n, &["ANTINTRUS", "INTRUSIONE", "ALLARME"]) && !any(&n, &["INCEND", "EMERGEN"]) {
        return Some("intrusion");
    }
    if any(&n, &["RETE_DATI", "DATI", "TELEF", "CITOF", "TV_CIT_TEL", "LAN"]) || token(&n, "TV") {
        return Some("data");
    }
    if any(&n, &["TERRA", "EQUIPOT"]) || token(&n, "PE") {
        return Some("earth");
    }
    if any(&n, &["FORZA", "PRESE", "QUADRI", "CONDOTTISBARRA", "BLINDOSBARRA"]) || token(&n, "FM") {
        return Some("power");
    }
    if any(&n, &["LUCE", "ILLUMIN", "BLINDOLUCE"]) {
        return Some("lighting");
    }
    if any(&n, &["CONDOTTI", "CANALI", "PASSERELLE", "TUBAZION"]) {
        return Some("distribution");
    }
    None
}

pub(crate) fn is_electrical_layer(name: &str) -> bool {
    matches!(group_id(name), Some("power" | "lighting" | "emergency" | "fire" | "photovoltaic" | "data" | "intrusion" | "earth" | "distribution"))
}

pub(crate) fn is_title_block(drawing: &Drawing, name: &str) -> bool {
    if ["att_tav", "Cart_pl", "0UNIA3", "00A3SCHEMI", "CADCRAFT_ISO7200"].iter().any(|known| name.eq_ignore_ascii_case(known)) {
        return true;
    }
    let Some(block) = drawing.block(name) else { return false };
    let has = |wanted: &str| {
        block.entities.iter().any(|entity| matches!(&entity.kind, EntityKind::AttDef(attribute) if attribute.tag.eq_ignore_ascii_case(wanted)))
    };
    let identity_fields = ["TAVOLA", "FOGLIO", "PROGETTO", "COMMITTENTE", "CLIENTE", "DENOMINAZIONE"];
    has("SCALA") && identity_fields.iter().filter(|tag| has(tag)).take(2).count() >= 2
}

fn floor_id(name: &str) -> Option<&'static str> {
    let n = normalized(name);
    if any(&n, &["PIANO_TERRA", "PIANOTERRA"]) || token(&n, "PT") || token(&n, "P0") {
        Some("PT")
    } else if token(&n, "P1") || any(&n, &["PIANO_PRIMO", "PIANO1"]) {
        Some("P1")
    } else if token(&n, "P2") || any(&n, &["PIANO_SECONDO", "PIANO2"]) {
        Some("P2")
    } else if any(&n, &["COPERTURA", "TETTO"]) || token(&n, "COP") {
        Some("COP")
    } else {
        None
    }
}

fn analyze(s: &mut Session, _p: &Value) -> Result<Value> {
    let d = s.doc()?;
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut on_counts = BTreeMap::<&str, usize>::new();
    let mut names = BTreeMap::<&str, Vec<&str>>::new();
    let mut floors = BTreeSet::new();
    let mut unmatched = Vec::new();
    let mut truncated = false;
    for (index, layer) in d.layers.iter().enumerate() {
        if index >= MAX_LAYERS {
            truncated = true;
            break;
        }
        if let Some(floor) = floor_id(&layer.name) {
            floors.insert(floor);
        }
        if let Some(id) = group_id(&layer.name) {
            *counts.entry(id).or_default() += 1;
            if layer.on && !layer.frozen {
                *on_counts.entry(id).or_default() += 1;
            }
            let group_names = names.entry(id).or_default();
            if group_names.len() < MAX_REPORTED_NAMES {
                group_names.push(&layer.name);
            }
        } else if unmatched.len() < MAX_REPORTED_NAMES {
            unmatched.push(layer.name.as_str());
        }
    }
    let groups = GROUPS
        .iter()
        .map(|group| {
            let count = counts.get(group.id).copied().unwrap_or(0);
            let visible = on_counts.get(group.id).copied().unwrap_or(0);
            json!({
                "id": group.id,
                "label": group.label,
                "count": count,
                "visible": visible,
                "allVisible": count > 0 && count == visible,
                "anyVisible": visible > 0,
                "layers": names.get(group.id).cloned().unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "groups": groups,
        "floors": floors,
        "unmatched": unmatched,
        "unmatchedCount": d.layers.iter().take(MAX_LAYERS).filter(|layer| group_id(&layer.name).is_none()).count(),
        "truncated": truncated,
    }))
}

fn visible_groups(p: &Value) -> Result<BTreeSet<&str>> {
    let Some(items) = p.get("visible").and_then(Value::as_array) else {
        return Err(bad(APPLY, "`visible` deve essere un elenco di gruppi"));
    };
    if items.len() > MAX_VISIBLE_GROUPS {
        return Err(bad(APPLY, "troppi gruppi richiesti"));
    }
    let known = GROUPS.iter().map(|group| group.id).collect::<BTreeSet<_>>();
    let mut visible = BTreeSet::new();
    for item in items {
        let id = item.as_str().ok_or_else(|| bad(APPLY, "ogni gruppo deve essere testo"))?;
        if !known.contains(id) {
            return Err(bad(APPLY, format!("gruppo impianto sconosciuto `{id}`")));
        }
        visible.insert(id);
    }
    Ok(visible)
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let visible = visible_groups(p)?;
    let selected_floor = str_param(p, "floor").map(normalized).filter(|floor| !floor.is_empty() && floor != "ALL");
    let keep_unmatched = bool_or(p, "keepUnmatched", true);
    let d = s.doc_mut()?;
    let current = d.header.str("CLAYER", "0");
    let current_will_hide = d
        .layer(&current)
        .is_some_and(|layer| group_id(&layer.name).is_some_and(|id| !visible.contains(id)) || (!keep_unmatched && group_id(&layer.name).is_none()));
    if current_will_hide {
        d.header.set_str("CLAYER", "0");
    }
    let mut changed = Vec::new();
    let mut changed_count = 0_usize;
    let mut controlled = 0_usize;
    for (index, layer) in d.layers.iter_mut().enumerate() {
        if index >= MAX_LAYERS {
            break;
        }
        let group = group_id(&layer.name);
        let floor_matches = selected_floor.as_deref().is_none_or(|selected| floor_id(&layer.name).is_none_or(|floor| floor == selected));
        let desired = if layer.name == "0" {
            true
        } else {
            match group {
                Some("common") => visible.contains("common"),
                Some(id) => floor_matches && visible.contains(id),
                None if keep_unmatched => layer.on,
                None => false,
            }
        };
        if group.is_some() {
            controlled += 1;
        }
        if layer.on != desired {
            layer.on = desired;
            changed_count += 1;
            if changed.len() < MAX_REPORTED_NAMES {
                changed.push(layer.name.clone());
            }
        }
    }
    Ok(json!({
        "changed": changed_count,
        "changedLayers": changed,
        "controlled": controlled,
        "currentLayer": d.header.str("CLAYER", "0"),
        "floor": selected_floor,
        "visible": visible,
    }))
}

fn symbol_scale(s: &mut Session, p: &Value) -> Result<Value> {
    let scale = f64_req(SYMBOL_SCALE, p, "scale")?;
    if !scale.is_finite() || !(1e-9..=1_000_000.0).contains(&scale) {
        return Err(bad(SYMBOL_SCALE, "`scale` deve essere un numero positivo compreso fra 0.000000001 e 1000000"));
    }
    let update_existing = bool_or(p, "updateExisting", false);
    let previous = s.doc()?.header.f64("DIMSCALE", 1.0);
    let previous_valid = previous.is_finite() && previous > 0.0;
    if update_existing && !previous_valid {
        return Err(bad(SYMBOL_SCALE, "la scala simboli precedente nel disegno non è valida"));
    }
    let ratio = if previous_valid { scale / previous } else { 1.0 };
    if update_existing && (!ratio.is_finite() || !(1e-9..=1_000_000_000.0).contains(&ratio)) {
        return Err(bad(SYMBOL_SCALE, "il rapporto fra la nuova scala e quella precedente è troppo grande"));
    }

    let mut targets = Vec::<(Handle, Vec2)>::new();
    let mut skipped_locked = 0_usize;
    if update_existing {
        let drawing = s.doc()?;
        let mut spaces = Vec::with_capacity(drawing.layouts.len().saturating_add(1));
        spaces.push(Space::Model);
        spaces.extend(drawing.layouts.iter().take(10_000).map(|layout| Space::Paper(layout.name.clone())));
        let mut examined = 0_usize;
        for space in spaces {
            let Some(entities) = drawing.space(&space) else { continue };
            for entity in entities.iter() {
                examined = examined.saturating_add(1);
                if examined > MAX_SYMBOL_REFS {
                    return Err(bad(SYMBOL_SCALE, format!("il disegno supera il limite di {MAX_SYMBOL_REFS} oggetti esaminabili")));
                }
                let EntityKind::Insert(insert) = &entity.kind else { continue };
                if !is_electrical_layer(&entity.common.layer) || insert.block.starts_with('*') || is_title_block(drawing, &insert.block) {
                    continue;
                }
                if drawing.layer(&entity.common.layer).is_some_and(|layer| layer.locked) {
                    skipped_locked = skipped_locked.saturating_add(1);
                    continue;
                }
                targets.push((entity.handle, insert.insert.xy()));
            }
        }
    }

    let drawing = s.doc_mut()?;
    drawing.header.set_f64("DIMSCALE", scale);
    for (handle, base) in &targets {
        let transform = Mat3::scale_about(*base, ratio);
        drawing.modify_entity(*handle, |entity| entity.kind.transform(&transform))?;
    }
    Ok(json!({
        "previousScale": if previous_valid { previous } else { 1.0 },
        "scale": scale,
        "updateExisting": update_existing,
        "factor": ratio,
        "updated": targets.len(),
        "skippedLocked": skipped_locked,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(ANALYZE, "Analizza layer degli impianti", analyze)
            .params("{} → gruppi, piani e layer non classificati")
            .noundo(),
        CommandSpec::new(APPLY, "Applica visibilità degli impianti", apply)
            .params("{visible:[common|architecture|power|lighting|emergency|fire|photovoltaic|data|intrusion|earth|distribution], floor?:PT|P1|P2|COP|all, keepUnmatched?:bool}"),
        CommandSpec::new(SYMBOL_SCALE, "Scala simboli elettrici", symbol_scale)
            .alias(&["scalasimboli"])
            .params("{scale: numero positivo, updateExisting?: bool (default false)} → {previousScale, scale, factor, updated, skippedLocked}"),
    ]
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{Common, Insert, Layer, Space};
    use cadcraft_geom::Vec3;
    use serde_json::json;

    use super::*;

    #[test]
    fn eplus_and_floor_names_are_classified() {
        assert_eq!(group_id("Fm"), Some("power"));
        assert_eq!(group_id("Luce_Sicurezza"), Some("emergency"));
        assert_eq!(group_id("VVF"), Some("fire"));
        assert_eq!(group_id("Fotovoltaico"), Some("photovoltaic"));
        assert_eq!(group_id("PT_RETE_DATI"), Some("data"));
        assert_eq!(group_id("ARCH|MURI"), Some("architecture"));
        assert_eq!(floor_id("PT_ELETTRICO_LUCE"), Some("PT"));
        assert_eq!(floor_id("P1_RIVELAZIONE_INCENDIO"), Some("P1"));
    }

    #[test]
    fn apply_hides_only_controlled_groups_and_keeps_common_and_unknown() {
        let mut s = Session::new();
        let d = s.doc_mut().unwrap();
        for name in ["BORDO", "ARCH|MURI", "PT_LUCE", "PT_EMERGENZA", "PT_VVF", "NOTE_LIBERE"] {
            d.layers.push(Layer::new(name));
        }
        d.header.set_str("CLAYER", "PT_VVF");
        let result = s.execute(APPLY, &json!({"visible":["common","architecture","emergency"],"floor":"PT","keepUnmatched":true})).unwrap();
        let d = s.doc().unwrap();
        assert!(d.layer("BORDO").unwrap().on);
        assert!(d.layer("ARCH|MURI").unwrap().on);
        assert!(!d.layer("PT_LUCE").unwrap().on);
        assert!(d.layer("PT_EMERGENZA").unwrap().on);
        assert!(!d.layer("PT_VVF").unwrap().on);
        assert!(d.layer("NOTE_LIBERE").unwrap().on);
        assert_eq!(d.header.str("CLAYER", ""), "0");
        assert!(result["changed"].as_u64().unwrap() >= 2);
    }

    fn insert(s: &mut Session, block: &str, layer: &str, at: f64, scale: f64) -> Handle {
        let insert = Insert {
            block: block.into(),
            insert: Vec3::new(at, 5.0, 0.0),
            scale: Vec3::new(scale, scale, 1.0),
            rotation: 0.0,
            attribs: Vec::new(),
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
        };
        s.doc_mut().unwrap().add(&Space::Model, Common { layer: layer.into(), ..Common::default() }, EntityKind::Insert(insert)).unwrap()
    }

    #[test]
    fn symbol_scale_updates_only_electrical_references_and_never_title_blocks() {
        let mut s = Session::new();
        let d = s.doc_mut().unwrap();
        d.layers.push(Layer::new("FM"));
        d.layers.push(Layer::new("ARCH|MURI"));
        d.header.set_f64("DIMSCALE", 0.1);
        let electrical = insert(&mut s, "PRESA", "FM", 10.0, 0.1);
        let architectural = insert(&mut s, "PORTA", "ARCH|MURI", 20.0, 0.1);
        let title = insert(&mut s, "Cart_pl", "FM", 30.0, 0.1);

        let result = s.execute(SYMBOL_SCALE, &json!({"scale":0.2, "updateExisting":true})).unwrap();
        assert_eq!(result["updated"], 1);
        assert_eq!(result["factor"], 2.0);
        assert_eq!(s.doc().unwrap().header.f64("DIMSCALE", 0.0), 0.2);
        let scale = |handle| {
            s.doc().unwrap().entity(handle).and_then(|entity| match &entity.kind {
                EntityKind::Insert(insert) => Some((insert.insert, insert.scale)),
                _ => None,
            })
        };
        assert_eq!(scale(electrical), Some((Vec3::new(10.0, 5.0, 0.0), Vec3::new(0.2, 0.2, 2.0))));
        assert_eq!(scale(architectural).map(|(_, scale)| scale.x), Some(0.1));
        assert_eq!(scale(title).map(|(_, scale)| scale.x), Some(0.1));
    }

    #[test]
    fn symbol_scale_for_future_insertions_does_not_touch_existing_references() {
        let mut s = Session::new();
        let d = s.doc_mut().unwrap();
        d.layers.push(Layer::new("FM"));
        d.header.set_f64("DIMSCALE", 0.1);
        let electrical = insert(&mut s, "PRESA", "FM", 10.0, 0.1);
        let result = s.execute(SYMBOL_SCALE, &json!({"scale":0.05, "updateExisting":false})).unwrap();
        assert_eq!(result["updated"], 0);
        assert_eq!(s.doc().unwrap().header.f64("DIMSCALE", 0.0), 0.05);
        assert!(
            s.doc()
                .unwrap()
                .entity(electrical)
                .is_some_and(|entity| matches!(&entity.kind, EntityKind::Insert(insert) if (insert.scale.x - 0.1).abs() < 1e-12))
        );
    }

    #[test]
    fn hostile_visible_groups_are_rejected_without_changing_layers() {
        let mut s = Session::new();
        s.doc_mut().unwrap().layers.push(Layer::new("Luce"));
        assert!(s.execute(APPLY, &json!({"visible":["not-a-system"]})).is_err());
        assert!(s.doc().unwrap().layer("Luce").unwrap().on);
    }
}
