//! Graphical setup for Eplus-compatible electrical plan sheets.

use cadcraft_doc::Handle;
use cadcraft_engine::Input;
use serde_json::json;

use crate::CadApp;

#[derive(Clone)]
struct State {
    uid: u64,
    paper: String,
    denominator: f64,
    units: String,
    landscape: bool,
    standard_profile: bool,
    sheet_marks: bool,
    fold_marks: bool,
    native_title_block: bool,
    automatic_symbol_scale: bool,
    symbol_scale: f64,
    detected_from_limits: bool,
    title_block_source: String,
    replace_existing: bool,
    error: Option<String>,
}

impl State {
    fn from_app(app: &CadApp) -> Option<Self> {
        let state = app.session.state().ok()?;
        let insunits = state.doc.header.i64("INSUNITS", 6);
        let (units, unit_mm) = match insunits {
            4 => ("mm", 1.0),
            5 => ("cm", 10.0),
            _ => ("m", 1000.0),
        };
        let symbol_scale = state.doc.header.f64("DIMSCALE", 0.1).clamp(1e-9, 1_000_000.0);
        let limits = state.doc.header.point("LIMMAX").map(|max| {
            let min = state.doc.header.point("LIMMIN").unwrap_or_default();
            ((max.x - min.x).abs(), (max.y - min.y).abs())
        });
        let detected = limits.and_then(|(width, height)| infer_sheet(width, height, unit_mm, symbol_scale * unit_mm));
        let detected_from_limits = detected.is_some();
        let (paper, landscape, denominator, automatic_symbol_scale, symbol_scale) = initial_sheet_settings(detected, symbol_scale, unit_mm);
        Some(Self {
            uid: state.uid,
            paper,
            denominator,
            units: units.into(),
            landscape,
            standard_profile: true,
            sheet_marks: true,
            fold_marks: true,
            native_title_block: true,
            automatic_symbol_scale,
            symbol_scale,
            detected_from_limits,
            title_block_source: app.ui.planimetry_title_block_source.clone(),
            replace_existing: false,
            error: None,
        })
    }

    fn unit_mm(&self) -> f64 {
        match self.units.as_str() {
            "mm" => 1.0,
            "cm" => 10.0,
            _ => 1000.0,
        }
    }

    fn paper_mm(&self) -> (f64, f64) {
        let portrait = match self.paper.as_str() {
            "A0" => (841.0, 1189.0),
            "A1" => (594.0, 841.0),
            "A2" => (420.0, 594.0),
            "A4" => (210.0, 297.0),
            _ => (297.0, 420.0),
        };
        if self.landscape { (portrait.1, portrait.0) } else { portrait }
    }

    fn computed_symbol_scale(&self) -> f64 {
        self.denominator / self.unit_mm()
    }
}

fn initial_sheet_settings(detected: Option<(String, bool, f64)>, stored_symbol_scale: f64, unit_mm: f64) -> (String, bool, f64, bool, f64) {
    if let Some((paper, landscape, denominator)) = detected {
        let automatic = relative_difference(stored_symbol_scale, denominator / unit_mm) <= 1e-6;
        (paper, landscape, denominator, automatic, stored_symbol_scale)
    } else {
        let denominator = 100.0;
        ("A3".into(), true, denominator, true, denominator / unit_mm)
    }
}

const COMMON_SCALES: &[f64] =
    &[1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 30.0, 40.0, 50.0, 75.0, 100.0, 125.0, 150.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 2500.0, 5000.0, 10_000.0];
const ISO_RECOMMENDED_SCALES: &[f64] = &[1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0];

fn relative_difference(a: f64, b: f64) -> f64 {
    (a - b).abs() / a.abs().max(b.abs()).max(1e-12)
}

/// Recognise an ISO paper and a standard drawing scale from model-space limits. Geometry is the
/// primary validation; the DIMSCALE-derived denominator is only the tie-breaker required by the
/// ISO series (for example A4 1:100 and A2 1:50 have the same model dimensions). A two-percent
/// tolerance accepts harmless imported rounding.
fn infer_sheet(width: f64, height: f64, unit_mm: f64, denominator_hint: f64) -> Option<(String, bool, f64)> {
    if !width.is_finite()
        || !height.is_finite()
        || !unit_mm.is_finite()
        || !denominator_hint.is_finite()
        || width <= 0.0
        || height <= 0.0
        || unit_mm <= 0.0
        || denominator_hint <= 0.0
    {
        return None;
    }
    let sizes = [("A0", 841.0, 1189.0), ("A1", 594.0, 841.0), ("A2", 420.0, 594.0), ("A3", 297.0, 420.0), ("A4", 210.0, 297.0)];
    let mut best: Option<(f64, f64, &str, bool, f64)> = None;
    for (name, portrait_width, portrait_height) in sizes {
        for (landscape, paper_width, paper_height) in [(false, portrait_width, portrait_height), (true, portrait_height, portrait_width)] {
            for denominator in COMMON_SCALES {
                let expected_width = paper_width * denominator / unit_mm;
                let expected_height = paper_height * denominator / unit_mm;
                let error = relative_difference(width, expected_width).max(relative_difference(height, expected_height));
                let hint_error = relative_difference(*denominator, denominator_hint);
                let score = error + hint_error;
                if best.is_none_or(|candidate| score < candidate.0) {
                    best = Some((score, error, name, landscape, *denominator));
                }
            }
        }
    }
    best.filter(|candidate| candidate.1 <= 0.02).map(|(_, _, name, landscape, denominator)| (name.into(), landscape, denominator))
}

fn number(v: f64) -> String {
    let text = format!("{v:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn is_iso_recommended_scale(value: f64) -> bool {
    ISO_RECOMMENDED_SCALES.iter().any(|candidate| relative_difference(value, *candidate) <= 1e-9)
}

pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let id = egui::Id::unique("electrical-plan-sheet");
    let Some(current) = app.session.state().ok() else {
        *open = false;
        return;
    };
    let mut state = ctx.data_mut(|data| data.get_temp::<State>(id)).filter(|state| state.uid == current.uid).or_else(|| State::from_app(app));
    let Some(mut state) = state.take() else {
        *open = false;
        return;
    };
    let has_title_block = current.doc.block("Cart_pl").is_some();
    let has_existing_sheet = current.doc.model.iter().any(|entity| {
        (entity.common.layer.eq_ignore_ascii_case("BORDO")
            && matches!(&entity.kind, cadcraft_doc::EntityKind::LwPolyline(poly) if poly.closed && poly.vertices.len() == 4))
            || matches!(&entity.kind, cadcraft_doc::EntityKind::Insert(insert) if insert.block.eq_ignore_ascii_case("Cart_pl") || insert.block.eq_ignore_ascii_case("CADCRAFT_ISO7200"))
    });
    let open_path = current.path.clone();
    let mut apply = false;
    let mut cancel = false;

    egui::Window::new("Foglio planimetrico")
        .id(id.with("window"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center() - egui::vec2(280.0, 235.0))
        .default_width(560.0)
        .show(ctx, |ui| {
            ui.label("Imposta formato, scala del disegno e scala dei simboli.");
            if state.detected_from_limits {
                ui.small(format!(
                    "Rilevato dal disegno: {} {} 1:{} in {}.",
                    state.paper,
                    if state.landscape { "orizzontale" } else { "verticale" },
                    number(state.denominator),
                    state.units
                ));
            }
            ui.separator();
            ui.group(|ui| {
                ui.strong("Impostazione del foglio");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut state.standard_profile, true, "UNI/ISO");
                    ui.radio_value(&mut state.standard_profile, false, "Compatibilità Eplus");
                });
                if state.standard_profile {
                    ui.checkbox(&mut state.sheet_marks, "Segni di rifilatura e centratura");
                    ui.add_enabled_ui(matches!(state.paper.as_str(), "A0" | "A1" | "A2"), |ui| {
                        ui.checkbox(&mut state.fold_marks, "Segni di piegatura modulare verso A4");
                    });
                    ui.small("Margine di archiviazione 20 mm a sinistra; 10 mm sugli altri lati. Il formato A4 resta verticale.");
                } else {
                    ui.small("Mantiene la doppia cornice Eplus da 5 e 10 mm.");
                }
            });
            if state.standard_profile && state.paper == "A4" {
                state.landscape = false;
            }
            ui.add_space(8.0);
            ui.columns(2, |columns| {
                columns[0].group(|ui| {
                    ui.set_min_width(245.0);
                    ui.strong("Dimensioni");
                    egui::Grid::new("plan-sheet-dimensions").num_columns(2).spacing(egui::vec2(10.0, 7.0)).show(ui, |ui| {
                        ui.label("Formato:");
                        craft_ui::ComboBox::from_id_salt("plan-paper").selected_text(&state.paper).show_ui(ui, |ui| {
                            for paper in ["A0", "A1", "A2", "A3", "A4"] {
                                ui.selectable_value(&mut state.paper, paper.into(), paper);
                            }
                        });
                        ui.end_row();
                        ui.label("Disposizione:");
                        craft_ui::ComboBox::from_id_salt("plan-orientation")
                            .selected_text(if state.landscape { "Orizzontale" } else { "Verticale" })
                            .show_ui(ui, |ui| {
                                ui.add_enabled_ui(!(state.standard_profile && state.paper == "A4"), |ui| {
                                    ui.selectable_value(&mut state.landscape, true, "Orizzontale");
                                });
                                ui.selectable_value(&mut state.landscape, false, "Verticale");
                            });
                        ui.end_row();
                    });
                    ui.add_space(8.0);
                    ui.strong("Unità di misura");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut state.units, "m".into(), "Metri");
                        ui.radio_value(&mut state.units, "cm".into(), "Centimetri");
                        ui.radio_value(&mut state.units, "mm".into(), "Millimetri");
                    });
                });
                columns[1].group(|ui| {
                    ui.set_min_width(245.0);
                    ui.strong("Scala");
                    egui::Grid::new("plan-sheet-scale").num_columns(3).spacing(egui::vec2(8.0, 7.0)).show(ui, |ui| {
                        ui.label("Scala del disegno:");
                        ui.label("1 :");
                        ui.add(egui::DragValue::new(&mut state.denominator).range(1.0..=1_000_000.0).speed(1.0));
                        ui.end_row();
                        ui.label("Fattore scala simboli:");
                        ui.label("");
                        ui.add_enabled(
                            !state.automatic_symbol_scale,
                            egui::DragValue::new(&mut state.symbol_scale).range(1e-9..=1_000_000.0).speed(0.01),
                        );
                        ui.end_row();
                    });
                    ui.checkbox(&mut state.automatic_symbol_scale, "Calcola automaticamente");
                    if state.automatic_symbol_scale {
                        state.symbol_scale = state.computed_symbol_scale();
                    }
                    ui.add_space(8.0);
                    ui.strong("Scala di plottaggio");
                    ui.label(format!("Millimetri = unità di disegno  {:.4}:1", state.unit_mm() / state.denominator));
                    if state.standard_profile && !is_iso_recommended_scale(state.denominator) {
                        ui.colored_label(crate::theme::Tokens::get().warn, "Scala personalizzata: non appartiene alla serie ISO 5455 raccomandata.");
                    }
                });
            });
            ui.add_space(8.0);
            let (paper_w, paper_h) = state.paper_mm();
            let unit_mm = state.unit_mm();
            let real_w = paper_w * state.denominator / unit_mm;
            let real_h = paper_h * state.denominator / unit_mm;
            ui.group(|ui| {
                ui.strong("Risultato");
                ui.label(format!(
                    "{} {} × {} {}  — carta {} × {} mm",
                    number(real_w),
                    state.units,
                    number(real_h),
                    state.units,
                    number(paper_w),
                    number(paper_h)
                ));
                ui.small("La libreria simboli userà automaticamente il fattore indicato sopra.");
            });
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.strong("Cartiglio");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut state.native_title_block, true, "CADCraft ISO 7200");
                    ui.radio_value(&mut state.native_title_block, false, "Cart_pl Eplus");
                });
                if state.native_title_block {
                    ui.label("Cartiglio originale incluso: non richiede un disegno sorgente.");
                    ui.small("I campi obbligatori vengono verificati automaticamente prima dell'inserimento.");
                } else {
                    if has_title_block {
                        ui.label("Il blocco Cart_pl è già disponibile nel disegno aperto.");
                    } else if state.title_block_source.is_empty() {
                        ui.label("Scegli un disegno che contenga il blocco Cart_pl. La scelta verrà ricordata.");
                    } else {
                        let name = std::path::Path::new(&state.title_block_source)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or(&state.title_block_source);
                        ui.label(format!("Sorgente memorizzata: {name}")).on_hover_text(&state.title_block_source);
                    }
                    ui.horizontal(|ui| {
                        let use_open = ui.add_enabled(open_path.is_some(), egui::Button::new("Usa il disegno aperto"));
                        if use_open.clicked()
                            && let Some(path) = &open_path
                        {
                            state.title_block_source = path.clone();
                            app.ui.planimetry_title_block_source = path.clone();
                            state.error = None;
                        }
                        if ui.button("Scegli altro disegno...").clicked()
                            && let Some(path) = app.services.pick_open.as_ref().and_then(|picker| picker())
                        {
                            state.title_block_source = path.clone();
                            app.ui.planimetry_title_block_source = path;
                            state.error = None;
                        }
                    });
                }
            });
            if has_existing_sheet {
                ui.add_space(4.0);
                ui.checkbox(&mut state.replace_existing, "Sostituisci la squadratura e il cartiglio esistenti");
                if !state.replace_existing {
                    ui.small("Questo disegno contiene già un foglio. Puoi memorizzarlo come sorgente e poi premere Annulla.");
                }
            }
            if let Some(error) = &state.error {
                ui.colored_label(crate::theme::Tokens::get().warn, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let source_available = state.native_title_block || has_title_block || !state.title_block_source.is_empty();
                apply =
                    ui.add_enabled(source_available && (!has_existing_sheet || state.replace_existing), egui::Button::new("Crea foglio")).clicked();
                cancel = ui.button("Annulla").clicked();
            });
        });

    if apply {
        let result = app.run(
            "electrical.sheet.create",
            json!({
                "paper": state.paper,
                "scaleDenominator": state.denominator,
                "units": state.units,
                "landscape": state.landscape,
                "symbolScale": state.symbol_scale,
                "standardProfile": state.standard_profile,
                "sheetMarks": state.sheet_marks,
                "foldMarks": state.fold_marks,
                "nativeTitleBlock": state.native_title_block,
                "titleBlockFile": state.title_block_source,
                "replaceExisting": state.replace_existing,
            }),
        );
        match result {
            Ok(value) => {
                app.canvas.zoom_pending = true;
                if let Some(handle) = value["titleBlockHandle"].as_str().and_then(Handle::parse_hex) {
                    app.start("attedit");
                    let _ = app.session.input(Input::Pick(vec![handle]));
                }
                if value["titleBlockValidation"]["valid"] == false {
                    let missing = value["titleBlockValidation"]["missingFields"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|field| field.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    app.session.echo(format!("Avviso ISO 7200: nel cartiglio mancano i campi {missing}."));
                }
                app.session.echo(format!(
                    "Foglio planimetrico {} 1:{}; dimensione reale {} × {} {}.",
                    value["paper"].as_str().unwrap_or(""),
                    number(state.denominator),
                    number(value["drawingWidth"].as_f64().unwrap_or(0.0)),
                    number(value["drawingHeight"].as_f64().unwrap_or(0.0)),
                    value["units"].as_str().unwrap_or("")
                ));
                ctx.data_mut(|data| data.remove::<State>(id));
                *open = false;
                return;
            }
            Err(error) => state.error = Some(error),
        }
    }
    if cancel || !*open {
        ctx.data_mut(|data| data.remove::<State>(id));
        *open = false;
    } else {
        ctx.data_mut(|data| data.insert_temp(id, state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_reference_a3_sheets_without_trusting_dimscale() {
        assert_eq!(infer_sheet(42.0, 29.7, 1000.0, 100.0), Some(("A3".into(), true, 100.0)));
        assert_eq!(infer_sheet(420.0, 297.0, 1.0, 1.0), Some(("A3".into(), true, 1.0)));
        assert_eq!(infer_sheet(420.0, 297.0, 1000.0, 1000.0), Some(("A3".into(), true, 1000.0)));
    }

    #[test]
    fn recognises_portrait_and_rejects_non_sheet_limits() {
        assert_eq!(infer_sheet(21.0, 29.7, 1000.0, 100.0), Some(("A4".into(), false, 100.0)));
        assert_eq!(infer_sheet(123.0, 17.0, 1000.0, 100.0), None);
        assert_eq!(infer_sheet(f64::NAN, 29.7, 1000.0, 100.0), None);
    }

    #[test]
    fn distinguishes_iso_recommended_and_custom_scales() {
        for scale in ISO_RECOMMENDED_SCALES {
            assert!(is_iso_recommended_scale(*scale));
        }
        for custom in [25.0, 30.0, 40.0, 75.0, 125.0, 150.0, 250.0, 2500.0] {
            assert!(!is_iso_recommended_scale(custom));
        }
    }

    #[test]
    fn unrecognised_drawings_start_at_one_to_one_hundred_with_automatic_symbol_scale() {
        assert_eq!(initial_sheet_settings(None, 7.5, 1000.0), ("A3".into(), true, 100.0, true, 0.1));
        assert_eq!(initial_sheet_settings(None, 7.5, 10.0), ("A3".into(), true, 100.0, true, 10.0));
        assert_eq!(initial_sheet_settings(None, 7.5, 1.0), ("A3".into(), true, 100.0, true, 100.0));
    }

    #[test]
    fn recognised_sheets_keep_their_scale_and_manual_symbol_factor() {
        assert_eq!(initial_sheet_settings(Some(("A2".into(), false, 50.0)), 2.5, 10.0), ("A2".into(), false, 50.0, false, 2.5));
        assert_eq!(initial_sheet_settings(Some(("A3".into(), true, 100.0)), 0.1, 1000.0), ("A3".into(), true, 100.0, true, 0.1));
    }
}
