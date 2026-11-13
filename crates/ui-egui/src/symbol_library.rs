//! Local symbol libraries: a ZIP containing one DXF/DWG per symbol, with previews and INSERT.
//!
//! Library files stay outside the repository and outside the drawing until the user inserts a
//! symbol. The ZIP is treated as hostile input: entry counts and uncompressed sizes are capped,
//! paths are never extracted, and every error is reported in the window instead of panicking.

use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write as _};
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Entity, EntityKind, Space};
use cadcraft_engine::Input;
use cadcraft_geom::Vec2;
use cadcraft_render::DisplayList;
use egui::{RichText, Sense, Stroke, vec2};
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::{Reader, XmlVersion};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::CadApp;
use crate::canvas::Xf;
use crate::theme::Tokens;

const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_SYMBOL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PREVIEWS: usize = 128;
const MAX_CATALOG_XML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CATALOG_ROWS: usize = 10_000;
const MAX_CATALOG_COLUMNS: usize = 64;
const MAX_CATALOG_STRINGS: usize = 100_000;
const MAX_CATALOG_TEXT_BYTES: usize = 4096;
const MAX_USER_LIBRARY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_USER_SYMBOLS: usize = 5_000;
const MAX_EDITOR_CHOICES: usize = 20_000;
static TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Loose entities used only for a library thumbnail. Attribute definitions normally stay hidden
/// so a symbol is not covered by prompts and tags; `NOTEC` is the exception because its
/// default value is part of the symbol's readable identity (`EDP`, `TV`, `10 A`, ...). Convert it
/// to ordinary text in the temporary preview without changing the source drawing or block.
fn preview_entities(drawing: &Drawing) -> Vec<Entity> {
    drawing
        .model
        .iter()
        .filter_map(|entity| match &entity.kind {
            EntityKind::AttDef(attribute) if attribute.tag.eq_ignore_ascii_case("NOTEC") && !attribute.text.value.trim().is_empty() => {
                let mut preview = entity.as_ref().clone();
                preview.kind = EntityKind::Text(attribute.text.clone());
                Some(preview)
            }
            EntityKind::AttDef(_) => None,
            _ => Some(entity.as_ref().clone()),
        })
        .collect()
}

#[derive(Clone, Debug)]
struct Symbol {
    code: String,
    entry: String,
    index: usize,
    bytes: u64,
    description: String,
    category: String,
    family: String,
    layer: String,
    /// A CADCraft personal symbol stored beside the base ZIP; `None` means an entry in the ZIP.
    file: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct CatalogRow {
    description: String,
    category: String,
    family: String,
    layer: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct UserSymbol {
    code: String,
    file: String,
    description: String,
    category: String,
    family: String,
    layer: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct UserLibrary {
    version: u32,
    symbols: Vec<UserSymbol>,
    hidden: BTreeSet<String>,
    overrides: HashMap<String, CatalogRow>,
}

#[derive(Clone, Debug)]
struct SymbolEdit {
    code: String,
    description: String,
    category: String,
    family: String,
    layer: String,
}

#[derive(Clone, Debug)]
struct DrawingSymbolImport {
    source_block: String,
    edit: SymbolEdit,
    reference_scale: [f64; 3],
    drawing_scale: f64,
    duplicate: bool,
    replace_existing: bool,
    warnings: Vec<String>,
    error: Option<String>,
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
    catalog_path: Option<PathBuf>,
    catalog_warning: Option<String>,
    category: String,
    pending: Option<PendingInsert>,
    default_checked: bool,
    user_path: Option<PathBuf>,
    user: UserLibrary,
    personal_in_archive: bool,
    editor: Option<SymbolEdit>,
    drawing_import: Option<DrawingSymbolImport>,
    delete_confirm: Option<String>,
    manage_mode: bool,
}

#[derive(Clone)]
struct PendingInsert {
    uid: u64,
    space: Space,
    entities_before: usize,
    block: String,
    description: String,
    layer: String,
}

impl State {
    pub fn source_path(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    pub fn load(&mut self, path: impl AsRef<Path>) {
        match index_archive(path.as_ref()) {
            Ok(mut symbols) => {
                let (catalog_path, mut catalog_warning) = match adjacent_catalog(path.as_ref()) {
                    Some(catalog_path) => match read_catalog(&catalog_path) {
                        Ok(catalog) => {
                            for symbol in &mut symbols {
                                if let Some(row) = catalog.get(&symbol.code.to_ascii_uppercase()) {
                                    symbol.description.clone_from(&row.description);
                                    symbol.category.clone_from(&row.category);
                                    symbol.family.clone_from(&row.family);
                                    symbol.layer.clone_from(&row.layer);
                                }
                            }
                            (Some(catalog_path), None)
                        }
                        Err(error) => (Some(catalog_path), Some(error)),
                    },
                    None => (None, None),
                };
                let user_path = sidecar_path(path.as_ref());
                let user = if user_path.is_file() {
                    match read_user_library(&user_path) {
                        Ok(user) => user,
                        Err(error) => {
                            append_warning(&mut catalog_warning, error);
                            UserLibrary::default()
                        }
                    }
                } else {
                    UserLibrary { version: 1, ..UserLibrary::default() }
                };
                if let Err(error) = merge_user_library(path.as_ref(), &mut symbols, &user) {
                    append_warning(&mut catalog_warning, error);
                }
                self.source = Some(path.as_ref().to_path_buf());
                self.symbols = symbols;
                self.selected = (!self.symbols.is_empty()).then_some(0);
                self.previews.clear();
                self.error = None;
                self.catalog_path = catalog_path;
                self.catalog_warning = catalog_warning;
                self.category.clear();
                self.user_path = Some(user_path);
                self.user = user;
                self.personal_in_archive = archive_uses_personal_folder(path.as_ref()).unwrap_or(false);
                self.editor = None;
                self.drawing_import = None;
                self.delete_confirm = None;
            }
            Err(error) => {
                self.personal_in_archive = false;
                self.error = Some(error);
            }
        }
    }

    fn save_user(&mut self) -> Result<(), String> {
        let path = self.user_path.clone().ok_or_else(|| "Seleziona prima una libreria simboli.".to_string())?;
        self.user.version = 1;
        save_user_library(&path, &self.user)
    }

    /// Load a user-provided library bundled beside the executable before trying the historical
    /// workstation path.  Relative portable paths keep the electrical workspace useful after it
    /// is copied to another computer, without embedding third-party symbols in the application.
    fn ensure_default_loaded(&mut self) {
        if self.default_checked || !self.symbols.is_empty() || self.source.is_some() {
            return;
        }
        self.default_checked = true;
        #[cfg(target_os = "windows")]
        {
            let bundled = std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join("Librerie").join("Libreria_CADCraft_Elettro.zip")))
                .or_else(|| std::env::current_dir().ok().map(|path| path.join("Librerie").join("Libreria_CADCraft_Elettro.zip")));
            if let Some(path) = bundled.filter(|path| path.is_file()) {
                self.load(path);
                return;
            }
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
            let entities = preview_entities(&drawing);
            let rough = cadcraft_render::build_entities(&drawing, entities.iter(), &cadcraft_render::Options::default());
            let size = rough.bounds.width().max(rough.bounds.height());
            let options = cadcraft_render::Options { tolerance: (size / 240.0).max(1e-9), ..Default::default() };
            let list = if rough.bounds.is_empty() { rough } else { cadcraft_render::build_entities(&drawing, entities.iter(), &options) };
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

fn xml_attr(element: &BytesStart, name: &[u8]) -> Option<String> {
    element.attributes().flatten().find_map(|attribute| {
        let key = attribute.key.into_inner().as_bytes();
        (key == name || key.strip_suffix(name).is_some_and(|prefix| prefix.ends_with(b":")))
            .then(|| attribute.normalized_value(XmlVersion::Explicit1_0).ok().map(|value| value.into_owned()))
            .flatten()
    })
}

fn xml_text(text: &BytesText) -> Result<String, String> {
    Ok(text.xml_content(XmlVersion::Explicit1_0).into_owned())
}

fn read_xlsx_entry(archive: &mut ZipArchive<File>, name: &str, required: bool) -> Result<Option<Vec<u8>>, String> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) if !required => return Ok(None),
        Err(error) => return Err(format!("Catalog: cannot read {name}: {error}")),
    };
    if entry.size() > MAX_CATALOG_XML_BYTES {
        return Err(format!("Catalog: {name} is larger than the safety limit."));
    }
    let capacity = usize::try_from(entry.size()).map_err(|_| format!("Catalog: {name} is too large."))?;
    let mut bytes = Vec::with_capacity(capacity);
    entry.by_ref().take(MAX_CATALOG_XML_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|error| format!("Catalog: {name}: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CATALOG_XML_BYTES {
        return Err(format!("Catalog: {name} expands beyond the safety limit."));
    }
    Ok(Some(bytes))
}

fn workbook_sheet(archive: &mut ZipArchive<File>, wanted: &str) -> Result<String, String> {
    let workbook = read_xlsx_entry(archive, "xl/workbook.xml", true)?.ok_or_else(|| "Catalog: workbook.xml is missing.".to_string())?;
    let mut relationship = None;
    let mut reader = Reader::from_reader(workbook.as_slice());
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event().map_err(|error| format!("Catalog workbook.xml: {error}"))? {
            Event::Start(element) | Event::Empty(element) if element.local_name().as_ref() == "sheet" => {
                if xml_attr(&element, b"name").is_some_and(|name| name.eq_ignore_ascii_case(wanted)) {
                    relationship = xml_attr(&element, b"id");
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let relationship = relationship.ok_or_else(|| format!("Catalog: worksheet {wanted:?} was not found."))?;
    let relationships =
        read_xlsx_entry(archive, "xl/_rels/workbook.xml.rels", true)?.ok_or_else(|| "Catalog: workbook relationships are missing.".to_string())?;
    let mut target = None;
    let mut reader = Reader::from_reader(relationships.as_slice());
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event().map_err(|error| format!("Catalog workbook relationships: {error}"))? {
            Event::Start(element) | Event::Empty(element) if element.local_name().as_ref() == "Relationship" => {
                if xml_attr(&element, b"Id").as_deref() == Some(&relationship) {
                    target = xml_attr(&element, b"Target");
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let target = target.ok_or_else(|| format!("Catalog: worksheet relationship {relationship:?} is missing."))?;
    let target = target.trim_start_matches('/');
    Ok(if target.starts_with("xl/") { target.to_string() } else { format!("xl/{target}") })
}

fn shared_strings(archive: &mut ZipArchive<File>) -> Result<Vec<String>, String> {
    let Some(xml) = read_xlsx_entry(archive, "xl/sharedStrings.xml", false)? else { return Ok(Vec::new()) };
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_item = false;
    let mut in_text = false;
    let mut reader = Reader::from_reader(xml.as_slice());
    reader.config_mut().trim_text(false);
    loop {
        match reader.read_event().map_err(|error| format!("Catalog shared strings: {error}"))? {
            Event::Start(element) => match element.local_name().as_ref() {
                "si" => {
                    if strings.len() >= MAX_CATALOG_STRINGS {
                        return Err("Catalog: too many shared strings.".into());
                    }
                    current.clear();
                    in_item = true;
                }
                "t" if in_item => in_text = true,
                _ => {}
            },
            Event::Text(text) if in_text => {
                let value = xml_text(&text)?;
                if current.len().saturating_add(value.len()) <= MAX_CATALOG_TEXT_BYTES {
                    current.push_str(&value);
                }
            }
            Event::End(element) => match element.local_name().as_ref() {
                "t" => in_text = false,
                "si" if in_item => {
                    strings.push(current.clone());
                    in_item = false;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(strings)
}

fn parse_cell_column(reference: &str) -> Option<usize> {
    let mut value = 0_usize;
    let mut letters = 0_usize;
    for byte in reference.bytes().take_while(u8::is_ascii_alphabetic) {
        value = value.checked_mul(26)?.checked_add(usize::from(byte.to_ascii_uppercase().saturating_sub(b'A')) + 1)?;
        letters = letters.saturating_add(1);
    }
    (letters > 0).then_some(value.saturating_sub(1)).filter(|column| *column < MAX_CATALOG_COLUMNS)
}

fn worksheet_rows(xml: &[u8], shared: &[String]) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row: Option<Vec<String>> = None;
    let mut cell_column = None;
    let mut cell_type = String::new();
    let mut cell_value = String::new();
    let mut capture = false;
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    loop {
        match reader.read_event().map_err(|error| format!("Catalog worksheet: {error}"))? {
            Event::Start(element) => match element.local_name().as_ref() {
                "row" => row = Some(Vec::new()),
                "c" if row.is_some() => {
                    cell_column = xml_attr(&element, b"r").as_deref().and_then(parse_cell_column);
                    cell_type = xml_attr(&element, b"t").unwrap_or_default();
                    cell_value.clear();
                }
                "v" | "t" if cell_column.is_some() => capture = true,
                _ => {}
            },
            Event::Text(text) if capture => {
                let value = xml_text(&text)?;
                if cell_value.len().saturating_add(value.len()) <= MAX_CATALOG_TEXT_BYTES {
                    cell_value.push_str(&value);
                }
            }
            Event::End(element) => match element.local_name().as_ref() {
                "v" | "t" => capture = false,
                "c" => {
                    if let (Some(row), Some(column)) = (&mut row, cell_column.take()) {
                        if row.len() <= column {
                            row.resize(column.saturating_add(1), String::new());
                        }
                        let value = if cell_type == "s" {
                            cell_value.trim().parse::<usize>().ok().and_then(|index| shared.get(index)).cloned().unwrap_or_default()
                        } else {
                            cell_value.clone()
                        };
                        if let Some(slot) = row.get_mut(column) {
                            *slot = value;
                        }
                    }
                }
                "row" => {
                    if let Some(row) = row.take() {
                        rows.push(row);
                        if rows.len() >= MAX_CATALOG_ROWS {
                            return Err("Catalog: too many rows.".into());
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rows)
}

fn read_catalog(path: &Path) -> Result<HashMap<String, CatalogRow>, String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|error| format!("{} is not a readable XLSX catalog: {error}", path.display()))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err("Catalog: too many XLSX entries.".into());
    }
    let sheet = workbook_sheet(&mut archive, "Catalogo")?;
    let shared = shared_strings(&mut archive)?;
    let xml = read_xlsx_entry(&mut archive, &sheet, true)?.ok_or_else(|| format!("Catalog: {sheet} is missing."))?;
    let rows = worksheet_rows(&xml, &shared)?;
    let Some(headers) = rows.first() else { return Err("Catalog: the Catalogo worksheet is empty.".into()) };
    let column = |name: &str| headers.iter().position(|header| header.trim().eq_ignore_ascii_case(name));
    let code = column("Codice").ok_or_else(|| "Catalog: the Codice column is missing.".to_string())?;
    let description = column("Descrizione").ok_or_else(|| "Catalog: the Descrizione column is missing.".to_string())?;
    let category = column("Categoria");
    let family = column("Famiglia");
    let layer = column("Layer nel DXF");
    let value = |row: &[String], index: Option<usize>| index.and_then(|index| row.get(index)).map(|value| value.trim()).unwrap_or("").to_string();
    let mut catalog = HashMap::new();
    for row in rows.iter().skip(1) {
        let Some(code) = row.get(code).map(|code| code.trim()).filter(|code| !code.is_empty()) else { continue };
        catalog.entry(code.to_ascii_uppercase()).or_insert_with(|| CatalogRow {
            description: row.get(description).map(|value| value.trim()).unwrap_or("").to_string(),
            category: value(row, category),
            family: value(row, family),
            layer: value(row, layer),
        });
    }
    Ok(catalog)
}

fn adjacent_catalog(archive: &Path) -> Option<PathBuf> {
    let parent = archive.parent()?;
    let candidates = [archive.with_extension("xlsx"), parent.join("Catalogo_simboli_CadCraft.xlsx"), parent.join("catalogo.xlsx")];
    candidates.into_iter().find(|path| path.is_file())
}

fn sidecar_path(archive: &Path) -> PathBuf {
    archive.with_extension("cadcraft.json")
}

fn user_symbol_folder(archive: &Path) -> Result<PathBuf, String> {
    let parent = archive.parent().ok_or_else(|| format!("{} non ha una cartella valida.", archive.display()))?;
    let stem = archive.file_stem().and_then(|value| value.to_str()).filter(|value| !value.trim().is_empty()).unwrap_or("Libreria");
    Ok(parent.join(format!("{stem}-CADCraft")))
}

fn append_warning(warning: &mut Option<String>, message: String) {
    match warning {
        Some(current) => {
            current.push('\n');
            current.push_str(&message);
        }
        None => *warning = Some(message),
    }
}

fn safe_relative_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return None;
    }
    path.components().all(|component| matches!(component, Component::Normal(_))).then(|| path.to_path_buf())
}

fn read_user_library(path: &Path) -> Result<UserLibrary, String> {
    let metadata = std::fs::metadata(path).map_err(|error| format!("Catalogo personale {}: {error}", path.display()))?;
    if metadata.len() > MAX_USER_LIBRARY_BYTES {
        return Err(format!("Catalogo personale {} oltre il limite di sicurezza.", path.display()));
    }
    let file = File::open(path).map_err(|error| format!("Catalogo personale {}: {error}", path.display()))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_USER_LIBRARY_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Catalogo personale {}: {error}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_USER_LIBRARY_BYTES {
        return Err(format!("Catalogo personale {} oltre il limite di sicurezza.", path.display()));
    }
    let mut user: UserLibrary =
        serde_json::from_slice(&bytes).map_err(|error| format!("Catalogo personale {} non valido: {error}", path.display()))?;
    if user.symbols.len() > MAX_USER_SYMBOLS || user.hidden.len() > MAX_USER_SYMBOLS || user.overrides.len() > MAX_USER_SYMBOLS {
        return Err("Il catalogo personale contiene troppi simboli o modifiche.".into());
    }
    user.version = 1;
    Ok(user)
}

fn save_user_library(path: &Path, user: &UserLibrary) -> Result<(), String> {
    if user.symbols.len() > MAX_USER_SYMBOLS || user.hidden.len() > MAX_USER_SYMBOLS || user.overrides.len() > MAX_USER_SYMBOLS {
        return Err("Il catalogo personale contiene troppi simboli o modifiche.".into());
    }
    let parent = path.parent().ok_or_else(|| format!("{} non ha una cartella valida.", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(user).map_err(|error| format!("Catalogo personale: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_USER_LIBRARY_BYTES {
        return Err("Il catalogo personale supera il limite di sicurezza.".into());
    }
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".cadcraft-symbols-{}-{id}.tmp", std::process::id()));
    let write_result: Result<(), String> = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;
        file.write_all(&bytes).map_err(|error| format!("{}: {error}", temp.display()))?;
        file.flush().map_err(|error| format!("{}: {error}", temp.display()))?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    let backup = path.with_extension("cadcraft.json.bak");
    if path.exists() {
        if backup.exists() {
            std::fs::remove_file(&backup).map_err(|error| format!("{}: {error}", backup.display()))?;
        }
        std::fs::rename(path, &backup).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, path);
        }
        let _ = std::fs::remove_file(&temp);
        return Err(format!("{}: {error}", path.display()));
    }
    if backup.exists()
        && let Err(error) = std::fs::remove_file(&backup)
    {
        log::warn!("symbol library backup {}: {error}", backup.display());
    }
    Ok(())
}

fn merge_user_library(archive: &Path, symbols: &mut Vec<Symbol>, user: &UserLibrary) -> Result<(), String> {
    let parent = archive.parent().ok_or_else(|| format!("{} non ha una cartella valida.", archive.display()))?;
    for personal in user.symbols.iter().take(MAX_USER_SYMBOLS) {
        let Some(relative) = safe_relative_path(&personal.file) else {
            continue;
        };
        let file = parent.join(relative);
        let metadata = match std::fs::metadata(&file) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_SYMBOL_BYTES => metadata,
            _ => {
                // A portable library may keep the personal drawing inside the ZIP's opt-in
                // `libPersonale/` folder.  The archive index already created its Symbol; use the
                // sidecar's saved catalogue data without looking for a duplicate loose file.
                if let Some(symbol) = symbols.iter_mut().find(|symbol| symbol.code.eq_ignore_ascii_case(&personal.code)) {
                    symbol.description.clone_from(&personal.description);
                    symbol.category.clone_from(&personal.category);
                    symbol.family.clone_from(&personal.family);
                    symbol.layer.clone_from(&personal.layer);
                }
                continue;
            }
        };
        if personal.code.trim().is_empty() || !symbol_extension(&personal.file) {
            continue;
        }
        symbols.retain(|symbol| !symbol.code.eq_ignore_ascii_case(&personal.code));
        symbols.push(Symbol {
            code: personal.code.trim().to_string(),
            entry: personal.file.clone(),
            index: usize::MAX,
            bytes: metadata.len(),
            description: personal.description.clone(),
            category: personal.category.clone(),
            family: personal.family.clone(),
            layer: personal.layer.clone(),
            file: Some(file),
        });
    }
    for symbol in symbols.iter_mut() {
        if let Some(row) = user.overrides.get(&symbol.code.to_ascii_uppercase()) {
            symbol.description.clone_from(&row.description);
            symbol.category.clone_from(&row.category);
            symbol.family.clone_from(&row.family);
            symbol.layer.clone_from(&row.layer);
        }
    }
    symbols.retain(|symbol| !user.hidden.contains(&symbol.code.to_ascii_uppercase()));
    symbols.sort_by_key(|symbol| symbol.code.to_ascii_lowercase());
    Ok(())
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
        symbols.push(Symbol {
            code,
            entry: name,
            index,
            bytes: entry.size(),
            description: String::new(),
            category: String::new(),
            family: String::new(),
            layer: String::new(),
            file: None,
        });
    }
    // An opt-in `libPersonale/` entry intentionally overrides a base-library entry with the same
    // code. Keep it first before case-insensitive de-duplication.
    symbols.sort_by_key(|symbol| (symbol.code.to_ascii_lowercase(), !personal_archive_entry(&symbol.entry)));
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
    if let Some(symbol_path) = &symbol.file {
        let metadata = std::fs::metadata(symbol_path).map_err(|error| format!("{}: {error}", symbol_path.display()))?;
        if !metadata.is_file() || metadata.len() > MAX_SYMBOL_BYTES {
            return Err(format!("{} non è un simbolo valido o supera il limite.", symbol.code));
        }
        let file = File::open(symbol_path).map_err(|error| format!("{}: {error}", symbol_path.display()))?;
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        file.take(MAX_SYMBOL_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|error| format!("{}: {error}", symbol.code))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SYMBOL_BYTES {
            return Err(format!("{} supera il limite del simbolo.", symbol.code));
        }
        return Ok(bytes);
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
            let transform =
                Xf { rect, center: Vec2::new((bounds.min.x + bounds.max.x) / 2.0, (bounds.min.y + bounds.max.y) / 2.0), scale, twist: 0.0 };
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

fn safe_file_stem(value: &str) -> String {
    let safe: String =
        value.chars().map(|character| if "<>:\"/\\|?*".contains(character) || character.is_control() { '_' } else { character }).collect();
    let safe = safe.trim().trim_end_matches(['.', ' ']);
    if safe.is_empty() { "simbolo".into() } else { safe.to_string() }
}

fn copy_personal_file(source: &Path, destination: &Path) -> Result<u64, String> {
    let metadata = std::fs::metadata(source).map_err(|error| format!("{}: {error}", source.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_SYMBOL_BYTES {
        return Err("Il file scelto non è valido o supera 32 MB.".into());
    }
    let mut input = File::open(source).map_err(|error| format!("{}: {error}", source.display()))?;
    let mut output =
        OpenOptions::new().write(true).create_new(true).open(destination).map_err(|error| format!("{}: {error}", destination.display()))?;
    let copied = std::io::copy(&mut std::io::Read::by_ref(&mut input).take(MAX_SYMBOL_BYTES.saturating_add(1)), &mut output)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    if copied > MAX_SYMBOL_BYTES {
        let _ = std::fs::remove_file(destination);
        return Err("Il simbolo supera il limite di 32 MB.".into());
    }
    output.flush().map_err(|error| format!("{}: {error}", destination.display()))?;
    Ok(copied)
}

fn add_personal_symbol(app: &mut CadApp, source_file: &Path) -> Result<(), String> {
    let archive = app.symbol_library.source.clone().ok_or_else(|| "Seleziona prima una libreria ZIP.".to_string())?;
    if !symbol_extension(&source_file.to_string_lossy()) {
        return Err("Scegli un simbolo DWG o DXF.".into());
    }
    let code = symbol_code(&source_file.to_string_lossy()).ok_or_else(|| "Il file non ha un nome simbolo valido.".to_string())?;
    if app.symbol_library.symbols.iter().any(|symbol| symbol.code.eq_ignore_ascii_case(&code)) {
        return Err(format!("Il simbolo {code} esiste già. Usa Modifica grafica per sostituirlo con una copia personale."));
    }
    let folder = user_symbol_folder(&archive)?;
    std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let extension = source_file
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| matches!(value.to_ascii_lowercase().as_str(), "dwg" | "dxf"))
        .unwrap_or("dwg");
    let destination = folder.join(format!("{}.{}", safe_file_stem(&code), extension.to_ascii_lowercase()));
    if destination.exists() {
        return Err(format!("{} esiste già nella libreria personale.", destination.display()));
    }
    copy_personal_file(source_file, &destination)?;
    let parent = archive.parent().ok_or_else(|| format!("{} non ha una cartella valida.", archive.display()))?;
    let relative = destination.strip_prefix(parent).map_err(|_| "Percorso della libreria personale non valido.".to_string())?;
    let personal = UserSymbol {
        code: code.clone(),
        file: relative.to_string_lossy().into_owned(),
        description: code.clone(),
        category: "Personale".into(),
        family: String::new(),
        layer: "FM".into(),
    };
    app.symbol_library.user.symbols.push(personal);
    app.symbol_library.user.hidden.remove(&code.to_ascii_uppercase());
    if let Err(error) = app.symbol_library.save_user() {
        app.symbol_library.user.symbols.retain(|symbol| !symbol.code.eq_ignore_ascii_case(&code));
        let _ = std::fs::remove_file(&destination);
        return Err(error);
    }
    app.symbol_library.load(&archive);
    app.symbol_library.selected = app.symbol_library.symbols.iter().position(|symbol| symbol.code.eq_ignore_ascii_case(&code));
    app.symbol_library.editor = app.symbol_library.selected.and_then(|index| app.symbol_library.symbols.get(index)).map(SymbolEdit::from);
    Ok(())
}

fn architectural_layer(name: &str) -> bool {
    let normalized: String =
        name.chars().map(|character| if character.is_ascii_alphanumeric() { character.to_ascii_uppercase() } else { '_' }).collect();
    ["ARCH", "MUR", "PIANTA", "ARRED", "EDILE", "BASE_ARCH", "RIF_ARCH"].iter().any(|word| normalized.contains(word))
        || name.split_once('|').is_some_and(|(xref, _)| {
            let xref = xref.to_ascii_uppercase();
            ["ARCH", "EDILE", "BASE", "PIANTA"].iter().any(|word| xref.contains(word))
        })
}

fn title_block(drawing: &Drawing, name: &str) -> bool {
    if ["att_tav", "Cart_pl", "0UNIA3", "00A3SCHEMI", "CADCRAFT_ISO7200", "SG2018"].iter().any(|known| name.eq_ignore_ascii_case(known)) {
        return true;
    }
    let Some(block) = drawing.block(name) else { return false };
    let has = |wanted: &str| {
        block.entities.iter().any(|entity| matches!(&entity.kind, EntityKind::AttDef(attribute) if attribute.tag.eq_ignore_ascii_case(wanted)))
    };
    let identity = ["TAVOLA", "FOGLIO", "PROGETTO", "COMMITTENTE", "CLIENTE", "DENOMINAZIONE"];
    has("SCALA") && identity.iter().filter(|tag| has(tag)).take(2).count() >= 2
}

fn title_block_attributes(insert: &cadcraft_doc::Insert) -> bool {
    if insert.attribs.len() > 4096 {
        return false;
    }
    let has = |wanted: &[&str]| insert.attribs.iter().any(|attribute| wanted.iter().any(|tag| attribute.tag.eq_ignore_ascii_case(tag)));
    let page = has(&["SHEET", "NUMFOG", "FOGLIO", "TAVOLA"]);
    let sequence = has(&["SHEETS", "NSHEET", "TOTALE_FOGLI", "NUMFOGSEG"]);
    let groups = usize::from(has(&["OGGETTO1", "OGGETTO2", "IMPIANTO", "IMPIANTO2", "DESCRIZIONE1", "DESCRIZIONE2"]))
        + usize::from(has(&["COMUNE", "PROVINCIA", "INDIRIZZO", "CLIENTE", "COMMITTENTE", "PROGETTO"]))
        + usize::from(has(&["DATA", "DISEG", "DISEGNO", "PROG", "VISTO", "MATRICOLA", "SCALA"]));
    page && (groups >= 2 || (sequence && groups >= 1))
}

fn reference_description(drawing: &Drawing, block_name: &str, insert: &cadcraft_doc::Insert) -> String {
    if let Some(description) = drawing.block(block_name).map(|block| block.description.trim()).filter(|description| !description.is_empty()) {
        return description.to_string();
    }
    const TAGS: [&str; 9] = ["DESCRIZIONE", "DESCRIPTION", "DESC", "DENOMINAZIONE", "NOME", "OGGETTO", "UTENZA", "TIPO", "SIGLA"];
    TAGS.iter()
        .find_map(|tag| {
            insert
                .attribs
                .iter()
                .find(|attribute| attribute.tag.eq_ignore_ascii_case(tag))
                .map(|attribute| attribute.text.value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| block_name.to_string())
}

fn valid_personal_code(code: &str) -> bool {
    let code = code.trim();
    !code.is_empty()
        && code.len() <= 255
        && !code.starts_with('*')
        && !code.chars().any(|character| character.is_control() || "<>/\\\":;?*|=,".contains(character))
}

fn begin_import_from_selection(app: &mut CadApp) -> Result<(), String> {
    let selection = app.session.selection();
    if selection.len() != 1 {
        return Err("Seleziona un solo blocco nel disegno, quindi premi Aggiungi dal disegno.".into());
    }
    let drawing = app.session.doc().map_err(|error| error.to_string())?;
    let entity = drawing.entity(selection[0]).ok_or_else(|| "Il blocco selezionato non è più disponibile.".to_string())?;
    let EntityKind::Insert(insert) = &entity.kind else {
        return Err("L’oggetto selezionato non è un blocco. Seleziona un riferimento di blocco INSERT.".into());
    };
    if insert.block.starts_with('*') {
        return Err("I blocchi anonimi o dinamici generati automaticamente non possono essere aggiunti direttamente alla libreria.".into());
    }
    if title_block(drawing, &insert.block) || title_block_attributes(insert) {
        return Err("Il blocco selezionato è un cartiglio o una squadratura e non può essere aggiunto alla libreria elettrica.".into());
    }
    if architectural_layer(&entity.common.layer) {
        return Err("Il blocco selezionato appartiene alla planimetria architettonica. Scegli un simbolo elettrico.".into());
    }
    let definition = drawing.block(&insert.block).ok_or_else(|| format!("La definizione del blocco {} non è disponibile.", insert.block))?;
    if definition.xref_path.is_some() {
        return Err("Un riferimento esterno non può essere salvato come simbolo. Creare prima un normale blocco locale.".into());
    }
    if definition.entities.is_empty() {
        return Err("La definizione del blocco selezionato è vuota.".into());
    }
    let existing = app.symbol_library.symbols.iter().find(|symbol| symbol.code.eq_ignore_ascii_case(&insert.block));
    let category = existing
        .map(|symbol| symbol.category.clone())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| (!app.symbol_library.category.trim().is_empty()).then(|| app.symbol_library.category.clone()))
        .or_else(|| app.symbol_library.symbols.iter().map(|symbol| symbol.category.trim()).find(|value| !value.is_empty()).map(str::to_string))
        .unwrap_or_else(|| "Personale".into());
    let layer = if entity.common.layer.trim().is_empty() {
        existing.map_or_else(|| "0".into(), |symbol| symbol.layer.clone())
    } else {
        entity.common.layer.clone()
    };
    let drawing_scale = drawing.header.f64("DIMSCALE", 1.0);
    let reference_scale = [insert.scale.x, insert.scale.y, insert.scale.z];
    let mut warnings = Vec::new();
    if reference_scale.iter().any(|value| !value.is_finite() || value.abs() < 1e-12) {
        return Err("Il riferimento selezionato contiene una scala nulla o non valida.".into());
    }
    if (reference_scale[0].abs() - reference_scale[1].abs()).abs() > reference_scale[0].abs().max(reference_scale[1].abs()) * 1e-6 {
        warnings.push("Il riferimento usa scale X e Y differenti. La definizione sarà salvata 1:1 senza deformazioni.".into());
    }
    if reference_scale[0].is_sign_negative() || reference_scale[1].is_sign_negative() {
        warnings.push("Il riferimento è specchiato. La libreria conserverà la definizione originale non specchiata.".into());
    }
    if drawing_scale.is_finite()
        && drawing_scale > 0.0
        && (reference_scale[0].abs() - drawing_scale).abs() > drawing_scale.max(reference_scale[0].abs()) * 1e-6
    {
        warnings.push(format!(
            "La scala X del riferimento ({:.6}) differisce dalla scala simboli del disegno ({drawing_scale:.6}). Non verrà incorporata nel file.",
            reference_scale[0]
        ));
    }
    if layer.eq_ignore_ascii_case("0") {
        warnings.push("Il riferimento è sul layer 0: scegliere nella finestra il corretto layer elettrico della libreria.".into());
    }
    let description = existing.map_or_else(|| reference_description(drawing, &insert.block, insert), |symbol| symbol.description.clone());
    app.symbol_library.drawing_import = Some(DrawingSymbolImport {
        source_block: insert.block.clone(),
        edit: SymbolEdit {
            code: insert.block.clone(),
            description,
            category,
            family: existing.map_or_else(String::new, |symbol| symbol.family.clone()),
            layer,
        },
        reference_scale,
        drawing_scale: if drawing_scale.is_finite() && drawing_scale > 0.0 { drawing_scale } else { 1.0 },
        duplicate: existing.is_some(),
        replace_existing: existing.is_some(),
        warnings,
        error: None,
    });
    Ok(())
}

fn standalone_block_drawing(source: &Drawing, name: &str) -> Result<Drawing, String> {
    const MAX_DEPENDENCIES: usize = 4_096;
    let root = source.block(name).ok_or_else(|| format!("La definizione del blocco {name} non è disponibile."))?;
    let mut wanted = BTreeSet::<String>::new();
    let mut pending = root
        .entities
        .iter()
        .filter_map(|entity| match &entity.kind {
            EntityKind::Insert(insert) => Some(insert.block.clone()),
            EntityKind::Dimension(dimension) => dimension.block.clone(),
            _ => None,
        })
        .collect::<Vec<_>>();
    while let Some(dependency) = pending.pop() {
        if dependency.eq_ignore_ascii_case(name) {
            return Err("Il blocco contiene un riferimento circolare a se stesso.".into());
        }
        if wanted.len() >= MAX_DEPENDENCIES {
            return Err(format!("Il blocco contiene più di {MAX_DEPENDENCIES} definizioni annidate."));
        }
        let Some(block) = source.block(&dependency) else {
            return Err(format!("La definizione annidata {dependency} non è disponibile."));
        };
        if !wanted.insert(block.name.to_ascii_uppercase()) {
            continue;
        }
        pending.extend(block.entities.iter().filter_map(|entity| match &entity.kind {
            EntityKind::Insert(insert) => Some(insert.block.clone()),
            EntityKind::Dimension(dimension) => dimension.block.clone(),
            _ => None,
        }));
    }
    let mut drawing = source.clone();
    drawing.foreign = Default::default();
    drawing.model = root.entities.clone();
    drawing.blocks.retain(|_, block| wanted.contains(&block.name.to_ascii_uppercase()));
    for layout in &mut drawing.layouts {
        layout.entities = Default::default();
    }
    drawing.active_layout = None;
    drawing.views.clear();
    drawing.vports.clear();
    drawing.ucss.clear();
    drawing.layer_states.clear();
    drawing.groups.clear();
    drawing.constraints.clear();
    drawing.parametric = Default::default();
    drawing.hidden_objects.clear();
    drawing.read_notes.clear();
    drawing.refedit = None;
    drawing.skipped_records.clear();
    drawing.header.set("INSBASE", cadcraft_doc::HVal::Point(root.base));
    Ok(drawing)
}

fn unique_personal_path(folder: &Path, code: &str) -> Result<PathBuf, String> {
    let stem = safe_file_stem(code);
    for suffix in 0..=10_000_u32 {
        let file = if suffix == 0 { format!("{stem}.dxf") } else { format!("{stem}-dal-disegno-{suffix}.dxf") };
        let path = folder.join(file);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("La cartella personale contiene troppe versioni dello stesso simbolo.".into())
}

fn personal_archive_entry(name: &str) -> bool {
    let normalized = name.replace('\\', "/").to_ascii_lowercase();
    normalized.starts_with("libpersonale/") || normalized.starts_with("personale/")
}

fn archive_uses_personal_folder(path: &Path) -> Result<bool, String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|error| format!("{}: {error}", path.display()))?;
    for index in 0..archive.len().min(MAX_ARCHIVE_ENTRIES.saturating_add(1)) {
        let entry = archive.by_index(index).map_err(|error| format!("{}: {error}", path.display()))?;
        let name = entry.name().map_err(|error| format!("{}: {error}", path.display()))?;
        if personal_archive_entry(&name) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Atomically add or replace one drawing under an opted-in ZIP's `libPersonale/` folder. Existing
/// entries are copied in compressed form and the first pre-update archive is retained as a
/// recovery copy. The live archive is restored if the final rename fails.
fn write_personal_archive_symbol(archive_path: &Path, code: &str, bytes: &[u8]) -> Result<String, String> {
    if bytes.len() as u64 > MAX_SYMBOL_BYTES {
        return Err("Il simbolo esportato supera il limite di 32 MB.".into());
    }
    let parent = archive_path.parent().ok_or_else(|| format!("{} non ha una cartella valida.", archive_path.display()))?;
    let entry_name = format!("libPersonale/{}.dxf", safe_file_stem(code));
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".cadcraft-library-{}-{id}.tmp", std::process::id()));
    let input = File::open(archive_path).map_err(|error| format!("{}: {error}", archive_path.display()))?;
    let mut source = ZipArchive::new(input).map_err(|error| format!("{}: {error}", archive_path.display()))?;
    if source.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!("La libreria contiene più di {MAX_ARCHIVE_ENTRIES} elementi."));
    }
    let output = OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;
    let write_result: Result<(), String> = (|| {
        let mut writer = ZipWriter::new(output);
        for index in 0..source.len() {
            let entry = source.by_index(index).map_err(|error| format!("Libreria, elemento {index}: {error}"))?;
            let name = entry.name().map_err(|error| format!("Libreria, nome elemento {index}: {error}"))?.into_owned();
            let same_personal_symbol = personal_archive_entry(&name) && symbol_code(&name).is_some_and(|stored| stored.eq_ignore_ascii_case(code));
            if same_personal_symbol {
                continue;
            }
            writer.raw_copy_file(entry).map_err(|error| format!("Libreria, copia di {name}: {error}"))?;
        }
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).unix_permissions(0o644);
        writer.start_file(&entry_name, options).map_err(|error| format!("Libreria, creazione di {entry_name}: {error}"))?;
        writer.write_all(bytes).map_err(|error| format!("Libreria, scrittura di {entry_name}: {error}"))?;
        let file = writer.finish().map_err(|error| format!("Libreria, chiusura: {error}"))?;
        file.sync_all().map_err(|error| format!("{}: {error}", temp.display()))?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }

    let validation: Result<(), String> = (|| {
        let file = File::open(&temp).map_err(|error| format!("{}: {error}", temp.display()))?;
        let mut archive = ZipArchive::new(file).map_err(|error| format!("Verifica libreria: {error}"))?;
        let mut entry = archive.by_name(&entry_name).map_err(|error| format!("Verifica {entry_name}: {error}"))?;
        if entry.size() != bytes.len() as u64 {
            return Err(format!("Verifica {entry_name}: dimensione non valida"));
        }
        let mut written = Vec::with_capacity(bytes.len());
        entry
            .by_ref()
            .take(MAX_SYMBOL_BYTES.saturating_add(1))
            .read_to_end(&mut written)
            .map_err(|error| format!("Verifica {entry_name}: {error}"))?;
        if written != bytes {
            return Err(format!("Verifica {entry_name}: contenuto non valido"));
        }
        Ok(())
    })();
    if let Err(error) = validation {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }

    let original_name = archive_path.file_name().and_then(|name| name.to_str()).unwrap_or("libreria.zip");
    let baseline = parent.join(format!("{original_name}.cadcraft-original.zip"));
    if !baseline.exists() {
        std::fs::copy(archive_path, &baseline).map_err(|error| format!("Backup {}: {error}", baseline.display()))?;
    }
    let displaced = parent.join(format!(".cadcraft-library-current-{}-{id}.zip", std::process::id()));
    if let Err(error) = std::fs::rename(archive_path, &displaced) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("{}: {error}", archive_path.display()));
    }
    if let Err(error) = std::fs::rename(&temp, archive_path) {
        let restore = std::fs::rename(&displaced, archive_path);
        let _ = std::fs::remove_file(&temp);
        return match restore {
            Ok(()) => Err(format!("Aggiornamento libreria non riuscito: {error}")),
            Err(restore_error) => {
                Err(format!("Aggiornamento libreria non riuscito: {error}; ripristino necessario da {}: {restore_error}", displaced.display()))
            }
        };
    }
    if let Err(error) = std::fs::remove_file(&displaced) {
        log::warn!("temporary previous symbol library {}: {error}", displaced.display());
    }
    Ok(entry_name)
}

fn save_drawing_import(app: &mut CadApp, import: &DrawingSymbolImport) -> Result<(), String> {
    let archive = app.symbol_library.source.clone().ok_or_else(|| "Seleziona prima una libreria ZIP.".to_string())?;
    let code = import.edit.code.trim();
    if !valid_personal_code(code) {
        return Err("Il nome del simbolo è vuoto o contiene caratteri non ammessi.".into());
    }
    let duplicate = app.symbol_library.symbols.iter().any(|symbol| symbol.code.eq_ignore_ascii_case(code));
    if duplicate && !import.replace_existing {
        return Err(format!("Il simbolo {code} esiste già. Scegli Sostituisci oppure indica un nuovo nome."));
    }
    let sections = choice_values(app.symbol_library.symbols.iter().map(|symbol| symbol.category.clone()), "");
    if sections.is_empty() {
        if import.edit.category.trim().is_empty() {
            return Err("Scegli una sezione per il simbolo.".into());
        }
    } else if !sections.iter().any(|value| value.eq_ignore_ascii_case(import.edit.category.trim())) {
        return Err("Scegli una sezione già presente nella libreria.".into());
    }
    let layers = choice_values(
        app.symbol_library
            .symbols
            .iter()
            .map(|symbol| symbol.layer.clone())
            .chain(app.session.doc().ok().into_iter().flat_map(|drawing| drawing.layers.iter().map(|layer| layer.name.clone()))),
        "",
    );
    if import.edit.layer.trim().is_empty() || !layers.iter().any(|value| value.eq_ignore_ascii_case(import.edit.layer.trim())) {
        return Err("Scegli un layer già presente nel disegno o nella libreria.".into());
    }
    if architectural_layer(import.edit.layer.trim()) {
        return Err("Il layer scelto appartiene alla planimetria architettonica, non alla libreria elettrica.".into());
    }

    let drawing = app.session.doc().map_err(|error| error.to_string())?;
    let standalone = standalone_block_drawing(drawing, &import.source_block)?;
    let portable_archive = app.symbol_library.personal_in_archive;
    let folder = user_symbol_folder(&archive)?;
    let suggested = folder.join(format!("{}.dxf", safe_file_stem(code)));
    let bytes = cadcraft_io::write(&standalone, &suggested).map_err(|error| format!("Impossibile esportare {code}: {error}"))?;
    if bytes.len() as u64 > MAX_SYMBOL_BYTES {
        return Err("Il simbolo esportato supera il limite di 32 MB.".into());
    }
    let parent = archive.parent().ok_or_else(|| format!("{} non ha una cartella valida.", archive.display()))?;
    let mut loose_destination = None;
    let relative = if portable_archive {
        PathBuf::from(write_personal_archive_symbol(&archive, code, &bytes)?)
    } else {
        std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        let destination = unique_personal_path(&folder, code)?;
        let mut output =
            OpenOptions::new().write(true).create_new(true).open(&destination).map_err(|error| format!("{}: {error}", destination.display()))?;
        output.write_all(&bytes).map_err(|error| format!("{}: {error}", destination.display()))?;
        output.flush().map_err(|error| format!("{}: {error}", destination.display()))?;
        let relative = destination.strip_prefix(parent).map_err(|_| "Percorso della libreria personale non valido.".to_string())?.to_path_buf();
        loose_destination = Some(destination);
        relative
    };
    let previous = app.symbol_library.user.clone();
    let row = CatalogRow {
        description: import.edit.description.trim().to_string(),
        category: import.edit.category.trim().to_string(),
        family: import.edit.family.trim().to_string(),
        layer: import.edit.layer.trim().to_string(),
    };
    app.symbol_library.user.symbols.retain(|symbol| !symbol.code.eq_ignore_ascii_case(code));
    app.symbol_library.user.symbols.push(UserSymbol {
        code: code.to_string(),
        file: relative.to_string_lossy().replace('\\', "/"),
        description: row.description.clone(),
        category: row.category.clone(),
        family: row.family.clone(),
        layer: row.layer.clone(),
    });
    app.symbol_library.user.overrides.insert(code.to_ascii_uppercase(), row);
    app.symbol_library.user.hidden.remove(&code.to_ascii_uppercase());
    if let Err(error) = app.symbol_library.save_user() {
        app.symbol_library.user = previous;
        if let Some(destination) = loose_destination {
            let _ = std::fs::remove_file(&destination);
        }
        return Err(error);
    }
    app.symbol_library.load(&archive);
    app.symbol_library.selected = app.symbol_library.symbols.iter().position(|symbol| symbol.code.eq_ignore_ascii_case(code));
    Ok(())
}

impl From<&Symbol> for SymbolEdit {
    fn from(symbol: &Symbol) -> Self {
        Self {
            code: symbol.code.clone(),
            description: symbol.description.clone(),
            category: symbol.category.clone(),
            family: symbol.family.clone(),
            layer: symbol.layer.clone(),
        }
    }
}

fn choice_values(values: impl IntoIterator<Item = String>, current: &str) -> Vec<String> {
    let mut values =
        values.into_iter().take(MAX_EDITOR_CHOICES).map(|value| value.trim().to_string()).filter(|value| !value.is_empty()).collect::<Vec<_>>();
    if !current.trim().is_empty() {
        values.push(current.trim().to_string());
    }
    values.sort_by_key(|value| value.to_ascii_lowercase());
    values.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    values
}

fn editor_choices(app: &CadApp, edit: &SymbolEdit) -> (Vec<String>, Vec<String>) {
    let sections = choice_values(app.symbol_library.symbols.iter().map(|symbol| symbol.category.clone()), &edit.category);
    let library_layers = app.symbol_library.symbols.iter().map(|symbol| symbol.layer.clone());
    let drawing_layers =
        app.session.doc().ok().into_iter().flat_map(|drawing| drawing.layers.iter().take(MAX_EDITOR_CHOICES).map(|layer| layer.name.clone()));
    let layers = choice_values(library_layers.chain(drawing_layers), &edit.layer);
    (sections, layers)
}

fn save_symbol_edit(app: &mut CadApp, edit: &SymbolEdit) -> Result<(), String> {
    let archive = app.symbol_library.source.clone().ok_or_else(|| "Seleziona prima una libreria ZIP.".to_string())?;
    let (sections, layers) = editor_choices(app, edit);
    if edit.category.trim().is_empty() || !sections.iter().any(|value| value.eq_ignore_ascii_case(edit.category.trim())) {
        return Err("Scegli una sezione già presente nella libreria.".into());
    }
    if edit.layer.trim().is_empty() {
        return Err("Indica il layer elettrico del simbolo.".into());
    }
    if !layers.iter().any(|value| value.eq_ignore_ascii_case(edit.layer.trim())) {
        return Err("Scegli un layer già presente nel disegno o nella libreria.".into());
    }
    let row = CatalogRow {
        description: edit.description.trim().to_string(),
        category: edit.category.trim().to_string(),
        family: edit.family.trim().to_string(),
        layer: edit.layer.trim().to_string(),
    };
    app.symbol_library.user.overrides.insert(edit.code.to_ascii_uppercase(), row.clone());
    if let Some(personal) = app.symbol_library.user.symbols.iter_mut().find(|symbol| symbol.code.eq_ignore_ascii_case(&edit.code)) {
        personal.description.clone_from(&row.description);
        personal.category.clone_from(&row.category);
        personal.family.clone_from(&row.family);
        personal.layer.clone_from(&row.layer);
    }
    app.symbol_library.save_user()?;
    app.symbol_library.load(&archive);
    app.symbol_library.selected = app.symbol_library.symbols.iter().position(|symbol| symbol.code.eq_ignore_ascii_case(&edit.code));
    Ok(())
}

fn open_personal_graphic(app: &mut CadApp, symbol: &Symbol) -> Result<(), String> {
    if let Some(path) = &symbol.file {
        app.open_path(path);
        return Ok(());
    }
    let archive = app.symbol_library.source.clone().ok_or_else(|| "Seleziona prima una libreria ZIP.".to_string())?;
    let bytes = read_entry(&archive, symbol)?;
    let folder = user_symbol_folder(&archive)?;
    std::fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let extension = Path::new(&symbol.entry).extension().and_then(|value| value.to_str()).unwrap_or("dwg").to_ascii_lowercase();
    let destination = folder.join(format!("{}.{}", safe_file_stem(&symbol.code), extension));
    if !destination.exists() {
        let mut output =
            OpenOptions::new().write(true).create_new(true).open(&destination).map_err(|error| format!("{}: {error}", destination.display()))?;
        output.write_all(&bytes).map_err(|error| format!("{}: {error}", destination.display()))?;
        output.flush().map_err(|error| format!("{}: {error}", destination.display()))?;
    }
    let parent = archive.parent().ok_or_else(|| format!("{} non ha una cartella valida.", archive.display()))?;
    let relative = destination.strip_prefix(parent).map_err(|_| "Percorso della libreria personale non valido.".to_string())?;
    app.symbol_library.user.symbols.retain(|personal| !personal.code.eq_ignore_ascii_case(&symbol.code));
    app.symbol_library.user.symbols.push(UserSymbol {
        code: symbol.code.clone(),
        file: relative.to_string_lossy().into_owned(),
        description: symbol.description.clone(),
        category: symbol.category.clone(),
        family: symbol.family.clone(),
        layer: symbol.layer.clone(),
    });
    app.symbol_library.save_user()?;
    app.symbol_library.load(&archive);
    app.symbol_library.selected = app.symbol_library.symbols.iter().position(|candidate| candidate.code.eq_ignore_ascii_case(&symbol.code));
    app.open_path(&destination);
    Ok(())
}

fn hide_symbol(app: &mut CadApp, code: &str) -> Result<(), String> {
    let archive = app.symbol_library.source.clone().ok_or_else(|| "Seleziona prima una libreria ZIP.".to_string())?;
    app.symbol_library.user.hidden.insert(code.to_ascii_uppercase());
    app.symbol_library.save_user()?;
    app.symbol_library.load(archive);
    Ok(())
}

/// Model-space unifilar drawings use canonical positive-integer layers as sheets.  When one of
/// those sheets is current, the block reference must follow the active sheet instead of the
/// catalog's planimetry layer; otherwise changing page would leave the newly inserted symbol on a
/// different sheet.  Names such as `01` are deliberately not pages, consistently with the sheet
/// navigation and management commands.
fn active_sheet_layer(drawing: &Drawing) -> Option<String> {
    let current = drawing.header.str("CLAYER", "0");
    let number = current.parse::<u64>().ok()?;
    (number > 0 && number.to_string() == current && drawing.layer(&current).is_some()).then_some(current)
}

fn insert(app: &mut CadApp, symbol: &Symbol) -> Result<(), String> {
    let space = app.session.space();
    let state = app.session.state().map_err(|error| error.to_string())?;
    let entities_before =
        state.doc.space(&space).map(|entities| entities.len()).ok_or_else(|| "No drawing is available for symbol insertion.".to_string())?;
    // Legacy-compatible model-space sheets store their paper-to-model factor in DIMSCALE:
    // 1:50 m = 0.05, 1:100 m = 0.1, 1:200 m = 0.2. Drawings without a usable value stay 1:1.
    let scale = state.doc.header.f64("DIMSCALE", 1.0);
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let target_layer = active_sheet_layer(&state.doc).unwrap_or_else(|| symbol.layer.trim().to_string());
    let pending = PendingInsert {
        uid: state.uid,
        space,
        entities_before,
        block: symbol.code.clone(),
        description: symbol.description.clone(),
        layer: target_layer,
    };
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
    // Preset only the standard scale.  After the insertion point INSERT deliberately keeps its
    // rotation step: the user sees the block pivot around the chosen point and confirms the
    // direction with a second click (or presses Enter to keep zero degrees).
    app.session.input(Input::Keyword("Scale".into())).map_err(|error| error.to_string())?;
    app.session.input(Input::Text(scale.to_string())).map_err(|error| error.to_string())?;
    if !app.session.current_prompt().is_some_and(|prompt| prompt.accept.point) {
        return Err(format!("CADCraft could not prepare {} for insertion.", symbol.code));
    }
    app.symbol_library.pending = Some(pending);
    Ok(())
}

/// A library insertion asks for its point and rotation. After those graphical steps, accept every
/// attribute's stored default and finish INSERT without opening another form. The catalog
/// description is stored in the standard block description so legends and other CAD programs can
/// reuse it. The loop is bounded because imported drawings are hostile input.
pub fn tick(app: &mut CadApp, _ctx: &egui::Context) {
    let Some(pending) = app.symbol_library.pending.clone() else { return };
    if !app.session.state().is_ok_and(|state| state.uid == pending.uid) {
        app.symbol_library.pending = None;
        return;
    }
    let attribute_prompt =
        |app: &CadApp| app.session.current_prompt().is_some_and(|prompt| prompt.accept.text && !prompt.accept.point && !prompt.accept.number);
    if attribute_prompt(app) {
        for _ in 0..=4096 {
            if !attribute_prompt(app) {
                break;
            }
            if let Err(error) = app.session.input(Input::Enter) {
                app.session.echo(error.to_string());
                app.session.cancel();
                app.symbol_library.pending = None;
                return;
            }
        }
        if attribute_prompt(app) {
            app.session.echo("Symbol insertion stopped: more than 4096 attribute prompts.");
            app.session.cancel();
            app.symbol_library.pending = None;
            return;
        }
    }
    if app.session.current_prompt().is_some() {
        return;
    }
    app.symbol_library.pending = None;
    let inserted = app.session.doc().ok().and_then(|drawing| {
        let entities = drawing.space(&pending.space)?;
        if entities.len() != pending.entities_before.saturating_add(1) {
            return None;
        }
        entities.last().filter(|entity| matches!(&entity.kind, EntityKind::Insert(_))).map(|entity| entity.handle)
    });
    if !pending.layer.is_empty()
        && let Some(handle) = inserted
    {
        if app.session.doc().is_ok_and(|drawing| drawing.layer(&pending.layer).is_none())
            && let Err(error) = app.run("layer.new", json!({ "name": pending.layer.clone() }))
        {
            app.session.echo(error);
        }
        if app.session.doc().is_ok_and(|drawing| drawing.layer(&pending.layer).is_some())
            && let Err(error) = app.run("properties.set", json!({ "handles": [handle.hex()], "layer": pending.layer.clone() }))
        {
            app.session.echo(error);
        }
    }
    if !pending.description.trim().is_empty()
        && let Err(error) = app.run("blocks.description", json!({ "name": pending.block, "description": pending.description }))
    {
        app.session.echo(error);
    }
}

/// Copy catalog descriptions into matching standard block definitions. This also upgrades blocks
/// inserted by an earlier CADCraft build, so a newly generated legend does not fall back to codes.
pub fn sync_descriptions(app: &mut CadApp) {
    let updates = app.session.doc().map_or_else(
        |_| Vec::new(),
        |drawing| {
            app.symbol_library
                .symbols
                .iter()
                .filter(|symbol| !symbol.description.trim().is_empty())
                .filter_map(|symbol| {
                    drawing
                        .block(&symbol.code)
                        .filter(|block| block.description != symbol.description)
                        .map(|_| (symbol.code.clone(), symbol.description.clone()))
                })
                .collect::<Vec<_>>()
        },
    );
    for (name, description) in updates {
        if let Err(error) = app.run("blocks.description", json!({ "name": name, "description": description })) {
            app.session.echo(error);
        }
    }
}

/// Draw the reusable symbol-browser contents. Returns `true` after an insertion starts so the
/// legacy floating dialog can close; the docked panel deliberately stays open.
fn contents(app: &mut CadApp, ui: &mut egui::Ui) -> bool {
    let mut picked = None;
    let mut picked_symbol = None;
    let mut import_from_drawing = false;
    let mut insert_symbol = None;
    let mut edit_symbol = None;
    let mut edit_graphic = None;
    let mut remove_symbol = None;
    let mut restore_removed = false;
    ui.horizontal(|ui| {
        if ui.button(crate::tl!("Add library…")).clicked() {
            picked = app.services.pick_symbol_library.as_ref().and_then(|picker| picker());
        }
        if let Some(source) = app.symbol_library.source_path() {
            ui.label(RichText::new(source.file_name().unwrap_or_default().to_string_lossy()).strong());
            ui.label(format!("{} {}", app.symbol_library.symbols.len(), crate::tl!("symbols")));
            if app.symbol_library.personal_in_archive {
                ui.label(RichText::new("Personale integrata nello ZIP").small().color(Tokens::get().text_dim));
            }
        } else {
            ui.label(crate::tl!("Select a ZIP symbol library."));
        }
    });
    if let Some(error) = &app.symbol_library.error {
        ui.colored_label(Tokens::get().warn, error);
    }
    if let Some(catalog) = &app.symbol_library.catalog_path {
        ui.label(format!("Catalogo: {}", catalog.file_name().unwrap_or_default().to_string_lossy()));
    }
    if let Some(warning) = &app.symbol_library.catalog_warning {
        ui.colored_label(Tokens::get().warn, warning);
    }
    if !app.symbol_library.symbols.is_empty() {
        ui.separator();
        ui.group(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("GESTIONE LIBRERIA").strong());
                let mode_label = if app.symbol_library.manage_mode { "Termina gestione" } else { "Gestisci blocchi…" };
                if ui.selectable_label(app.symbol_library.manage_mode, mode_label).clicked() {
                    app.symbol_library.manage_mode = !app.symbol_library.manage_mode;
                }
            });
            if app.symbol_library.manage_mode {
                ui.label("Seleziona un blocco nell’elenco, quindi scegli l’operazione.");
                if ui.button("Aggiungi nuovo blocco DWG/DXF…").clicked() {
                    picked_symbol = app.services.pick_symbol_file.as_ref().and_then(|picker| picker());
                }
                if ui.button("Aggiungi dal disegno…").clicked() {
                    import_from_drawing = true;
                }
                ui.label(RichText::new("Per importare dal disegno, seleziona prima un solo blocco elettrico.").small().color(Tokens::get().text_dim));
                let selected_symbol = app.symbol_library.selected.and_then(|index| app.symbol_library.symbols.get(index)).cloned();
                if let Some(symbol) = &selected_symbol {
                    ui.label(format!("Blocco selezionato: {}", symbol.code));
                } else {
                    ui.label("Nessun blocco selezionato");
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(selected_symbol.is_some(), egui::Button::new("Modifica dati…")).clicked() {
                        edit_symbol = selected_symbol.clone();
                    }
                    if ui.add_enabled(selected_symbol.is_some(), egui::Button::new("Modifica grafica")).clicked() {
                        edit_graphic = selected_symbol.clone();
                    }
                    if ui.add_enabled(selected_symbol.is_some(), egui::Button::new("Rimuovi…")).clicked() {
                        remove_symbol = selected_symbol;
                    }
                });
                if !app.symbol_library.user.hidden.is_empty() && ui.small_button("Ripristina blocchi rimossi").clicked() {
                    restore_removed = true;
                }
            } else {
                ui.label(RichText::new("Premi “Gestisci blocchi…” per aggiungere, modificare o rimuovere.").small().color(Tokens::get().text_dim));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            let categories: BTreeSet<String> = app
                .symbol_library
                .symbols
                .iter()
                .map(|symbol| symbol.category.trim())
                .filter(|category| !category.is_empty())
                .map(str::to_string)
                .collect();
            let category_label = ui.label("Sezione");
            let category_combo = craft_ui::ComboBox::from_id_salt("symbol-library-category")
                .selected_text(if app.symbol_library.category.is_empty() { "Tutti i simboli" } else { &app.symbol_library.category })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.symbol_library.category, String::new(), "Tutti i simboli");
                    for category in categories {
                        ui.selectable_value(&mut app.symbol_library.category, category.clone(), category);
                    }
                });
            category_combo.response.labelled_by(category_label.id);
        });
        ui.horizontal(|ui| {
            let search_label = ui.label(crate::tl!("Search"));
            ui.text_edit_singleline(&mut app.symbol_library.search).labelled_by(search_label.id);
        });
        ui.separator();

        let needle = app.symbol_library.search.trim().to_ascii_lowercase();
        let category = app.symbol_library.category.clone();
        let filtered: Vec<usize> = app
            .symbol_library
            .symbols
            .iter()
            .enumerate()
            .filter_map(|(index, symbol)| {
                let in_category = category.is_empty() || symbol.category.eq_ignore_ascii_case(&category);
                let searchable =
                    format!("{} {} {} {} {}", symbol.code, symbol.description, symbol.category, symbol.family, symbol.layer).to_ascii_lowercase();
                (in_category && (needle.is_empty() || searchable.contains(&needle))).then_some(index)
            })
            .collect();
        let mut selected = app.symbol_library.selected;
        let list_height = (ui.available_height() - 44.0).max(120.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(list_height).show_rows(ui, 82.0, filtered.len(), |ui, rows| {
            for row in rows {
                let Some(index) = filtered.get(row).copied() else { continue };
                let Some(symbol) = app.symbol_library.symbols.get(index).cloned() else { continue };
                let preview = app.symbol_library.preview(&symbol);
                let row_response = ui.horizontal(|ui| {
                    let image = draw_preview(ui, &preview);
                    let label = ui.vertical(|ui| {
                        let title =
                            if symbol.description.is_empty() { symbol.code.clone() } else { format!("{}  {}", symbol.code, symbol.description) };
                        let label = ui.selectable_label(selected == Some(index), RichText::new(title).size(15.0));
                        let details = [symbol.category.as_str(), symbol.family.as_str(), symbol.layer.as_str()]
                            .into_iter()
                            .filter(|value| !value.trim().is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ");
                        if !details.is_empty() {
                            ui.label(RichText::new(details).small().color(Tokens::get().text_dim));
                        }
                        label
                    });
                    (image, label)
                });
                let clicked = row_response.inner.0.clicked() || row_response.inner.1.inner.clicked();
                if clicked {
                    selected = Some(index);
                    if !app.symbol_library.manage_mode {
                        insert_symbol = Some(symbol);
                    }
                }
                if let Preview::Failed(error) = preview {
                    row_response.response.on_hover_text(error);
                }
            }
        });
        app.symbol_library.selected = selected;
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let selected_symbol = selected.and_then(|index| app.symbol_library.symbols.get(index)).cloned();
            if ui.add_enabled(selected_symbol.is_some(), egui::Button::new(crate::tl!("Insert symbol"))).clicked() {
                insert_symbol = selected_symbol;
            }
        });
        let help = if app.symbol_library.manage_mode {
            "Modalità Gestisci: seleziona un simbolo, poi modifica i dati, la grafica o rimuovilo."
        } else {
            "Un clic sul simbolo, poi indica posizione e rotazione. Invio mantiene 0°."
        };
        ui.label(RichText::new(help).small().color(Tokens::get().text_dim));
    }
    if let Some(path) = picked {
        app.symbol_library.load(path);
    }
    if let Some(path) = picked_symbol
        && let Err(error) = add_personal_symbol(app, Path::new(&path))
    {
        app.symbol_library.error = Some(error);
    }
    if import_from_drawing && let Err(error) = app.run("ui.symbols.from_drawing", Value::Null) {
        app.symbol_library.error = Some(error);
    }
    if let Some(symbol) = edit_symbol {
        app.symbol_library.editor = Some(SymbolEdit::from(&symbol));
    }
    if let Some(symbol) = edit_graphic
        && let Err(error) = open_personal_graphic(app, &symbol)
    {
        app.symbol_library.error = Some(error);
    }
    if let Some(symbol) = remove_symbol {
        app.symbol_library.delete_confirm = Some(symbol.code);
    }
    if restore_removed {
        let archive = app.symbol_library.source.clone();
        app.symbol_library.user.hidden.clear();
        match app.symbol_library.save_user() {
            Ok(()) => {
                if let Some(archive) = archive {
                    app.symbol_library.load(archive);
                }
            }
            Err(error) => app.symbol_library.error = Some(error),
        }
    }
    symbol_editor(app, ui.ctx());
    drawing_import_editor(app, ui.ctx());
    remove_confirmation(app, ui.ctx());
    if let Some(symbol) = insert_symbol {
        match insert(app, &symbol) {
            Ok(()) => return true,
            Err(error) => {
                app.session.echo(error.clone());
                app.symbol_library.error = Some(error);
            }
        }
    }
    false
}

fn symbol_editor(app: &mut CadApp, ctx: &egui::Context) {
    let Some(mut edit) = app.symbol_library.editor.clone() else { return };
    let (sections, layers) = editor_choices(app, &edit);
    let mut keep_open = true;
    let mut save = false;
    let mut cancel = false;
    egui::Window::new("Modifica dati simbolo")
        .id(egui::Id::unique("symbol-library-editor"))
        .collapsible(false)
        .resizable(false)
        .open(&mut keep_open)
        .show(ctx, |ui| {
            ui.label(RichText::new(&edit.code).strong());
            egui::Grid::new("symbol-library-editor-grid").num_columns(2).show(ui, |ui| {
                ui.label("Descrizione");
                ui.text_edit_singleline(&mut edit.description);
                ui.end_row();
                let section_label = ui.label("Sezione");
                let section_combo = craft_ui::ComboBox::from_id_salt("symbol-editor-section")
                    .selected_text(if edit.category.trim().is_empty() { "Seleziona…" } else { &edit.category })
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for section in &sections {
                            ui.selectable_value(&mut edit.category, section.clone(), section);
                        }
                    });
                section_combo.response.labelled_by(section_label.id);
                ui.end_row();
                ui.label("Famiglia");
                ui.text_edit_singleline(&mut edit.family);
                ui.end_row();
                let layer_label = ui.label("Layer elettrico");
                let layer_combo = craft_ui::ComboBox::from_id_salt("symbol-editor-layer")
                    .selected_text(if edit.layer.trim().is_empty() { "Seleziona…" } else { &edit.layer })
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for layer in &layers {
                            ui.selectable_value(&mut edit.layer, layer.clone(), layer);
                        }
                    });
                layer_combo.response.labelled_by(layer_label.id);
                ui.end_row();
            });
            ui.label(RichText::new("Sezione: valori del catalogo. Layer elettrico: valori della libreria e del disegno aperto.").small());
            ui.label(RichText::new("I dati vengono salvati nel catalogo personale; la libreria originale importata resta invariata.").small());
            ui.horizontal(|ui| {
                save = ui.button("Salva").clicked();
                cancel = ui.button("Annulla").clicked();
            });
        });
    if save {
        match save_symbol_edit(app, &edit) {
            Ok(()) => app.symbol_library.editor = None,
            Err(error) => {
                app.symbol_library.error = Some(error);
                app.symbol_library.editor = Some(edit);
            }
        }
    } else if cancel || !keep_open {
        app.symbol_library.editor = None;
    } else {
        app.symbol_library.editor = Some(edit);
    }
}

fn drawing_import_editor(app: &mut CadApp, ctx: &egui::Context) {
    let Some(mut import) = app.symbol_library.drawing_import.clone() else { return };
    let (sections, layers) = editor_choices(app, &import.edit);
    let duplicate_now = app.symbol_library.symbols.iter().any(|symbol| symbol.code.eq_ignore_ascii_case(import.edit.code.trim()));
    let mut keep_open = true;
    let mut save = false;
    let mut cancel = false;
    egui::Window::new("Aggiungi blocco dal disegno")
        .id(egui::Id::unique("symbol-library-import-drawing"))
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .open(&mut keep_open)
        .show(ctx, |ui| {
            ui.label(format!("Definizione sorgente: {}", import.source_block));
            ui.label(format!(
                "Scala del riferimento: X {:.6} · Y {:.6} · Z {:.6}",
                import.reference_scale[0], import.reference_scale[1], import.reference_scale[2]
            ));
            ui.label(format!("Scala simboli del disegno: {:.6}", import.drawing_scale));
            ui.label(
                RichText::new(
                    "La definizione viene salvata 1:1 con il proprio punto base. La scala del riferimento selezionato non viene incorporata.",
                )
                .small()
                .color(Tokens::get().text_dim),
            );
            for warning in &import.warnings {
                ui.colored_label(Tokens::get().warn, warning);
            }
            ui.separator();
            egui::Grid::new("symbol-library-import-drawing-grid").num_columns(2).spacing([12.0, 7.0]).show(ui, |ui| {
                ui.label("Nome simbolo");
                ui.text_edit_singleline(&mut import.edit.code);
                ui.end_row();
                ui.label("Descrizione");
                ui.text_edit_singleline(&mut import.edit.description);
                ui.end_row();
                let section_label = ui.label("Sezione");
                let section_combo = craft_ui::ComboBox::from_id_salt("symbol-import-section")
                    .selected_text(if import.edit.category.trim().is_empty() { "Seleziona…" } else { &import.edit.category })
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for section in &sections {
                            ui.selectable_value(&mut import.edit.category, section.clone(), section);
                        }
                    });
                section_combo.response.labelled_by(section_label.id);
                ui.end_row();
                ui.label("Famiglia");
                ui.text_edit_singleline(&mut import.edit.family);
                ui.end_row();
                let layer_label = ui.label("Layer elettrico");
                let layer_combo = craft_ui::ComboBox::from_id_salt("symbol-import-layer")
                    .selected_text(if import.edit.layer.trim().is_empty() { "Seleziona…" } else { &import.edit.layer })
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for layer in &layers {
                            ui.selectable_value(&mut import.edit.layer, layer.clone(), layer);
                        }
                    });
                layer_combo.response.labelled_by(layer_label.id);
                ui.end_row();
            });
            if duplicate_now {
                ui.separator();
                ui.colored_label(Tokens::get().warn, format!("Il nome {} è già presente nella libreria.", import.edit.code.trim()));
                ui.radio_value(&mut import.replace_existing, true, "Sostituisci con una copia personale dal disegno");
                ui.radio_value(&mut import.replace_existing, false, "Salva con un nuovo nome (modificare Nome simbolo)");
            } else if import.duplicate {
                ui.label(
                    RichText::new("È stato indicato un nuovo nome: il simbolo originale resterà invariato.").small().color(Tokens::get().text_dim),
                );
                import.replace_existing = false;
            }
            if let Some(error) = &import.error {
                ui.colored_label(Tokens::get().warn, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                save = ui.button("Salva nella libreria").clicked();
                cancel = ui.button("Annulla").clicked();
            });
        });
    if save {
        match save_drawing_import(app, &import) {
            Ok(()) => {
                app.symbol_library.drawing_import = None;
                app.session.echo(format!("Simbolo {} aggiunto alla libreria personale.", import.edit.code.trim()));
            }
            Err(error) => {
                import.error = Some(error);
                app.symbol_library.drawing_import = Some(import);
            }
        }
    } else if cancel || !keep_open {
        app.symbol_library.drawing_import = None;
    } else {
        app.symbol_library.drawing_import = Some(import);
    }
}

fn remove_confirmation(app: &mut CadApp, ctx: &egui::Context) {
    let Some(code) = app.symbol_library.delete_confirm.clone() else { return };
    let mut open = true;
    let mut remove = false;
    let mut cancel = false;
    egui::Window::new("Rimuovi simbolo").id(egui::Id::unique("symbol-library-remove")).collapsible(false).resizable(false).open(&mut open).show(
        ctx,
        |ui| {
            ui.label(format!("Nascondere {code} dalla libreria?"));
            ui.label("Il file non verrà cancellato e potrà essere ripristinato.");
            ui.horizontal(|ui| {
                remove = ui.button("Rimuovi").clicked();
                cancel = ui.button("Annulla").clicked();
            });
        },
    );
    if remove {
        match hide_symbol(app, &code) {
            Ok(()) => app.symbol_library.delete_confirm = None,
            Err(error) => {
                app.symbol_library.error = Some(error);
                app.symbol_library.delete_confirm = Some(code);
            }
        }
    } else if cancel || !open {
        app.symbol_library.delete_confirm = None;
    }
}

pub fn panel(app: &mut CadApp, ui: &mut egui::Ui) {
    app.symbol_library.ensure_default_loaded();
    let close = ui
        .horizontal(|ui| ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.small_button(crate::tl!("Close"))).inner.clicked())
        .inner;
    let _ = contents(app, ui);
    if close
        && let Err(error) = crate::docking::command(app, false, &json!({ "operation": "close", "panel": crate::docking::Panel::SymbolLibrary.id() }))
    {
        app.set_status(error);
    }
}

pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let mut inserted = false;
    egui::Window::new(crate::tl!("Symbol Library"))
        .id(egui::Id::unique("Symbol Library"))
        .open(open)
        .default_size(vec2(380.0, 620.0))
        .min_size(vec2(280.0, 400.0))
        .show(ctx, |ui| inserted = contents(app, ui));
    if inserted {
        *open = false;
    }
}

/// `SIMBOLI` opens the window; `simboli {"path":"library.zip"}` loads a catalog without a
/// file dialog, for scripts and the control channel.
pub fn route(app: &mut CadApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if matches!(id, "simbolodaldisegno" | "ui.symbols.from_drawing") {
        if !params.is_null() {
            return Some(Err("simbolodaldisegno: il comando grafico non accetta parametri".into()));
        }
        return Some(begin_import_from_selection(app).map(|()| json!({ "dialog": "symbol-from-drawing" })));
    }
    if !matches!(id, "simboli" | "libreriasimboli" | "ui.dialog.symbols") {
        return None;
    }
    if params.is_null() {
        return Some(crate::docking::command(app, false, &json!({ "operation": "open", "panel": crate::docking::Panel::SymbolLibrary.id() })));
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
    use crate::Services;
    use cadcraft_engine::Session;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use serde_json::json;
    use zip::write::SimpleFileOptions;

    fn test_catalog(path: &Path) -> Result<(), String> {
        let file = OpenOptions::new().write(true).create_new(true).open(path).map_err(|error| error.to_string())?;
        let mut archive = zip::ZipWriter::new(file);
        let files = [
            (
                "xl/workbook.xml",
                r#"<x:workbook xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><x:sheets><x:sheet name="Catalogo" sheetId="1" r:id="rId1"/></x:sheets></x:workbook>"#,
            ),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships><Relationship Id="rId1" Target="/xl/worksheets/sheet1.xml"/></Relationships>"#),
            (
                "xl/worksheets/sheet1.xml",
                r#"<x:worksheet xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><x:sheetData><x:row r="1"><x:c r="A1" t="inlineStr"><x:is><x:t>Codice</x:t></x:is></x:c><x:c r="B1" t="inlineStr"><x:is><x:t>Descrizione</x:t></x:is></x:c><x:c r="C1" t="inlineStr"><x:is><x:t>Categoria</x:t></x:is></x:c><x:c r="D1" t="inlineStr"><x:is><x:t>Famiglia</x:t></x:is></x:c><x:c r="E1" t="inlineStr"><x:is><x:t>Layer nel DXF</x:t></x:is></x:c></x:row><x:row r="2"><x:c r="A2" t="inlineStr"><x:is><x:t>CA6</x:t></x:is></x:c><x:c r="B2" t="inlineStr"><x:is><x:t>Rivelatore ottico</x:t></x:is></x:c><x:c r="C2" t="inlineStr"><x:is><x:t>Antincendio</x:t></x:is></x:c><x:c r="D2" t="inlineStr"><x:is><x:t>Rivelazione</x:t></x:is></x:c><x:c r="E2" t="inlineStr"><x:is><x:t>VVF</x:t></x:is></x:c></x:row></x:sheetData></x:worksheet>"#,
            ),
        ];
        for (name, contents) in files {
            archive.start_file(name, SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(contents.as_bytes()).map_err(|error| error.to_string())?;
        }
        archive.finish().map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn symbol_names_are_case_insensitive_and_path_safe() {
        assert!(symbol_extension("planimetria/PRESA.DWG"));
        assert!(symbol_extension("a/b.dxf"));
        assert!(!symbol_extension("a/b.bak"));
        assert_eq!(symbol_code("quadri/INTERRUTTORE.dwg").as_deref(), Some("INTERRUTTORE"));
        assert_eq!(symbol_code("quadri/.dwg"), None);
    }

    #[test]
    fn thumbnail_shows_notec_value_but_hides_other_attribute_definitions() {
        use cadcraft_doc::{Attrib, Common, Text};
        use cadcraft_geom::Vec3;

        let attribute = |tag: &str, value: &str| {
            EntityKind::AttDef(Attrib {
                tag: tag.into(),
                text: Text {
                    insert: Vec3::ZERO,
                    align_pt: None,
                    height: 1.0,
                    value: value.into(),
                    rotation: 0.0,
                    width_factor: 1.0,
                    oblique: 0.0,
                    style: "Standard".into(),
                    halign: Default::default(),
                    valign: Default::default(),
                },
                invisible: true,
                constant: false,
                prompt: String::new(),
                props: Default::default(),
                multiline: None,
                modes: Default::default(),
            })
        };
        let mut drawing = Drawing::new_metric();
        assert!(drawing.add(&Space::Model, Common::default(), attribute("NOTEC", "EDP")).is_ok());
        assert!(drawing.add(&Space::Model, Common::default(), attribute("DESCRIZIONE", "Presa dati")).is_ok());
        assert!(drawing.add(&Space::Model, Common::default(), attribute("NOTEC", "")).is_ok());

        let preview = preview_entities(&drawing);
        assert_eq!(preview.len(), 1);
        assert!(matches!(&preview[0].kind, EntityKind::Text(text) if text.value == "EDP"));
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

    #[test]
    fn personal_sidecar_replaces_overrides_and_reversibly_hides_symbols() {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("cadcraft-symbol-personal-{}-{id}", std::process::id()));
        assert!(std::fs::create_dir(&folder).is_ok());
        let library = folder.join("Library.zip");
        let personal_folder = folder.join("Library-CADCraft");
        assert!(std::fs::create_dir(&personal_folder).is_ok());
        let personal_file = personal_folder.join("A.dxf");
        assert!(std::fs::write(&personal_file, b"personal symbol").is_ok());
        let archive_result: Result<(), String> = (|| {
            let file = OpenOptions::new().write(true).create_new(true).open(&library).map_err(|error| error.to_string())?;
            let mut archive = zip::ZipWriter::new(file);
            archive.start_file("A.dwg", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"base A").map_err(|error| error.to_string())?;
            archive.start_file("B.dwg", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"base B").map_err(|error| error.to_string())?;
            archive.finish().map_err(|error| error.to_string())?;
            Ok(())
        })();
        assert!(archive_result.is_ok(), "{archive_result:?}");
        let mut user = UserLibrary { version: 1, ..UserLibrary::default() };
        user.symbols.push(UserSymbol {
            code: "A".into(),
            file: "Library-CADCraft/A.dxf".into(),
            description: "Personale".into(),
            category: "Personale".into(),
            family: String::new(),
            layer: "FM".into(),
        });
        user.hidden.insert("B".into());
        user.overrides.insert(
            "A".into(),
            CatalogRow { description: "Presa personalizzata".into(), category: "Forza motrice".into(), family: "Prese".into(), layer: "FM".into() },
        );
        let saved = save_user_library(&sidecar_path(&library), &user);
        assert!(saved.is_ok(), "{saved:?}");

        let mut state = State::default();
        state.load(&library);
        assert!(state.error.is_none(), "{:?}", state.error);
        assert_eq!(state.symbols.len(), 1);
        let symbol = state.symbols.first();
        assert_eq!(symbol.map(|symbol| symbol.code.as_str()), Some("A"));
        assert_eq!(symbol.map(|symbol| symbol.description.as_str()), Some("Presa personalizzata"));
        assert!(symbol.is_some_and(|symbol| symbol.file.as_deref() == Some(personal_file.as_path())));
        let bytes = symbol.map(|symbol| read_entry(&library, symbol));
        assert!(matches!(bytes, Some(Ok(bytes)) if bytes == b"personal symbol"));
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn portable_personal_folder_is_updated_atomically_and_uses_saved_catalogue_data() {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("cadcraft-symbol-portable-{}-{id}", std::process::id()));
        assert!(std::fs::create_dir(&folder).is_ok());
        let library = folder.join("Library.zip");
        let archive_result: Result<(), String> = (|| {
            let file = OpenOptions::new().write(true).create_new(true).open(&library).map_err(|error| error.to_string())?;
            let mut archive = zip::ZipWriter::new(file);
            archive.start_file("OLD.dwg", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"base old").map_err(|error| error.to_string())?;
            archive.add_directory("libPersonale/", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.start_file("libPersonale/OLD.dxf", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"previous personal").map_err(|error| error.to_string())?;
            archive.start_file("KEEP.dxf", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"keep me").map_err(|error| error.to_string())?;
            archive.finish().map_err(|error| error.to_string())?;
            Ok(())
        })();
        assert!(archive_result.is_ok(), "{archive_result:?}");
        assert_eq!(archive_uses_personal_folder(&library), Ok(true));
        let updated = write_personal_archive_symbol(&library, "OLD", b"new personal drawing");
        assert_eq!(updated.as_deref(), Ok("libPersonale/OLD.dxf"));
        assert!(folder.join("Library.zip.cadcraft-original.zip").is_file());

        let file = File::open(&library);
        assert!(file.is_ok());
        if let Ok(file) = file {
            let mut archive = ZipArchive::new(file).unwrap();
            let old_entries = (0..archive.len())
                .filter_map(|index| archive.by_index(index).ok().and_then(|entry| entry.name().ok().map(|name| name.into_owned())))
                .filter(|name| personal_archive_entry(name) && symbol_code(name).as_deref() == Some("OLD"))
                .count();
            assert_eq!(old_entries, 1);
            let mut personal = archive.by_name("libPersonale/OLD.dxf").unwrap();
            let mut bytes = Vec::new();
            assert!(personal.read_to_end(&mut bytes).is_ok());
            assert_eq!(bytes, b"new personal drawing");
        }

        let mut user = UserLibrary { version: 1, ..UserLibrary::default() };
        user.symbols.push(UserSymbol {
            code: "OLD".into(),
            file: "libPersonale/OLD.dxf".into(),
            description: "Descrizione personale".into(),
            category: "6 Impiantistica".into(),
            family: "Prova".into(),
            layer: "FM".into(),
        });
        assert!(save_user_library(&sidecar_path(&library), &user).is_ok());
        let mut state = State::default();
        state.load(&library);
        let personal = state.symbols.iter().find(|symbol| symbol.code == "OLD");
        assert_eq!(personal.map(|symbol| symbol.entry.as_str()), Some("libPersonale/OLD.dxf"));
        assert_eq!(personal.map(|symbol| symbol.description.as_str()), Some("Descrizione personale"));
        assert_eq!(personal.map(|symbol| symbol.category.as_str()), Some("6 Impiantistica"));
        assert_eq!(personal.map(|symbol| symbol.layer.as_str()), Some("FM"));
        assert_eq!(personal.map(|symbol| read_entry(&library, symbol)), Some(Ok(b"new personal drawing".to_vec())));
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn malformed_personal_sidecar_warns_and_keeps_base_library_available() {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("cadcraft-symbol-bad-sidecar-{}-{id}", std::process::id()));
        assert!(std::fs::create_dir(&folder).is_ok());
        let library = folder.join("Library.zip");
        let result: Result<(), String> = (|| {
            let file = OpenOptions::new().write(true).create_new(true).open(&library).map_err(|error| error.to_string())?;
            let mut archive = zip::ZipWriter::new(file);
            archive.start_file("A.dwg", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"base A").map_err(|error| error.to_string())?;
            archive.finish().map_err(|error| error.to_string())?;
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
        assert!(std::fs::write(sidecar_path(&library), b"{").is_ok());
        let mut state = State::default();
        state.load(&library);
        assert!(state.error.is_none());
        assert_eq!(state.symbols.len(), 1);
        assert!(state.catalog_warning.as_deref().is_some_and(|warning| warning.contains("non valido")));
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn management_controls_fit_the_symbol_panel() {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("cadcraft-symbol-panel-{}-{id}", std::process::id()));
        assert!(std::fs::create_dir(&folder).is_ok());
        let library = folder.join("Library.zip");
        let result: Result<(), String> = (|| {
            let file = OpenOptions::new().write(true).create_new(true).open(&library).map_err(|error| error.to_string())?;
            let mut archive = zip::ZipWriter::new(file);
            archive.start_file("PRESA.dwg", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"invalid preview is handled").map_err(|error| error.to_string())?;
            archive.finish().map_err(|error| error.to_string())?;
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
        let mut app = CadApp::new(Session::new(), Services::default());
        app.symbol_library.load(&library);
        app.symbol_library.manage_mode = true;
        let mut harness = Harness::builder().with_size(vec2(430.0, 760.0)).build_ui_state(
            |ui, app: &mut CadApp| {
                panel(app, ui);
            },
            app,
        );
        harness.run_steps(3);
        for label in ["GESTIONE LIBRERIA", "Aggiungi nuovo blocco", "Aggiungi dal disegno", "Modifica dati", "Modifica grafica", "Rimuovi"] {
            assert!(harness.query_by_label_contains(label).is_some(), "missing control {label}");
        }
        let render_directory = std::env::var_os("CADCRAFT_SYMBOL_LIBRARY_SHOTS").map(PathBuf::from);
        if let Some(directory) = &render_directory {
            assert!(std::fs::create_dir_all(directory).is_ok());
            let image = harness.render();
            assert!(image.is_ok(), "{image:?}");
            if let Ok(image) = image {
                assert!(image.save(directory.join("symbol-library-management-check.png")).is_ok());
            }
        }
        harness.get_by_label("Modifica dati…").click();
        harness.run_steps(3);
        harness.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Layer elettrico");
        if let Some(directory) = render_directory {
            let image = harness.render();
            assert!(image.is_ok(), "{image:?}");
            if let Ok(image) = image {
                assert!(image.save(directory.join("symbol-library-editor-choices-check.png")).is_ok());
            }
        }
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn editor_choices_come_from_catalog_and_open_drawing() {
        let mut app = CadApp::new(Session::new(), Services::default());
        assert!(app.run("layer.new", json!({"name":"IMPIANTO LUCE"})).is_ok());
        app.symbol_library.symbols = vec![
            Symbol {
                code: "A".into(),
                entry: "A.dwg".into(),
                index: 0,
                bytes: 0,
                description: String::new(),
                category: "1 Unifilare".into(),
                family: String::new(),
                layer: "FM".into(),
                file: None,
            },
            Symbol {
                code: "B".into(),
                entry: "B.dwg".into(),
                index: 1,
                bytes: 0,
                description: String::new(),
                category: "Fotovoltaico".into(),
                family: String::new(),
                layer: "FV".into(),
                file: None,
            },
        ];
        let edit =
            SymbolEdit { code: "A".into(), description: String::new(), category: "1 Unifilare".into(), family: String::new(), layer: "FM".into() };
        let (sections, layers) = editor_choices(&app, &edit);
        assert_eq!(sections, vec!["1 Unifilare", "Fotovoltaico"]);
        assert!(layers.iter().any(|value| value == "FM"));
        assert!(layers.iter().any(|value| value == "FV"));
        assert!(layers.iter().any(|value| value == "IMPIANTO LUCE"));
        assert_eq!(layers.iter().filter(|value| value.eq_ignore_ascii_case("FM")).count(), 1);
    }

    fn drawing_with_symbol(scale: f64) -> (Drawing, cadcraft_doc::Handle) {
        use cadcraft_doc::{Block, Common, Insert, Line};
        use cadcraft_geom::Vec3;

        let mut drawing = Drawing::new_metric();
        drawing.header.set_f64("DIMSCALE", scale);
        let mut block = Block::new("PRESA_TEST");
        block.base = Vec3::new(2.0, 3.0, 0.0);
        block.description = "Presa di prova".into();
        block.entities.push(Entity {
            handle: cadcraft_doc::Handle(0x500),
            common: Common { layer: "FM".into(), ..Common::default() },
            kind: EntityKind::Line(Line { a: Vec3::new(2.0, 3.0, 0.0), b: Vec3::new(12.0, 3.0, 0.0) }),
        });
        drawing.blocks.insert(block.name.clone(), Arc::new(block));
        let reference = Insert {
            block: "PRESA_TEST".into(),
            insert: Vec3::new(100.0, 200.0, 0.0),
            scale: Vec3::new(scale, scale, scale),
            rotation: 0.0,
            attribs: Vec::new(),
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
            clip: None,
        };
        let handle = drawing.add(&Space::Model, Common { layer: "FM".into(), ..Common::default() }, EntityKind::Insert(reference)).unwrap();
        (drawing, handle)
    }

    #[test]
    fn drawing_import_keeps_block_definition_at_one_to_one_and_preserves_base() {
        use cadcraft_geom::Vec3;

        let (drawing, _) = drawing_with_symbol(0.1);
        let exported = standalone_block_drawing(&drawing, "PRESA_TEST");
        assert!(exported.is_ok(), "{exported:?}");
        let exported = exported.unwrap_or_else(|_| Drawing::new_metric());
        assert_eq!(exported.header.point("INSBASE"), Some(Vec3::new(2.0, 3.0, 0.0)));
        assert_eq!(exported.model.len(), 1);
        assert!(
            matches!(&exported.model.iter().next().map(|entity| &entity.kind), Some(EntityKind::Line(line)) if line.a == Vec3::new(2.0, 3.0, 0.0) && line.b == Vec3::new(12.0, 3.0, 0.0))
        );
        assert!(exported.block("PRESA_TEST").is_none(), "the root definition becomes model-space geometry");
    }

    #[test]
    fn selected_reference_opens_import_without_baking_its_scale() {
        let (drawing, handle) = drawing_with_symbol(0.1);
        let mut app = CadApp::new(Session::new(), Services::default());
        *app.session.doc_mut().unwrap() = drawing;
        app.session.set_selection(vec![handle]);
        app.symbol_library.symbols.push(Symbol {
            code: "BASE".into(),
            entry: "BASE.dxf".into(),
            index: 0,
            bytes: 0,
            description: String::new(),
            category: "Planimetrico".into(),
            family: String::new(),
            layer: "FM".into(),
            file: None,
        });
        let result = begin_import_from_selection(&mut app);
        assert!(result.is_ok(), "{result:?}");
        let import = app.symbol_library.drawing_import.as_ref();
        assert_eq!(import.map(|state| state.source_block.as_str()), Some("PRESA_TEST"));
        assert_eq!(import.map(|state| state.reference_scale), Some([0.1, 0.1, 0.1]));
        assert_eq!(import.map(|state| state.drawing_scale), Some(0.1));
        assert!(import.is_some_and(|state| state.warnings.is_empty()));
    }

    #[test]
    fn architecture_and_title_blocks_are_rejected_from_electrical_library() {
        use cadcraft_doc::{Block, Common, Insert};
        use cadcraft_geom::Vec3;

        let (mut drawing, _) = drawing_with_symbol(0.1);
        let architectural = drawing
            .add(
                &Space::Model,
                Common { layer: "ARCH|MURI".into(), ..Common::default() },
                EntityKind::Insert(Insert {
                    block: "PRESA_TEST".into(),
                    insert: Vec3::ZERO,
                    scale: Vec3::new(1.0, 1.0, 1.0),
                    rotation: 0.0,
                    attribs: Vec::new(),
                    cols: 1,
                    rows: 1,
                    col_spacing: 0.0,
                    row_spacing: 0.0,
                    clip: None,
                }),
            )
            .unwrap();
        drawing.blocks.insert("Cart_pl".into(), Arc::new(Block::new("Cart_pl")));
        let title = drawing
            .add(
                &Space::Model,
                Common { layer: "FM".into(), ..Common::default() },
                EntityKind::Insert(Insert {
                    block: "Cart_pl".into(),
                    insert: Vec3::ZERO,
                    scale: Vec3::new(1.0, 1.0, 1.0),
                    rotation: 0.0,
                    attribs: Vec::new(),
                    cols: 1,
                    rows: 1,
                    col_spacing: 0.0,
                    row_spacing: 0.0,
                    clip: None,
                }),
            )
            .unwrap();
        let mut app = CadApp::new(Session::new(), Services::default());
        *app.session.doc_mut().unwrap() = drawing;
        app.session.set_selection(vec![architectural]);
        assert!(begin_import_from_selection(&mut app).is_err());
        app.session.set_selection(vec![title]);
        assert!(begin_import_from_selection(&mut app).is_err());
    }

    #[test]
    fn saving_selected_block_creates_reusable_personal_dxf() {
        use cadcraft_geom::Vec3;

        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("cadcraft-symbol-from-drawing-{}-{id}", std::process::id()));
        assert!(std::fs::create_dir(&folder).is_ok());
        let library = folder.join("Library.zip");
        let archive_result: Result<(), String> = (|| {
            let file = OpenOptions::new().write(true).create_new(true).open(&library).map_err(|error| error.to_string())?;
            let mut archive = zip::ZipWriter::new(file);
            archive.start_file("BASE.dxf", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"synthetic base symbol").map_err(|error| error.to_string())?;
            archive.add_directory("libPersonale/", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.finish().map_err(|error| error.to_string())?;
            Ok(())
        })();
        assert!(archive_result.is_ok(), "{archive_result:?}");

        let (drawing, handle) = drawing_with_symbol(0.1);
        let mut app = CadApp::new(Session::new(), Services::default());
        *app.session.doc_mut().unwrap() = drawing;
        app.session.set_selection(vec![handle]);
        app.symbol_library.load(&library);
        if let Some(base) = app.symbol_library.symbols.first_mut() {
            base.category = "Planimetrico".into();
            base.layer = "FM".into();
        }
        assert!(begin_import_from_selection(&mut app).is_ok());
        let import = app.symbol_library.drawing_import.clone();
        assert!(import.is_some());
        if let Some(import) = import {
            let saved = save_drawing_import(&mut app, &import);
            assert!(saved.is_ok(), "{saved:?}");
        }
        let symbol = app.symbol_library.symbols.iter().find(|symbol| symbol.code == "PRESA_TEST");
        assert!(symbol.is_some());
        assert_eq!(symbol.map(|symbol| symbol.entry.as_str()), Some("libPersonale/PRESA_TEST.dxf"));
        assert!(symbol.is_some_and(|symbol| symbol.file.is_none()));
        assert!(folder.join("Library.zip.cadcraft-original.zip").is_file());
        let bytes = symbol.map(|symbol| read_entry(&library, symbol));
        assert!(matches!(&bytes, Some(Ok(bytes)) if !bytes.is_empty()));
        if let Some(Ok(bytes)) = bytes {
            let reopened = cadcraft_io::read(&bytes, "PRESA_TEST.dxf");
            assert!(reopened.is_ok(), "{reopened:?}");
            if let Ok(reopened) = reopened {
                assert_eq!(reopened.header.point("INSBASE"), Some(Vec3::new(2.0, 3.0, 0.0)));
                assert_eq!(reopened.model.len(), 1);
            }
        }
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn import_from_drawing_is_a_graphical_checked_workflow() {
        let (drawing, handle) = drawing_with_symbol(0.1);
        let mut app = CadApp::new(Session::new(), Services::default());
        *app.session.doc_mut().unwrap() = drawing;
        app.session.set_selection(vec![handle]);
        app.symbol_library.symbols.push(Symbol {
            code: "BASE".into(),
            entry: "BASE.dxf".into(),
            index: 0,
            bytes: 0,
            description: "Simbolo base".into(),
            category: "Planimetrico".into(),
            family: String::new(),
            layer: "FM".into(),
            file: None,
        });
        app.symbol_library.manage_mode = true;
        let mut harness = Harness::builder().with_size(vec2(760.0, 820.0)).build_ui_state(
            |ui, app: &mut CadApp| {
                panel(app, ui);
            },
            app,
        );
        harness.run_steps(3);
        harness.get_by_label("Aggiungi dal disegno…").click();
        harness.run_steps(3);
        assert!(harness.query_by_label_contains("Aggiungi blocco dal disegno").is_some());
        assert!(harness.query_by_label_contains("Scala del riferimento").is_some());
        assert!(harness.query_by_label_contains("salvata 1:1").is_some());
        assert!(harness.query_by_label_contains("Salva nella libreria").is_some());
        if let Some(directory) = std::env::var_os("CADCRAFT_SYMBOL_LIBRARY_SHOTS").map(PathBuf::from) {
            assert!(std::fs::create_dir_all(&directory).is_ok());
            let image = harness.render();
            assert!(image.is_ok(), "{image:?}");
            if let Ok(image) = image {
                assert!(image.save(directory.join("symbol-library-import-from-drawing.png")).is_ok());
            }
        }
    }

    #[test]
    fn adjacent_xlsx_catalog_adds_imported_description_category_family_and_layer() {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("cadcraft-symbol-catalog-{}-{id}", std::process::id()));
        assert!(std::fs::create_dir(&folder).is_ok());
        let library = folder.join("Libreria_CADCraft_Elettro.zip");
        let catalog = folder.join("Catalogo_simboli_CadCraft.xlsx");
        let result: Result<(), String> = (|| {
            let file = OpenOptions::new().write(true).create_new(true).open(&library).map_err(|error| error.to_string())?;
            let mut archive = zip::ZipWriter::new(file);
            archive.start_file("libElettro02/CA6.dwg", SimpleFileOptions::default()).map_err(|error| error.to_string())?;
            archive.write_all(b"synthetic drawing fixture").map_err(|error| error.to_string())?;
            archive.finish().map_err(|error| error.to_string())?;
            test_catalog(&catalog)?;
            Ok(())
        })();
        assert!(result.is_ok(), "{result:?}");
        let mut state = State::default();
        state.load(&library);
        assert!(state.error.is_none(), "{:?}", state.error);
        assert!(state.catalog_warning.is_none(), "{:?}", state.catalog_warning);
        let symbol = state.symbols.first();
        assert_eq!(symbol.map(|symbol| symbol.code.as_str()), Some("CA6"));
        assert_eq!(symbol.map(|symbol| symbol.description.as_str()), Some("Rivelatore ottico"));
        assert_eq!(symbol.map(|symbol| symbol.category.as_str()), Some("Antincendio"));
        assert_eq!(symbol.map(|symbol| symbol.family.as_str()), Some("Rivelazione"));
        assert_eq!(symbol.map(|symbol| symbol.layer.as_str()), Some("VVF"));
        let _ = std::fs::remove_file(catalog);
        let _ = std::fs::remove_file(library);
        let _ = std::fs::remove_dir(folder);
    }

    #[test]
    fn library_insert_uses_the_active_numeric_sheet_instead_of_the_catalog_layer() {
        let mut app = CadApp::new(Session::new(), Services::default());
        assert!(app.run("line", json!({"points":[[0,0],[1,0]]})).is_ok());
        let handles: Vec<_> = app.session.doc().map(|drawing| drawing.model.iter().map(|entity| entity.handle.hex()).collect()).unwrap_or_default();
        assert!(app.run("block", json!({"name": "INTERRUTTORE", "base": [0,0], "handles": handles, "keep": "delete"})).is_ok());
        assert!(app.run("layer.new", json!({"name": "2"})).is_ok());
        assert!(app.run("layer.current", json!({"name": "2"})).is_ok());
        let symbol = Symbol {
            code: "INTERRUTTORE".into(),
            entry: "unifilari/INTERRUTTORE.dwg".into(),
            index: 0,
            bytes: 0,
            description: "Interruttore".into(),
            category: "Schemi unifilari".into(),
            family: String::new(),
            layer: "SCHEMA_ELETTRICO".into(),
            file: None,
        };

        assert!(insert(&mut app, &symbol).is_ok());
        assert!(app.session.input(Input::Point(Vec2::new(10.0, 20.0))).is_ok());
        assert!(app.session.input(Input::Enter).is_ok());
        tick(&mut app, &egui::Context::default());

        let inserted_layer = app.session.doc().ok().and_then(|drawing| {
            drawing.model.iter().find_map(|entity| matches!(&entity.kind, EntityKind::Insert(_)).then_some(entity.common.layer.as_str()))
        });
        assert_eq!(inserted_layer, Some("2"));
        assert!(app.session.doc().is_ok_and(|drawing| drawing.layer("SCHEMA_ELETTRICO").is_none()));
        assert_eq!(app.session.doc().ok().map(|drawing| drawing.header.str("CLAYER", "0")).as_deref(), Some("2"));
    }

    #[test]
    fn library_insert_asks_for_rotation_then_uses_defaults_without_another_form() {
        let mut app = CadApp::new(Session::new(), Services::default());
        assert!(app.run("setvar", json!({"name":"DIMSCALE", "value":0.1})).is_ok());
        for (tag, value, y) in [("SIGLA", "Q1", 1), ("DESCRIZIONE", "Quadro", 2)] {
            assert!(app.run("attdef", json!({"tag": tag, "default": value, "at": [0, y]})).is_ok());
        }
        let handles: Vec<_> = app.session.doc().map(|drawing| drawing.model.iter().map(|entity| entity.handle.hex()).collect()).unwrap_or_default();
        assert!(app.run("block", json!({"name": "QUADRO", "base": [0,0], "handles": handles, "keep": "delete"})).is_ok());
        let symbol = Symbol {
            code: "QUADRO".into(),
            entry: "quadri/QUADRO.dwg".into(),
            index: 0,
            bytes: 0,
            description: "Quadro elettrico".into(),
            category: String::new(),
            family: String::new(),
            layer: "SIMBOLI ELETTRICI".into(),
            file: None,
        };
        assert!(insert(&mut app, &symbol).is_ok());
        assert!(app.session.current_prompt().is_some_and(|prompt| prompt.accept.point));
        assert!(app.session.input(Input::Point(Vec2::new(10.0, 20.0))).is_ok());
        assert!(app.session.current_prompt().is_some_and(|prompt| prompt.accept.point && prompt.accept.number));
        assert!(app.session.input(Input::Point(Vec2::new(10.0, 30.0))).is_ok());
        assert!(app.session.current_prompt().is_some_and(|prompt| prompt.accept.text));
        tick(&mut app, &egui::Context::default());
        assert!(app.session.editing_attributes().is_none());
        let inserted = app.session.doc().ok().and_then(|drawing| {
            drawing.model.iter().find_map(|entity| match &entity.kind {
                EntityKind::Insert(insert) => Some(insert),
                _ => None,
            })
        });
        assert_eq!(inserted.map(|insert| insert.scale.x), Some(0.1));
        assert!(inserted.is_some_and(|insert| (insert.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-12));
        assert_eq!(
            inserted.map(|insert| insert.attribs.iter().map(|attribute| attribute.text.value.as_str()).collect::<Vec<_>>()),
            Some(vec!["Q1", "Quadro"])
        );
        assert_eq!(
            app.session.doc().ok().and_then(|drawing| drawing.block("QUADRO")).map(|block| block.description.as_str()),
            Some("Quadro elettrico")
        );
        let inserted_layer = app.session.doc().ok().and_then(|drawing| {
            drawing.model.iter().find_map(|entity| matches!(&entity.kind, EntityKind::Insert(_)).then_some(entity.common.layer.as_str()))
        });
        assert_eq!(inserted_layer, Some("SIMBOLI ELETTRICI"));
        assert!(app.session.doc().is_ok_and(|drawing| drawing.layer("SIMBOLI ELETTRICI").is_some()));
        assert_eq!(app.session.doc().ok().map(|drawing| drawing.header.str("CLAYER", "0")).as_deref(), Some("0"));
    }
}
