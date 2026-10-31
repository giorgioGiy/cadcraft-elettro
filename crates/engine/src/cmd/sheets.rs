//! Navigate drawings whose sheets are canonical positive integer layer names.
use super::{CommandSpec, Result, Session, Value, bad};
use serde_json::json;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("sheet.previous", "Foglio precedente", |s, _| navigate(s, false))
            .menu(&["View", "Fogli", "Foglio precedente"])
            .alias(&["fp"]),
        CommandSpec::new("sheet.next", "Foglio successivo", |s, _| navigate(s, true)).menu(&["View", "Fogli", "Foglio successivo"]).alias(&["av"]),
    ]
}

fn number(name: &str) -> Option<u64> {
    let n = name.parse::<u64>().ok()?;
    (n > 0 && n.to_string() == name).then_some(n)
}

fn navigate(s: &mut Session, forward: bool) -> Result<Value> {
    let d = s.doc_mut()?;
    let current = d.header.str("CLAYER", "0");
    let destination = match number(&current) {
        Some(n) if forward => n.checked_add(1),
        Some(n) => n.checked_sub(1).filter(|n| *n > 0),
        None => Some(1),
    }
    .ok_or_else(|| bad("sheet", "Nessun foglio disponibile in questa direzione"))?;
    let target = destination.to_string();
    if d.layer(&target).is_none() {
        return Err(bad("sheet", format!("Il layer {target} non esiste")));
    }
    for layer in &mut d.layers {
        if number(&layer.name).is_some() {
            layer.frozen = layer.name != target;
            if layer.name == target {
                layer.on = true;
            }
        }
    }
    d.header.set_str("CLAYER", &target);
    Ok(json!({"sheet": target, "message": format!("FOGLIO {target} attivo")}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_preserves_common_layers_and_supports_undo() {
        let mut s = Session::new();
        for name in ["1", "2", "3", "01", "Cartiglio"] {
            s.execute("layer.new", &json!({"name": name})).unwrap();
        }
        s.execute("layer.set", &json!({"name": "Cartiglio", "frozen": true})).unwrap();
        s.execute("layer.set", &json!({"name": "2", "on": false})).unwrap();
        s.execute("sheet.next", &json!({})).unwrap();
        s.execute("sheet.next", &json!({})).unwrap();
        let d = s.doc().unwrap();
        assert_eq!(d.header.str("CLAYER", ""), "2");
        assert!(d.layer("2").unwrap().visible());
        assert!(d.layer("1").unwrap().frozen);
        assert!(d.layer("3").unwrap().frozen);
        assert!(d.layer("Cartiglio").unwrap().frozen);
        assert!(!d.layer("01").unwrap().frozen);
        s.execute("u", &json!({})).unwrap();
        assert_eq!(s.doc().unwrap().header.str("CLAYER", ""), "1");
        let before = s.doc().unwrap().layers.clone();
        assert!(s.execute("sheet.previous", &json!({})).is_err());
        assert_eq!(s.doc().unwrap().layers, before);
    }

    #[test]
    fn gaps_and_overflow_leave_drawing_unchanged() {
        let mut s = Session::new();
        for name in ["1", "3", "18446744073709551615"] {
            s.execute("layer.new", &json!({"name": name})).unwrap();
        }
        s.execute("layer.current", &json!({"name": "1"})).unwrap();
        assert!(s.execute("sheet.next", &json!({})).is_err());
        assert_eq!(s.doc().unwrap().header.str("CLAYER", ""), "1");
        s.execute("layer.current", &json!({"name": "18446744073709551615"})).unwrap();
        assert!(s.execute("sheet.next", &json!({})).is_err());
        s.execute("sheet.previous", &json!({})).unwrap_err();
    }
}
