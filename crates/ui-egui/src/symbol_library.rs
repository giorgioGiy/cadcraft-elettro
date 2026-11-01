//! Local symbol libraries: a ZIP containing one DXF/DWG per symbol, with previews and INSERT.
//!
//! Library files stay outside the repository and outside the drawing until the user inserts a
//! symbol. The ZIP is treated as hostile input: entry counts and uncompressed sizes are capped,
//! paths are never extracted, and every error is reported in the window instead of panicking.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cadcraft_color::Rgb;
use cadcraft_doc::EntityKind;
use cadcraft_engine::Input;
use cadcraft_geom::Vec2;
use cadcraft_render::DisplayList;
use egui::{RichText, Sense, Stroke, vec2};
use serde_json::{Value, json};
use zip::ZipArchive;

use crate::CadApp;
use crate::canvas::Xf;
use crate::theme::Tokens;

const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_SYMBOL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PREVIEWS: usize = 128;
static TEMP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct Symbol {
    code: String,
    entry: String,
    index: usize,
    bytes: u64,
}

#[derive(Clone)]
enum Preview {
    Ready(Arc<DisplayList>),
    Failed(String),
}

#[derive(Default)]
pub struct State {
    source: Option<PathBuf>,
    symbols: Vec<Symbol>,
    search: String,
    selected: Option<usize>,
    previews: HashMap<String, Preview>,
    error: Option<String>,
    /// The scale preset sent to INSERT before the insertion point. The first local plan-symbol
    /// library uses 5; the user can change it for other libraries or drawings.
    scale: f64,
}

impl State {
    pub fn source_path(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    pub fn load(&mut self, path: impl AsRef<Path>) {
        match index_archive(path.as_ref()) {
            Ok(symbols) => {
                self.source = Some(path.as_ref().to_path_buf());
                self.symbols = symbols;
                self.selected = (!self.symbols.is_empty()).then_some(0);
                self.previews.clear();
                self.error = None;
                if !self.scale.is_finite() || self.scale.abs() < 1e-12 {
                    self.scale = 5.0;
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn preview(&mut self, symbol: &Symbol) -> Preview {
        if let Some(preview) = self.previews.get(&symbol.entry) {
            return preview.clone();
        }
        if self.previews.len() >= MAX_PREVIEWS
            && let Some(key) = self.previews.keys().next().cloned()
        {
            self.previews.remove(&key);
        }
        let preview = match self.source.as_deref().ok_or_else(|| "No symbol library selected.".to_string()).and_then(|source| {
            let bytes = read_entry(source, symbol)?;
            let drawing = cadcraft_io::read(&bytes, &symbol.entry).map_err(|error| format!("{}: {error}", symbol.code))?;
            let visible = || drawing.model.iter().map(|entity| entity.as_ref()).filter(|entity| !matches!(&entity.kind, EntityKind::AttDef(_)));
            let rough = cadcraft_render::build_entities(&drawing, visible(), &cadcraft_render::Options::default());
            let size = rough.bounds.width().max(rough.bounds.height());
            let options = cadcraft_render::Options { tolerance: (size / 240.0).max(1e-9), ..Default::default() };
            let list = if rough.bounds.is_empty() { rough } else { cadcraft_render::build_entities(&drawing, visible(), &options) };
            Ok(Arc::new(list))
        }) {
            Ok(list) => Preview::Ready(list),
            Err(error) => Preview::Failed(error),
        };
        self.previews.insert(symbol.entry.clone(), preview.clone());
        preview
    }
}

fn symbol_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dwg") || extension.eq_ignore_ascii_case("dxf"))
}

fn symbol_code(name: &str) -> Option<String> {
    Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty() && !stem.starts_with('.'))
        .map(str::to_string)
}

fn index_archive(path: &Path) -> Result<Vec<Symbol>, String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|error| format!("{} is not a readable ZIP archive: {error}", path.display()))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!("The archive contains too many entries (maximum {MAX_ARCHIVE_ENTRIES})."));
    }
    let mut symbols = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| format!("Cannot read ZIP entry {index}: {error}"))?;
        let name = entry.name().map_err(|error| format!("Cannot read ZIP entry name {index}: {error}"))?.into_owned();
        if entry.is_dir() || !symbol_extension(&name) {
            continue;
        }
        if entry.size() > MAX_SYMBOL_BYTES {
            continue;
        }
        let Some(code) = symbol_code(&name) else { continue };
        symbols.push(Symbol { code, entry: name, index, bytes: entry.size() });
    }
    symbols.sort_by_key(|symbol| symbol.code.to_ascii_lowercase());
    symbols.dedup_by(|left, right| left.code.eq_ignore_ascii_case(&right.code));
    if symbols.is_empty() {
        return Err("The archive contains no DXF or DWG symbols.".into());
    }
    Ok(symbols)
}

fn read_entry(path: &Path, symbol: &Symbol) -> Result<Vec<u8>, String> {
    if symbol.bytes > MAX_SYMBOL_BYTES {
        return Err(format!("{} is larger than the symbol limit.", symbol.code));
    }
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut entry = archive.by_index(symbol.index).map_err(|error| format!("{}: {error}", symbol.entry))?;
    let name = entry.name().map_err(|error| format!("{}: {error}", symbol.entry))?;
    if name.as_ref() != symbol.entry || entry.size() > MAX_SYMBOL_BYTES {
        return Err(format!("The archive changed while reading {}.", symbol.code));
    }
    let capacity = usize::try_from(entry.size()).map_err(|_| format!("{} is too large.", symbol.code))?;
    let mut bytes = Vec::with_capacity(capacity);
    entry.by_ref().take(MAX_SYMBOL_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|error| format!("{}: {error}", symbol.code))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SYMBOL_BYTES {
        return Err(format!("{} expands beyond the symbol limit.", symbol.code));
    }
    Ok(bytes)
}

fn draw_preview(ui: &mut egui::Ui, preview: &Preview) -> egui::Response {
    let tokens = Tokens::get();
    let (rect, response) = ui.allocate_exact_size(vec2(92.0, 64.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, tokens.canvas);
    painter.rect_stroke(rect, 3.0, Stroke::new(1.0, tokens.border), egui::StrokeKind::Inside);
    match preview {
        Preview::Ready(list) if !list.bounds.is_empty() && !list.prims.is_empty() => {
            let bounds = list.bounds;
            let inner = rect.shrink(6.0);
            let scale = (f64::from(inner.width()) / bounds.width().max(1e-9)).min(f64::from(inner.height()) / bounds.height().max(1e-9));
            let transform = Xf { rect, center: Vec2::new((bounds.min.x + bounds.max.x) / 2.0, (bounds.min.y + bounds.max.y) / 2.0), scale };
            crate::canvas::draw_list(&painter, &transform, list, Rgb(tokens.canvas.r(), tokens.canvas.g(), tokens.canvas.b()), false);
        }
        Preview::Ready(_) => {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "—", crate::theme::small(), tokens.text_dim);
        }
        Preview::Failed(_) => {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "!", crate::theme::small(), tokens.warn);
        }
    }
    response
}

fn temp_symbol_path(symbol: &Symbol) -> Result<(PathBuf, PathBuf), String> {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let folder = std::env::temp_dir().join(format!("cadcraft-symbol-{}-{id}", std::process::id()));
    std::fs::create_dir(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let extension = Path::new(&symbol.entry).extension().and_then(|value| value.to_str()).unwrap_or("dwg");
    // ZIP entry names never become paths here. `code` came from a file stem; replace the few
    // characters Windows doesn't accept so a hostile archive cannot escape the temp folder.
    let safe: String = symbol.code.chars().map(|ch| if "<>:\"/\\|?*".contains(ch) { '_' } else { ch }).collect();
    let name = if safe.trim().is_empty() { "symbol" } else { safe.trim() };
    Ok((folder.clone(), folder.join(format!("{name}.{extension}"))))
}

fn insert(app: &mut CadApp, symbol: &Symbol, scale: f64) -> Result<(), String> {
    if app.session.doc().is_ok_and(|drawing| drawing.block(&symbol.code).is_some()) {
        app.start("insert");
        app.session.input(Input::Text(symbol.code.clone())).map_err(|error| error.to_string())?;
    } else {
        let source = app.symbol_library.source.as_deref().ok_or_else(|| "No symbol library selected.".to_string())?;
        let bytes = read_entry(source, symbol)?;
        let (folder, path) = temp_symbol_path(symbol)?;
        let result: Result<(), String> = (|| {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            file.write_all(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
            app.start("insert");
            app.session.input(Input::Text(path.to_string_lossy().into_owned())).map_err(|error| error.to_string())?;
            Ok(())
        })();
        if let Err(error) = std::fs::remove_file(&path) {
            log::warn!("symbol library temp file {}: {error}", path.display());
        }
        if let Err(error) = std::fs::remove_dir(&folder) {
            log::warn!("symbol library temp folder {}: {error}", folder.display());
        }
        result?;
    }
    if scale.is_finite() && scale.abs() >= 1e-12 && (scale - 1.0).abs() >= 1e-12 {
        app.session.input(Input::Keyword("Scale".into())).map_err(|error| error.to_string())?;
        app.session.input(Input::Text(scale.to_string())).map_err(|error| error.to_string())?;
    }
    app.session.input(Input::Keyword("Rotate".into())).map_err(|error| error.to_string())?;
    app.session.input(Input::Text("0".into())).map_err(|error| error.to_string())?;
    Ok(())
}

pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let mut picked = None;
    let mut insert_symbol = None;
    egui::Window::new(crate::tl!("Symbol Library"))
        .id(egui::Id::unique("Symbol Library"))
        .open(open)
        .default_size(vec2(760.0, 620.0))
        .min_size(vec2(520.0, 400.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button(crate::tl!("Add library…")).clicked() {
                    picked = app.services.pick_symbol_library.as_ref().and_then(|picker| picker());
                }
                if let Some(source) = app.symbol_library.source_path() {
                    ui.label(RichText::new(source.file_name().unwrap_or_default().to_string_lossy()).strong());
                    ui.label(format!("{} {}", app.symbol_library.symbols.len(), crate::tl!("symbols")));
                } else {
                    ui.label(crate::tl!("Select a ZIP symbol library."));
                }
            });
            if let Some(error) = &app.symbol_library.error {
                ui.colored_label(Tokens::get().warn, error);
            }
            if app.symbol_library.symbols.is_empty() {
                return;
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(crate::tl!("Search"));
                ui.text_edit_singleline(&mut app.symbol_library.search);
                ui.separator();
                ui.label(crate::tl!("Insertion scale"));
                ui.add(egui::DragValue::new(&mut app.symbol_library.scale).speed(0.5).range(0.001..=1_000_000.0));
            });
            ui.separator();

            let needle = app.symbol_library.search.trim().to_ascii_lowercase();
            let filtered: Vec<usize> = app
                .symbol_library
                .symbols
                .iter()
                .enumerate()
                .filter_map(|(index, symbol)| (needle.is_empty() || symbol.code.to_ascii_lowercase().contains(&needle)).then_some(index))
                .collect();
            let mut selected = app.symbol_library.selected;
            let list_height = (ui.available_height() - 44.0).max(120.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(list_height).show_rows(ui, 72.0, filtered.len(), |ui, rows| {
                for row in rows {
                    let Some(index) = filtered.get(row).copied() else { continue };
                    let Some(symbol) = app.symbol_library.symbols.get(index).cloned() else { continue };
                    let preview = app.symbol_library.preview(&symbol);
                    let row_response = ui.horizontal(|ui| {
                        let image = draw_preview(ui, &preview);
                        let label = ui.selectable_label(selected == Some(index), RichText::new(&symbol.code).size(16.0));
                        (image, label)
                    });
                    let clicked = row_response.inner.0.clicked() || row_response.inner.1.clicked();
                    let double_clicked = row_response.inner.0.double_clicked() || row_response.inner.1.double_clicked();
                    if clicked || double_clicked {
                        selected = Some(index);
                    }
                    if double_clicked {
                        insert_symbol = Some(symbol);
                    }
                    if let Preview::Failed(error) = preview {
                        row_response.response.on_hover_text(error);
                    }
                }
            });
            app.symbol_library.selected = selected;
            ui.separator();
            ui.horizontal(|ui| {
                let selected_symbol = selected.and_then(|index| app.symbol_library.symbols.get(index)).cloned();
                if ui.add_enabled(selected_symbol.is_some(), egui::Button::new(crate::tl!("Insert symbol"))).clicked() {
                    insert_symbol = selected_symbol;
                }
                ui.label(crate::tl!("Double-click a symbol, then specify its insertion point."));
            });
        });
    if let Some(path) = picked {
        app.symbol_library.load(path);
    }
    if let Some(symbol) = insert_symbol {
        let scale = app.symbol_library.scale;
        match insert(app, &symbol, scale) {
            Ok(()) => *open = false,
            Err(error) => {
                app.session.echo(error.clone());
                app.symbol_library.error = Some(error);
            }
        }
    }
}

/// `SIMBOLI` opens the window; `simboli {"path":"library.zip"}` loads a catalog without a
/// file dialog, for scripts and the control channel.
pub fn route(app: &mut CadApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, "simboli" | "libreriasimboli" | "ui.dialog.symbols") {
        return None;
    }
    if params.is_null() {
        app.ui.dialog = Some("symbols".into());
        return Some(Ok(Value::Null));
    }
    let Some(path) = params.get("path").and_then(Value::as_str).filter(|path| !path.trim().is_empty()) else {
        return Some(Err("simboli: `path` is required".into()));
    };
    app.symbol_library.load(path);
    if let Some(error) = &app.symbol_library.error {
        return Some(Err(error.clone()));
    }
    Some(Ok(json!({ "path": path, "symbols": app.symbol_library.symbols.len() })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    #[test]
    fn symbol_names_are_case_insensitive_and_path_safe() {
        assert!(symbol_extension("planimetria/PRESA.DWG"));
        assert!(symbol_extension("a/b.dxf"));
        assert!(!symbol_extension("a/b.bak"));
        assert_eq!(symbol_code("quadri/INTERRUTTORE.dwg").as_deref(), Some("INTERRUTTORE"));
        assert_eq!(symbol_code("quadri/.dwg"), None);
    }

    #[test]
    fn malformed_archive_is_an_error_not_a_panic() {
        let path = std::env::temp_dir().join(format!("cadcraft-symbol-invalid-{}.zip", std::process::id()));
        let write = std::fs::write(&path, b"not a zip");
        assert!(write.is_ok());
        assert!(index_archive(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn archive_index_accepts_dwg_and_dxf_entries_only() {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("cadcraft-symbol-index-{}-{id}.zip", std::process::id()));
        let result: Result<(), String> = (|| {
            let file = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|error| error.to_string())?;
            let mut archive = zip::ZipWriter::new(file);
            archive.start_file("planimetria/PRESA.DWG", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"synthetic fixture").map_err(|error| error.to_string())?;
            archive.start_file("quadri/INTERRUTTORE.dxf", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"synthetic fixture").map_err(|error| error.to_string())?;
            archive.start_file("catalogo.inf", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"ignored").map_err(|error| error.to_string())?;
            archive.finish().map_err(|error| error.to_string())?;
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
        let indexed = index_archive(&path);
        assert!(indexed.is_ok(), "{indexed:?}");
        let symbols = indexed.unwrap_or_default();
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols.first().map(|symbol| symbol.code.as_str()), Some("INTERRUTTORE"));
        assert_eq!(symbols.get(1).map(|symbol| symbol.code.as_str()), Some("PRESA"));
        let _ = std::fs::remove_file(path);
    }
}
