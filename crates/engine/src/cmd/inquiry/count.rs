//! COUNT, COUNTLIST and COUNTTABLE: block references counted in model space, in the layouts and
//! nested inside other blocks (multiplied by the references holding them), or in a selection; and
//! the objects shaped like a picked one.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use cadcraft_doc::{Attrib, Drawing, Entity, EntityKind, Handle, Insert, MAX_BLOCK_DEPTH, Space};
use cadcraft_geom::{Mat3, Vec2, Vec3};
use serde_json::{Value, json};

use super::super::{bad, bool_or, point_req, size_param, str_param, targets};

use super::SelfCheck;
use crate::{Accept, Input, Interactive, Prompt, Result, Session, Step};

/// Merges done while expanding nested references; a hostile file stops here (and says so).
const MAX_WORK: u64 = 5_000_000;
/// Top-level objects reported (and highlighted) per count.
const MAX_HANDLES: usize = 100_000;

/// COUNT's JSON parameters.
pub(super) const PARAMS: &str = "{block?, object?: handle (count objects like this one), space?: current|model|all|<layout> (default current), \
     handles?: [...] (count in these objects), nested?: bool (default true), highlight?: bool} → \
     {blocks: {name: n}, nested: {name: n}, spaces: {space: {name: n}}, total} | {block, count, nested, handles}, plus `message` and \
     `selfCheck` (found/analyzed/skipped counts with reasons, and the caps hit: work, handles, blockDepth)";

/// COUNT's interactive command-line prompt machine.
pub(super) fn machine() -> Box<dyn Interactive> {
    Box::new(CountM)
}

/// COUNTTABLE's interactive command-line prompt machine (it asks for the insertion point).
pub(super) fn table_machine() -> Box<dyn Interactive> {
    Box::new(CountTableM)
}

/// SYMBOLLEGEND's prompt machine: active page or a pick/window selection, quantities, point.
pub(super) fn legend_machine() -> Box<dyn Interactive> {
    Box::new(SymbolLegendM::default())
}

/// Where COUNT looks.
#[derive(Clone, Debug, PartialEq)]
enum Scope {
    Space(Space),
    All,
    /// These top-level objects (a selection).
    Objects(Vec<Handle>),
}

impl Scope {
    fn parse(s: &Session, cmd: &str, p: &Value, default_all: bool) -> Result<Scope> {
        if p.get("handles").is_some_and(|v| !v.is_null()) || p.get("handle").is_some_and(|v| !v.is_null()) {
            return Ok(Scope::Objects(targets(s, p)?));
        }
        let given = match p.get("space") {
            None | Some(Value::Null) => None,
            Some(Value::String(t)) => Some(t.trim().to_string()),
            Some(v) => return Err(bad(cmd, format!("`space` must be current, model, all or a layout name, got {v}"))),
        };
        match given.as_deref().map(str::to_ascii_lowercase).as_deref() {
            None if default_all => Ok(Scope::All),
            None | Some("current") => Ok(Scope::Space(s.space())),
            Some("all") => Ok(Scope::All),
            Some("model") => Ok(Scope::Space(Space::Model)),
            Some(_) => {
                let name = given.unwrap_or_default();
                let d = s.doc()?;
                let l =
                    d.layouts.iter().find(|l| l.name.eq_ignore_ascii_case(&name)).ok_or_else(|| bad(cmd, format!("no layout named \"{name}\"")))?;
                Ok(Scope::Space(Space::Paper(l.name.clone())))
            }
        }
    }
    /// "model space", "layout Layout1", "all spaces", "the selection".
    fn label(&self) -> String {
        match self {
            Scope::Space(Space::Model) => "model space".into(),
            Scope::Space(Space::Paper(n)) => format!("layout {n}"),
            Scope::All => "all spaces".into(),
            Scope::Objects(_) => "the selection".into(),
        }
    }
    /// The top-level objects in scope, each with the name of its space.
    fn objects<'a>(&self, d: &'a Drawing) -> Vec<(String, &'a std::sync::Arc<Entity>)> {
        let name = |sp: &Space| sp.tab_name().to_string();
        let mut out = Vec::new();
        let mut add = |sp: &Space| {
            if let Some(store) = d.space(sp) {
                out.extend(store.iter().map(|e| (name(sp), e)));
            }
        };
        match self {
            Scope::Space(sp) => add(sp),
            Scope::All => {
                add(&Space::Model);
                for l in &d.layouts {
                    add(&Space::Paper(l.name.clone()));
                }
            }
            Scope::Objects(hs) => {
                return hs.iter().filter_map(|h| Some((d.space_of(*h).map_or_else(|| "Model".into(), |sp| name(&sp)), d.entity(*h)?))).collect();
            }
        }
        out
    }
}

/// Copies one reference places (a MINSERT's rows × columns).
fn copies(i: &Insert) -> u64 {
    u64::from(i.cols.max(1)).saturating_mul(u64::from(i.rows.max(1)))
}

/// Nested block references per block definition, memoised by (block, levels left) so shared and
/// cyclic definitions don't expand exponentially.
struct Expander<'a> {
    d: &'a Drawing,
    memo: HashMap<(String, usize), Rc<BTreeMap<String, u64>>>,
    work: u64,
    truncated: bool,
    /// A block at the end of the depth cap still held references.
    depth_capped: bool,
}

impl<'a> Expander<'a> {
    /// An expander that has not traversed any block yet.
    fn new(d: &'a Drawing) -> Self {
        Expander { d, memo: HashMap::new(), work: 0, truncated: false, depth_capped: false }
    }
    /// The definition's own name for a referenced block (names are case-insensitive).
    fn canonical(&self, name: &str) -> String {
        self.d.block(name).map_or_else(|| name.to_string(), |b| b.name.clone())
    }
    /// References inside block `name` and its nested blocks, `levels` deep: name → copies.
    fn expand(&mut self, name: &str, levels: usize) -> Rc<BTreeMap<String, u64>> {
        let key = (name.to_ascii_lowercase(), levels);
        if let Some(m) = self.memo.get(&key) {
            return m.clone();
        }
        if levels == 0 {
            // More references than MAX_BLOCK_DEPTH covers: say so rather than silently stop.
            if self.d.block(name).is_some_and(|b| b.entities.iter().any(|e| matches!(&e.kind, EntityKind::Insert(_)))) {
                self.depth_capped = true;
            }
            let out = Rc::new(BTreeMap::new());
            self.memo.insert(key, out.clone());
            return out;
        }
        let mut out: BTreeMap<String, u64> = BTreeMap::new();
        if let Some(b) = self.d.block(name) {
            let b = b.clone();
            for e in b.entities.iter() {
                let EntityKind::Insert(i) = &e.kind else { continue };
                if self.work >= MAX_WORK {
                    self.truncated = true;
                    break;
                }
                let m = copies(i);
                let inner = self.canonical(&i.block);
                let sub = self.expand(&inner, levels - 1);
                self.work = self.work.saturating_add(1).saturating_add(sub.len() as u64);
                add(&mut out, &inner, m);
                for (k, v) in sub.iter() {
                    add(&mut out, k, m.saturating_mul(*v));
                }
            }
        }
        let out = Rc::new(out);
        self.memo.insert(key, out.clone());
        out
    }
}

fn add(m: &mut BTreeMap<String, u64>, k: &str, n: u64) {
    let v = m.entry(k.to_string()).or_default();
    *v = v.saturating_add(n);
}

/// Anonymous blocks (`*U3`: arrays, dynamic block copies) are traversed but not reported.
fn reported(name: &str) -> bool {
    !name.starts_with('*')
}

#[derive(Default)]
struct Tally {
    /// Every reference: top-level and nested.
    blocks: BTreeMap<String, u64>,
    /// The nested part of `blocks`.
    nested: BTreeMap<String, u64>,
    /// Per space (Model or a layout): name → every reference there.
    spaces: BTreeMap<String, BTreeMap<String, u64>>,
    /// Top-level objects holding `want` (when one block is asked for).
    handles: Vec<Handle>,
    truncated: bool,
    /// Top-level objects scanned, and the block references among them.
    found: u64,
    analyzed: u64,
    /// Top-level objects that are not block references, by type.
    skipped: BTreeMap<&'static str, u64>,
    /// More top-level objects held the block than `MAX_HANDLES`.
    handles_capped: bool,
    depth_capped: bool,
}

/// Count block references in `scope`; with `want`, also collect the top-level objects holding it.
fn tally(d: &Drawing, scope: &Scope, nested: bool, want: Option<&str>) -> Tally {
    let mut x = Expander::new(d);
    let mut t = Tally::default();
    for (space, e) in scope.objects(d) {
        t.found = t.found.saturating_add(1);
        let EntityKind::Insert(i) = &e.kind else {
            let n = t.skipped.entry(e.kind.type_name()).or_default();
            *n = n.saturating_add(1);
            continue;
        };
        t.analyzed = t.analyzed.saturating_add(1);
        let name = x.canonical(&i.block);
        let m = copies(i);
        let mut holds = want.is_some_and(|w| w.eq_ignore_ascii_case(&name));
        let per = t.spaces.entry(space).or_default();
        if reported(&name) {
            add(&mut t.blocks, &name, m);
            add(per, &name, m);
        }
        if nested {
            let sub = x.expand(&name, MAX_BLOCK_DEPTH.saturating_sub(1));
            for (k, v) in sub.iter().filter(|(k, _)| reported(k)) {
                let n = m.saturating_mul(*v);
                add(&mut t.blocks, k, n);
                add(&mut t.nested, k, n);
                add(per, k, n);
                holds |= want.is_some_and(|w| w.eq_ignore_ascii_case(k));
            }
        }
        if holds {
            if t.handles.len() < MAX_HANDLES {
                t.handles.push(e.handle);
            } else {
                t.handles_capped = true;
            }
        }
    }
    t.spaces.retain(|_, m| !m.is_empty());
    t.truncated = x.truncated;
    t.depth_capped = x.depth_capped;
    t
}

/// The self-check for a COUNT-style result: what was scanned, counted and skipped, and the caps
/// that stopped it.
fn self_check(t: &Tally) -> Value {
    let mut check = SelfCheck::new();
    check.found(t.found);
    check.analyzed(t.analyzed);
    for (ty, n) in &t.skipped {
        check.skip(ty, "not a block reference", *n);
    }
    if t.truncated {
        check.cap("work", MAX_WORK);
    }
    if t.handles_capped {
        check.cap("handles", u64::try_from(MAX_HANDLES).unwrap_or(u64::MAX));
    }
    if t.depth_capped {
        check.cap("blockDepth", u64::try_from(MAX_BLOCK_DEPTH).unwrap_or(u64::MAX));
    }
    check.to_json()
}

/// A translation-free signature of an object's geometry: objects with equal keys are copies of
/// each other (moved, not rotated or scaled). Coordinates are rounded to 1e-6.
fn shape_key(k: &EntityKind) -> Option<String> {
    let origin = k.grips().first().copied().filter(|p| p.is_finite())?;
    let mut k = k.clone();
    k.transform(&Mat3::translate(Vec2::ZERO - origin));
    let mut v = serde_json::to_value(&k).ok()?;
    round(&mut v, 0);
    Some(v.to_string())
}

fn round(v: &mut Value, depth: usize) {
    if depth > 64 {
        return;
    }
    match v {
        Value::Number(n) if n.is_f64() => {
            if let Some(f) = n.as_f64() {
                let r = (f * 1e6).round() / 1e6;
                *v = json!(if r == 0.0 { 0.0 } else { r });
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| round(x, depth + 1)),
        Value::Object(o) => o.values_mut().for_each(|x| round(x, depth + 1)),
        _ => {}
    }
}

fn hex(hs: &[Handle]) -> Vec<String> {
    hs.iter().map(|h| h.hex()).collect()
}

fn count_line(name: &str, n: u64, nested: u64) -> String {
    if nested > 0 { format!("  {name}: {n} ({nested} nested)") } else { format!("  {name}: {n}") }
}

/// The per-block list as JSON plus the command-line text.
fn list(t: &Tally, scope: &Scope) -> Value {
    let wh = scope.label();
    let message = if t.blocks.is_empty() {
        format!("No block references in {wh}.")
    } else {
        let mut m = format!("Block references in {wh}:");
        for (name, n) in &t.blocks {
            m.push('\n');
            m.push_str(&count_line(name, *n, t.nested.get(name).copied().unwrap_or(0)));
        }
        m.push_str(&format!("\n  Total: {}", t.blocks.values().fold(0u64, |a, b| a.saturating_add(*b))));
        if t.truncated {
            m.push_str("\n  (nesting too large: counts are partial)");
        }
        m
    };
    let total = t.blocks.values().fold(0u64, |a, b| a.saturating_add(*b));
    json!({
        "blocks": t.blocks,
        "nested": t.nested,
        "spaces": t.spaces,
        "total": total,
        "truncated": t.truncated,
        "selfCheck": self_check(t),
        "message": message,
    })
}

/// Count block `b` (its references, nested ones included) in `scope`.
fn count_block(d: &Drawing, scope: &Scope, nested: bool, b: &str) -> Value {
    let t = tally(d, scope, nested, Some(b));
    let name = d.block(b).map_or_else(|| b.to_string(), |blk| blk.name.clone());
    let n = t.blocks.get(&name).copied().unwrap_or(0);
    let k = t.nested.get(&name).copied().unwrap_or(0);
    let wh = scope.label();
    let message = if k > 0 { format!("Block {name}: {n} in {wh} ({k} nested).") } else { format!("Block {name}: {n} in {wh}.") };
    json!({
        "block": name,
        "count": n,
        "nested": k,
        "handles": hex(&t.handles),
        "truncated": t.truncated,
        "selfCheck": self_check(&t),
        "message": message,
    })
}

/// Count the objects in `scope` shaped like `h` (a block reference: references of its block).
fn count_like(d: &Drawing, scope: &Scope, nested: bool, h: Handle) -> Result<Value> {
    let e = d.entity(h).ok_or_else(|| bad("count", format!("no object with handle {}", h.hex())))?;
    if let EntityKind::Insert(i) = &e.kind {
        return Ok(count_block(d, scope, nested, &i.block));
    }
    let ty = e.kind.type_name();
    let key = shape_key(&e.kind).ok_or_else(|| bad("count", format!("a {ty} can't be counted by shape")))?;
    let mut check = SelfCheck::new();
    let mut hs: Vec<Handle> = Vec::new();
    let mut capped = false;
    for (_, o) in scope.objects(d) {
        check.found(1);
        if o.kind.type_name() != ty {
            check.skip(o.kind.type_name(), "a different type", 1);
            continue;
        }
        check.analyzed(1);
        if shape_key(&o.kind).as_deref() == Some(key.as_str()) {
            if hs.len() < MAX_HANDLES {
                hs.push(o.handle);
            } else {
                capped = true;
            }
        }
    }
    if capped {
        check.cap("handles", u64::try_from(MAX_HANDLES).unwrap_or(u64::MAX));
    }
    let n = hs.len();
    let message = format!("{ty}: {n} like the selected one in {}.", scope.label());
    Ok(json!({ "object": ty, "count": n, "handles": hex(&hs), "selfCheck": check.to_json(), "message": message }))
}

/// The body of COUNT and COUNTLIST: list the blocks in scope, count one block, or count the
/// objects shaped like a picked one. `default_all` makes an omitted `space` mean every space
/// (COUNTLIST) instead of the current one (COUNT).
fn run(s: &mut Session, cmd: &str, p: &Value, default_all: bool) -> Result<Value> {
    let scope = Scope::parse(s, cmd, p, default_all)?;
    let nested = bool_or(p, "nested", true);
    let d = s.doc()?;
    let r = if let Some(b) = str_param(p, "block") {
        count_block(d, &scope, nested, b)
    } else if let Some(v) = p.get("object").filter(|v| !v.is_null()) {
        let h = v
            .as_str()
            .and_then(Handle::parse_hex)
            .or_else(|| v.as_u64().map(Handle))
            .ok_or_else(|| bad(cmd, format!("`object`: {v} is not an object handle")))?;
        count_like(d, &scope, nested, h)?
    } else {
        list(&tally(d, &scope, nested, None), &scope)
    };
    if bool_or(p, "highlight", false)
        && let Some(hs) = r.get("handles").and_then(Value::as_array)
    {
        let hs = hs.iter().filter_map(|v| v.as_str().and_then(Handle::parse_hex)).collect();
        s.set_selection(hs);
    }
    Ok(r)
}

pub(super) fn run_count(s: &mut Session, p: &Value) -> Result<Value> {
    run(s, "count", p, false)
}

pub(super) fn run_countlist(s: &mut Session, p: &Value) -> Result<Value> {
    run(s, "countlist", p, true)
}

/// The current table style (CTABLESTYLE, else the first).
fn table_style(s: &Session) -> cadcraft_doc::TableStyle {
    let Ok(d) = s.doc() else { return Default::default() };
    let cur = d.header.str("CTABLESTYLE", "Standard");
    d.table_styles.iter().find(|t| t.name.eq_ignore_ascii_case(&cur)).or(d.table_styles.first()).cloned().unwrap_or_default()
}

/// COUNTTABLE: insert a table of the block counts in scope at `at`, and report what the tally
/// covered (and refused: no block references in scope is an error, not an empty table).
pub(super) fn run_counttable(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "counttable";
    let at = point_req(CMD, p, "at")?;
    let scope = Scope::parse(s, CMD, p, false)?;
    let t = tally(s.doc()?, &scope, bool_or(p, "nested", true), None);
    if t.blocks.is_empty() {
        return Err(bad(CMD, format!("No block references in {}.", scope.label())));
    }
    let cells: Vec<Vec<String>> = t.blocks.iter().map(|(name, n)| vec![name.clone(), n.to_string()]).collect();
    let header = Some(vec!["Name".to_string(), "Count".to_string()]);
    let table = super::super::table::make_table(&table_style(s), at, cells.len(), 2, None, None, Some("Block Count"), header, &cells);
    let h = s.add_entity(EntityKind::Table(table))?;
    Ok(json!({
        "handle": h.hex(),
        "rows": cells.len(),
        "selfCheck": self_check(&t),
        "message": format!("Table of {} block(s) inserted.", cells.len()),
    }))
}

const LEGEND_GROUP_DESCRIPTION: &str = "CADCraft symbol legend";
const MAX_LEGEND_ROWS: usize = 1_000;
const LEGEND_SYMBOL_WIDTH_EM: f64 = 5.5;
const LEGEND_QUANTITY_WIDTH_EM: f64 = 5.5;
const LEGEND_TITLE_HEIGHT_EM: f64 = 3.2;
const LEGEND_HEADER_HEIGHT_EM: f64 = 1.8;
const LEGEND_DATA_HEIGHT_EM: f64 = 2.0;

#[derive(Clone)]
struct LegendRow {
    block: String,
    description: String,
    /// `NOTEC` is a per-reference electrical label (for example `10 A`, `EDP` or `TV`). Keep it in
    /// block coordinates so the fitted legend copy can place it with the symbol at any scale.
    notec: Option<Attrib>,
    count: u64,
}

/// Prefer the standard block-definition description (filled by symbol libraries), then a
/// readable instance attribute, and use the block code only as a last resort.
fn legend_description(d: &Drawing, i: &Insert) -> String {
    if let Some(description) = d.block(&i.block).map(|block| block.description.trim()).filter(|description| !description.is_empty()) {
        return description.to_string();
    }
    const TAGS: [&str; 9] = ["DESCRIZIONE", "DESCRIPTION", "DESC", "DENOMINAZIONE", "NOME", "OGGETTO", "UTENZA", "TIPO", "SIGLA"];
    TAGS.iter()
        .find_map(|tag| {
            i.attribs
                .iter()
                .find(|a| a.tag.eq_ignore_ascii_case(tag))
                .map(|a| a.text.value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| i.block.clone())
}

/// A visible `NOTEC` attribute expressed in the block definition's coordinates. Reference
/// attributes are stored in drawing coordinates, so normalize them before fitting the legend
/// symbol. Degenerate hostile insert transforms simply omit the note instead of failing.
fn legend_notec(d: &Drawing, insert: &Insert) -> Option<Attrib> {
    let block = d.block(&insert.block)?;
    let inverse = insert.transform(block.base.xy()).inverse()?;
    let mut note =
        insert.attribs.iter().find(|attribute| attribute.tag.eq_ignore_ascii_case("NOTEC") && !attribute.text.value.trim().is_empty())?.clone();
    note.invisible = false;
    let mut entity = EntityKind::AttDef(note);
    entity.transform(&inverse);
    match entity {
        EntityKind::AttDef(note) => Some(note),
        _ => None,
    }
}

/// Place a normalized legend note with one fitted block reference. Attributes draw in world
/// coordinates rather than inheriting the INSERT transform, hence this explicit transformation.
fn placed_legend_notec(d: &Drawing, row: &LegendRow, insert: &Insert) -> Vec<Attrib> {
    let (Some(note), Some(block)) = (&row.notec, d.block(&row.block)) else { return Vec::new() };
    let mut entity = EntityKind::AttDef(note.clone());
    entity.transform(&insert.transform(block.base.xy()));
    match entity {
        EntityKind::AttDef(note) => vec![note],
        _ => Vec::new(),
    }
}

/// Handles used only to display symbols inside an older generated legend must not be counted as
/// drawing symbols when the legend is made again.
fn legend_members(d: &Drawing) -> std::collections::HashSet<Handle> {
    d.groups.iter().filter(|group| group.description == LEGEND_GROUP_DESCRIPTION).flat_map(|group| group.members.iter().copied().skip(1)).collect()
}

fn legend_rows(s: &Session, p: &Value) -> Result<Vec<LegendRow>> {
    const CMD: &str = "symbollegend";
    let selected = p.get("handles").filter(|value| !value.is_null()).map(|_| targets(s, p)).transpose()?;
    let d = s.doc()?;
    let excluded = legend_members(d);
    let active = d.header.str("CLAYER", "0");
    let all_layers = bool_or(p, "allLayers", false);
    let electrical_only = bool_or(p, "electricalOnly", true);
    let requested_layer = match p.get("layer") {
        None => None,
        Some(Value::String(layer)) if !layer.trim().is_empty() => Some(layer.trim()),
        Some(Value::String(_)) => return Err(bad(CMD, "Layer name cannot be empty.")),
        Some(_) => return Err(bad(CMD, "layer must be a string.")),
    };
    let handles: Vec<Handle> = match selected {
        Some(handles) => handles,
        None => {
            let entities = d.space(&s.space()).ok_or_else(|| bad(CMD, "The active drawing space is not available."))?;
            if all_layers {
                entities.iter().map(|entity| entity.handle).collect()
            } else {
                let layer = requested_layer.unwrap_or(&active);
                let canonical = d.layer(layer).ok_or_else(|| bad(CMD, format!("Layer \"{layer}\" does not exist.")))?.name.as_str();
                entities.iter().filter(|entity| entity.common.layer.eq_ignore_ascii_case(canonical)).map(|entity| entity.handle).collect()
            }
        }
    };
    let mut rows: BTreeMap<String, LegendRow> = BTreeMap::new();
    for handle in handles.into_iter().take(MAX_HANDLES) {
        if excluded.contains(&handle) {
            continue;
        }
        let Some(entity) = d.entity(handle) else { continue };
        let EntityKind::Insert(insert) = &entity.kind else { continue };
        if insert.block.starts_with('*') || super::super::electrical_layers::is_title_block(d, &insert.block) {
            continue;
        }
        if electrical_only {
            let electrical_layer = super::super::electrical_layers::is_electrical_layer(&entity.common.layer);
            if !electrical_layer {
                continue;
            }
        }
        let block = d.block(&insert.block).map_or_else(|| insert.block.clone(), |definition| definition.name.clone());
        let notec = legend_notec(d, insert);
        // Different NOTEC values are different legend variants. Combining them would display one
        // arbitrary rating/label while reporting the quantity of every variant.
        let note_key = notec.as_ref().map(|attribute| attribute.text.value.trim().to_ascii_lowercase()).unwrap_or_default();
        let key = format!("{}\0{note_key}", block.to_ascii_lowercase());
        let copies = copies(insert);
        let row = rows.entry(key).or_insert_with(|| LegendRow { block: block.clone(), description: legend_description(d, insert), notec, count: 0 });
        row.count = row.count.saturating_add(copies);
        if rows.len() > MAX_LEGEND_ROWS {
            return Err(bad(CMD, format!("A legend can contain at most {MAX_LEGEND_ROWS} different symbols.")));
        }
    }
    if rows.is_empty() {
        return Err(bad(CMD, "No symbol block references were found."));
    }
    let mut rows: Vec<_> = rows.into_values().collect();
    rows.sort_by(|left, right| {
        left.description
            .to_ascii_lowercase()
            .cmp(&right.description.to_ascii_lowercase())
            .then_with(|| left.block.to_ascii_lowercase().cmp(&right.block.to_ascii_lowercase()))
    });
    Ok(rows)
}

fn symbollegend_table(s: &Session, rows: &[LegendRow], at: Vec2, quantity: bool, scale: f64) -> cadcraft_doc::Table {
    let columns = if quantity { 3 } else { 2 };
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let mut cells = vec![String::new(), row.description.clone()];
            if quantity {
                cells.push(row.count.to_string());
            }
            cells
        })
        .collect();
    let mut header = vec!["SEGNO".to_string(), "DESCRIZIONE".to_string()];
    if quantity {
        header.push("QUANTITÀ".into());
    }
    let mut style = table_style(s);
    // Imported electrical drawings often keep the generic 0.18 table style even though their
    // drawing text is 2.5 units high. TEXTSIZE is the useful drafting-scale signal in that case.
    let drawing_text = s.doc().map_or(style.text_height, |drawing| drawing.header.f64("TEXTSIZE", style.text_height));
    if drawing_text.is_finite() && drawing_text > 0.0 {
        style.text_height = style.text_height.max(drawing_text);
    }
    style.text_height *= scale;
    let mut table = super::super::table::make_table(&style, at, rows.len(), columns, None, None, Some("LEGENDA SEGNI GRAFICI"), Some(header), &cells);
    let text_height = table.text_height.abs().max(0.18);
    let symbol_width = text_height * LEGEND_SYMBOL_WIDTH_EM;
    let description_chars = rows.iter().map(|row| row.description.chars().count()).max().unwrap_or(18).clamp(18, 60) as f64;
    let description_width = text_height * (description_chars * 0.60 + 2.5);
    table.col_widths =
        if quantity { vec![symbol_width, description_width, text_height * LEGEND_QUANTITY_WIDTH_EM] } else { vec![symbol_width, description_width] };
    if let Some(height) = table.row_heights.first_mut() {
        *height = text_height * LEGEND_TITLE_HEIGHT_EM;
    }
    if let Some(height) = table.row_heights.get_mut(1) {
        *height = text_height * LEGEND_HEADER_HEIGHT_EM;
    }
    for height in table.row_heights.iter_mut().skip(2) {
        *height = text_height * LEGEND_DATA_HEIGHT_EM;
    }
    // Descriptions are secondary information: keep them legible but below the symbol and count
    // emphasis. The override must use the already-scaled table height, otherwise TEXTSIZE and
    // the legend's Scale field affect the headings but leave descriptions at the old size.
    let description_height = text_height * 0.78;
    for row in table.cells.iter_mut().skip(2) {
        if let Some(cell) = row.get_mut(1) {
            cell.text_height = Some(description_height);
        }
        if quantity && let Some(cell) = row.get_mut(2) {
            cell.style = "Header".into();
        }
    }
    table
}

enum LegendScope {
    Selection(Vec<Handle>),
    Layer(String),
    AllLayers,
}

fn symbollegend_params(scope: &LegendScope, quantity: bool, scale: f64, at: Vec2) -> Value {
    let mut params = json!({ "at": [at.x, at.y], "quantity": quantity, "scale": scale });
    match scope {
        LegendScope::Selection(handles) => params["handles"] = json!(hex(handles)),
        LegendScope::Layer(layer) => params["layer"] = json!(layer),
        LegendScope::AllLayers => params["allLayers"] = json!(true),
    }
    params
}

/// Insert the editable table and one fitted block reference over each first-column data cell.
/// The table and its graphic references form one selectable group and one Undo operation.
pub(super) fn run_symbollegend(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "symbollegend";
    let at = point_req(CMD, p, "at")?;
    let quantity = bool_or(p, "quantity", true);
    let scale = size_param(CMD, p, "scale", false)?.unwrap_or(1.0);
    if !(0.1..=10.0).contains(&scale) {
        return Err(bad(CMD, "scale must be between 0.1 and 10."));
    }
    let rows = legend_rows(s, p)?;
    let table = symbollegend_table(s, &rows, at, quantity, scale);
    let text_height = table.text_height.abs().max(0.18);
    let symbol_width = table.col_widths.first().copied().unwrap_or(text_height * 8.0);

    // Compute the fitted references before mutating the document. An empty or malformed block
    // leaves its cell blank instead of failing the whole operation.
    let mut graphics = Vec::new();
    let title_and_header_height: f64 = table.row_heights.iter().take(2).copied().sum();
    let data_height = table.row_heights.get(2).copied().unwrap_or(text_height * LEGEND_DATA_HEIGHT_EM);
    for (index, row) in rows.iter().enumerate() {
        let mut dummy_insert = Insert {
            block: row.block.clone(),
            insert: Vec3::ZERO,
            scale: Vec3::new(1.0, 1.0, 1.0),
            rotation: 0.0,
            attribs: Vec::new(),
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
            clip: None,
        };
        dummy_insert.attribs = placed_legend_notec(s.doc()?, row, &dummy_insert);
        let dummy = Entity::new(Handle(0), EntityKind::Insert(dummy_insert));
        let bounds = cadcraft_doc::entity_bounds(s.doc()?, &dummy, 0);
        if bounds.is_empty() || !bounds.width().is_finite() || !bounds.height().is_finite() {
            continue;
        }
        let fit_x = symbol_width * 0.72 / bounds.width().max(1e-12);
        let fit_y = data_height * 0.72 / bounds.height().max(1e-12);
        let scale = fit_x.min(fit_y).clamp(1e-9, 1e9);
        let center = Vec2::new(at.x + symbol_width / 2.0, at.y - title_and_header_height - data_height * (index as f64 + 0.5));
        let insert = center - bounds.center() * scale;
        let mut graphic = Insert {
            block: row.block.clone(),
            insert: insert.to3(0.0),
            scale: Vec3::new(scale, scale, 1.0),
            rotation: 0.0,
            attribs: Vec::new(),
            cols: 1,
            rows: 1,
            col_spacing: 0.0,
            row_spacing: 0.0,
            clip: None,
        };
        graphic.attribs = placed_legend_notec(s.doc()?, row, &graphic);
        graphics.push(EntityKind::Insert(graphic));
    }

    let table_handle = s.add_entity(EntityKind::Table(table))?;
    let mut members = vec![table_handle];
    for graphic in graphics {
        members.push(s.add_entity(graphic)?);
    }
    let group = super::super::groups::create(s, None, LEGEND_GROUP_DESCRIPTION, &members, true)?;
    Ok(json!({
        "handle": table_handle.hex(),
        "symbols": rows.len(),
        "quantity": quantity,
        "group": group,
        "message": format!("Symbol legend inserted: {} row(s).", rows.len()),
    }))
}

/// Echo a result's `message` at the command line, one line at a time.
fn echo_lines(s: &mut Session, r: &Value) {
    if let Some(m) = r.get("message").and_then(Value::as_str) {
        for l in m.lines() {
            s.echo(l.to_string());
        }
    }
}

/// COUNT at the command line: pick an object to count the ones like it (highlighted), select
/// several to list the blocks among them, or list the blocks in this space or in all of them.
struct CountM;

impl Interactive for CountM {
    fn name(&self) -> &'static str {
        "COUNT"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Select object to count or [List/Spaces]", Accept::SELECT).kw(&["List", "Spaces"]).default("List")
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let p = match i {
            Input::Pick(hs) => match hs.as_slice() {
                [] => return Ok(Step::Continue),
                [h] => json!({ "object": h.hex(), "highlight": true }),
                many => json!({ "handles": hex(many) }),
            },
            Input::Keyword(k) if k == "Spaces" => json!({ "space": "all" }),
            Input::Keyword(_) | Input::Enter => json!({}),
            _ => return Ok(Step::Continue),
        };
        let r = run(s, "count", &p, false)?;
        echo_lines(s, &r);
        Ok(Step::Done)
    }
}

struct CountTableM;

impl Interactive for CountTableM {
    fn name(&self) -> &'static str {
        "COUNTTABLE"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Specify insertion point", Accept::POINT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Point(at) => {
                let r = run_counttable(s, &json!({ "at": [at.x, at.y] }))?;
                echo_lines(s, &r);
                Ok(Step::Done)
            }
            Input::Enter => Ok(Step::Done),
            _ => Ok(Step::Continue),
        }
    }
}

#[derive(Default)]
struct SymbolLegendM {
    scope: Option<LegendScope>,
    ask_layer: bool,
    scale: Option<f64>,
    quantity: Option<bool>,
}

impl Interactive for SymbolLegendM {
    fn name(&self) -> &'static str {
        "SYMBOLLEGEND"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        if self.scale.is_none() {
            Prompt::new("Legend scale", Accept::NUMBER).default("1")
        } else if self.quantity.is_none() {
            Prompt::new("Include symbol quantities?", Accept::TEXT).kw(&["Yes", "No"]).default("Yes")
        } else if self.scope.is_none() && self.ask_layer {
            let active = s.doc().map_or_else(|_| "0".to_string(), |drawing| drawing.header.str("CLAYER", "0"));
            Prompt::new("Enter symbol layer name", Accept::TEXT).default(active)
        } else if self.scope.is_none() {
            Prompt::new("Select symbols by pick/window or [Layer/All]", Accept::SELECT).kw(&["Layer", "All"]).default("Layer")
        } else {
            Prompt::new("Specify legend insertion point", Accept::POINT)
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if self.scale.is_none() {
            match i {
                Input::Cancel => return Ok(Step::Cancel),
                Input::Text(value) => {
                    let scale = value.trim().parse::<f64>().ok().filter(|value| value.is_finite() && (0.1..=10.0).contains(value));
                    let Some(scale) = scale else {
                        s.echo("Scale must be between 0.1 and 10.");
                        return Ok(Step::Continue);
                    };
                    self.scale = Some(scale);
                }
                Input::Enter => self.scale = Some(1.0),
                _ => return Ok(Step::Continue),
            }
            return Ok(Step::Continue);
        }
        if self.quantity.is_none() {
            match i {
                Input::Cancel => return Ok(Step::Cancel),
                Input::Keyword(answer) => self.quantity = Some(answer == "Yes"),
                Input::Text(answer) => self.quantity = Some(!answer.trim().to_ascii_lowercase().starts_with('n')),
                Input::Enter => self.quantity = Some(true),
                _ => return Ok(Step::Continue),
            }
            return Ok(Step::Continue);
        }
        if self.scope.is_none() && self.ask_layer {
            match i {
                Input::Cancel => return Ok(Step::Cancel),
                Input::Text(layer) if !layer.trim().is_empty() => self.scope = Some(LegendScope::Layer(layer.trim().to_string())),
                Input::Enter => {
                    let active = s.doc()?.header.str("CLAYER", "0");
                    self.scope = Some(LegendScope::Layer(active));
                }
                _ => return Ok(Step::Continue),
            }
            self.ask_layer = false;
            return Ok(Step::Continue);
        }
        if self.scope.is_none() {
            match i {
                Input::Cancel => return Ok(Step::Cancel),
                Input::Pick(handles) if !handles.is_empty() => self.scope = Some(LegendScope::Selection(handles)),
                Input::Keyword(answer) if answer == "All" => self.scope = Some(LegendScope::AllLayers),
                Input::Keyword(answer) if answer == "Layer" => self.ask_layer = true,
                Input::Enter => self.ask_layer = true,
                _ => return Ok(Step::Continue),
            }
            return Ok(Step::Continue);
        }
        match i {
            Input::Cancel => Ok(Step::Cancel),
            Input::Point(at) => {
                let Some(scope) = self.scope.as_ref() else { return Ok(Step::Continue) };
                let params = symbollegend_params(scope, self.quantity.unwrap_or(true), self.scale.unwrap_or(1.0), at);
                let result = run_symbollegend(s, &params)?;
                echo_lines(s, &result);
                Ok(Step::Done)
            }
            // A right-click is translated to Enter by the desktop UI. At a point prompt it
            // confirms the current cursor position, just like a primary click, rather than
            // leaving the command apparently stuck.
            Input::Enter => {
                let Some(scope) = self.scope.as_ref() else { return Ok(Step::Continue) };
                let params = symbollegend_params(scope, self.quantity.unwrap_or(true), self.scale.unwrap_or(1.0), s.cursor);
                let result = run_symbollegend(s, &params)?;
                echo_lines(s, &result);
                Ok(Step::Done)
            }
            _ => Ok(Step::Continue),
        }
    }
    fn preview(&self, s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        let Some(quantity) = self.quantity else { return Vec::new() };
        let Some(scope) = self.scope.as_ref() else { return Vec::new() };
        let params = symbollegend_params(scope, quantity, self.scale.unwrap_or(1.0), cursor);
        let Ok(rows) = legend_rows(s, &params) else { return Vec::new() };
        vec![EntityKind::Table(symbollegend_table(s, &rows, cursor, quantity, self.scale.unwrap_or(1.0)))]
    }
}
