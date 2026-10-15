//! Shared SQL/PL-SQL lexer. Understands `--` and `/* */` comments, `'..'`
//! strings, `q'[..]'` literals and `"quoted"` identifiers, so keywords inside
//! them are never mistaken for syntax. Offsets are `char` indices.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    /// Unquoted identifier or keyword, upper-cased.
    Word(String),
    /// `"Quoted Identifier"`, case preserved, quotes removed.
    Quoted(String),
    Punct(char),
    /// String/number literals.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub tok: Tok,
    /// Start (inclusive) and end (exclusive) as char indices into the source.
    pub start: usize,
    pub end: usize,
}

pub fn lex(src: &str) -> Vec<Token> {
    let c: Vec<char> = src.chars().collect();
    let n = c.len();
    let mut i = 0;
    let mut out = Vec::new();
    let mut push = |tok: Tok, start: usize, end: usize| out.push(Token { tok, start, end });
    while i < n {
        let ch = c[i];
        let s = i;
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '-' && i + 1 < n && c[i + 1] == '-' {
            while i < n && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && i + 1 < n && c[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(c[i] == '*' && c[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
        } else if (ch == 'q' || ch == 'Q')
            && i + 2 < n
            && c[i + 1] == '\''
            && !c[i + 2].is_whitespace()
        {
            let open = c[i + 2];
            let close = match open {
                '[' => ']',
                '{' => '}',
                '(' => ')',
                '<' => '>',
                o => o,
            };
            i += 3;
            while i + 1 < n && !(c[i] == close && c[i + 1] == '\'') {
                i += 1;
            }
            i = (i + 2).min(n);
            push(Tok::Other, s, i);
        } else if ch == '\'' {
            i += 1;
            while i < n {
                if c[i] == '\'' {
                    if i + 1 < n && c[i + 1] == '\'' {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            push(Tok::Other, s, i);
        } else if ch == '"' {
            i += 1;
            let b = i;
            while i < n && c[i] != '"' {
                i += 1;
            }
            let name: String = c[b..i].iter().collect();
            i = (i + 1).min(n);
            push(Tok::Quoted(name), s, i);
        } else if ch.is_alphabetic() || ch == '_' {
            while i < n && (c[i].is_alphanumeric() || matches!(c[i], '_' | '$' | '#')) {
                i += 1;
            }
            push(
                Tok::Word(c[s..i].iter().collect::<String>().to_uppercase()),
                s,
                i,
            );
        } else if ch.is_ascii_digit() {
            while i < n && (c[i].is_ascii_alphanumeric() || c[i] == '.') {
                i += 1;
            }
            push(Tok::Other, s, i);
        } else {
            i += 1;
            push(Tok::Punct(ch), s, i);
        }
    }
    out
}

/// Convert a UTF-16 offset (what JavaScript/CodeMirror use) to a char index.
pub fn utf16_to_char_index(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, ch) in s.chars().enumerate() {
        if units >= utf16 {
            return i;
        }
        units += ch.len_utf16();
    }
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_char_indices() {
        let t = lex("select a.b from \"My T\"");
        let words: Vec<_> = t.iter().map(|t| (t.tok.clone(), t.start, t.end)).collect();
        assert_eq!(words[0], (Tok::Word("SELECT".into()), 0, 6));
        assert_eq!(words[2], (Tok::Punct('.'), 8, 9));
        assert_eq!(words.last().unwrap(), &(Tok::Quoted("My T".into()), 16, 22));
    }

    #[test]
    fn utf16_conversion_handles_astral_chars() {
        // '😀' is 2 UTF-16 units but 1 char.
        assert_eq!(utf16_to_char_index("a😀b", 3), 2);
        assert_eq!(utf16_to_char_index("abc", 99), 3);
    }
}
