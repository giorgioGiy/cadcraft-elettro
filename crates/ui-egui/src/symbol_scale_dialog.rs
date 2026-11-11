//! Graphical electrical-symbol scale editor.

use egui::RichText;
use serde_json::json;

use crate::CadApp;
use crate::theme::Tokens;

const COMMAND: &str = "electrical.symbolscale";

#[derive(Clone)]
struct State {
    previous: f64,
    scale: f64,
    update_existing: bool,
    error: Option<String>,
}

fn initial(app: &CadApp) -> State {
    let previous =
        app.session.doc().ok().map(|drawing| drawing.header.f64("DIMSCALE", 1.0)).filter(|scale| scale.is_finite() && *scale > 0.0).unwrap_or(1.0);
    State { previous, scale: previous, update_existing: false, error: None }
}

pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let id = egui::Id::unique("electrical-symbol-scale");
    let mut state = ctx.data_mut(|data| data.get_temp::<State>(id)).unwrap_or_else(|| initial(app));
    let mut apply = false;
    let mut cancel = false;
    egui::Window::new("Scala simboli")
        .id(id.with("window"))
        .open(open)
        .default_width(470.0)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Modifica il fattore usato dalla libreria per i simboli elettrici.");
            ui.separator();
            egui::Grid::new("electrical-symbol-scale-values").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Scala attuale:");
                ui.label(format!("{:.6}", state.previous));
                ui.end_row();
                let label = ui.label("Nuova scala simboli:");
                ui.add(egui::DragValue::new(&mut state.scale).range(1e-9..=1_000_000.0).speed(0.01).max_decimals(6)).labelled_by(label.id);
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.strong("Applicazione");
            ui.radio_value(&mut state.update_existing, false, "Applica solo ai prossimi simboli");
            ui.label(RichText::new("I blocchi già presenti non vengono modificati.").small().color(Tokens::get().text_dim));
            ui.add_space(4.0);
            ui.radio_value(&mut state.update_existing, true, "Aggiorna anche i simboli elettrici già presenti");
            ui.label(
                RichText::new("Sono aggiornati solo i blocchi sui layer elettrici riconosciuti. Architettonico, cartigli, squadrature e layer bloccati restano invariati.")
                    .small()
                    .color(Tokens::get().text_dim),
            );
            if state.update_existing {
                let factor = state.scale / state.previous;
                ui.label(format!("Fattore applicato ai simboli esistenti: {factor:.6}"));
            }
            if let Some(error) = &state.error {
                ui.colored_label(Tokens::get().warn, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                apply = ui.button("Applica").clicked();
                cancel = ui.button("Annulla").clicked();
            });
        });

    if apply {
        match app.run(COMMAND, json!({ "scale": state.scale, "updateExisting": state.update_existing })) {
            Ok(result) => {
                let updated = result["updated"].as_u64().unwrap_or(0);
                let skipped = result["skippedLocked"].as_u64().unwrap_or(0);
                app.session.echo(if state.update_existing {
                    format!("Scala simboli impostata a {}; {updated} simboli aggiornati, {skipped} su layer bloccati non modificati.", state.scale)
                } else {
                    format!("Scala simboli impostata a {} per i prossimi inserimenti.", state.scale)
                });
                if updated > 0 {
                    app.canvas.zoom_pending = true;
                }
                ctx.data_mut(|data| data.remove::<State>(id));
                *open = false;
            }
            Err(error) => state.error = Some(error),
        }
    } else if cancel || !*open {
        ctx.data_mut(|data| data.remove::<State>(id));
        *open = false;
    } else {
        ctx.data_mut(|data| data.insert_temp(id, state));
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_engine::Session;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::Services;

    fn harness(render: bool) -> Harness<'static, CadApp> {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.ui.dialog = Some("symbol_scale".into());
        let builder = Harness::builder().with_size(egui::vec2(620.0, 430.0));
        let builder = if render { builder.wgpu() } else { builder };
        let mut harness = builder.build_ui_state(
            |ui, app: &mut CadApp| {
                crate::theme::apply(ui.ctx(), egui::Theme::Dark, false);
                crate::dialogs::show(app, ui.ctx());
            },
            app,
        );
        harness.run_steps(3);
        harness
    }

    #[test]
    fn dialog_exposes_both_safe_application_modes() {
        let harness = harness(false);
        let _ = harness.get_by_label("Applica solo ai prossimi simboli");
        let _ = harness.get_by_label("Aggiorna anche i simboli elettrici già presenti");
        let _ = harness.get_by_label("Applica");
        let _ = harness.get_by_label("Annulla");
    }

    #[test]
    fn render_symbol_scale_dialog_when_requested() {
        let Some(directory) = std::env::var_os("CADCRAFT_SYMBOL_SCALE_SHOTS").map(std::path::PathBuf::from) else { return };
        std::fs::create_dir_all(&directory).expect("create screenshot directory");
        harness(true).render().expect("render symbol scale dialog").save(directory.join("symbol-scale-dialog.png")).expect("save screenshot");
    }
}
