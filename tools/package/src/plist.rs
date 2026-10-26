//! Property lists in Apple's XML form: read, merged and written.
//!
//! Merging takes a dictionary into another: a key the first lacks is added,
//! dictionaries merge key by key, arrays are united (each item once, the
//! first's in their order), and a key both give is fine only when its values
//! are the same.

use std::fmt::Write as _;
use std::ops::Range;

use crate::xml::{self, Element, escape};

/// A property list value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Integer(i64),
    /// A real, as written.
    Real(String),
    Bool(bool),
    /// A date, as written.
    Date(String),
    /// Data, as its Base64 is written.
    Data(String),
    Array(Vec<Value>),
    Dict(Dict),
}

impl Value {
    pub fn string(text: impl Into<String>) -> Value {
        Value::String(text.into())
    }

    /// It as a message names it.
    fn describe(&self) -> String {
        match self {
            Value::String(text) => format!("\"{text}\""),
            Value::Integer(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Real(text) | Value::Date(text) => text.clone(),
            Value::Data(_) => "data".to_owned(),
            Value::Array(_) => "an array".to_owned(),
            Value::Dict(_) => "a dictionary".to_owned(),
        }
    }

    fn write(&self, out: &mut String, depth: usize) {
        let pad = "\t".repeat(depth);
        let mut leaf = |tag: &str, text: &str| {
            let _ = writeln!(out, "{pad}<{tag}>{}</{tag}>", escape(text, false));
        };
        match self {
            Value::String(text) => leaf("string", text),
            Value::Integer(n) => leaf("integer", &n.to_string()),
            Value::Real(text) => leaf("real", text),
            Value::Date(text) => leaf("date", text),
            Value::Data(text) => leaf("data", text),
            Value::Bool(b) => {
                let _ = writeln!(out, "{pad}<{b}/>");
            }
            Value::Array(items) if items.is_empty() => {
                let _ = writeln!(out, "{pad}<array/>");
            }
            Value::Array(items) => {
                let _ = writeln!(out, "{pad}<array>");
                for item in items {
                    item.write(out, depth + 1);
                }
                let _ = writeln!(out, "{pad}</array>");
            }
            Value::Dict(dict) if dict.0.is_empty() => {
                let _ = writeln!(out, "{pad}<dict/>");
            }
            Value::Dict(dict) => {
                let _ = writeln!(out, "{pad}<dict>");
                for entry in &dict.0 {
                    let _ = writeln!(out, "{pad}\t<key>{}</key>", escape(&entry.key, false));
                    entry.value.write(out, depth + 1);
                }
                let _ = writeln!(out, "{pad}</dict>");
            }
        }
    }
}

/// A dictionary: its entries in order.
#[derive(Debug, Clone, Default)]
pub struct Dict(pub Vec<Entry>);

/// A key of a dictionary and its value.
#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    /// Who gave it, as the caller numbers what it merges.
    pub from: usize,
    /// Where its key is in the text it was read from.
    pub at: Range<usize>,
}

/// Two dictionaries are the same when they hold the same keys with the same
/// values, in any order.
impl PartialEq for Dict {
    fn eq(&self, other: &Dict) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .all(|entry| other.get(&entry.key) == Some(&entry.value))
    }
}

impl Dict {
    /// A dictionary of `entries`, given by 0.
    pub fn of(entries: Vec<(&str, Value)>) -> Dict {
        let mut dict = Dict::default();
        for (key, value) in entries {
            dict.set(key, value);
        }
        dict
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|e| e.key == key).map(|e| &e.value)
    }

    /// Sets `key` to `value`, given by 0, in its place or after the rest.
    pub fn set(&mut self, key: &str, value: Value) {
        match self.0.iter_mut().find(|e| e.key == key) {
            Some(entry) => entry.value = value,
            None => self.0.push(Entry {
                key: key.to_owned(),
                value,
                from: 0,
                at: 0..0,
            }),
        }
    }

    /// Adds `entries` after the rest, each given by 0.
    pub fn extend<'a>(&mut self, entries: impl IntoIterator<Item = (&'a str, Value)>) {
        for (key, value) in entries {
            self.set(key, value);
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|e| e.key.as_str())
    }
}

/// `dict` as a property list file.
pub fn write(dict: &Dict) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n",
    );
    match dict.0.is_empty() {
        true => out.push_str("<dict/>\n"),
        false => Value::Dict(dict.clone()).write(&mut out, 0),
    }
    out.push_str("</plist>\n");
    out
}

/// `dict` as entitlements are encoded in DER: version 1, then the
/// dictionary, each entry a sequence of its key and value, keys in order.
pub fn der(dict: &Dict) -> Vec<u8> {
    let mut body = Vec::new();
    tlv(0x02, &[1], &mut body);
    der_dict(dict, &mut body);
    let mut out = Vec::with_capacity(body.len() + 6);
    tlv(0x70, &body, &mut out);
    out
}

/// A dictionary: context tag 16, constructed, holding its entries by key.
fn der_dict(dict: &Dict, out: &mut Vec<u8>) {
    let mut entries: Vec<&Entry> = dict.0.iter().collect();
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    let mut body = Vec::new();
    for entry in entries {
        let mut pair = Vec::new();
        tlv(0x0c, entry.key.as_bytes(), &mut pair);
        der_value(&entry.value, &mut pair);
        tlv(0x30, &pair, &mut body);
    }
    tlv(0xb0, &body, out);
}

fn der_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Bool(b) => tlv(0x01, &[if *b { 0xff } else { 0 }], out),
        Value::Integer(n) => {
            let bytes = n.to_be_bytes();
            // The fewest bytes that keep the sign.
            let mut start = 0;
            while start < 7
                && ((bytes[start] == 0 && bytes[start + 1] & 0x80 == 0)
                    || (bytes[start] == 0xff && bytes[start + 1] & 0x80 != 0))
            {
                start += 1;
            }
            tlv(0x02, &bytes[start..], out);
        }
        Value::String(text) | Value::Real(text) | Value::Date(text) => {
            tlv(0x0c, text.as_bytes(), out)
        }
        Value::Data(base64) => tlv(0x04, &decode_base64(base64), out),
        Value::Array(items) => {
            let mut body = Vec::new();
            for item in items {
                der_value(item, &mut body);
            }
            tlv(0x30, &body, out);
        }
        Value::Dict(dict) => der_dict(dict, out),
    }
}

/// A DER tag, the length of `body` and `body`.
fn tlv(tag: u8, body: &[u8], out: &mut Vec<u8>) {
    out.push(tag);
    let len = body.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(body);
}

/// The bytes of `text`, Base64; what does not read is left out.
fn decode_base64(text: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for v in text.bytes().filter_map(value) {
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// The dictionary the property list `text` holds, each entry given by
/// `from`.
pub(crate) fn read(text: &str, from: usize) -> Result<Dict, xml::Error> {
    let root = xml::parse(text)?;
    let not = |at: usize| xml::Error {
        at,
        message: "a property list is a `<plist>` holding one `<dict>`".to_owned(),
    };
    if root.name != "plist" || root.stray_text().is_some() {
        return Err(not(root.span.start));
    }
    let mut elements = root.elements();
    let (Some(dict), None) = (elements.next(), elements.next()) else {
        return Err(not(root.span.start));
    };
    match value(dict, from)? {
        Value::Dict(dict) => Ok(dict),
        _ => Err(not(dict.span.start)),
    }
}

/// The value `element` writes.
fn value(element: &Element, from: usize) -> Result<Value, xml::Error> {
    let error = |message: String| xml::Error {
        at: element.span.start,
        message,
    };
    let leaf = || {
        if element.elements().next().is_some() {
            return Err(error(format!("a `<{}>` holds text alone", element.name)));
        }
        Ok(element.text())
    };
    Ok(match element.name.as_str() {
        "string" => Value::String(leaf()?),
        "integer" => {
            let text = leaf()?;
            Value::Integer(
                text.trim()
                    .parse()
                    .map_err(|_| error(format!("`{}` is not an integer", text.trim())))?,
            )
        }
        "real" => Value::Real(leaf()?.trim().to_owned()),
        "date" => Value::Date(leaf()?.trim().to_owned()),
        "data" => Value::Data(leaf()?.split_whitespace().collect()),
        "true" | "false" if element.children.is_empty() => Value::Bool(element.name == "true"),
        "array" => {
            if let Some(text) = element.stray_text() {
                return Err(error(format!("an `<array>` holds values, not `{text}`")));
            }
            Value::Array(
                element
                    .elements()
                    .map(|item| value(item, from))
                    .collect::<Result<_, _>>()?,
            )
        }
        "dict" => {
            if let Some(text) = element.stray_text() {
                return Err(error(format!(
                    "a `<dict>` holds keys and values, not `{text}`"
                )));
            }
            let mut dict = Dict::default();
            let mut items = element.elements();
            while let Some(key) = items.next() {
                if key.name != "key" {
                    return Err(xml::Error {
                        at: key.span.start,
                        message: format!("a `<{}>` where a `<key>` was expected", key.name),
                    });
                }
                let name = key.text();
                let Some(item) = items.next() else {
                    return Err(xml::Error {
                        at: key.span.start,
                        message: format!("`{name}` has no value"),
                    });
                };
                if dict.get(&name).is_some() {
                    return Err(xml::Error {
                        at: key.span.start,
                        message: format!("`{name}` is given twice"),
                    });
                }
                dict.0.push(Entry {
                    key: name,
                    value: value(item, from)?,
                    from,
                    at: key.span.clone(),
                });
            }
            Value::Dict(dict)
        }
        other => return Err(error(format!("`<{other}>` is not a property list value"))),
    })
}

/// Two values of one key, which do not merge.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    /// The key, under the keys holding it (`A.B`).
    pub key: String,
    /// Where the incoming key is.
    pub at: Range<usize>,
    /// Who gave the value already there.
    pub other: usize,
    /// The value already there, as a message names it.
    pub ours: String,
    /// The incoming value, as a message names it.
    pub theirs: String,
}

/// Merges `from` into `into`.
pub fn merge(into: &mut Dict, from: Dict) -> Result<(), Conflict> {
    merge_under(into, from, "")
}

fn merge_under(into: &mut Dict, from: Dict, under: &str) -> Result<(), Conflict> {
    for entry in from.0 {
        let Some(have) = into.0.iter_mut().find(|e| e.key == entry.key) else {
            into.0.push(entry);
            continue;
        };
        let key = format!("{under}{}", entry.key);
        match (&mut have.value, entry.value) {
            (Value::Dict(have), Value::Dict(theirs)) => {
                merge_under(have, theirs, &format!("{key}."))?;
            }
            (Value::Array(have), Value::Array(theirs)) => {
                for item in theirs {
                    if !have.contains(&item) {
                        have.push(item);
                    }
                }
            }
            (have_value, theirs) if *have_value == theirs => {}
            (have_value, theirs) => {
                return Err(Conflict {
                    key,
                    at: entry.at,
                    other: have.from,
                    ours: have_value.describe(),
                    theirs: theirs.describe(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
        \t<key>UIBackgroundModes</key>\n\t<array>\n\t\t<string>fetch</string>\n\t\t<string>processing</string>\n\t</array>\n\
        \t<key>Scene</key>\n\t<dict>\n\t\t<key>Multiple</key>\n\t\t<false/>\n\t</dict>\n</dict>\n</plist>\n";

    #[test]
    fn a_plist_reads_and_writes_back() {
        let dict = read(MODES, 3).unwrap();
        assert_eq!(
            dict.keys().collect::<Vec<_>>(),
            ["UIBackgroundModes", "Scene"]
        );
        assert_eq!(dict.0[0].from, 3);
        assert_eq!(&MODES[dict.0[0].at.clone()], "<key>UIBackgroundModes</key>");
        let written = write(&dict);
        assert!(
            written.contains(
                "\t<key>UIBackgroundModes</key>\n\t<array>\n\t\t<string>fetch</string>\n"
            )
        );
        assert!(written.contains("\t\t<key>Multiple</key>\n\t\t<false/>\n"));
        assert_eq!(read(&written, 0).unwrap(), dict);
        let all = "<plist><dict><key>n</key><integer>-4</integer><key>r</key><real>1.5</real>\
                   <key>d</key><data>AAEC\n AwQ=</data><key>e</key><array/></dict></plist>";
        let dict = read(all, 0).unwrap();
        assert_eq!(dict.get("n"), Some(&Value::Integer(-4)));
        assert_eq!(dict.get("d"), Some(&Value::Data("AAECAwQ=".into())));
        assert_eq!(read(&write(&dict), 0).unwrap(), dict);
    }

    #[test]
    fn entitlements_encode_in_der_keys_in_order() {
        let dict = Dict::of(vec![
            ("b", Value::Bool(true)),
            ("a", Value::string("development")),
            (
                "n",
                Value::Array(vec![Value::Integer(128), Value::Integer(-1)]),
            ),
        ]);
        let der = der(&dict);
        let body = [
            &[0x02, 0x01, 0x01, 0xb0, 0x28][..],
            &[0x30, 0x10, 0x0c, 0x01, b'a', 0x0c, 0x0b],
            b"development",
            &[0x30, 0x06, 0x0c, 0x01, b'b', 0x01, 0x01, 0xff],
            &[
                0x30, 0x0c, 0x0c, 0x01, b'n', 0x30, 0x07, 0x02, 0x02, 0x00, 0x80, 0x02, 0x01, 0xff,
            ],
        ]
        .concat();
        assert_eq!(der[..2], [0x70, body.len() as u8]);
        assert_eq!(der[2..], body[..]);
        let mut long = Vec::new();
        tlv(0x04, &[0; 300], &mut long);
        assert_eq!(long[..4], [0x04, 0x82, 0x01, 0x2c]);
        assert_eq!(decode_base64("AAEC\nAwQ="), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn what_is_no_plist_says_where() {
        let fails = |text: &str| read(text, 0).unwrap_err();
        assert!(
            fails("<dict/>")
                .message
                .contains("`<plist>` holding one `<dict>`")
        );
        assert!(
            fails("<plist><array/></plist>")
                .message
                .contains("one `<dict>`")
        );
        let twice = "<plist><dict><key>a</key><true/><key>a</key><false/></dict></plist>";
        assert_eq!(fails(twice).message, "`a` is given twice");
        assert_eq!(fails(twice).at, twice.rfind("<key>").unwrap());
        assert!(
            fails("<plist><dict><key>a</key></dict></plist>")
                .message
                .contains("no value")
        );
        assert!(
            fails("<plist><dict><string>a</string></dict></plist>")
                .message
                .contains("`<key>`")
        );
        assert!(
            fails("<plist><dict><key>a</key><integer>x</integer></dict></plist>")
                .message
                .contains("not an integer")
        );
        assert!(
            fails("<plist><dict><key>a</key><map/></dict></plist>")
                .message
                .contains("`<map>`")
        );
    }

    #[test]
    fn arrays_unite_dictionaries_merge_and_scalars_must_agree() {
        let mut into = Dict::of(vec![
            (
                "UIBackgroundModes",
                Value::Array(vec![Value::string("fetch")]),
            ),
            (
                "Scene",
                Value::Dict(Dict::of(vec![("Multiple", Value::Bool(false))])),
            ),
            ("Name", Value::string("notes")),
        ]);
        merge(&mut into, read(MODES, 1).unwrap()).unwrap();
        let push = "<plist><dict><key>UIBackgroundModes</key><array><string>remote-notification</string>\
                    <string>fetch</string></array><key>Name</key><string>notes</string>\
                    <key>Scene</key><dict><key>Single</key><true/></dict></dict></plist>";
        merge(&mut into, read(push, 2).unwrap()).unwrap();
        assert_eq!(
            into.get("UIBackgroundModes"),
            Some(&Value::Array(vec![
                Value::string("fetch"),
                Value::string("processing"),
                Value::string("remote-notification"),
            ]))
        );
        let Some(Value::Dict(scene)) = into.get("Scene") else {
            panic!("{into:?}");
        };
        assert_eq!(scene.keys().collect::<Vec<_>>(), ["Multiple", "Single"]);

        let clash =
            "<plist><dict><key>Scene</key><dict><key>Multiple</key><true/></dict></dict></plist>";
        let conflict = merge(&mut into, read(clash, 3).unwrap()).unwrap_err();
        assert_eq!(conflict.key, "Scene.Multiple");
        assert_eq!(
            (
                conflict.other,
                conflict.ours.as_str(),
                conflict.theirs.as_str()
            ),
            (0, "false", "true")
        );
        assert_eq!(&clash[conflict.at], "<key>Multiple</key>");
        let typed = "<plist><dict><key>Name</key><array/></dict></plist>";
        let conflict = merge(&mut into, read(typed, 4).unwrap()).unwrap_err();
        assert_eq!(
            (conflict.ours.as_str(), conflict.theirs.as_str()),
            ("\"notes\"", "an array")
        );
    }
}
