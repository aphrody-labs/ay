//! Pure-Rust `.env` parser and loader (modeled on `oven-sh/bun`'s `bun_dotenv`),
//! embedded in `MiniEventLoop` / `Runtime` with zero JSC and zero regex.

use std::collections::BTreeMap;

/// Loaded `.env` key-value map supporting quotes, escapes, `export`, comments, and `${VAR}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DotEnvLoader {
    vars: BTreeMap<String, String>,
}

impl DotEnvLoader {
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
        }
    }

    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.vars.len()
    }

    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    #[inline]
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    #[inline]
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Parse `.env` source text and merge entries into `self`.
    pub fn load_str(&mut self, input: &str) -> usize {
        let mut added = 0usize;
        for raw_line in input.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let body = line.strip_prefix("export ").map_or(line, str::trim_start);
            let Some((raw_key, raw_val)) = body.split_once('=') else {
                continue;
            };
            let key = raw_key.trim();
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                continue;
            }
            let value = self.parse_value(raw_val.trim());
            self.vars.insert(key.to_owned(), value);
            added += 1;
        }
        added
    }

    fn parse_value(&self, raw: &str) -> String {
        if let Some(inner) = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            // Single quotes: literal, no escape or variable expansion.
            return inner.to_owned();
        }
        if let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            let unescaped = unescape_double_quoted(inner);
            return self.expand_vars(&unescaped);
        }
        // Unquoted: strip inline ` # comment`
        let clean = match raw.find(" #") {
            Some(idx) => raw[..idx].trim_end(),
            None => raw,
        };
        self.expand_vars(clean)
    }

    fn expand_vars(&self, input: &str) -> String {
        if !input.contains('$') {
            return input.to_owned();
        }
        let mut out = String::with_capacity(input.len());
        let bytes = input.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$'
                && i + 1 < bytes.len()
                && bytes[i + 1] == b'{'
                && let Some(end) = input[i + 2..].find('}')
            {
                let var_name = &input[i + 2..i + 2 + end];
                if let Some(val) = self.get(var_name) {
                    out.push_str(val);
                }
                i += 3 + end;
                continue;
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }
}

fn unescape_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_parses_quotes_escapes_comments_and_interpolation() {
        let mut env = DotEnvLoader::new();
        let n = env.load_str(
            "# comment\n\
             export HOST=aphrody.com # inline\n\
             PORT=8787\n\
             URL=\"https://${HOST}:${PORT}/v1\\n\"\n\
             LITERAL='${HOST}'\n",
        );
        assert_eq!(n, 4);
        assert_eq!(env.get("HOST"), Some("aphrody.com"));
        assert_eq!(env.get("PORT"), Some("8787"));
        assert_eq!(env.get("URL"), Some("https://aphrody.com:8787/v1\n"));
        assert_eq!(env.get("LITERAL"), Some("${HOST}"));
    }
}
