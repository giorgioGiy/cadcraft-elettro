//! Graphical electrical-system visibility presets over standard drawing layers.

use serde_json::{Value, json};

use crate::CadApp;
use crate::theme::Tokens;

const ANALYZE: &str = "electrical.layers.analyze";
const APPLY: &str = "electrical.layers.apply";
const ALL_GROUPS: &[&str] =
    &["common", "architecture", "power", "lighting", "emergency", "fire", "photovoltaic", "data", "intrusion", "earth", "distribution"];

#[derive(Clone)]
struct GroupRow {
    id: String,
    label: String,
    count: usize,
    selected: bool,
    layers: Vec<String>,
}

#[derive(Clone)]
struct State {
    groups: Vec<GroupRow>,
    floors: Vec<String>,
    floor: String,
    keep_unmatched: bool,
    state_name: String,
    error: Option<String>,
}

fn strings(value: Option<&Value>, max: usize) -> Vec<String> {
    value.and_then(Value::as_array).into_iter().flatten().take(max).filter_map(Value::as_str).map(str::to_owned).collect()
}

fn load(app: &mut CadApp) -> Result<State, String> {
    let value = app.run(ANALYZE, json!({}))?;
    let groups = value["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .take(64)
        .filter_map(|group| {
            Some(GroupRow {
                id: group["id"].as_str()?.to_owned(),
                label: group["label"].as_str()?.to_owned(),
                count: group["count"].as_u64().and_then(|count| usize::try_from(count).ok()).unwrap_or(0),
                selected: group["anyVisible"].as_bool().unwrap_or(false),
                layers: strings(group.get("layers"), 256),
            })
        })
        .collect::<Vec<_>>();
    Ok(State { groups, floors: strings(value.get("floors"), 32), floor: "all".into(), keep_unmatched: true, state_name: String::new(), error: None })
}

fn select(state: &mut State, ids: &[&str]) {
    for group in &mut state.groups {
        group.selected = ids.contains(&group.id.as_str());
    }
}

fn selected(state: &State) -> Vec<&str> {
    state.groups.iter().filter(|group| group.selected).map(|group| group.id.as_str()).collect()
}

fn apply(app: &mut CadApp, state: &mut State) -> bool {
    let params = json!({
        "visible": selected(state),
        "floor": state.floor,
        "keepUnmatched": state.keep_unmatched,
    });
    match app.run(APPLY, params) {
        Ok(result) => {
            let changed = result["changed"].as_u64().unwrap_or(0);
            app.session.echo(format!("Visibilità impianti applicata: {changed} layer modificati."));
            state.error = None;
            true
        }
        Err(error) => {
            state.error = Some(error);
            false
        }
    }
}

fn preset_button(ui: &mut egui::Ui, state: &mut State, label: &str, ids: &[&str]) {
    if ui.button(label).clicked() {
        select(state, ids);
        state.state_name = label.to_owned();
    }
}

fn saved_states(app: &CadApp) -> Vec<String> {
    app.session.doc().ok().map(|drawing| drawing.layer_states.iter().take(100).map(|state| state.name.clone()).collect()).unwrap_or_default()
}

pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let id = egui::Id::unique("electrical_layer_presets");
    let mut state = match ctx.data_mut(|data| data.get_temp::<State>(id)) {
        Some(state) => state,
        None => match load(app) {
            Ok(state) => state,
            Err(error) => State {
                groups: Vec::new(),
                floors: Vec::new(),
                floor: "all".into(),
                keep_unmatched: true,
                state_name: String::new(),
                error: Some(error),
            },
        },
    };
    let saved = saved_states(app);
    let mut close = false;
    let mut reload = false;
    egui::Window::new("Visibilità impianti")
        .id(id.with("window"))
        .open(open)
        .default_width(620.0)
        .resizable(true)
        .show(ctx, |ui| {
            ui.label("Accendi e spegni gruppi di layer senza modificare la geometria del disegno.");
            ui.horizontal(|ui| {
                ui.label("Piano:");
                craft_ui::ComboBox::from_id_salt("electrical_floor")
                    .selected_text(if state.floor == "all" { "Tutti / file del singolo piano" } else { &state.floor })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut state.floor, "all".into(), "Tutti / file del singolo piano");
                        for floor in &state.floors {
                            ui.selectable_value(&mut state.floor, floor.clone(), floor);
                        }
                    });
            });
            ui.separator();
            ui.label(egui::RichText::new("Configurazioni rapide").strong());
            ui.horizontal_wrapped(|ui| {
                preset_button(ui, &mut state, "Impianto elettrico", &["common", "architecture", "power", "lighting", "earth", "distribution"]);
                preset_button(ui, &mut state, "Emergenza", &["common", "architecture", "emergency", "distribution"]);
                preset_button(ui, &mut state, "Rivelazione incendio", &["common", "architecture", "fire", "distribution"]);
                preset_button(ui, &mut state, "Fotovoltaico", &["common", "architecture", "photovoltaic", "earth", "distribution"]);
                preset_button(ui, &mut state, "Rete dati", &["common", "architecture", "data", "distribution"]);
                preset_button(ui, &mut state, "Tutti", ALL_GROUPS);
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(330.0).show(ui, |ui| {
                egui::Grid::new("electrical_layer_groups")
                    .num_columns(4)
                    .striped(true)
                    .spacing([12.0, 5.0])
                    .show(ui, |ui| {
                        ui.strong("Visibile");
                        ui.strong("Impianto / gruppo");
                        ui.strong("Layer");
                        ui.strong("");
                        ui.end_row();
                        for index in 0..state.groups.len() {
                            let group = &mut state.groups[index];
                            ui.checkbox(&mut group.selected, "");
                            ui.label(&group.label).on_hover_text(if group.layers.is_empty() {
                                "Nessun layer riconosciuto in questo disegno".into()
                            } else {
                                group.layers.join("\n")
                            });
                            ui.label(group.count.to_string());
                            if ui.add_enabled(group.count > 0, egui::Button::new("Solo")).clicked() {
                                let target = group.id.clone();
                                let label = group.label.clone();
                                for other in &mut state.groups {
                                    other.selected = matches!(other.id.as_str(), "common" | "architecture") || other.id == target;
                                }
                                state.state_name = label;
                            }
                            ui.end_row();
                        }
                    });
            });
            ui.checkbox(&mut state.keep_unmatched, "Mantieni invariati i layer non riconosciuti");
            ui.small("Squadratura e cartiglio sono separati dagli impianti. I riferimenti architettonici con prefisso ARCH/BASE vengono riconosciuti come planimetria.");
            ui.separator();
            ui.label(egui::RichText::new("Configurazioni salvate nel disegno").strong());
            ui.horizontal(|ui| {
                ui.label("Nome:");
                ui.add(egui::TextEdit::singleline(&mut state.state_name).desired_width(220.0));
                if ui.add_enabled(!state.state_name.trim().is_empty(), egui::Button::new("Applica e salva")).clicked()
                    && apply(app, &mut state)
                {
                    let name = state.state_name.trim().to_owned();
                    match app.run("layerstate.save", json!({"name":name})) {
                        Ok(_) => app.session.echo(format!("Configurazione layer `{name}` salvata.")),
                        Err(error) => state.error = Some(error),
                    }
                }
            });
            if !saved.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for name in &saved {
                        if ui.button(name).on_hover_text("Ripristina questa configurazione").clicked() {
                            match app.run("layerstate.restore", json!({"name":name})) {
                                Ok(_) => reload = true,
                                Err(error) => state.error = Some(error),
                            }
                        }
                    }
                });
            }
            if let Some(error) = &state.error {
                ui.colored_label(Tokens::get().warn, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Applica").clicked() {
                    let _ = apply(app, &mut state);
                }
                if ui.button("Aggiorna elenco").clicked() {
                    reload = true;
                }
                if ui.button("Chiudi").clicked() {
                    close = true;
                }
            });
        });
    if reload {
        match load(app) {
            Ok(new_state) => state = new_state,
            Err(error) => state.error = Some(error),
        }
    }
    if close || !*open {
        ctx.data_mut(|data| data.remove::<State>(id));
        *open = false;
    } else {
        ctx.data_mut(|data| data.insert_temp(id, state));
    }
}
