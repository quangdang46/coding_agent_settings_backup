//! TOON (Tabular Optimized Object Notation) serialiser.
//!
//! TOON is a compact text format aimed at LLM consumption: indented
//! key/value blocks with array tables given as `header[N]:` followed by CSV
//! rows. We implement a self-contained serialiser over [`serde_json::Value`].
//!
//! Spec summary used here:
//! - Scalars print as their canonical JSON form (numbers/booleans/null) or
//!   bare strings when they don't contain the delimiter set, otherwise as
//!   double-quoted strings.
//! - Objects print one key per line: `key: value` (or `key:` followed by
//!   nested indented block).
//! - Arrays of homogeneous objects (same key set) collapse to a table:
//!   `name[N]{col1,col2,...}:` plus one CSV row per element.
//! - Other arrays print as `name[N]:` then one indented `- value` per element.

use serde_json::{Map, Value};
use std::fmt::Write as _;

/// Convert a [`serde_json::Value`] to its TOON string representation.
pub fn to_toon(value: &Value) -> String {
    let mut out = String::new();
    emit(value, 0, &mut out);
    out.trim_end().to_string()
}

fn emit(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            push_indent(out, indent);
            out.push_str(&scalar_repr(value));
            out.push('\n');
        }
        Value::Object(map) => emit_object(map, indent, out),
        Value::Array(arr) => emit_array("", arr, indent, out),
    }
}

fn emit_object(map: &Map<String, Value>, indent: usize, out: &mut String) {
    for (k, v) in map {
        match v {
            Value::Object(child) => {
                push_indent(out, indent);
                let _ = writeln!(out, "{k}:");
                if child.is_empty() {
                    push_indent(out, indent + 1);
                    out.push_str("{}\n");
                } else {
                    emit_object(child, indent + 1, out);
                }
            }
            Value::Array(arr) => emit_array(k, arr, indent, out),
            other => {
                push_indent(out, indent);
                let _ = writeln!(out, "{k}: {}", scalar_repr(other));
            }
        }
    }
}

fn emit_array(name: &str, arr: &[Value], indent: usize, out: &mut String) {
    let n = arr.len();
    if let Some(headers) = homogeneous_object_headers(arr) {
        push_indent(out, indent);
        let header_list = headers.join(",");
        if name.is_empty() {
            let _ = writeln!(out, "[{n}]{{{header_list}}}:");
        } else {
            let _ = writeln!(out, "{name}[{n}]{{{header_list}}}:");
        }
        for item in arr {
            push_indent(out, indent + 1);
            let obj = item.as_object().expect("checked homogeneous");
            let row: Vec<String> = headers
                .iter()
                .map(|h| csv_repr(obj.get(h).unwrap_or(&Value::Null)))
                .collect();
            let _ = writeln!(out, "{}", row.join(","));
        }
        return;
    }

    push_indent(out, indent);
    if name.is_empty() {
        let _ = writeln!(out, "[{n}]:");
    } else {
        let _ = writeln!(out, "{name}[{n}]:");
    }
    for item in arr {
        match item {
            Value::Object(child) => {
                push_indent(out, indent + 1);
                out.push_str("-\n");
                emit_object(child, indent + 2, out);
            }
            Value::Array(inner) => {
                emit_array("", inner, indent + 1, out);
            }
            scalar => {
                push_indent(out, indent + 1);
                let _ = writeln!(out, "- {}", scalar_repr(scalar));
            }
        }
    }
}

fn homogeneous_object_headers(arr: &[Value]) -> Option<Vec<String>> {
    if arr.is_empty() {
        return None;
    }
    let first = arr.first()?.as_object()?;
    if first.is_empty() {
        return None;
    }
    // Disqualify if any object value contains a non-scalar — the table form
    // can't represent nested objects/arrays.
    let headers: Vec<String> = first.keys().cloned().collect();
    for v in arr {
        let obj = v.as_object()?;
        if obj.len() != headers.len() {
            return None;
        }
        for h in &headers {
            match obj.get(h) {
                Some(Value::Null)
                | Some(Value::Bool(_))
                | Some(Value::Number(_))
                | Some(Value::String(_)) => {}
                _ => return None,
            }
        }
    }
    Some(headers)
}

fn scalar_repr(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if needs_quoting(s) {
                format!("\"{}\"", escape(s))
            } else {
                s.clone()
            }
        }
        _ => "?".into(),
    }
}

fn csv_repr(value: &Value) -> String {
    match value {
        Value::String(s) => {
            if s.contains(',') || s.contains('"') || s.contains('\n') {
                format!("\"{}\"", escape(s))
            } else {
                s.clone()
            }
        }
        other => scalar_repr(other),
    }
}

fn needs_quoting(s: &str) -> bool {
    s.is_empty()
        || s.contains(':')
        || s.contains(',')
        || s.contains('"')
        || s.contains('\n')
        || s.starts_with(' ')
        || s.ends_with(' ')
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalar_object() {
        let v = json!({"a": 1, "b": "hi"});
        let t = to_toon(&v);
        assert!(t.contains("a: 1"));
        assert!(t.contains("b: hi"));
    }

    #[test]
    fn array_of_objects_uses_table_form() {
        let v = json!([
            {"key": "a", "n": 1},
            {"key": "b", "n": 2},
        ]);
        let t = to_toon(&v);
        assert!(t.contains("[2]{key,n}:"));
        assert!(t.contains("a,1"));
        assert!(t.contains("b,2"));
    }

    #[test]
    fn nested_objects_indented() {
        let v = json!({"outer": {"inner": "x"}});
        let t = to_toon(&v);
        assert!(t.contains("outer:"));
        assert!(t.contains("  inner: x"));
    }

    #[test]
    fn array_of_strings_dashed() {
        let v = json!({"xs": ["a", "b"]});
        let t = to_toon(&v);
        assert!(t.contains("xs[2]:"));
        assert!(t.contains("- a"));
        assert!(t.contains("- b"));
    }

    #[test]
    fn quoted_string_with_colon() {
        let v = json!({"k": "a:b"});
        let t = to_toon(&v);
        assert!(t.contains("k: \"a:b\""));
    }
}
