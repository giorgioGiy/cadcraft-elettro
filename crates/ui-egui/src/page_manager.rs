//! Model-space page manager. Every drawing edit runs one engine command.
use crate::CadApp;
use cadcraft_doc::Space;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default, Deserialize)]
struct Page {
    number: String,
    description1: String,
    description2: String,
    values: BTreeMap<String, String>,
    blocks: Vec<String>,
    #[serde(default, rename = "titleBlocks")]
    title_blocks: Vec<String>,
}
#[derive(Clone, Copy, PartialEq, Default)]
enum Mode {
    #[default]
    None,
    Insert,
    Delete,
    Move,
    Renumber,
    Attributes,
    Print,
}
#[derive(Clone)]
struct State {
    uid: u64,
    revision: u64,
    rows: Vec<Page>,
    selected: BTreeSet<String>,
    focus: Option<String>,
    anchor: Option<usize>,
    title_block: String,
    template: String,
    copy_blocks: BTreeSet<String>,
    position: u64,
    start: u64,
    mode: Mode,
    tag: String,
    value: String,
    pending: BTreeMap<String, String>,
    error: Option<String>,
    preview: bool,
    texture: Option<egui::TextureHandle>,
    preview_key: Option<(u64, String)>,
    paper: String,
    landscape: bool,
}
impl State {
    fn new(uid: u64) -> Self {
        Self {
            uid,
            revision: u64::MAX,
            rows: Vec::new(),
            selected: BTreeSet::new(),
            focus: None,
            anchor: None,
            title_block: "auto".into(),
            template: String::new(),
            copy_blocks: BTreeSet::new(),
            position: 1,
            start: 1,
            mode: Mode::None,
            tag: String::new(),
            value: String::new(),
            pending: BTreeMap::new(),
            error: None,
            preview: true,
            texture: None,
            preview_key: None,
            paper: "A4".into(),
            landscape: true,
        }
    }
}
fn refresh(app: &mut CadApp, st: &mut State) -> Result<(), String> {
    let initial = st.revision == u64::MAX;
    let result = app.run("sheet.list", json!({"block":st.title_block}))?;
    st.rows = serde_json::from_value(result.get("pages").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())?;
    st.selected.retain(|n| st.rows.iter().any(|r| r.number == *n));
    if st.focus.as_ref().is_none_or(|n| !st.rows.iter().any(|r| r.number == *n)) {
        st.focus = result
            .get("current")
            .and_then(Value::as_str)
            .filter(|n| st.rows.iter().any(|r| r.number == *n))
            .map(str::to_string)
            .or_else(|| st.rows.first().map(|r| r.number.clone()));
    }
    st.revision = app.session.state().map_err(|e| e.to_string())?.revision;
    if initial && let Some(n) = &st.focus {
        st.selected.insert(n.clone());
    }
    Ok(())
}
fn button(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    let response = ui.add_enabled(enabled, egui::Button::new(label));
    crate::control::record_widget(ui, &response, label, "button", None);
    response.clicked()
}
fn prepare(st: &mut State, mode: Mode) {
    st.mode = mode;
    st.error = None;
    st.pending.clear();
    st.tag.clear();
    st.value.clear();
    st.template = st.focus.clone().or_else(|| st.rows.first().map(|r| r.number.clone())).unwrap_or_default();
    st.position = st.rows.iter().position(|r| r.number == st.template).map_or(1, |i| i as u64 + 1);
    st.copy_blocks = st
        .rows
        .iter()
        .find(|r| r.number == st.template)
        .map(|r| r.blocks.iter().filter(|b| r.title_blocks.contains(b) || b.to_ascii_uppercase().starts_with("FG_")).cloned().collect())
        .unwrap_or_default();
}
fn preview(app: &CadApp, ctx: &egui::Context, st: &mut State) -> Result<(), String> {
    let Some(number) = st.focus.clone() else {
        st.texture = None;
        return Ok(());
    };
    let revision = app.session.state().map_err(|e| e.to_string())?.revision;
    if st.preview_key.as_ref() == Some(&(revision, number.clone())) {
        return Ok(());
    }
    st.preview_key = Some((revision, number.clone()));
    st.texture = None;
    let mut d = app.session.doc().map_err(|e| e.to_string())?.clone();
    for layer in &mut d.layers {
        if layer.name.parse::<u64>().is_ok_and(|n| n > 0 && n.to_string() == layer.name) {
            layer.frozen = layer.name != number;
            if layer.name == number {
                layer.on = true;
            }
        }
    }
    let list = cadcraft_render::build(&d, &Space::Model, &Default::default());
    if list.bounds.is_empty() {
        return Ok(());
    }
    let view = cadcraft_render::raster::View::fit(&list.bounds, 640, 360, 0.05);
    let image = cadcraft_render::raster::render(&list, &view, &Default::default()).ok_or("Impossibile generare l'anteprima")?;
    let image = egui::ColorImage::from_rgba_premultiplied([640, 360], image.data());
    st.texture = Some(ctx.load_texture("sheet-manager-preview", image, egui::TextureOptions::LINEAR));
    Ok(())
}
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| show(app, ctx, open))) {
        log::error!("page manager interrupted: {e:?}");
        app.session.echo("Gestione pagine interrotta per un errore interno; operazione non confermata.");
        *open = false;
    }
}
fn show(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let id = egui::Id::unique("cc_page_manager");
    let Ok(doc) = app.session.state() else {
        *open = false;
        return;
    };
    let mut st = ctx.data_mut(|d| d.get_temp::<State>(id)).filter(|s| s.uid == doc.uid).unwrap_or_else(|| State::new(doc.uid));
    if (st.rows.is_empty() || st.revision != doc.revision)
        && let Err(e) = refresh(app, &mut st)
    {
        st.error = Some(e);
    }
    let mut command: Option<(&str, Value)> = None;
    let mut close = false;
    egui::Window::new("Gestione pagine")
        .id(id.with("compact-layout"))
        .open(open)
        .default_pos(ctx.content_rect().center() - egui::vec2(300.0, 285.0))
        .default_size(egui::vec2(600.0, 570.0))
        .min_size(egui::vec2(420.0, 300.0))
        .max_size((ctx.content_rect().size() - egui::vec2(40.0, 40.0)).max(egui::vec2(400.0, 300.0)))
        .resizable(true)
        .show(ctx, |ui| {
            ui.label("Pagine dello schema gestite come layer numerici");
            ui.horizontal_wrapped(|ui| {
                if button(ui, "Seleziona tutte", !st.rows.is_empty()) {
                    st.selected = st.rows.iter().map(|r| r.number.clone()).collect();
                }
                if button(ui, "Nessuna", !st.selected.is_empty()) {
                    st.selected.clear();
                }
                ui.label(format!("{} selezionate / {} pagine", st.selected.len(), st.rows.len()));
                if button(ui, "Aggiorna", true) {
                    if let Err(e) = refresh(app, &mut st) {
                        st.error = Some(e);
                    }
                    st.preview_key = None;
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("sheet-manager-body").max_height((ctx.content_rect().height() - 170.0).max(160.0)).show(ui, |ui| {
                let table_width = (ui.available_width() - 120.0).max(240.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(table_width);
                        egui::ScrollArea::both()
                            .id_salt("sheet-manager-list")
                            .max_width(table_width)
                            .max_height((ctx.content_rect().height() * 0.28).clamp(100.0, 210.0))
                            .show(ui, |ui| {
                                egui::Grid::new("sheet-manager-table").num_columns(4).striped(true).min_col_width(28.0).show(ui, |ui| {
                                    ui.label("");
                                    ui.strong("Numero");
                                    ui.strong("Descrizione riga 1");
                                    ui.strong("Descrizione riga 2");
                                    ui.end_row();
                                    for (index, row) in st.rows.clone().iter().enumerate() {
                                        let selected = st.selected.contains(&row.number);
                                        let mut checked = selected;
                                        let check = ui.checkbox(&mut checked, "");
                                        crate::control::record_widget(
                                            ui,
                                            &check,
                                            &format!("Seleziona pagina {}", row.number),
                                            "checkbox",
                                            Some(checked),
                                        );
                                        if check.changed() {
                                            if checked {
                                                st.selected.insert(row.number.clone());
                                            } else {
                                                st.selected.remove(&row.number);
                                            }
                                            st.focus = Some(row.number.clone());
                                            st.anchor = Some(index);
                                        }
                                        let number = ui.selectable_label(selected, &row.number);
                                        let one = ui.selectable_label(selected, if row.description1.is_empty() { "—" } else { &row.description1 });
                                        let two = ui.selectable_label(selected, if row.description2.is_empty() { "—" } else { &row.description2 });
                                        if number.clicked() || one.clicked() || two.clicked() {
                                            let modifiers = ui.input(|i| i.modifiers);
                                            if modifiers.shift {
                                                let anchor = st.anchor.unwrap_or(index).min(st.rows.len().saturating_sub(1));
                                                if !modifiers.command {
                                                    st.selected.clear();
                                                }
                                                for r in st.rows.iter().skip(anchor.min(index)).take(anchor.abs_diff(index) + 1) {
                                                    st.selected.insert(r.number.clone());
                                                }
                                            } else if modifiers.command {
                                                if !st.selected.remove(&row.number) {
                                                    st.selected.insert(row.number.clone());
                                                }
                                                st.anchor = Some(index);
                                            } else {
                                                st.selected.clear();
                                                st.selected.insert(row.number.clone());
                                                st.anchor = Some(index);
                                            }
                                            st.focus = Some(row.number.clone());
                                        }
                                        if number.double_clicked() || one.double_clicked() || two.double_clicked() {
                                            command = Some(("sheet.show", json!({"page":row.number})));
                                        }
                                        ui.end_row();
                                    }
                                });
                            });
                    });
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        let selected = !st.selected.is_empty();
                        if button(ui, "Inserisci...", !st.rows.is_empty()) {
                            prepare(&mut st, Mode::Insert);
                        }
                        if button(ui, "Elimina...", selected) {
                            prepare(&mut st, Mode::Delete);
                        }
                        if button(ui, "Sposta...", selected) {
                            prepare(&mut st, Mode::Move);
                        }
                        if button(ui, "Rinumera...", selected) {
                            prepare(&mut st, Mode::Renumber);
                        }
                        if button(ui, "Dati cartiglio...", selected) {
                            prepare(&mut st, Mode::Attributes);
                        }
                        if button(ui, "Stampa...", selected) {
                            prepare(&mut st, Mode::Print);
                        }
                        if button(ui, "Apri pagina", st.focus.is_some()) {
                            command = Some(("sheet.show", json!({"page":st.focus})));
                        }
                    });
                });
                ui.separator();
                let mut changed_block = false;
                ui.horizontal(|ui| {
                    ui.label("Blocco cartiglio:");
                    let names: BTreeSet<_> = st.rows.iter().flat_map(|r| r.blocks.iter().cloned()).collect();
                    craft_ui::ComboBox::from_id_salt("sheet-title-block")
                        .selected_text(if st.title_block == "auto" { "Automatico" } else { &st.title_block })
                        .show_ui(ui, |ui| {
                            changed_block |= ui.selectable_value(&mut st.title_block, "auto".into(), "Automatico (entrambi i formati)").changed();
                            for name in names {
                                changed_block |= ui.selectable_value(&mut st.title_block, name.clone(), name).changed();
                            }
                        });
                });
                if let Some(row) = st.rows.iter().find(|r| Some(&r.number) == st.focus.as_ref()) {
                    ui.small(format!(
                        "Cartiglio della pagina: {}",
                        if row.title_blocks.is_empty() { "non riconosciuto".into() } else { row.title_blocks.join(", ") }
                    ));
                }
                if changed_block {
                    st.pending.clear();
                    if let Err(e) = refresh(app, &mut st) {
                        st.error = Some(e);
                    }
                }
                if st.mode != Mode::None {
                    ui.group(|ui| {
                        let selected: Vec<_> = st.selected.iter().cloned().collect();
                        let mut params = json!({"pages":selected,"block":st.title_block});
                        let mut apply_command = None;
                        match st.mode {
                            Mode::Print => {
                                ui.strong(format!("Stampa: {} pagine selezionate", st.selected.len()));
                                ui.horizontal(|ui| {
                                    ui.label("Formato carta:");
                                    craft_ui::ComboBox::from_id_salt("sheet-print-paper").selected_text(&st.paper).show_ui(ui, |ui| {
                                        for paper in ["A4", "A3", "A2", "A1", "A0"] {
                                            ui.selectable_value(&mut st.paper, paper.into(), paper);
                                        }
                                    });
                                    ui.checkbox(&mut st.landscape, "Orizzontale");
                                });
                                ui.label("Un unico PDF multifoglio. Ogni pagina viene adattata e centrata sul foglio.");
                                ui.label("Le pagine selezionate sono raccolte in ordine numerico, includendo i layer comuni.");
                                if button(ui, "Stampa PDF...", !selected.is_empty() && selected.len() <= 256) {
                                    if let Some(pick) = &app.services.pick_save {
                                        if let Some(path) = pick("schema.pdf") {
                                            params["path"] = json!(path);
                                            params["paper"] = json!(st.paper);
                                            params["landscape"] = json!(st.landscape);
                                            apply_command = Some("sheet.plot");
                                        }
                                    } else {
                                        st.error = Some("Salvataggio PDF non disponibile in questa interfaccia.".into());
                                    }
                                }
                                if selected.len() > 256 {
                                    ui.label("Stampare al massimo 256 pagine alla volta.");
                                }
                            }
                            Mode::Delete => {
                                ui.strong(format!("Eliminare {} pagine e i loro oggetti?", st.selected.len()));
                                ui.label(selected.join(", "));
                                if button(ui, "Conferma eliminazione", true) {
                                    apply_command = Some("sheet.delete");
                                }
                            }
                            Mode::Move => {
                                ui.strong("Sposta le pagine selezionate nell'elenco");
                                ui.label("La geometria rimane nella stessa posizione. La numerazione diventa consecutiva da 1.");
                                ui.horizontal(|ui| {
                                    ui.label("Posizione finale della prima pagina:");
                                    ui.add(
                                        egui::DragValue::new(&mut st.position).range(1..=st.rows.len().saturating_sub(st.selected.len()) as u64 + 1),
                                    );
                                });
                                params["position"] = json!(st.position);
                                if button(ui, "Applica spostamento", true) {
                                    apply_command = Some("sheet.move");
                                }
                            }
                            Mode::Renumber => {
                                ui.strong("Rinumera le pagine selezionate");
                                ui.horizontal(|ui| {
                                    ui.label("Inizia da:");
                                    ui.add(egui::DragValue::new(&mut st.start).range(1..=1_000_000));
                                });
                                ui.label("I numeri già usati da pagine non selezionate non vengono sovrascritti.");
                                params["start"] = json!(st.start);
                                if button(ui, "Applica numerazione", true) {
                                    apply_command = Some("sheet.renumber");
                                }
                            }
                            Mode::Insert => {
                                ui.strong("Inserisci pagina vuota da cornice e cartiglio esistenti");
                                let old = st.template.clone();
                                ui.horizontal(|ui| {
                                    ui.label("Pagina modello:");
                                    craft_ui::ComboBox::from_id_salt("sheet-template").selected_text(&st.template).show_ui(ui, |ui| {
                                        for r in &st.rows {
                                            ui.selectable_value(&mut st.template, r.number.clone(), &r.number);
                                        }
                                    });
                                    ui.label("Posizione:");
                                    ui.add(egui::DragValue::new(&mut st.position).range(1..=st.rows.len() as u64 + 1));
                                });
                                if old != st.template {
                                    st.copy_blocks.clear();
                                }
                                if let Some(row) = st.rows.iter().find(|r| r.number == st.template) {
                                    ui.label("Copia soltanto questi blocchi (non gli apparecchi):");
                                    for name in &row.blocks {
                                        let mut checked = st.copy_blocks.contains(name);
                                        if ui.checkbox(&mut checked, name).changed() {
                                            if checked {
                                                st.copy_blocks.insert(name.clone());
                                            } else {
                                                st.copy_blocks.remove(name);
                                            }
                                        }
                                    }
                                }
                                params["template"] = json!(st.template);
                                params["position"] = json!(st.position);
                                params["blocks"] = json!(st.copy_blocks);
                                if button(ui, "Inserisci pagina", !st.copy_blocks.is_empty()) {
                                    apply_command = Some("sheet.insert");
                                }
                            }
                            Mode::Attributes => {
                                ui.strong(format!("Dati del cartiglio: {} pagine selezionate", st.selected.len()));
                                let tags: BTreeSet<_> =
                                    st.rows.iter().filter(|r| st.selected.contains(&r.number)).flat_map(|r| r.values.keys().cloned()).collect();
                                let old_tag = st.tag.clone();
                                if !tags.contains(&st.tag) {
                                    st.tag = tags.first().cloned().unwrap_or_default();
                                }
                                ui.horizontal(|ui| {
                                    ui.label("Attributo:");
                                    craft_ui::ComboBox::from_id_salt("sheet-attribute-tag").selected_text(&st.tag).show_ui(ui, |ui| {
                                        for tag in tags {
                                            ui.selectable_value(&mut st.tag, tag.clone(), tag);
                                        }
                                    });
                                });
                                if old_tag != st.tag {
                                    let mut values = st.rows.iter().filter(|r| st.selected.contains(&r.number)).filter_map(|r| r.values.get(&st.tag));
                                    st.value = values.next().filter(|first| values.all(|v| v == *first)).cloned().unwrap_or_default();
                                }
                                ui.horizontal(|ui| {
                                    ui.label("Valore:");
                                    ui.add(egui::TextEdit::singleline(&mut st.value).char_limit(65_536));
                                    if button(ui, "Imposta", !st.tag.is_empty()) {
                                        st.pending.insert(st.tag.clone(), st.value.clone());
                                    }
                                });
                                for (tag, value) in st.pending.clone() {
                                    ui.horizontal(|ui| {
                                        ui.label(format!("{tag} = {value}"));
                                        if ui.small_button("Rimuovi").clicked() {
                                            st.pending.remove(&tag);
                                        }
                                    });
                                }
                                ui.label("Solo gli attributi elencati sopra verranno applicati alle pagine selezionate.");
                                params["values"] = json!(st.pending);
                                if button(ui, "Salva dati cartiglio", !st.pending.is_empty()) {
                                    apply_command = Some("sheet.attributes");
                                }
                            }
                            Mode::None => {}
                        }
                        if let Some(id) = apply_command {
                            command = Some((id, params));
                        }
                        if button(ui, "Annulla operazione", true) {
                            st.mode = Mode::None;
                            st.pending.clear();
                        }
                    });
                }
                ui.checkbox(&mut st.preview, "Anteprima della pagina evidenziata");
                if st.preview {
                    if let Err(e) = preview(app, ctx, &mut st) {
                        st.error = Some(e);
                    }
                    if let Some(texture) = &st.texture {
                        let width = ui.available_width().min(420.0);
                        ui.image((texture.id(), egui::vec2(width, width * 9.0 / 16.0)));
                    }
                }
            });
            if let Some(error) = &st.error {
                ui.colored_label(crate::theme::Tokens::get().text_dim, error);
            }
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.small("Undo annulla le operazioni. Salvare poi il disegno.");
                close = button(ui, "Chiudi", true);
            });
        });
    if let Some((command, params)) = command {
        match app.run(command, params) {
            Ok(result) => {
                if let Some(mapping) = result.get("mapping").and_then(Value::as_object) {
                    st.selected = st.selected.iter().map(|n| mapping.get(n).and_then(Value::as_str).unwrap_or(n).to_string()).collect();
                    st.focus = st.focus.map(|n| mapping.get(&n).and_then(Value::as_str).unwrap_or(&n).to_string());
                }
                if command == "sheet.insert"
                    && let Some(n) = result.get("page").and_then(Value::as_u64)
                {
                    st.selected = BTreeSet::from([n.to_string()]);
                    st.focus = Some(n.to_string());
                }
                st.mode = Mode::None;
                st.pending.clear();
                st.error = None;
                st.preview_key = None;
                if let Err(e) = refresh(app, &mut st) {
                    st.error = Some(e);
                }
            }
            Err(e) => st.error = Some(e),
        }
    }
    if close {
        *open = false;
    }
    if *open {
        ctx.data_mut(|d| d.insert_temp(id, st));
    } else {
        ctx.data_mut(|d| d.remove::<State>(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use cadcraft_engine::Session;

    #[test]
    fn manager_opens_all_operation_panels_without_changing_the_drawing() {
        let mut app = CadApp::new(Session::new(), Services::default());
        for n in 1..=3 {
            app.run("layer.new", json!({"name":n.to_string(),"current":true})).unwrap();
        }
        let before = app.session.doc().unwrap().clone();
        app.start("pagine");
        assert_eq!(app.ui.dialog.as_deref(), Some("pages"));
        let ctx = egui::Context::default();
        let uid = app.session.state().unwrap().uid;
        let mut st = State::new(uid);
        refresh(&mut app, &mut st).unwrap();
        st.preview = false;
        st.selected = BTreeSet::from(["1".into(), "3".into()]);
        let id = egui::Id::unique("cc_page_manager");
        for mode in [Mode::None, Mode::Insert, Mode::Delete, Mode::Move, Mode::Renumber, Mode::Attributes, Mode::Print] {
            st.mode = mode;
            ctx.data_mut(|d| d.insert_temp(id, st.clone()));
            let mut open = true;
            let input =
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 900.0))), ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| dialog(&mut app, ui.ctx(), &mut open));
            output.textures_delta.clear();
            assert!(open);
            let result = ctx.data_mut(|d| d.get_temp::<State>(id)).unwrap();
            assert_eq!(result.rows.len(), 3);
            assert_eq!(result.selected.len(), 2);
            assert_eq!(*app.session.doc().unwrap(), before);
        }
    }
}
