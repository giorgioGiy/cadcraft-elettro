//! Attribute form backed by ATTEDIT's transactional interactive command.
use cadcraft_doc::{EntityKind, Handle, Insert};
use cadcraft_engine::Input;

use crate::{CadApp, theme::Tokens};
use std::collections::BTreeMap;

#[derive(Clone)]
struct Group {
    label: String,
    indices: Vec<usize>,
}

/// Names used on electrical schematics.  The DXF tags remain available in the hover text so
/// drawings with a custom convention are still diagnosable.
fn attribute_label(tag: &str) -> Option<&'static str> {
    Some(match tag.to_ascii_uppercase().as_str() {
        "U_DEN" => "Utenza — Denominazione",
        "U_TIPO" => "Utenza — Tipo",
        "U_SIGLA" => "Utenza — Sigla",
        "U_KW" => "Utenza — Potenza totale [kW]",
        "U_IB" => "Utenza — Ib [A]",
        "I_TIPO" => "Interruttore / sezionatore — Tipo",
        "I_POLI" => "Interruttore / sezionatore — Poli",
        "I_IN" => "Interruttore / sezionatore — In [A]",
        "I_ICU" => "Interruttore / sezionatore — Potere d'interruzione [kA]",
        "D_IDN" => "Differenziale — Idn [A]",
        "D_T" => "Differenziale — Tempo [s]",
        "F_TIPO" => "Fusibile — Tipo",
        "F_PORTATA" => "Fusibile — Portata [A]",
        "C_TIPO" => "Contattore — Tipo",
        "C_IN" => "Contattore — Portata [A]",
        "R_TIPO" => "Relè termico — Tipo",
        "R_TARA" => "Relè termico — Taratura [A]",
        "L_TIPO" => "Linea — Tipo cavo",
        "L_FORMA" => "Linea — Formazione",
        "L_LUNG" => "Linea — Lunghezza [m]",
        "L_IZ" => "Linea — Iz [A]",
        "L_CDT" => "Linea — Caduta di tensione [%]",
        "AMP" => "Amperometro",
        "VOLT" => "Voltmetro",
        "TRASF" => "Trasformatore",
        "ACCESS" => "Accessori",
        _ => return None,
    })
}

// Repeated tags in combined electrical frames belong to separate apparatus columns.
// Keep the original attribute indices: equal tags must never merge their values.
fn groups(ins: &Insert) -> Vec<Group> {
    let mut counts = BTreeMap::<String, usize>::new();
    for a in &ins.attribs {
        *counts.entry(a.tag.to_ascii_uppercase()).or_default() += 1;
    }
    let repeats = counts.values().copied().max().unwrap_or(0);
    if repeats < 2 {
        return vec![Group { label: "Attributi del blocco".into(), indices: (0..ins.attribs.len()).collect() }];
    }
    let mut out = vec![Group { label: "Cartiglio / dati comuni".into(), indices: Vec::new() }];
    out.extend((1..=repeats).map(|n| Group { label: format!("Colonna {n}"), indices: Vec::new() }));
    let mut occurrences = BTreeMap::<String, usize>::new();
    for (index, a) in ins.attribs.iter().enumerate() {
        let tag = a.tag.to_ascii_uppercase();
        let group = if counts.get(&tag).copied().unwrap_or(0) > 1 {
            let n = occurrences.entry(tag).or_default();
            *n += 1;
            *n
        } else {
            0
        };
        if let Some(g) = out.get_mut(group) {
            g.indices.push(index);
        }
    }
    if let Some(columns) = out.get_mut(1..) {
        let position = |group: &Group| {
            group
                .indices
                .iter()
                .filter_map(|index| ins.attribs.get(*index))
                .find(|a| a.tag.eq_ignore_ascii_case("U_DEN"))
                .or_else(|| group.indices.first().and_then(|index| ins.attribs.get(*index)))
                .map_or(0.0, |a| a.text.insert.x)
        };
        columns.sort_by(|a, b| position(a).total_cmp(&position(b)));
        for (index, group) in columns.iter_mut().enumerate() {
            group.label = format!("Calata {}", index + 1);
            if let Some(a) =
                group.indices.iter().filter_map(|i| ins.attribs.get(*i)).find(|a| a.tag.eq_ignore_ascii_case("U_DEN") && !a.text.value.is_empty())
            {
                group.label.push_str(&format!(" — {}", a.text.value.chars().take(60).collect::<String>()));
            }
        }
    }
    out.retain(|g| !g.indices.is_empty());
    out
}

/// The repeated electrical column closest to the block-selection click.  Its attributes are
/// already in drawing coordinates, so the horizontal distance selects the physical calata even
/// when the whole page is one combined INSERT.
fn group_at(ins: &Insert, picked_x: Option<f64>) -> usize {
    let groups = groups(ins);
    if groups.len() < 2 {
        return 0;
    }
    let Some(x) = picked_x.filter(|x| x.is_finite()) else { return 1 };
    groups
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, group)| {
            let column_x = group
                .indices
                .iter()
                .filter_map(|attribute| ins.attribs.get(*attribute))
                .find(|attribute| attribute.tag.eq_ignore_ascii_case("U_DEN"))
                .or_else(|| group.indices.first().and_then(|attribute| ins.attribs.get(*attribute)))?
                .text
                .insert
                .x;
            column_x.is_finite().then_some((index, (column_x - x).abs()))
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map_or(1, |(index, _)| index)
}

#[derive(Clone)]
struct State {
    uid: u64,
    handle: Handle,
    original: Insert,
    values: Vec<String>,
    group: usize,
    show_hidden: bool,
}

fn state(app: &CadApp, handle: Handle) -> Option<State> {
    let doc = app.session.state().ok()?;
    let EntityKind::Insert(ins) = &app.session.doc().ok()?.entity(handle)?.kind else { return None };
    // Keep an unexpectedly large imported block from allocating an unbounded editor form.
    let bytes = ins.attribs.iter().try_fold(0_usize, |n, a| n.checked_add(a.text.value.len()));
    if ins.attribs.len() > 4096 || bytes.is_none_or(|n| n > 4 * 1024 * 1024) {
        return None;
    }
    let group = group_at(ins, app.session.editing_attributes_pick().map(|point| point.x));
    Some(State {
        uid: doc.uid,
        handle,
        original: ins.clone(),
        values: ins.attribs.iter().map(|a| a.text.value.clone()).collect(),
        // A repeated electrical block is normally opened to compile a calata.  The common
        // cartouche remains the first choice in the selector, but must not obscure this task.
        group,
        // Electrical symbols often store fields such as Ib or the breaker type as invisible
        // ATTRIBs.  They are still user data and must be fillable in the calata form.
        show_hidden: group > 0,
    })
}

fn save(app: &mut CadApp, st: &State) -> Result<(), String> {
    let current = state(app, st.handle).ok_or("Il blocco non è più disponibile")?;
    if current.uid != st.uid || current.original != st.original || app.session.editing_attributes() != Some(st.handle) {
        return Err("Il blocco è cambiato durante la modifica. Annullare e riaprire l'editor.".into());
    }
    for (a, value) in st.original.attribs.iter().zip(&st.values) {
        if !a.constant {
            app.session.input(Input::Text(value.clone())).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn editor_id() -> egui::Id {
    egui::Id::unique("cc_attribute_editor")
}

/// A front-end failure must not close the application or discard other drawings.
fn guarded(app: &mut CadApp, ctx: &egui::Context, draw: impl FnOnce(&mut CadApp, &egui::Context)) {
    if let Err(error) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw(app, ctx))) {
        let message = error.downcast_ref::<String>().map(String::as_str).or_else(|| error.downcast_ref::<&str>().copied()).unwrap_or("unknown error");
        log::error!("attribute editor interrupted: {message}\n{}", std::backtrace::Backtrace::force_capture());
        app.session.cancel();
        ctx.data_mut(|data| data.remove::<State>(editor_id()));
        app.session.echo("Editor attributi interrotto per un errore interno. Modifiche non confermate annullate; dettagli nel registro CADCraft.");
    }
}

pub fn show(app: &mut CadApp, ctx: &egui::Context) {
    guarded(app, ctx, show_form);
}

fn show_form(app: &mut CadApp, ctx: &egui::Context) {
    let id = editor_id();
    let Some(handle) = app.session.editing_attributes() else {
        ctx.data_mut(|d| d.remove::<State>(id));
        return;
    };
    let Some(current) = state(app, handle) else {
        app.session.cancel();
        app.session.echo("Impossibile aprire l'editor: blocco non disponibile o attributi troppo grandi (massimo 4096 attributi e 4 MiB di valori).");
        return;
    };
    let mut st = ctx.data_mut(|d| d.get_temp::<State>(id)).filter(|s| s.handle == handle && s.uid == current.uid).unwrap_or(current);
    let mut apply = false;
    let mut cancel = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    egui::Modal::new(id).show(ctx, |ui| {
        let groups = groups(&st.original);
        st.group = st.group.min(groups.len().saturating_sub(1));
        let indices = groups.get(st.group).map(|g| g.indices.as_slice()).unwrap_or(&[]);
        let is_calata = groups.len() > 1 && st.group > 0;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let measure = |text: &str| ui.painter().layout_no_wrap(text.chars().take(160).collect(), font.clone(), Tokens::get().text).size().x;
        let tag_width = indices
            .iter()
            .filter_map(|index| st.original.attribs.get(*index))
            .map(|a| measure(attribute_label(&a.tag).unwrap_or(&a.tag)))
            .fold(measure("Tipo di attributo"), f32::max)
            .min(170.0);
        let value_width =
            st.original.attribs.iter().map(|a| measure(a.text.value.lines().next().unwrap_or(""))).fold(200.0, f32::max).min(320.0) + 12.0;
        let width = (tag_width + value_width + 30.0).min((ctx.content_rect().width() - 40.0).max(220.0));
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 4.0;
        let tag_width = tag_width.min(width * 0.40);
        let value_width = (width - tag_width - 30.0).max(80.0);
        ui.heading(if is_calata { "Edita calata" } else { "Modifica attributi del blocco" });
        ui.label(format!("{} — {}", st.original.block, st.handle.hex()));
        if groups.len() > 1 {
            ui.horizontal(|ui| {
                ui.label("Sezione da compilare:");
                let selector = craft_ui::ComboBox::from_id_salt("attribute-group")
                    .selected_text(groups.get(st.group).map_or("", |g| g.label.as_str()))
                    .show_ui(ui, |ui| {
                        for (index, group) in groups.iter().enumerate() {
                            let response = ui.selectable_value(&mut st.group, index, &group.label);
                            crate::control::record_widget(ui, &response, &group.label, "option", Some(st.group == index));
                        }
                    });
                crate::control::record_widget(ui, &selector.response, "Sezione da compilare", "combobox", None);
                let previous = ui.add_enabled(st.group > 1, egui::Button::new("◀ Calata precedente"));
                crate::control::record_widget(ui, &previous, "Calata precedente", "button", None);
                if previous.clicked() {
                    st.group = st.group.saturating_sub(1).max(1);
                }
                let next = ui.add_enabled(st.group + 1 < groups.len(), egui::Button::new("Calata successiva ▶"));
                crate::control::record_widget(ui, &next, "Calata successiva", "button", None);
                if next.clicked() {
                    st.group = (st.group + 1).min(groups.len().saturating_sub(1));
                }
            });
            ui.small("Ogni calata è separata dalle altre e dal cartiglio. Salva applica tutte le modifiche preparate.");
        }
        let hidden = ui.checkbox(&mut st.show_hidden, "Includi i campi nascosti della calata");
        crate::control::record_widget(ui, &hidden, "Includi i campi nascosti della calata", "checkbox", Some(st.show_hidden));
        let indices = groups.get(st.group).map(|g| g.indices.as_slice()).unwrap_or(&[]);
        ui.separator();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.allocate_ui_with_layout(egui::vec2(tag_width, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.strong("Tipo di attributo");
            });
            ui.strong("Valore");
        });
        egui::ScrollArea::vertical().max_height((ctx.content_rect().height() * 0.52).clamp(100.0, 480.0)).show(ui, |ui| {
            egui::Grid::new("cc_attribute_values").num_columns(2).striped(true).spacing([12.0, 3.0]).show(ui, |ui| {
                for (index, (attribute, value)) in st.original.attribs.iter().zip(&mut st.values).enumerate() {
                    if !indices.contains(&index) || (attribute.invisible && !st.show_hidden) {
                        continue;
                    }
                    ui.allocate_ui_with_layout(egui::vec2(tag_width, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let label = attribute_label(&attribute.tag).unwrap_or(&attribute.tag);
                        ui.add(egui::Label::new(label).wrap()).on_hover_text(format!("Codice DXF: {}", attribute.tag));
                    });
                    let response = ui.add_enabled(
                        !attribute.constant,
                        egui::TextEdit::multiline(value)
                            .id(egui::Id::unique(("cc_attribute_value", handle, index)))
                            .desired_rows(1)
                            .desired_width(value_width)
                            .char_limit(65_536),
                    );
                    crate::control::record_widget(ui, &response, attribute_label(&attribute.tag).unwrap_or(&attribute.tag), "textedit", None);
                    response.on_hover_text(if attribute.constant {
                        "Attributo costante: non modificabile"
                    } else {
                        "Modifica il valore dell'attributo"
                    });
                    ui.end_row();
                }
            });
        });
        ui.separator();
        ui.label(egui::RichText::new("Applica i valori, poi salva il disegno su disco.").small().color(Tokens::get().text_dim));
        ui.horizontal(|ui| {
            let save = ui.button("Salva");
            crate::control::record_widget(ui, &save, "Salva", "button", None);
            apply = save.clicked();
            let discard = ui.button("Annulla");
            crate::control::record_widget(ui, &discard, "Annulla", "button", None);
            cancel |= discard.clicked();
        });
    });
    if cancel {
        app.session.cancel();
        ctx.data_mut(|d| d.remove::<State>(id));
    } else if apply {
        if let Err(e) = save(app, &st) {
            app.session.echo(e);
            app.session.cancel();
        }
        ctx.data_mut(|d| d.remove::<State>(id));
    } else {
        ctx.data_mut(|d| d.insert_temp(id, st));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use cadcraft_engine::Session;
    use serde_json::json;

    fn fixture() -> (CadApp, Handle) {
        let mut app = CadApp::new(Session::new(), Services::default());
        for (tag, value, y) in [("SIGLA", "Q1", 1), ("DESCRIZIONE", "Quadro", 2)] {
            app.run("attdef", json!({"tag": tag, "default": value, "at": [0, y]})).unwrap();
        }
        let handles: Vec<_> = app.session.doc().unwrap().model.iter().map(|e| e.handle.hex()).collect();
        app.run("block", json!({"name": "QUADRO", "base": [0,0], "handles": handles, "keep": "delete"})).unwrap();
        let result = app.run("insert", json!({"name": "QUADRO", "at": [0,0]})).unwrap();
        let handle = Handle::parse_hex(result["handle"].as_str().unwrap()).unwrap();
        app.start("attributi");
        app.session.input(Input::Pick(vec![handle])).unwrap();
        assert_eq!(app.session.editing_attributes(), Some(handle));
        (app, handle)
    }

    #[test]
    fn repeated_tags_are_grouped_and_saved_by_occurrence() {
        let (mut app, handle) = fixture();
        app.session.cancel();
        app.session
            .doc_mut()
            .unwrap()
            .modify_entity(handle, |e| {
                let EntityKind::Insert(i) = &mut e.kind else { panic!("insert") };
                let mut first = i.attribs.first().unwrap().clone();
                first.tag = "U_DEN".into();
                first.text.value = "Motore 1".into();
                first.text.insert.x = 10.0;
                let mut second = first.clone();
                second.text.value = "Motore 2".into();
                second.text.insert.x = 20.0;
                i.attribs.push(first);
                i.attribs.push(second);
            })
            .unwrap();
        app.session.cursor.x = 19.0;
        app.start("attributi");
        app.session.input(Input::Pick(vec![handle])).unwrap();
        let mut draft = state(&app, handle).unwrap();
        let before = draft.original.clone();
        let groups = groups(&draft.original);
        assert_eq!(groups.len(), 3);
        assert_eq!(draft.group, 2, "the calata nearest the selection click opens directly");
        assert!(draft.show_hidden, "hidden electrical fields must be fillable by default");
        assert_eq!(groups[1].label, "Calata 1 — Motore 1");
        assert_eq!(attribute_label("u_den"), Some("Utenza — Denominazione"));
        assert_eq!(attribute_label("L_CDT"), Some("Linea — Caduta di tensione [%]"));
        assert_eq!(attribute_label("CUSTOM"), None);
        assert_eq!(groups[0].indices, vec![0, 1]);
        assert_eq!(groups[1].indices, vec![2]);
        assert_eq!(groups[2].indices, vec![3]);
        draft.group = 2;
        draft.values[3] = "Solo motore 2".into();
        save(&mut app, &draft).unwrap();
        let after = state(&app, handle).unwrap();
        assert_eq!(after.values[2], "Motore 1");
        assert_eq!(after.values[3], "Solo motore 2");
        assert_eq!(after.values[0], "Q1");
        app.session.undo().unwrap();
        assert_eq!(state(&app, handle).unwrap().original, before);
    }

    #[test]
    fn form_saves_all_values_in_one_undo_and_cancel_discards_draft() {
        let (mut app, handle) = fixture();
        let mut draft = state(&app, handle).unwrap();
        let original = draft.original.clone();
        draft.values = vec!["Q2".into(), "".into()];
        // Editing the form changes no drawing data before Save.
        assert_eq!(state(&app, handle).unwrap().original, original);
        save(&mut app, &draft).unwrap();
        assert!(app.session.editing_attributes().is_none());
        let after = state(&app, handle).unwrap();
        assert_eq!(after.values, vec!["Q2", ""]);
        app.session.undo().unwrap();
        assert_eq!(state(&app, handle).unwrap().original, original);
        app.start("attedit");
        app.session.input(Input::Pick(vec![handle])).unwrap();
        app.session.cancel();
        assert_eq!(state(&app, handle).unwrap().original, original);
    }

    #[test]
    fn form_opens_and_disappears_after_cancel() {
        let (mut app, handle) = fixture();
        let ctx = egui::Context::default();
        let frame = |app: &mut CadApp| {
            let input =
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))), ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| show(app, ui.ctx()));
            output.textures_delta.clear();
            output
        };
        frame(&mut app);
        let output = frame(&mut app);
        let tag_positions: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && matches!(text.galley.text(), "SIGLA" | "DESCRIZIONE")
                {
                    return Some(text.pos);
                }
                None
            })
            .collect();
        assert_eq!(tag_positions.len(), 2);
        let (first, second) = (tag_positions.first().unwrap(), tag_positions.get(1).unwrap());
        assert!((first.x - second.x).abs() < 1.0, "attribute tags share the first column");
        assert!(second.y > first.y, "each attribute has its own row");
        let id = egui::Id::unique("cc_attribute_editor");
        assert_eq!(ctx.data_mut(|d| d.get_temp::<State>(id)).unwrap().handle, handle);
        app.session.cancel();
        frame(&mut app);
        assert!(ctx.data_mut(|d| d.get_temp::<State>(id)).is_none());
    }

    #[test]
    fn constants_are_preserved_and_invisible_values_are_editable() {
        let (mut app, handle) = fixture();
        app.session.cancel();
        app.session
            .doc_mut()
            .unwrap()
            .modify_entity(handle, |entity| {
                if let EntityKind::Insert(ins) = &mut entity.kind {
                    ins.attribs.first_mut().unwrap().constant = true;
                    ins.attribs.get_mut(1).unwrap().invisible = true;
                }
            })
            .unwrap();
        app.start("attedit");
        app.session.input(Input::Pick(vec![handle])).unwrap();
        let mut draft = state(&app, handle).unwrap();
        draft.values = vec!["ignored".into(), "Dati nascosti aggiornati".into()];
        save(&mut app, &draft).unwrap();
        assert_eq!(state(&app, handle).unwrap().values, vec!["Q1", "Dati nascosti aggiornati"]);
    }

    #[test]
    fn editor_failure_cancels_pending_values_without_losing_the_drawing() {
        let (mut app, handle) = fixture();
        let before = state(&app, handle).unwrap().original;
        let ctx = egui::Context::default();
        let draft = state(&app, handle).unwrap();
        ctx.data_mut(|data| data.insert_temp(editor_id(), draft));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            guarded(&mut app, &ctx, |app, _| {
                app.session.input(Input::Text("valore non confermato".into())).unwrap();
                panic!("simulated attribute form failure");
            });
        }));
        assert!(outcome.is_ok(), "a form panic must not escape into the app");
        assert!(!app.session.prompting());
        assert_eq!(state(&app, handle).unwrap().original, before);
        assert!(ctx.data_mut(|data| data.get_temp::<State>(editor_id())).is_none());
        assert!(app.session.log.iter().any(|line| line.contains("Editor attributi interrotto")));
    }
}
