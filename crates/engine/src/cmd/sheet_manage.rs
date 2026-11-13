//! Model-space sheets on positive integer layers. Edits commit a validated COW copy.
use super::{CommandSpec, Result, Session, bad, str_param};
use cadcraft_doc::{Drawing, Entity, EntityKind, EntityStore, Insert, Layer, Space};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const LIMIT: usize = 4096;
fn num(name: &str) -> Option<u64> {
    name.parse::<u64>().ok().filter(|n| *n > 0 && n.to_string() == name)
}
fn pages(d: &Drawing) -> Result<Vec<String>> {
    let mut out: Vec<_> = d.layers.iter().filter_map(|l| num(&l.name).map(|n| (n, l.name.clone()))).collect();
    if out.len() > LIMIT {
        return Err(bad("sheet", "Massimo 4096 pagine"));
    }
    out.sort_by_key(|p| p.0);
    Ok(out.into_iter().map(|p| p.1).collect())
}
fn block(p: &Value) -> &str {
    str_param(p, "block").unwrap_or("auto")
}
fn title_matches(drawing: &Drawing, insert: &Insert, choice: &str) -> bool {
    insert.block.eq_ignore_ascii_case(choice)
        || (choice.eq_ignore_ascii_case("auto") && super::electrical_layers::is_title_block_insert(drawing, insert))
}
fn title_role(tag: &str) -> &str {
    match tag.trim().to_ascii_uppercase().as_str() {
        "SHEET" | "FOGLIO" | "TAVOLA" => "NUMFOG",
        "NSHEET" => "NUMFOGSEG",
        "SHEETS" => "TOTALE_FOGLI",
        _ => tag,
    }
}
fn shared_title_layer(name: &str) -> bool {
    name.eq_ignore_ascii_case("-LOGO") || name.eq_ignore_ascii_case("-CARTIGLIO")
}
fn page_attribute(insert: &Insert) -> Option<String> {
    insert
        .attribs
        .iter()
        .find(|attribute| title_role(&attribute.tag).eq_ignore_ascii_case("NUMFOG"))
        .map(|attribute| attribute.text.value.trim())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.to_string())
}
fn title_page(drawing: &Drawing, entity: &Entity, insert: &Insert) -> Option<String> {
    if num(&entity.common.layer).is_some() && drawing.layer(&entity.common.layer).is_some() {
        return Some(entity.common.layer.clone());
    }
    if !shared_title_layer(&entity.common.layer) {
        return None;
    }
    page_attribute(insert).filter(|page| drawing.layer(page).is_some())
}
fn title_key<'a>(name: &str, tag: &'a str) -> Option<&'a str> {
    if !["0UNIA3", "00A3SCHEMI"].iter().any(|known| name.eq_ignore_ascii_case(known)) {
        return Some(tag);
    }
    match tag.to_ascii_uppercase().as_str() {
        "OGGETTO1" => Some("IMPIANTO"),
        "OGGETTO2" => Some("IMPIANTO2"),
        "SHEET" => Some("NUMFOG"),
        "NSHEET" => Some("NUMFOGSEG"),
        "SHEETS" => Some("TOTALE_FOGLI"),
        "COMMI" => Some("CLIENTE1"),
        "OGGETTO3" | "UBICAZIONE" | "COMUNE" | "PROVINCIA" | "DISEGNO" | "DATA" | "DISEG" | "PROG" | "VISTO" | "REV1" | "DATAR1" | "REV2"
        | "DATAR2" | "REV3" | "DATAR3" => Some(tag),
        _ => None,
    }
}
fn selected(d: &Drawing, p: &Value) -> Result<Vec<String>> {
    let values = p
        .get("pages")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty() && a.len() <= LIMIT)
        .ok_or_else(|| bad("sheet", "Selezionare da 1 a 4096 pagine"))?;
    let mut set = BTreeSet::new();
    for value in values {
        let name = value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.as_u64().map(|n| n.to_string()))
            .ok_or_else(|| bad("sheet", "Numero pagina non valido"))?;
        if num(&name).is_none() || d.layer(&name).is_none() {
            return Err(bad("sheet", format!("Pagina {name} inesistente")));
        }
        set.insert(name);
    }
    Ok(pages(d)?.into_iter().filter(|n| set.contains(n)).collect())
}
fn unlocked(d: &Drawing, names: &[String]) -> Result<()> {
    if let Some(l) = d.layers.iter().find(|l| names.contains(&l.name) && l.locked) {
        return Err(bad("sheet", format!("Layer {} bloccato", l.name)));
    }
    Ok(())
}
fn transaction(s: &mut Session, p: &Value, edit: fn(&mut Drawing, &Value) -> Result<Value>) -> Result<Value> {
    if s.state()?.block_edit.is_some() || s.doc()?.refedit.is_some() {
        return Err(bad("sheet", "Chiudere prima l'editor dei blocchi"));
    }
    let mut drawing = s.doc()?.clone();
    let result = edit(&mut drawing, p)?;
    *s.doc_mut()? = drawing;
    s.set_selection(Vec::new());
    Ok(result)
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("sheet.list", "Elenco pagine", list).params("{block?: cartiglio} → {pages, current}").noundo(),
        CommandSpec::new("sheet.plot", "Stampa pagine", plot_pages)
            .params("{pages, path?: PDF, paper?: A4, landscape?: true} → {pages: count, sheetNumbers, bytes, path? | data}; one multipage PDF")
            .noundo(),
        CommandSpec::new("sheet.show", "Apri pagina", |s, p| {
            let result = transaction(s, p, show)?;
            let state = s.state_mut()?;
            state.space = Space::Model;
            state.mspace = None;
            Ok(result)
        })
        .params("{page}"),
        CommandSpec::new("sheet.delete", "Elimina pagine", |s, p| transaction(s, p, delete)).params("{pages: [numbers], block?}"),
        CommandSpec::new("sheet.move", "Sposta pagine", |s, p| transaction(s, p, move_pages))
            .params("{pages, position: final one-based position of selection, block?}"),
        CommandSpec::new("sheet.renumber", "Rinumera pagine", |s, p| transaction(s, p, renumber)).params("{pages, start: positive integer, block?}"),
        CommandSpec::new("sheet.insert", "Inserisci pagina", |s, p| transaction(s, p, insert))
            .params("{template: page, position, blocks: [frame and title block names], block?}"),
        CommandSpec::new("sheet.attributes", "Dati cartiglio pagine", |s, p| transaction(s, p, attributes))
            .params("{pages, block?, values: {TAG: value}}"),
    ]
}
fn plot_pages(s: &mut Session, p: &Value) -> Result<Value> {
    let names = selected(s.doc()?, p)?;
    if names.len() > 256 {
        return Err(bad("sheet.plot", "Stampare al massimo 256 pagine alla volta"));
    }
    let paper = str_param(p, "paper").unwrap_or("A4");
    if cadcraft_render::paper_size(paper).is_none() {
        return Err(bad("sheet.plot", "Formato carta non valido"));
    }
    let base = str_param(p, "path").map(std::path::Path::new);
    if base.is_some_and(|path| !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))) {
        return Err(bad("sheet.plot", "Scegliere un file con estensione .pdf"));
    }
    let mut options = json!({"layout":"Model", "sheetLayers":names, "paper":paper,
        "paperBlackLayers":["CARTIGLIO_ATTRIBUTI"],
        "landscape":p.get("landscape").and_then(Value::as_bool).unwrap_or(true),
        "fit":true, "center":true, "plotArea":"extents", "device":"None", "output":"pdf"});
    if let Some(path) = base {
        options["path"] = json!(path.to_string_lossy());
    }
    let mut result = s.execute("plot", &options)?;
    result["pages"] = json!(names.len());
    result["sheetNumbers"] = json!(names);
    result["message"] = json!(format!("Stampa PDF multifoglio completata: {} pagine", names.len()));
    Ok(result)
}
fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let d = s.doc()?;
    let order = pages(d)?;
    let mut grouped: BTreeMap<String, (usize, BTreeSet<String>, BTreeSet<String>, BTreeMap<String, String>)> =
        order.iter().map(|n| (n.clone(), Default::default())).collect();
    for e in d.model.iter() {
        if let Some((count, blocks, _, _)) = grouped.get_mut(&e.common.layer) {
            *count += 1;
            if let EntityKind::Insert(insert) = &e.kind {
                blocks.insert(insert.block.clone());
            }
        }
        if let EntityKind::Insert(i) = &e.kind
            && title_matches(d, i, block(p))
            && let Some(page) = title_page(d, e, i)
            && let Some((_, blocks, title_blocks, values)) = grouped.get_mut(&page)
        {
            if i.attribs.len() > 4096 || i.attribs.iter().any(|a| a.text.value.len() > 65_536) {
                return Err(bad("sheet.list", "Cartiglio troppo grande per il gestore pagine"));
            }
            blocks.insert(i.block.clone());
            title_blocks.insert(i.block.clone());
            for a in &i.attribs {
                if let Some(key) = title_key(&i.block, &a.tag) {
                    values.insert(key.into(), a.text.value.clone());
                }
            }
        }
    }
    let rows:Vec<_>=order.into_iter().map(|name| {
        let (count,blocks,title_blocks,values)=grouped.remove(&name).unwrap_or_default();
        let description=|keys:&[&str]| keys.iter().find_map(|k| values.iter().find(|(tag,_)| tag.eq_ignore_ascii_case(k)).map(|(_,v)|v.clone())).unwrap_or_default();
        let title_blocks: Vec<_> = title_blocks.into_iter().collect();
        json!({"number":name,"description1":description(&["DESCRIZIONE1","IMPIANTO","COMMESSA1","OGGETTO1"]),"description2":description(&["DESCRIZIONE2","IMPIANTO2","COMMESSA2","OGGETTO2"]),"values":values,"blocks":blocks,"titleBlocks":title_blocks,"entities":count})
    }).collect();
    Ok(json!({"pages":rows,"current":d.header.str("CLAYER","0")}))
}
fn activate(d: &mut Drawing, name: &str) -> Result<()> {
    if num(name).is_none() || d.layer(name).is_none() {
        return Err(bad("sheet.show", "Pagina inesistente"));
    }
    for l in &mut d.layers {
        if num(&l.name).is_some() {
            l.frozen = l.name != name;
            if l.name == name {
                l.on = true;
            }
        }
    }
    d.header.set_str("CLAYER", name);
    Ok(())
}
fn show(d: &mut Drawing, p: &Value) -> Result<Value> {
    let name = p
        .get("page")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| p.get("page").and_then(Value::as_u64).map(|n| n.to_string()))
        .ok_or_else(|| bad("sheet.show", "page richiesto"))?;
    activate(d, &name)?;
    Ok(json!({"page":name,"message":format!("FOGLIO {name} attivo")}))
}
fn remap_store(store: &mut EntityStore, map: &BTreeMap<String, String>) {
    let handles = store.handles();
    for h in handles {
        if !store.get(h).is_some_and(|e| needs_remap(e, map)) {
            continue;
        }
        store.modify(h, |e| {
            if let Some(n) = map.get(&e.common.layer) {
                e.common.layer = n.clone();
            }
            match &mut e.kind {
                EntityKind::Insert(i) => {
                    for a in &mut i.attribs {
                        if let Some(n) = a.props.layer.as_ref().and_then(|l| map.get(l)) {
                            a.props.layer = Some(n.clone());
                        }
                    }
                }
                EntityKind::Viewport(v) => {
                    for l in &mut v.frozen_layers {
                        if let Some(n) = map.get(l) {
                            *l = n.clone();
                        }
                    }
                    for (l, _) in &mut v.layer_colors {
                        if let Some(n) = map.get(l) {
                            *l = n.clone();
                        }
                    }
                }
                _ => {}
            }
        });
    }
}
fn needs_remap(e: &Entity, map: &BTreeMap<String, String>) -> bool {
    map.contains_key(&e.common.layer)
        || match &e.kind {
            EntityKind::Insert(i) => i.attribs.iter().any(|a| a.props.layer.as_ref().is_some_and(|l| map.contains_key(l))),
            EntityKind::Viewport(v) => v.frozen_layers.iter().any(|l| map.contains_key(l)) || v.layer_colors.iter().any(|(l, _)| map.contains_key(l)),
            _ => false,
        }
}
fn remap(d: &mut Drawing, map: &BTreeMap<String, String>, title: &str) -> Result<()> {
    let changed: Vec<_> = map.iter().filter(|(a, b)| a != b).map(|(a, _)| a.clone()).collect();
    unlocked(d, &changed)?;
    let mut final_names = BTreeSet::new();
    for l in &d.layers {
        let name = map.get(&l.name).unwrap_or(&l.name);
        if !final_names.insert(name.clone()) {
            return Err(bad("sheet", "Numerazione in conflitto con una pagina esistente"));
        }
    }
    for l in &mut d.layers {
        if let Some(n) = map.get(&l.name) {
            l.name = n.clone();
        }
    }
    remap_store(&mut d.model, map);
    for layout in &mut d.layouts {
        remap_store(&mut layout.entities, map);
    }
    for b in d.blocks.values_mut() {
        if b.entities.iter().any(|e| needs_remap(e, map)) {
            remap_store(&mut Arc::make_mut(b).entities, map);
        }
    }
    for state in &mut d.layer_states {
        for l in &mut state.layers {
            if let Some(n) = map.get(&l.name) {
                l.name = n.clone();
            }
        }
    }
    let foreign = Arc::make_mut(&mut d.foreign);
    foreign.records = std::mem::take(&mut foreign.records)
        .into_iter()
        .map(|((kind, name), extra)| {
            let n = if kind == "LAYER" { map.get(&name).cloned().unwrap_or(name) } else { name };
            ((kind, n), extra)
        })
        .collect();
    for (kind, name) in foreign.targets.values_mut() {
        if kind == "LAYER"
            && let Some(n) = map.get(name)
        {
            *name = n.clone();
        }
    }
    let current = d.header.str("CLAYER", "0");
    if let Some(n) = map.get(&current) {
        d.header.set_str("CLAYER", n);
    }
    remap_shared_title_pages(d, map, title)?;
    sync_numbers(d, title)?;
    Ok(())
}
fn remap_shared_title_pages(d: &mut Drawing, map: &BTreeMap<String, String>, title: &str) -> Result<()> {
    let changes: Vec<_> = d
        .model
        .iter()
        .filter_map(|entity| {
            let EntityKind::Insert(insert) = &entity.kind else { return None };
            if !shared_title_layer(&entity.common.layer) || !title_matches(d, insert, title) {
                return None;
            }
            let old = page_attribute(insert)?;
            map.get(&old).filter(|new| new.as_str() != old).map(|new| (entity.handle, entity.common.layer.clone(), new.clone()))
        })
        .collect();
    if let Some((_, layer, _)) = changes.iter().find(|(_, layer, _)| d.layer(layer).is_some_and(|item| item.locked)) {
        return Err(bad("sheet", format!("Il cartiglio associato si trova sul layer bloccato {layer}")));
    }
    for (handle, _, page) in changes {
        d.modify_entity(handle, |entity| {
            if let EntityKind::Insert(insert) = &mut entity.kind {
                for attribute in &mut insert.attribs {
                    if !attribute.constant && title_role(&attribute.tag).eq_ignore_ascii_case("NUMFOG") {
                        attribute.text.value = page.clone();
                    }
                }
            }
        })?;
    }
    Ok(())
}
fn sync_numbers(d: &mut Drawing, title: &str) -> Result<()> {
    let order = pages(d)?;
    let next: BTreeMap<_, _> = order.iter().enumerate().map(|(i, n)| (n.clone(), order.get(i + 1).cloned().unwrap_or_default())).collect();
    let total = order.len().to_string();
    let handles: Vec<_> = d
        .model
        .iter()
        .filter_map(|entity| {
            let EntityKind::Insert(insert) = &entity.kind else { return None };
            if !title_matches(d, insert, title) {
                return None;
            }
            let page = title_page(d, entity, insert)?;
            next.contains_key(&page).then_some((entity.handle, page))
        })
        .collect();
    for (h, page) in handles {
        if let Some(e) = d.model.get(h)
            && d.layer(&e.common.layer).is_some_and(|l| l.locked)
            && let EntityKind::Insert(i) = &e.kind
            && i.attribs.iter().any(|a| {
                !a.constant
                    && ((title_role(&a.tag).eq_ignore_ascii_case("NUMFOG") && a.text.value != page)
                        || (title_role(&a.tag).eq_ignore_ascii_case("NUMFOGSEG") && next.get(&page).is_some_and(|n| a.text.value != *n))
                        || (title_role(&a.tag).eq_ignore_ascii_case("TOTALE_FOGLI") && a.text.value != total))
            })
        {
            return Err(bad("sheet", "Un cartiglio da rinumerare si trova su un layer bloccato"));
        }
        d.modify_entity(h, |e| {
            if let EntityKind::Insert(i) = &mut e.kind {
                for a in &mut i.attribs {
                    if !a.constant && title_role(&a.tag).eq_ignore_ascii_case("TOTALE_FOGLI") {
                        a.text.value = total.clone();
                    }
                    if !a.constant && title_role(&a.tag).eq_ignore_ascii_case("NUMFOG") {
                        a.text.value = page.clone();
                    }
                    if !a.constant && title_role(&a.tag).eq_ignore_ascii_case("NUMFOGSEG") {
                        a.text.value = next.get(&page).cloned().unwrap_or_default();
                    }
                }
            }
        })?;
    }
    Ok(())
}
fn renumber(d: &mut Drawing, p: &Value) -> Result<Value> {
    let names = selected(d, p)?;
    let start =
        p.get("start").and_then(Value::as_u64).filter(|n| *n > 0).ok_or_else(|| bad("sheet.renumber", "Numero iniziale positivo richiesto"))?;
    let mut map = BTreeMap::new();
    for (i, n) in names.iter().enumerate() {
        let value = start.checked_add(i as u64).ok_or_else(|| bad("sheet", "Numerazione troppo grande"))?;
        map.insert(n.clone(), value.to_string());
    }
    remap(d, &map, block(p))?;
    Ok(json!({"mapping":map,"message":"Pagine rinumerate"}))
}
fn move_pages(d: &mut Drawing, p: &Value) -> Result<Value> {
    let chosen = selected(d, p)?;
    let mut order: Vec<_> = pages(d)?.into_iter().filter(|n| !chosen.contains(n)).collect();
    let pos = p
        .get("position")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0 && *n <= order.len() + 1)
        .ok_or_else(|| bad("sheet.move", "Posizione non valida"))?;
    order.splice(pos - 1..pos - 1, chosen);
    let map = order.into_iter().enumerate().map(|(i, n)| (n, (i + 1).to_string())).collect();
    remap(d, &map, block(p))?;
    Ok(json!({"mapping":map,"message":"Ordine delle pagine aggiornato"}))
}
fn delete(d: &mut Drawing, p: &Value) -> Result<Value> {
    let chosen = selected(d, p)?;
    unlocked(d, &chosen)?;
    if let Some(entity) = d.model.iter().find(|entity| {
        let EntityKind::Insert(insert) = &entity.kind else { return false };
        shared_title_layer(&entity.common.layer)
            && title_matches(d, insert, block(p))
            && title_page(d, entity, insert).is_some_and(|page| chosen.contains(&page))
            && d.layer(&entity.common.layer).is_some_and(|layer| layer.locked)
    }) {
        return Err(bad("sheet.delete", format!("Il cartiglio associato si trova sul layer bloccato {}", entity.common.layer)));
    }
    if d.layouts
        .iter()
        .flat_map(|l| l.entities.iter())
        .chain(d.blocks.values().flat_map(|b| b.entities.iter()))
        .any(|e| chosen.contains(&e.common.layer))
    {
        return Err(bad("sheet.delete", "Layer usati anche nei layout o nelle definizioni dei blocchi: eliminazione annullata"));
    }
    let handles: Vec<_> = d
        .model
        .iter()
        .filter(|entity| {
            if chosen.contains(&entity.common.layer) {
                return true;
            }
            let EntityKind::Insert(insert) = &entity.kind else { return false };
            shared_title_layer(&entity.common.layer)
                && title_matches(d, insert, block(p))
                && title_page(d, entity, insert).is_some_and(|page| chosen.contains(&page))
        })
        .map(|e| e.handle)
        .collect();
    for h in &handles {
        d.remove_entity(*h);
        d.hidden_objects.remove(h);
        Arc::make_mut(&mut d.foreign).entities.remove(h);
    }
    for group in &mut d.groups {
        group.members.retain(|h| !handles.contains(h));
    }
    d.layers.retain(|l| !chosen.contains(&l.name));
    let foreign = Arc::make_mut(&mut d.foreign);
    foreign.records.retain(|(kind, name), _| kind != "LAYER" || !chosen.contains(name));
    foreign.targets.retain(|_, (kind, name)| kind != "LAYER" || !chosen.contains(name));
    for state in &mut d.layer_states {
        state.layers.retain(|l| !chosen.contains(&l.name));
    }
    for layout in &mut d.layouts {
        let hs = layout.entities.handles();
        for h in hs {
            layout.entities.modify(h, |e| {
                if let EntityKind::Viewport(v) = &mut e.kind {
                    v.frozen_layers.retain(|l| !chosen.contains(l));
                    v.layer_colors.retain(|(l, _)| !chosen.contains(l));
                }
            });
        }
    }
    let remaining = pages(d)?;
    if chosen.contains(&d.header.str("CLAYER", "0")) {
        if let Some(n) = remaining.first() {
            activate(d, n)?;
        } else {
            if let Some(layer) = d.layer_mut("0") {
                layer.frozen = false;
                layer.on = true;
            }
            d.header.set_str("CLAYER", "0");
        }
    }
    sync_numbers(d, block(p))?;
    Ok(json!({"deleted":chosen,"entities":handles.len(),"message":"Pagine eliminate (annullabile con Undo)"}))
}
fn insert(d: &mut Drawing, p: &Value) -> Result<Value> {
    let template = str_param(p, "template").ok_or_else(|| bad("sheet.insert", "Pagina modello richiesta"))?;
    if num(template).is_none() || d.layer(template).is_none() {
        return Err(bad("sheet.insert", "Pagina modello inesistente"));
    }
    let order = pages(d)?;
    if order.len() >= LIMIT {
        return Err(bad("sheet.insert", "Massimo 4096 pagine"));
    }
    let pos = p
        .get("position")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0 && *n <= order.len() + 1)
        .ok_or_else(|| bad("sheet.insert", "Posizione non valida"))?;
    let names = p
        .get("blocks")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty() && a.len() <= 256)
        .ok_or_else(|| bad("sheet.insert", "Selezionare cornice e cartiglio da copiare"))?;
    let names: BTreeSet<_> = names.iter().filter_map(Value::as_str).collect();
    let copies: Vec<Entity> = d
        .model
        .iter()
        .filter(|entity| {
            let EntityKind::Insert(insert) = &entity.kind else { return false };
            names.contains(insert.block.as_str())
                && (entity.common.layer == template
                    || (title_matches(d, insert, block(p)) && title_page(d, entity, insert).as_deref() == Some(template)))
        })
        .map(|e| (**e).clone())
        .collect();
    if copies.is_empty() || copies.len() > 10_000 {
        return Err(bad("sheet.insert", "Nessun blocco modello valido, o troppi blocchi"));
    }
    d.handseed.checked_add(copies.len() as u64).ok_or_else(|| bad("sheet.insert", "Handle esauriti"))?;
    let temp =
        (1..=LIMIT + 1).map(|n| n.to_string()).find(|n| d.layer(n).is_none()).ok_or_else(|| bad("sheet.insert", "Numero pagina non disponibile"))?;
    let mut layer = d.layer(template).cloned().unwrap_or_else(|| Layer::new(&temp));
    layer.name = temp.clone();
    layer.locked = false;
    layer.vp_freeze_new = true;
    d.layers.push(layer);
    for layout in &mut d.layouts {
        for h in layout.entities.handles() {
            layout.entities.modify(h, |e| {
                if let EntityKind::Viewport(v) = &mut e.kind {
                    v.frozen_layers.push(temp.clone());
                }
            });
        }
    }
    for mut e in copies {
        let shared = shared_title_layer(&e.common.layer);
        if let EntityKind::Insert(i) = &mut e.kind
            && ["0UNIA3", "00A3SCHEMI"].iter().any(|known| i.block.eq_ignore_ascii_case(known))
        {
            for a in &mut i.attribs {
                if !a.constant && title_key(&i.block, &a.tag).is_none() {
                    a.text.value.clear();
                }
            }
        }
        if let EntityKind::Insert(insert) = &mut e.kind
            && shared
        {
            for attribute in &mut insert.attribs {
                if !attribute.constant && title_role(&attribute.tag).eq_ignore_ascii_case("NUMFOG") {
                    attribute.text.value = temp.clone();
                }
            }
        } else {
            e.common.layer = temp.clone();
        }
        d.add(&Space::Model, e.common, e.kind)?;
    }
    let mut sequence = order;
    sequence.insert(pos - 1, temp.clone());
    let map = sequence.into_iter().enumerate().map(|(i, n)| (n, (i + 1).to_string())).collect();
    remap(d, &map, block(p))?;
    activate(d, &pos.to_string())?;
    Ok(json!({"page":pos,"message":"Pagina vuota con cornice e cartiglio inserita"}))
}
fn attributes(d: &mut Drawing, p: &Value) -> Result<Value> {
    let chosen = selected(d, p)?;
    unlocked(d, &chosen)?;
    let values = p
        .get("values")
        .and_then(Value::as_object)
        .filter(|o| !o.is_empty() && o.len() <= 256)
        .ok_or_else(|| bad("sheet.attributes", "Impostare almeno un attributo"))?;
    if values.values().any(|v| v.as_str().is_none_or(|s| s.len() > 65_536)) {
        return Err(bad("sheet.attributes", "Valori testuali richiesti, massimo 65536 byte ciascuno"));
    }
    let hs: Vec<_> = d
        .model
        .iter()
        .filter_map(|entity| {
            let EntityKind::Insert(insert) = &entity.kind else { return None };
            if !title_matches(d, insert, block(p)) {
                return None;
            }
            title_page(d, entity, insert).filter(|page| chosen.contains(page)).map(|page| (entity.handle, page))
        })
        .collect();
    if hs.is_empty() {
        return Err(bad("sheet.attributes", "Cartiglio non trovato nelle pagine selezionate"));
    }
    if let Some(entity) =
        hs.iter().filter_map(|(handle, _)| d.model.get(*handle)).find(|entity| d.layer(&entity.common.layer).is_some_and(|layer| layer.locked))
    {
        return Err(bad("sheet.attributes", format!("Il cartiglio si trova sul layer bloccato {}", entity.common.layer)));
    }
    for page in &chosen {
        if !hs.iter().any(|(_, associated)| associated == page) {
            return Err(bad("sheet.attributes", format!("Cartiglio {} assente nella pagina {page}", block(p))));
        }
    }
    let mut changed = 0;
    for (h, _) in hs {
        d.modify_entity(h, |e| {
            if let EntityKind::Insert(i) = &mut e.kind {
                for a in &mut i.attribs {
                    if !a.constant
                        && let Some(v) = values
                            .iter()
                            .find(|(tag, _)| title_key(&i.block, &a.tag).is_some_and(|key| tag.eq_ignore_ascii_case(key)))
                            .and_then(|(_, v)| v.as_str())
                        && a.text.value != v
                    {
                        a.text.value = v.into();
                        changed += 1;
                    }
                }
            }
        })?;
    }
    if changed == 0 {
        return Err(bad("sheet.attributes", "Nessun valore modificabile diverso da quello attuale"));
    }
    Ok(json!({"changed":changed,"message":format!("Aggiornati {changed} valori del cartiglio")}))
}
