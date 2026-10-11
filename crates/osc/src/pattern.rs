//! OSC address patterns (OSC 1.0): `?` one character, `*` any run of characters, `[abc]`,
//! `[a-z]` and `[!a-z]` one character of (or not of) a set, `{foo,bar}` one of the words. Each
//! applies within one part of the address (between two `/`); the number of parts must match.
//! OSC 1.1's `//` (any number of parts) is not supported.
//!
//! Matching is bounded: a pattern is compiled once ([`Pattern::compile`], at most
//! [`MAX_PATTERN`] bytes and [`MAX_ALTERNATIVES`] words per `{}`), and each part is matched by
//! a walk over the positions of the name, one step per token and position, so the cost is at
//! most tokens times characters, without backtracking. Callers pass a [`Budget`] of steps;
//! when it runs out, nothing more matches.

/// Bytes of a pattern.
pub const MAX_PATTERN: usize = 512;
/// Words in one `{}`.
pub const MAX_ALTERNATIVES: usize = 32;

/// Steps a match may still take; shared by the matches of one message.
#[derive(Clone, Copy, Debug)]
pub struct Budget(pub u64);

impl Budget {
    fn spend(&mut self, n: usize) -> bool {
        let n = n as u64;
        if self.0 < n {
            self.0 = 0;
            return false;
        }
        self.0 -= n;
        true
    }

    pub fn exhausted(&self) -> bool {
        self.0 == 0
    }
}

/// Whether `address` holds pattern characters.
pub fn is_pattern(address: &str) -> bool {
    address.bytes().any(|b| matches!(b, b'*' | b'?' | b'[' | b'{'))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(u8),
    Any,
    Star,
    Class { negate: bool, ranges: Vec<(u8, u8)> },
    Alt(Vec<Vec<u8>>),
}

/// A compiled pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    parts: Vec<Vec<Tok>>,
}

impl Pattern {
    /// Compiles `pattern` (an address like `/scene/*/speed`). Errors on an unclosed `[` or `{`,
    /// on a pattern over [`MAX_PATTERN`] bytes and on too many words in a `{}`.
    pub fn compile(pattern: &str) -> Result<Pattern, &'static str> {
        if pattern.len() > MAX_PATTERN {
            return Err("pattern too long");
        }
        let body = pattern.strip_prefix('/').ok_or("a pattern starts with /")?;
        let mut parts = Vec::new();
        for part in body.split('/') {
            parts.push(compile_part(part.as_bytes())?);
        }
        Ok(Pattern { parts })
    }

    /// Number of parts (`/a/b` has two).
    pub fn depth(&self) -> usize {
        self.parts.len()
    }

    /// Whether `address` matches.
    pub fn matches(&self, address: &str, budget: &mut Budget) -> bool {
        let Some(body) = address.strip_prefix('/') else { return false };
        if body.split('/').count() != self.parts.len() {
            return false;
        }
        body.split('/').enumerate().all(|(i, name)| self.matches_part(i, name, budget))
    }

    /// Whether part `i` of the pattern matches one name (a step of a tree walk).
    pub fn matches_part(&self, i: usize, name: &str, budget: &mut Budget) -> bool {
        match self.parts.get(i) {
            Some(toks) => match_part(toks, name.as_bytes(), budget),
            None => false,
        }
    }

    /// Part `i` is a plain name (no pattern characters): that name.
    pub fn literal_part(&self, i: usize) -> Option<String> {
        let toks = self.parts.get(i)?;
        toks.iter()
            .map(|t| match t {
                Tok::Lit(c) => Some(char::from(*c)),
                _ => None,
            })
            .collect()
    }
}

fn compile_part(p: &[u8]) -> Result<Vec<Tok>, &'static str> {
    let mut toks = Vec::new();
    let mut i = 0;
    while let Some(&c) = p.get(i) {
        i += 1;
        match c {
            b'?' => toks.push(Tok::Any),
            b'*' => {
                // Runs of stars are one star.
                if toks.last() != Some(&Tok::Star) {
                    toks.push(Tok::Star);
                }
            }
            b'[' => {
                let close = p.get(i..).and_then(|r| r.iter().position(|b| *b == b']')).ok_or("unclosed [")?;
                let mut set = p.get(i..i + close).ok_or("unclosed [")?;
                i += close + 1;
                let negate = set.first() == Some(&b'!');
                if negate {
                    set = set.get(1..).unwrap_or(&[]);
                }
                let mut ranges = Vec::new();
                let mut j = 0;
                while let Some(&a) = set.get(j) {
                    match (set.get(j + 1), set.get(j + 2)) {
                        (Some(b'-'), Some(&b)) => {
                            ranges.push((a.min(b), a.max(b)));
                            j += 3;
                        }
                        _ => {
                            ranges.push((a, a));
                            j += 1;
                        }
                    }
                }
                toks.push(Tok::Class { negate, ranges });
            }
            b'{' => {
                let close = p.get(i..).and_then(|r| r.iter().position(|b| *b == b'}')).ok_or("unclosed {")?;
                let words = p.get(i..i + close).ok_or("unclosed {")?;
                i += close + 1;
                let alts: Vec<Vec<u8>> = words.split(|b| *b == b',').map(<[u8]>::to_vec).collect();
                if alts.len() > MAX_ALTERNATIVES {
                    return Err("too many words in {}");
                }
                toks.push(Tok::Alt(alts));
            }
            c => toks.push(Tok::Lit(c)),
        }
    }
    Ok(toks)
}

/// Sets reachable positions token by token: `at[j]` means the tokens so far can consume exactly
/// the first `j` bytes of `name`.
fn match_part(toks: &[Tok], name: &[u8], budget: &mut Budget) -> bool {
    let n = name.len();
    let mut at = vec![false; n + 1];
    if let Some(first) = at.first_mut() {
        *first = true;
    }
    let mut next = vec![false; n + 1];
    for tok in toks {
        let cost = match tok {
            Tok::Alt(alts) => (n + 1) * alts.iter().map(|a| a.len().max(1)).sum::<usize>(),
            _ => n + 1,
        };
        if !budget.spend(cost) {
            return false;
        }
        next.iter_mut().for_each(|x| *x = false);
        match tok {
            Tok::Star => {
                let mut any = false;
                for (a, x) in at.iter().zip(next.iter_mut()) {
                    any |= *a;
                    *x = any;
                }
            }
            Tok::Alt(alts) => {
                for (j, _) in at.iter().enumerate().filter(|(_, a)| **a) {
                    for alt in alts {
                        let end = j + alt.len();
                        if name.get(j..end) == Some(alt.as_slice())
                            && let Some(x) = next.get_mut(end)
                        {
                            *x = true;
                        }
                    }
                }
            }
            single => {
                for ((a, c), x) in at.iter().zip(name).zip(next.iter_mut().skip(1)) {
                    *x = *a && one(single, *c);
                }
            }
        }
        std::mem::swap(&mut at, &mut next);
        if !at.iter().any(|a| *a) {
            return false;
        }
    }
    at.last().copied().unwrap_or(false)
}

fn one(tok: &Tok, c: u8) -> bool {
    match tok {
        Tok::Lit(l) => *l == c,
        Tok::Any => true,
        Tok::Class { negate, ranges } => ranges.iter().any(|(a, b)| (*a..=*b).contains(&c)) != *negate,
        Tok::Star | Tok::Alt(_) => false,
    }
}

/// Compiles and matches once, with a generous budget. For tests and one-off checks.
pub fn matches(pattern: &str, address: &str) -> bool {
    Pattern::compile(pattern).is_ok_and(|p| p.matches(address, &mut Budget(1 << 20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wildcards() {
        assert!(matches("/scene/speed", "/scene/speed"));
        assert!(!matches("/scene/speed", "/scene/speeds"));
        assert!(matches("/scene/*", "/scene/speed"));
        assert!(matches("/scene/*", "/scene/"));
        assert!(!matches("/scene/*", "/scene/a/b"), "a star stays inside its part");
        assert!(matches("/*/*", "/a/b"));
        assert!(matches("/s?ene/sp*d", "/scene/speed"));
        assert!(matches("/fader[1-3]", "/fader2"));
        assert!(!matches("/fader[1-3]", "/fader4"));
        assert!(matches("/fader[!1-3]", "/fader4"));
        assert!(matches("/fader[13]", "/fader3"));
        assert!(matches("/{foo,bar}/x", "/bar/x"));
        assert!(!matches("/{foo,bar}/x", "/baz/x"));
        assert!(matches("/a{,b}c", "/ac"));
        assert!(matches("/a*b*c", "/aXXbYYc"));
        assert!(!matches("/a*b*c", "/aXXbYY"));
        assert!(matches("/*{x,y}", "/posx"));
        assert!(!matches("/a[b", "/ab"), "unclosed brackets match nothing");
        assert!(is_pattern("/a/*") && !is_pattern("/a/b"));
        assert_eq!(Pattern::compile("/a/b").unwrap().literal_part(1).as_deref(), Some("b"));
        assert_eq!(Pattern::compile("/a/b*").unwrap().literal_part(1), None);
    }

    #[test]
    fn hostile_patterns_stay_within_their_budget() {
        let p = Pattern::compile(&format!("/{}b", "*a".repeat(200))).unwrap();
        let name = format!("/{}", "a".repeat(400));
        let mut budget = Budget(100_000);
        let t = std::time::Instant::now();
        assert!(!p.matches(&name, &mut budget));
        assert!(budget.exhausted());
        assert!(t.elapsed() < std::time::Duration::from_millis(200));
        assert!(Pattern::compile(&"/a".repeat(300)).is_err());
        let many = format!("/{{{}}}", vec!["x"; 40].join(","));
        assert!(Pattern::compile(&many).is_err());
    }
}
