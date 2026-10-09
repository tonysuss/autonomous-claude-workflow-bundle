//! SKILL.md frontmatter: written by the generator, read back by the validator.
//!
//! The reader takes the YAML subset skill files use: plain, quoted and block
//! scalars, booleans, flow and block lists of scalars, and one level of nested
//! mapping. Anything else is reported as unsupported rather than guessed at.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Bool(bool),
    List(Vec<String>),
    Map(Vec<(String, String)>),
}

/// Ordered keys and values, as written at the top of a SKILL.md.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter(pub Vec<(String, Value)>);

impl Frontmatter {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    pub fn push(&mut self, key: &str, value: Value) {
        self.0.push((key.to_string(), value));
    }

    /// The frontmatter block, fences included.
    pub fn render(&self) -> String {
        let mut out = String::from("---\n");
        for (k, v) in &self.0 {
            match v {
                Value::Str(s) => {
                    let _ = writeln!(out, "{k}: {}", scalar(s));
                }
                Value::Bool(b) => {
                    let _ = writeln!(out, "{k}: {b}");
                }
                Value::List(items) => {
                    let items: Vec<String> = items.iter().map(|i| quoted(i)).collect();
                    let _ = writeln!(out, "{k}: [{}]", items.join(", "));
                }
                Value::Map(pairs) => {
                    let _ = writeln!(out, "{k}:");
                    for (mk, mv) in pairs {
                        let _ = writeln!(out, "  {mk}: {}", scalar(mv));
                    }
                }
            }
        }
        out.push_str("---\n");
        out
    }
}

fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Plain when nothing in the text could change its meaning in YAML.
fn scalar(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || " ._,/()-".contains(c))
        && !s.starts_with([' ', '-'])
        && !s.ends_with(' ')
        && !matches!(s, "true" | "false" | "null" | "yes" | "no" | "on" | "off");
    if safe { s.to_string() } else { quoted(s) }
}

/// Splits a file into its frontmatter and body. `Ok(None)` when the file has no frontmatter.
pub fn split(text: &str) -> Result<Option<(Frontmatter, &str)>, String> {
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return Ok(None);
    };
    if let Some(body) = rest.strip_prefix("---") {
        return Ok(Some((Frontmatter::default(), body.trim_start_matches(['\r', '\n']))));
    }
    let end = rest
        .match_indices("\n---")
        .find(|(i, _)| {
            let after = &rest[i + 4..];
            after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n")
        })
        .map(|(i, _)| i)
        .ok_or("the frontmatter has no closing ---")?;
    let yaml = &rest[..end + 1];
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    Ok(Some((parse(yaml)?, body)))
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn strip_comment(s: &str) -> &str {
    match s.find(" #") {
        Some(i) => s[..i].trim_end(),
        None => s,
    }
}

fn parse_scalar(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('"') {
        let inner = inner.strip_suffix('"').ok_or_else(|| format!("unterminated string {raw}"))?;
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(c @ ('"' | '\\' | '/')) => out.push(c),
                other => return Err(format!("unsupported escape \\{} in {raw}", other.unwrap_or(' '))),
            }
        }
        return Ok(Value::Str(out));
    }
    if let Some(inner) = raw.strip_prefix('\'') {
        let inner = inner.strip_suffix('\'').ok_or_else(|| format!("unterminated string {raw}"))?;
        return Ok(Value::Str(inner.replace("''", "'")));
    }
    if let Some(inner) = raw.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or_else(|| format!("unterminated list {raw}"))?;
        let mut items = Vec::new();
        for part in inner.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match parse_scalar(part)? {
                Value::Str(s) => items.push(s),
                Value::Bool(b) => items.push(b.to_string()),
                _ => return Err(format!("nested collections are not supported: {raw}")),
            }
        }
        return Ok(Value::List(items));
    }
    let plain = strip_comment(raw);
    if plain.starts_with(['{', '&', '*', '!', '|', '>', '%', '@', '`']) {
        return Err(format!("unsupported YAML value {plain}"));
    }
    Ok(match plain {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        s => Value::Str(s.to_string()),
    })
}

/// Reads the YAML between the fences.
pub fn parse(yaml: &str) -> Result<Frontmatter, String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut out = Frontmatter::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        i += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if indent_of(line) > 0 {
            return Err(format!("unexpected indented line: {line}"));
        }
        let (key, rest) = line.split_once(':').ok_or_else(|| format!("expected `key: value`, got: {line}"))?;
        let key = key.trim().to_string();
        if out.get(&key).is_some() {
            return Err(format!("{key} appears twice"));
        }
        let rest = rest.trim();
        // Indented lines that belong to this key.
        let mut block = Vec::new();
        while i < lines.len() && (lines[i].trim().is_empty() || indent_of(lines[i]) > 0) {
            block.push(lines[i]);
            i += 1;
        }
        while block.last().is_some_and(|l| l.trim().is_empty()) {
            block.pop();
        }
        let value = if matches!(rest, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            let min = block.iter().filter(|l| !l.trim().is_empty()).map(|l| indent_of(l)).min().unwrap_or(0);
            let texts: Vec<&str> = block.iter().map(|l| if l.len() >= min { &l[min..] } else { "" }).collect();
            Value::Str(if rest.starts_with('|') { texts.join("\n") } else { texts.join(" ") })
        } else if rest.is_empty() && block.iter().any(|l| l.trim_start().starts_with("- ")) {
            let mut items = Vec::new();
            for l in block.iter().filter(|l| !l.trim().is_empty()) {
                let item = l.trim_start().strip_prefix("- ").ok_or_else(|| format!("expected a list item: {l}"))?;
                match parse_scalar(item)? {
                    Value::Str(s) => items.push(s),
                    Value::Bool(b) => items.push(b.to_string()),
                    _ => return Err(format!("nested collections are not supported under {key}")),
                }
            }
            Value::List(items)
        } else if rest.is_empty() {
            let mut pairs = Vec::new();
            for l in block.iter().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')) {
                let (k, v) =
                    l.trim().split_once(':').ok_or_else(|| format!("expected `key: value` under {key}: {l}"))?;
                let v = match parse_scalar(v)? {
                    Value::Str(s) => s,
                    Value::Bool(b) => b.to_string(),
                    _ => return Err(format!("{key}.{} must be a string", k.trim())),
                };
                pairs.push((k.trim().to_string(), v));
            }
            Value::Map(pairs)
        } else if !block.is_empty() {
            return Err(format!("multi-line plain values are not supported ({key})"));
        } else {
            parse_scalar(rest)?
        };
        out.0.push((key, value));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_written_reads_back_the_same() {
        let mut fm = Frontmatter::default();
        fm.push("name", Value::Str("verify".into()));
        fm.push("description", Value::Str("Checks it: \"really\" works, with a \\ and #hash".into()));
        fm.push("disable-model-invocation", Value::Bool(true));
        fm.push("tools", Value::List(vec!["read".into(), "execute".into()]));
        fm.push("metadata", Value::Map(vec![("interlock-invocation".into(), "user".into())]));
        let text = format!("{}\nBody\n", fm.render());
        let (back, body) = split(&text).unwrap().unwrap();
        assert_eq!(back, fm);
        assert_eq!(body, "Body\n");
        assert!(text.starts_with("---\nname: verify\ndescription: \"Checks it"), "{text}");
    }

    #[test]
    fn reads_the_forms_other_skill_files_use() {
        let text = "---\nname: how\ndescription: >\n  Explains how\n  things work.\nallowed-tools:\n  - Read\n  - Grep\nlicense: 'MIT'\n---\n# How\n";
        let (fm, body) = split(text).unwrap().unwrap();
        assert_eq!(fm.str("description"), Some("Explains how things work."));
        assert_eq!(fm.get("allowed-tools"), Some(&Value::List(vec!["Read".into(), "Grep".into()])));
        assert_eq!(fm.str("license"), Some("MIT"));
        assert_eq!(body, "# How\n");
        assert_eq!(split("# no frontmatter\n").unwrap(), None);
    }

    #[test]
    fn rejects_what_it_cannot_read() {
        assert!(split("---\nname: x\n").is_err(), "no closing fence");
        assert!(parse("name: x\nname: y\n").unwrap_err().contains("twice"));
        assert!(parse("meta: {a: b}\n").is_err());
        assert!(parse("description: one\n  two\n").is_err());
    }
}
