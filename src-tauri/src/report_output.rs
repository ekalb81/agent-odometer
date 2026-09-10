//! Lossless, versioned structured exports shared by all CLI reports.
//!
//! Version 1 keeps existing command-specific JSON/CSV contracts. Version 2
//! wraps any report in an envelope; its CSV is a typed JSON-pointer stream,
//! preserving arrays, nulls, empty containers, and pricing provenance.

use anyhow::{bail, Result};
use serde_json::{json, Value};

pub const EXPORT_SCHEMA_VERSION: u32 = 2;

pub fn envelope(report: &str, data: Value) -> Value {
    json!({ "schema_version": EXPORT_SCHEMA_VERSION, "report": report, "data": data })
}

pub fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn leaves(value: &Value, path: &str, visit: &mut impl FnMut(&str, &'static str, String)) {
    match value {
        Value::Object(entries) if !entries.is_empty() => {
            for (key, child) in entries {
                let key = key.replace('~', "~0").replace('/', "~1");
                leaves(child, &format!("{path}/{key}"), visit);
            }
        }
        Value::Array(entries) if !entries.is_empty() => {
            for (index, child) in entries.iter().enumerate() {
                leaves(child, &format!("{path}/{index}"), visit);
            }
        }
        _ => visit(
            path,
            match value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            },
            value.to_string(),
        ),
    }
}

pub fn csv(report: &str, value: &Value) -> String {
    let mut output = String::from("schema_version,report,path,type,value\n");
    leaves(value, "", &mut |path, kind, value| {
        output.push_str(&format!(
            "{EXPORT_SCHEMA_VERSION},{},{},{},{}\n",
            csv_field(report),
            csv_field(path),
            kind,
            csv_field(&value)
        ));
    });
    output
}

fn markdown_cell(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('`', "&#96;")
        .replace('\r', "\\r")
        .replace('\n', "<br>")
}

pub fn markdown(report: &str, value: &Value, version: u32) -> String {
    let mut output = format!(
        "# {}\n\nExport schema: {version}\n\n| Field | Value |\n| --- | --- |\n",
        markdown_cell(report)
    );
    leaves(value, "", &mut |path, _, value| {
        output.push_str(&format!(
            "| {} | {} |\n",
            markdown_cell(path),
            markdown_cell(&value)
        ));
    });
    output
}

pub fn check_size(output: String, max_bytes: usize) -> Result<String> {
    if output.len() > max_bytes {
        bail!("report exceeds the {max_bytes}-byte output limit; narrow the date range or session limit");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_csv_preserves_types_empty_values_and_escaped_paths() {
        let value =
            json!({"a/b~c": [null, "null", 0, false, [], {}], "name": "comma,\"quote\"\nline"});
        let output = csv("tools", &value);
        assert!(output.starts_with("schema_version,report,path,type,value\n"));
        assert!(output.contains("2,tools,/a~1b~0c/0,null,null\n"));
        assert!(output.contains("2,tools,/a~1b~0c/1,string,\"\"\"null\"\"\"\n"));
        assert!(output.contains("2,tools,/a~1b~0c/4,array,[]\n"));
        assert!(output.contains("2,tools,/a~1b~0c/5,object,{}\n"));
        for text in ["a,b", "a\nb", "a\rb", "a\"b"] {
            assert!(csv_field(text).starts_with('"'));
        }
    }

    #[test]
    fn markdown_escapes_dynamic_metadata_and_carries_version() {
        let output = markdown("report", &json!({"model": "<script>|`x`\nnext"}), 2);
        assert!(output.contains("Export schema: 2"));
        assert!(!output.contains("<script>"));
        assert!(output.contains("&lt;script&gt;\\|&#96;x&#96;"));
    }

    #[test]
    fn output_limit_is_an_error_not_truncation() {
        assert!(check_size("12345".into(), 4).is_err());
        assert_eq!(check_size("1234".into(), 4).unwrap(), "1234");
    }
}
