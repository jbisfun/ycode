//! Logos-flavoured preprocessor for `*.x.lua` hook files.
//!
//! The output is plain Lua with the same number of lines, so Lua error messages point
//! at the original source line. Directives are only recognised outside strings and
//! comments.
//!
//! ```lua
//! %hook editor                       -- like `%hook Class`: an action namespace
//! - Copy                             -- like `- (void)method`: hooks `editor::Copy`
//!     %log("copying")
//!     %orig                          -- run the original action
//! end
//!
//! %new - Shout(args)                 -- brand-new action, run via `ycode::Run`
//!     ycode.log(args.text)
//! end
//! %end
//!
//! %group vimish                      -- hooks inside are inactive until %init(vimish)
//! %hook vim
//! - *                                -- every `vim::*` action
//!     %orig
//! end
//! %end
//! %end
//!
//! %hookf app::example(args)          -- a named hook point exposed by Rust code
//!     return %orig(args)
//! end
//!
//! %ctor                              -- runs once after all files are loaded
//!     %init(vimish)
//! end
//! ```

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub file: String,
    /// 1-based.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: error: {}", self.file, self.line, self.message)
    }
}

impl std::error::Error for Error {}

const DIRECTIVES: &[&str] = &[
    "hook", "hookf", "end", "orig", "log", "ctor", "group", "init", "new",
];
const UNSUPPORTED: &[&str] = &["subclass", "property", "c", "class", "dtor", "config"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Nest {
    Hook,
    Group,
}

struct State<'a> {
    file: &'a str,
    line: usize,
    stack: Vec<(Nest, usize)>,
    namespace: Option<String>,
    group: Option<String>,
    in_method: bool,
    pending_new: Option<usize>,
}

impl State<'_> {
    fn err(&self, message: impl Into<String>) -> Error {
        Error {
            file: self.file.to_string(),
            line: self.line,
            message: message.into(),
        }
    }

    fn in_hook(&self) -> bool {
        self.stack.iter().any(|(n, _)| *n == Nest::Hook)
    }

    fn def(&self, name: &str, is_new: bool, params: &str) -> String {
        let group = match &self.group {
            Some(g) => format!("{g:?}"),
            None => "nil".into(),
        };
        let src = format!("{}:{}", self.file, self.line);
        let params = params.trim();
        let params = if params.is_empty() {
            String::new()
        } else {
            format!(", {params}")
        };
        format!("__ycode.def({group}, {name:?}, {is_new}, {src:?}).fn = function(self{params})")
    }
}

/// Lexer state carried between lines: inside a `[==[ long string/comment ]==]`.
#[derive(Clone, Copy)]
enum Lex {
    Code,
    Long { level: usize },
}

/// Returns a per-byte mask (true = Lua code, false = string/comment) and the state for
/// the next line.
fn code_mask(line: &[u8], mut lex: Lex) -> (Vec<bool>, Lex) {
    let mut mask = vec![true; line.len()];
    let mut i = 0;
    let long_open = |i: usize| -> Option<(usize, usize)> {
        // `[` `=`* `[` starting at i; returns (level, length)
        if line.get(i) != Some(&b'[') {
            return None;
        }
        let mut j = i + 1;
        while line.get(j) == Some(&b'=') {
            j += 1;
        }
        (line.get(j) == Some(&b'[')).then(|| (j - i - 1, j - i + 1))
    };
    let long_close = |from: usize, level: usize| -> Option<usize> {
        // index just past the matching `]` `=`*level `]`
        let mut k = from;
        while k < line.len() {
            if line[k] == b']' {
                let mut j = k + 1;
                while line.get(j) == Some(&b'=') {
                    j += 1;
                }
                if j - k - 1 == level && line.get(j) == Some(&b']') {
                    return Some(j + 1);
                }
            }
            k += 1;
        }
        None
    };
    loop {
        if let Lex::Long { level } = lex {
            match long_close(i, level) {
                Some(end) => {
                    mask[i..end].fill(false);
                    i = end;
                    lex = Lex::Code;
                }
                None => {
                    mask[i..].fill(false);
                    return (mask, lex);
                }
            }
        }
        if i >= line.len() {
            return (mask, lex);
        }
        match line[i] {
            b'-' if line.get(i + 1) == Some(&b'-') => {
                if let Some((level, len)) = long_open(i + 2) {
                    mask[i..i + 2 + len].fill(false);
                    i += 2 + len;
                    lex = Lex::Long { level };
                } else {
                    mask[i..].fill(false);
                    return (mask, lex);
                }
            }
            b'[' if long_open(i).is_some() => {
                let (level, len) = long_open(i).unwrap();
                mask[i..i + len].fill(false);
                i += len;
                lex = Lex::Long { level };
            }
            q @ (b'"' | b'\'') => {
                let start = i;
                i += 1;
                while i < line.len() && line[i] != q {
                    if line[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let end = (i + 1).min(line.len());
                mask[start..end].fill(false);
                i = end;
            }
            _ => i += 1,
        }
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Parse `- Name` / `- Name(params)` / `- *`, the whole remaining code on the line.
fn parse_header(code: &str) -> Option<(String, String)> {
    let rest = code.trim_start().strip_prefix('-')?;
    if rest.starts_with('-') {
        return None;
    }
    let rest = rest.trim_start();
    let name_len = if rest.starts_with('*') {
        1
    } else {
        rest.bytes().take_while(|b| is_ident(*b)).count()
    };
    if name_len == 0 || rest.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let name = rest[..name_len].to_string();
    let rest = rest[name_len..].trim();
    if rest.is_empty() {
        return Some((name, String::new()));
    }
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    if inner.contains(['(', ')']) {
        return None;
    }
    Some((name, inner.to_string()))
}

pub fn preprocess(file: &str, src: &str) -> Result<String, Error> {
    let mut st = State {
        file,
        line: 0,
        stack: Vec::new(),
        namespace: None,
        group: None,
        in_method: false,
        pending_new: None,
    };
    let mut lex = Lex::Code;
    let mut out = String::with_capacity(src.len() + 1024);

    for (idx, raw) in src.split('\n').enumerate() {
        st.line = idx + 1;
        if idx > 0 {
            out.push('\n');
        }
        let (mask, next_lex) = code_mask(raw.as_bytes(), lex);
        lex = next_lex;
        // Code-only view of the line: strings and comments blanked out.
        let code: String = raw
            .bytes()
            .zip(&mask)
            .map(|(b, &c)| if c { b as char } else { ' ' })
            .collect();
        // Non-ASCII bytes only appear in strings/comments that we blanked, or would be a
        // Lua syntax error anyway; keep byte offsets aligned with `raw`.
        let code = if code.len() == raw.len() {
            code
        } else {
            raw.to_string()
        };

        let mut line = raw.to_string();
        let mut offset = 0; // where directive scanning starts

        // `%new` (optionally followed by the header on the same line).
        let trimmed = code.trim_start();
        let indent = code.len() - trimmed.len();
        if trimmed.starts_with("%new") && !trimmed[4..].starts_with(|c: char| is_ident(c as u8)) {
            if !st.in_hook() {
                return Err(st.err("%new found outside of %hook"));
            }
            st.pending_new = Some(st.line);
            line.replace_range(indent..indent + 4, "    ");
            offset = indent + 4;
        }

        // `- Name(params)` method header inside a %hook.
        if st.in_hook()
            && let Some((name, params)) = parse_header(&code[offset..])
        {
            let ns = st.namespace.clone().unwrap_or_default();
            let full = format!("{ns}::{name}");
            let is_new = st.pending_new.take().is_some();
            if is_new && name == "*" {
                return Err(st.err("%new cannot define a wildcard action"));
            }
            let lead = &raw[..offset + (code[offset..].len() - code[offset..].trim_start().len())];
            let lead: String = lead
                .chars()
                .map(|c| if c == '\t' { '\t' } else { ' ' })
                .collect();
            out.push_str(&lead);
            out.push_str(&st.def(&full, is_new, &params));
            st.in_method = true;
            continue;
        }
        if st.pending_new.is_some() && !code[offset..].trim().is_empty() {
            return Err(st.err("%new must be followed by a `- Name` method"));
        }

        // Everything else: scan for %directives left to right.
        let bytes = code.as_bytes();
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        let mut i = offset;
        while i < bytes.len() {
            if bytes[i] != b'%' || !mask[i] {
                i += 1;
                continue;
            }
            if i > 0 && (is_ident(bytes[i - 1]) || matches!(bytes[i - 1], b')' | b']' | b'}')) {
                i += 1; // `a%b`: Lua modulo
                continue;
            }
            let word_len = bytes[i + 1..].iter().take_while(|b| is_ident(**b)).count();
            let word = &code[i + 1..i + 1 + word_len];
            let end = i + 1 + word_len;
            if UNSUPPORTED.contains(&word) {
                return Err(st.err(format!("%{word} is not supported by ycode hooks")));
            }
            if !DIRECTIVES.contains(&word) {
                i = end.max(i + 1);
                continue;
            }
            let after = &code[end..];
            let ws = after.len() - after.trim_start().len();
            // Argument word for %hook/%group/%hookf: identifier chars plus `:` `*` `.`
            let arg_word = |s: &str| -> String {
                s.trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '*' | '.'))
                    .collect()
            };
            match word {
                "hook" => {
                    if st.in_hook() {
                        return Err(st.err("%hook inside a %hook; close it with %end first"));
                    }
                    let ns = arg_word(after);
                    if ns.is_empty() {
                        return Err(st.err("%hook needs a namespace, e.g. `%hook editor`"));
                    }
                    let ns = ns.trim_end_matches("::").to_string();
                    st.stack.push((Nest::Hook, st.line));
                    st.namespace = Some(ns.clone());
                    st.in_method = false;
                    let stop = end + ws + after.trim_start().find(&ns).unwrap_or(0) + ns.len();
                    edits.push((i, stop.min(code.len()), String::new()));
                    i = stop;
                }
                "hookf" => {
                    if st.in_hook() {
                        return Err(st.err("%hookf does not make sense inside a %hook"));
                    }
                    let name = arg_word(after);
                    if name.is_empty() {
                        return Err(st.err("%hookf needs a hook point name"));
                    }
                    let rest = after.trim_start()[name.len()..].trim();
                    let params = if rest.is_empty() {
                        String::new()
                    } else if let Some(inner) =
                        rest.strip_prefix('(').and_then(|r| r.strip_suffix(')'))
                    {
                        inner.to_string()
                    } else {
                        return Err(st.err("expected `%hookf name(params)`"));
                    };
                    st.in_method = true;
                    edits.push((i, code.trim_end().len(), st.def(&name, false, &params)));
                    i = code.len();
                }
                "group" => {
                    if st.in_hook() {
                        return Err(st.err("%group does not make sense inside a %hook"));
                    }
                    if st.stack.iter().any(|(n, _)| *n == Nest::Group) {
                        return Err(st.err("%group inside a %group"));
                    }
                    let g = arg_word(after);
                    if g.is_empty() {
                        return Err(st.err("%group needs a name"));
                    }
                    st.stack.push((Nest::Group, st.line));
                    st.group = Some(g.clone());
                    let stop = end + ws + g.len();
                    edits.push((i, stop, String::new()));
                    i = stop;
                }
                "end" => {
                    match st.stack.pop() {
                        Some((Nest::Hook, _)) => {
                            st.namespace = None;
                            st.in_method = false;
                            if st.pending_new.is_some() {
                                return Err(st.err("%new without a method before %end"));
                            }
                        }
                        Some((Nest::Group, _)) => st.group = None,
                        None => return Err(st.err("dangling %end")),
                    }
                    edits.push((i, end, String::new()));
                    i = end;
                }
                "ctor" => {
                    if st.in_hook() {
                        return Err(st.err("%ctor does not make sense inside a %hook"));
                    }
                    st.in_method = false;
                    let src = format!("{}:{}", st.file, st.line);
                    edits.push((i, end, format!("__ycode.ctor({src:?}).fn = function()")));
                    i = end;
                }
                "init" => {
                    let (text, stop) = if after[ws..].starts_with('(') {
                        let close = after.find(')').ok_or_else(|| st.err("unclosed %init("))?;
                        let g = after[ws + 1..close].trim();
                        if g.is_empty() {
                            ("ycode.init()".to_string(), end + close + 1)
                        } else if g.chars().all(|c| is_ident(c as u8)) {
                            (format!("ycode.init({g:?})"), end + close + 1)
                        } else {
                            return Err(st.err("%init takes a group name"));
                        }
                    } else {
                        ("ycode.init()".to_string(), end)
                    };
                    edits.push((i, stop, text));
                    i = stop;
                }
                "orig" | "log" => {
                    if !st.in_method {
                        return Err(st.err(format!(
                            "%{word} does not make sense outside a hooked method"
                        )));
                    }
                    let has_parens = after[ws..].starts_with('(');
                    let text = match (word, has_parens) {
                        ("orig", true) => "self.orig".to_string(),
                        ("orig", false) => "self.orig()".to_string(),
                        ("log", false) => "ycode.log(self.name)".to_string(),
                        _ => {
                            // %log(a, b) -> ycode.log(self.name, a, b); %log() -> ycode.log(self.name)
                            let empty = raw[end + ws + 1..].trim_start().starts_with(')');
                            let stop = end + ws + 1;
                            let text = if empty {
                                "ycode.log(self.name"
                            } else {
                                "ycode.log(self.name, "
                            };
                            edits.push((i, stop, text.to_string()));
                            i = stop;
                            continue;
                        }
                    };
                    let stop = if has_parens { end + ws } else { end };
                    edits.push((i, stop, text));
                    i = stop;
                }
                "new" => return Err(st.err("%new must start a line, before `- Name`")),
                _ => unreachable!(),
            }
        }
        for (start, end, text) in edits.into_iter().rev() {
            line.replace_range(start..end, &text);
        }
        out.push_str(&line);
    }

    if let Some((kind, line)) = st.stack.last() {
        let what = match kind {
            Nest::Hook => "%hook",
            Nest::Group => "%group",
        };
        st.line = *line;
        return Err(st.err(format!("missing %end ({what} opened here extends to EOF)")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pp(src: &str) -> String {
        preprocess("t.x.lua", src).unwrap()
    }

    #[test]
    fn hook_and_methods() {
        let out = pp(
            "%hook editor\n- Copy\n  %log\n  %orig\nend\n%new - Shout(args)\n  %log(args.text)\nend\n%end\n",
        );
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines.len(), 9);
        assert_eq!(lines[0], "");
        assert_eq!(
            lines[1],
            r#"__ycode.def(nil, "editor::Copy", false, "t.x.lua:2").fn = function(self)"#
        );
        assert_eq!(lines[2], "  ycode.log(self.name)");
        assert_eq!(lines[3], "  self.orig()");
        assert_eq!(
            lines[5].trim_start(),
            r#"__ycode.def(nil, "editor::Shout", true, "t.x.lua:6").fn = function(self, args)"#
        );
        assert_eq!(lines[6], "  ycode.log(self.name, args.text)");
    }

    #[test]
    fn groups_ctor_init_hookf() {
        let out = pp(
            "%group g\n%hook vim\n- *\nreturn %orig(1)\nend\n%end\n%end\n%ctor\n%init(g)\nend\n%hookf app::x(a, b)\nreturn %orig(a)\nend\n",
        );
        assert!(out.contains(r#"__ycode.def("g", "vim::*", false"#), "{out}");
        assert!(out.contains("return self.orig(1)"));
        assert!(out.contains(r#"__ycode.ctor("t.x.lua:8").fn = function()"#));
        assert!(out.contains(r#"ycode.init("g")"#));
        assert!(out.contains(
            r#"__ycode.def(nil, "app::x", false, "t.x.lua:11").fn = function(self, a, b)"#
        ));
        assert_eq!(out.lines().count(), 13);
    }

    #[test]
    fn ignores_strings_comments_and_modulo() {
        let out = pp("%hook e\n- A\n%log(\"x)\")\n%log( )\nend\n%end");
        assert!(
            out.contains("ycode.log(self.name, \"x)\")\nycode.log(self.name )"),
            "{out}"
        );
        let src = "%hook e\n- A\nlocal s = \"%orig\" -- %orig\nlocal m = 5%orig\n--[[ %end\n%orig ]] %orig\nend\n%end";
        let out = pp(src);
        assert!(out.contains("\"%orig\" -- %orig"));
        assert!(out.contains("5%orig"));
        assert!(out.contains("--[[ %end\n%orig ]] self.orig()"));
    }

    #[test]
    fn errors() {
        let e = |s| preprocess("f", s).unwrap_err();
        assert_eq!(
            e("%orig").message,
            "%orig does not make sense outside a hooked method"
        );
        assert_eq!(e("%end").message, "dangling %end");
        assert_eq!(e("%hook a\n%hook b").line, 2);
        assert!(e("%hook a\n").message.starts_with("missing %end"));
        assert!(
            e("%property (nonatomic) int x;")
                .message
                .contains("not supported")
        );
        assert_eq!(e("%hook a\n%new\nlocal x = 1\n%end").line, 3);
    }
}
