//! Graphical setup for the electrical symbol legend; the engine command still owns every edit.

use cadcraft_engine::Input;

use crate::CadApp;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Scope {
    #[default]
    CurrentLayer,
    NamedLayer,
    AllLayers,
    Selection,
}

impl Scope {
    fn key(self) -> &'static str {
        match self {
            Scope::CurrentLayer => "current",
            Scope::NamedLayer => "layer",
            Scope::AllLayers => "all",
            Scope::Selection => "selection",
        }
    }

    fn parse(value: &str) -> Scope {
        match value {
            "layer" => Scope::NamedLayer,
            "all" => Scope::AllLayers,
            "selection" => Scope::Selection,
            _ => Scope::CurrentLayer,
        }
    }
}

#[derive(Clone)]
struct State {
    uid: u64,
    scope: Scope,
    layer: String,
    quantity: bool,
    scale: f64,
    error: Option<String>,
}

fn dialog_id() -> egui::Id {
    egui::Id::unique("cc_symbol_legend_dialog")
}

fn pending_id() -> egui::Id {
    egui::Id::unique("cc_symbol_legend_pending")
}

impl State {
    fn new(app: &CadApp) -> Result<Self, String> {
        let state = app.session.state().map_err(|error| error.to_string())?;
        let current = state.drawing().header.str("CLAYER", "0");
        let layer = if app.ui.symbol_legend_layer.is_empty() { current } else { app.ui.symbol_legend_layer.clone() };
        Ok(Self {
            uid: state.uid,
            scope: Scope::parse(&app.ui.symbol_legend_scope),
            layer,
            quantity: app.ui.symbol_legend_quantity,
            scale: app.ui.symbol_legend_scale.clamp(0.1, 10.0),
            error: None,
        })
    }
}

fn begin(app: &mut CadApp, state: &State) -> Result<(), String> {
    app.session.cancel();
    crate::symbol_library::sync_descriptions(app);
    app.session.start("symbollegend").map_err(|error| error.to_string())?;
    app.session.input(Input::Text(state.scale.to_string())).map_err(|error| error.to_string())?;
    app.session.input(Input::Keyword(if state.quantity { "Yes" } else { "No" }.into())).map_err(|error| error.to_string())?;
    match state.scope {
        Scope::CurrentLayer => {
            let layer = app.session.state().map_err(|error| error.to_string())?.drawing().header.str("CLAYER", "0");
            app.session.input(Input::Keyword("Layer".into())).map_err(|error| error.to_string())?;
            app.session.input(Input::Text(layer)).map_err(|error| error.to_string())?;
        }
        Scope::NamedLayer => {
            app.session.input(Input::Keyword("Layer".into())).map_err(|error| error.to_string())?;
            app.session.input(Input::Text(state.layer.clone())).map_err(|error| error.to_string())?;
        }
        Scope::AllLayers => {
            app.session.input(Input::Keyword("All".into())).map_err(|error| error.to_string())?;
        }
        Scope::Selection => {}
    }
    Ok(())
}

/// Start a legend queued by the setup window.  Dialogs are painted after the canvas, so doing
/// this on the following dialog pass guarantees that the pointer release which clicked
/// `Continua` has already gone through a frame in which no point command was active.  Otherwise
/// that release can occasionally pass through the closing window and place the table at once.
pub fn start_pending(app: &mut CadApp, ctx: &egui::Context) {
    let Some(mut state) = ctx.data_mut(|data| {
        let state = data.get_temp::<State>(pending_id());
        data.remove::<State>(pending_id());
        state
    }) else {
        return;
    };
    if let Err(error) = begin(app, &state) {
        state.error = Some(error);
        app.ui.dialog = Some("symbollegend".into());
        ctx.data_mut(|data| data.insert_temp(dialog_id(), state));
    }
}

pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let id = dialog_id();
    let Ok(doc) = app.session.state() else {
        *open = false;
        return;
    };
    let mut state = match ctx.data_mut(|data| data.get_temp::<State>(id)).filter(|state| state.uid == doc.uid) {
        Some(state) => state,
        None => match State::new(app) {
            Ok(state) => state,
            Err(error) => {
                app.session.echo(error);
                *open = false;
                return;
            }
        },
    };
    let current_layer = doc.drawing().header.str("CLAYER", "0");
    let layers: Vec<String> = doc.drawing().layers.iter().map(|layer| layer.name.clone()).collect();
    let mut create = false;
    let mut close = false;
    egui::Window::new("Legenda segni grafici")
        .id(id.with("window"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center() - egui::vec2(220.0, 170.0))
        .default_width(440.0)
        .show(ctx, |ui| {
            ui.heading("Simboli da includere");
            ui.radio_value(&mut state.scope, Scope::CurrentLayer, format!("Layer corrente: {current_layer}"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut state.scope, Scope::NamedLayer, "Scegli layer:");
                ui.add_enabled_ui(state.scope == Scope::NamedLayer, |ui| {
                    craft_ui::ComboBox::from_id_salt("legend-layer")
                        .selected_text(if state.layer.is_empty() { "Seleziona..." } else { &state.layer })
                        .width(230.0)
                        .show_ui(ui, |ui| {
                            for layer in &layers {
                                ui.selectable_value(&mut state.layer, layer.clone(), layer);
                            }
                        });
                });
            });
            ui.radio_value(&mut state.scope, Scope::AllLayers, "Tutti i layer");
            ui.radio_value(&mut state.scope, Scope::Selection, "Selezione grafica a finestra");
            ui.separator();
            ui.checkbox(&mut state.quantity, "Mostra colonna quantità");
            ui.horizontal(|ui| {
                ui.label("Scala:");
                ui.add(egui::DragValue::new(&mut state.scale).range(0.1..=10.0).speed(0.1).fixed_decimals(2));
            });
            ui.label("Il testo e le colonne vengono dimensionati secondo TEXTSIZE del disegno.");
            if state.scope == Scope::Selection {
                ui.label("Dopo Continua, traccia la finestra di selezione sul disegno.");
            } else {
                ui.label("Dopo Continua, indica sul disegno il punto della legenda.");
            }
            if let Some(error) = &state.error {
                ui.colored_label(crate::theme::Tokens::get().warn, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let valid = state.scope != Scope::NamedLayer || layers.iter().any(|layer| layer.eq_ignore_ascii_case(state.layer.trim()));
                if ui.add_enabled(valid, egui::Button::new("Continua")).clicked() {
                    create = true;
                }
                if ui.button("Annulla").clicked() {
                    close = true;
                }
            });
        });
    state.scale = if state.scale.is_finite() { state.scale.clamp(0.1, 10.0) } else { 1.0 };
    app.ui.symbol_legend_scope = state.scope.key().into();
    app.ui.symbol_legend_layer = state.layer.clone();
    app.ui.symbol_legend_quantity = state.quantity;
    app.ui.symbol_legend_scale = state.scale;
    if create {
        // Queue instead of starting the point command while the Continue click is still alive.
        // `start_pending` runs after the canvas on the next frame.
        ctx.data_mut(|data| data.insert_temp(pending_id(), state.clone()));
        ctx.request_repaint();
        close = true;
    }
    if close {
        *open = false;
    }
    if *open {
        ctx.data_mut(|data| data.insert_temp(id, state));
    } else {
        ctx.data_mut(|data| data.remove::<State>(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use cadcraft_engine::Session;

    #[test]
    fn toolbar_command_opens_graphical_setup_without_starting_the_engine_command() {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.start("legenda");
        assert_eq!(app.ui.dialog.as_deref(), Some("symbollegend"));
        assert!(!app.session.prompting());
    }

    #[test]
    fn graphical_choices_are_persisted_in_preferences() {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.ui.symbol_legend_scope = "all".into();
        app.ui.symbol_legend_layer = "ILLUMINAZIONE".into();
        app.ui.symbol_legend_quantity = false;
        app.ui.symbol_legend_scale = 0.75;
        let saved = app.prefs_json();
        let mut restored = CadApp::new(Session::new(), Services::default());
        restored.load_prefs(&saved);
        assert_eq!(restored.ui.symbol_legend_scope, "all");
        assert_eq!(restored.ui.symbol_legend_layer, "ILLUMINAZIONE");
        assert!(!restored.ui.symbol_legend_quantity);
        assert_eq!(restored.ui.symbol_legend_scale, 0.75);
    }

    #[test]
    fn continue_click_is_finished_before_the_point_command_starts() {
        let mut app = CadApp::new(Session::new(), Services::default());
        let ctx = egui::Context::default();
        let state = State {
            uid: app.session.state().map(|state| state.uid).unwrap_or_default(),
            scope: Scope::CurrentLayer,
            layer: String::new(),
            quantity: true,
            scale: 1.0,
            error: None,
        };
        ctx.data_mut(|data| data.insert_temp(pending_id(), state));

        // The frame containing the Continue click has no active point prompt.
        assert!(!app.session.prompting());
        start_pending(&mut app, &ctx);
        assert!(app.session.prompting());
        assert_eq!(app.session.current_prompt().map(|prompt| prompt.message), Some("Specify legend insertion point".into()));
    }
}
