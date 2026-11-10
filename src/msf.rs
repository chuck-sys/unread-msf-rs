//! msf_unread — minimal Mork (.msf) reader that reports unread messages
//! in Thunderbird folder summary files. No external crates.
//!
//! Build:  rustc -O msf_unread.rs
//! Usage:  msf_unread [-v] <file.msf>...
//! Exit:   0 = at least one unread msg, 1 = none unread, 2 = error
//!
//! Handles: dicts (column + value atom spaces), tables (incl. meta + cuts),
//! rows (incl. row cuts / cell cuts / updates), atom refs, value escapes
//! (\x, $XX, line continuations), comments, and transaction groups
//! (aborted groups are discarded).

use std::collections::{BTreeSet, HashMap};
use std::io::{self, BufRead};

const MSGS_SCOPE: &str = "ns:msg:db:row:scope:msgs:all";
const MSGS_KIND: &str = "ns:msg:db:table:kind:msgs";
const MSG_FLAG_READ: u32 = 0x1;
const MSG_FLAG_EXPUNGED: u32 = 0x8;
const KEEP_COLS: &[&str] = &["flags", "subject"];

pub type RowKey = (String, String); // (scope, id)

#[derive(Default)]
struct Table {
    kind: Option<String>,
    rows: BTreeSet<RowKey>,
}

#[derive(Default)]
pub struct Mork {
    col_atoms: HashMap<String, Vec<u8>>,
    val_atoms: HashMap<String, Vec<u8>>,
    rows: HashMap<RowKey, HashMap<String, Vec<u8>>>,
    tables: HashMap<RowKey, Table>,
}

pub struct Parser<'m, R: BufRead> {
    s: Src<R>,
    m: &'m mut Mork,
}

fn s(v: &[u8]) -> String {
    String::from_utf8_lossy(v).into_owned()
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

struct Look {
    buf: [u8; 8],
    start: usize,
    len: usize,
}

impl Look {
    fn new() -> Self {
        Look { buf: [0; 8], start: 0, len: 0 }
    }

    fn get(&self, i: usize) -> u8 {
        self.buf[self.start + i]
    }

    /// Appends `bytes`; caller guarantees `len + bytes.len() <= LOOK_CAP`.
    fn push(&mut self, bytes: &[u8]) {
        if self.start + self.len + bytes.len() > 8 {
            // slide contents to the front to make room
            self.buf.copy_within(self.start..self.start + self.len, 0);
            self.start = 0;
        }
        let end = self.start + self.len;
        self.buf[end..end + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    fn pop_front(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let c = self.buf[self.start];
        self.start += 1;
        self.len -= 1;
        if self.len == 0 {
            self.start = 0;
        }
        Some(c)
    }
}

/// Wraps a `BufRead`. Reads come straight from the reader's buffer; only
/// when a lookahead (max 8 bytes) straddles a buffer refill are those few
/// bytes copied into `look`. Logical stream = `look` ++ rest of `r`.
struct Src<R: BufRead> {
    r: R,
    look: Look,
    err: Option<io::Error>,
}

impl<R: BufRead> Src<R> {
    fn new(r: R) -> Self {
        Src { r, look: Look::new(), err: None }
    }

    /// The reader's current buffer (empty at EOF or on error).
    /// (fill_buf is called twice to satisfy the borrow checker; the second
    /// call just returns the already-filled buffer.)
    fn buf(&mut self) -> &[u8] {
        loop {
            match self.r.fill_buf() {
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.err = Some(e);
                    return &[];
                }
            }
        }
        if self.err.is_some() {
            return &[];
        }
        self.r.fill_buf().unwrap_or(&[])
    }

    /// Ensures `look` holds at least `need` bytes (unless EOF).
    fn fill_look(&mut self, need: usize) -> bool {
        while self.look.len < need {
            let want = need - self.look.len;
            let b = self.buf();
            if b.is_empty() {
                return false;
            }
            let take = want.min(b.len());
            let mut tmp = [0u8; 8];
            tmp[..take].copy_from_slice(&b[..take]);
            self.r.consume(take);
            self.look.push(&tmp[..take]);
        }
        true
    }

    fn peek_at(&mut self, i: usize) -> Option<u8> {
        if self.look.len == 0 {
            // fast path: read directly from the BufRead's buffer
            let b = self.buf();
            if i < b.len() {
                return Some(b[i]);
            }
        }
        if self.fill_look(i + 1) { Some(self.look.get(i)) } else { None }
    }

    fn peek(&mut self) -> Option<u8> {
        self.peek_at(0)
    }

    fn starts(&mut self, pat: &[u8]) -> bool {
        (0..pat.len()).all(|i| self.peek_at(i) == Some(pat[i]))
    }

    fn next(&mut self) -> Option<u8> {
        if let Some(c) = self.look.pop_front() {
            return Some(c);
        }
        let c = *self.buf().first()?;
        self.r.consume(1);
        Some(c)
    }

    fn bump(&mut self) {
        self.next();
    }

    fn skip(&mut self, n: usize) {
        for _ in 0..n {
            self.bump();
        }
    }
}

impl<'m, R: BufRead> Parser<'m, R> {
    /// Parses Mork from any buffered reader, e.g. `BufReader<File>`,
    /// `stdin().lock()`, or a `&[u8]`.
    pub fn new(r: R, m: &'m mut Mork) -> Self {
        Parser { s: Src::new(r), m }
    }

    pub fn run(mut self) -> io::Result<()> {
        self.parse();
        match self.s.err.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.s.peek() {
            if c.is_ascii_whitespace() {
                self.s.bump();
            } else if self.s.starts(b"//") {
                while let Some(c) = self.s.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.s.bump();
                }
            } else if c == b'\\' && matches!(self.s.peek_at(1), Some(b'\n' | b'\r')) {
                self.s.bump(); // stray line continuation
            } else {
                break;
            }
        }
    }

    /// Reads a literal value up to (and consuming) an unescaped ')'.
    fn read_value(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(c) = self.s.next() {
            match c {
                b')' => break,
                b'\\' => match self.s.peek() {
                    Some(b'\r') => {
                        self.s.bump();
                        if self.s.peek() == Some(b'\n') {
                            self.s.bump();
                        }
                    }
                    Some(b'\n') => {
                        self.s.bump();
                        if self.s.peek() == Some(b'\r') {
                            self.s.bump();
                        }
                    }
                    Some(n) => {
                        out.push(n);
                        self.s.bump();
                    }
                    None => {}
                },
                b'$' => match (self.s.peek_at(0), self.s.peek_at(1)) {
                    (Some(h), Some(l)) if is_hex(h) && is_hex(l) => {
                        let hv = (h as char).to_digit(16).unwrap() as u8;
                        let lv = (l as char).to_digit(16).unwrap() as u8;
                        out.push(hv << 4 | lv);
                        self.s.skip(2);
                    }
                    _ => out.push(b'$'),
                },
                _ => out.push(c),
            }
        }
        out
    }

    fn read_hex(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.s.peek().filter(|&c| is_hex(c)) {
            out.push(c.to_ascii_uppercase() as char);
            self.s.bump();
        }
        out
    }

    /// Reads a token ending at whitespace or a structural char.
    fn read_token(&mut self) -> String {
        let mut out = Vec::new();
        while let Some(c) = self.s.peek() {
            if c.is_ascii_whitespace() || b"{}[]()<>".contains(&c) {
                break;
            }
            out.push(c);
            self.s.bump();
        }
        s(&out)
    }

    /// Reads up to '=' (consumed) or ')' (not consumed).
    fn read_until_eq(&mut self) -> String {
        let mut out = Vec::new();
        while let Some(c) = self.s.peek() {
            if c == b')' {
                break;
            }
            self.s.bump();
            if c == b'=' {
                break;
            }
            out.push(c);
        }
        s(&out).trim().to_string()
    }

    fn col_name(&self, id: &str) -> String {
        self.m.col_atoms.get(id).map(|v| s(v)).unwrap_or_else(|| format!("^{id}"))
    }

    /// Parses "id[:scope]" where scope is "^XX" (column atom) or a literal.
    fn parse_oid(&self, tok: &str, default_scope: &str) -> RowKey {
        let (id, scope) = match tok.split_once(':') {
            Some((id, sc)) => {
                let sc = match sc.strip_prefix('^') {
                    Some(r) => self.col_name(&r.to_ascii_uppercase()),
                    None => sc.to_string(),
                };
                (id, sc)
            }
            None => (tok, default_scope.to_string()),
        };
        (scope, id.trim_start_matches('^').to_ascii_uppercase())
    }

    fn parse(&mut self) {
        loop {
            self.skip_ws();
            match self.s.peek() {
                None => break,
                Some(b'<') => self.parse_dict(),
                Some(b'{') => self.parse_table(),
                Some(b'[') => {
                    self.parse_row("");
                }
                Some(b'@') => self.parse_group(),
                Some(_) => self.s.bump(), // tolerate junk
            }
        }
    }

    fn parse_dict(&mut self) {
        self.s.bump(); // '<'
        let mut cols = false;
        loop {
            self.skip_ws();
            match self.s.peek() {
                None => return,
                Some(b'>') => {
                    self.s.bump();
                    return;
                }
                Some(b'<') => {
                    // dict meta, e.g. <(a=c)>
                    self.s.bump();
                    loop {
                        self.skip_ws();
                        match self.s.peek() {
                            None => return,
                            Some(b'>') => {
                                self.s.bump();
                                break;
                            }
                            Some(b'(') => {
                                self.s.bump();
                                let name = self.read_until_eq();
                                let val = self.read_value();
                                if name == "a" {
                                    cols = val == b"c";
                                }
                            }
                            Some(_) => self.s.bump(),
                        }
                    }
                }
                Some(b'(') => {
                    self.s.bump();
                    let id = self.read_until_eq().to_ascii_uppercase();
                    let val = self.read_value();
                    if cols {
                        self.m.col_atoms.insert(id, val);
                    } else {
                        self.m.val_atoms.insert(id, val);
                    }
                }
                Some(_) => self.s.bump(),
            }
        }
    }

    fn parse_table(&mut self) {
        self.s.bump(); // '{'
        self.skip_ws();
        let cut_all = self.s.peek() == Some(b'-');
        if cut_all {
            self.s.bump();
        }
        let tok = self.read_token();
        let key = self.parse_oid(&tok, "");
        let scope = key.0.clone();
        let table = self.m.tables.entry(key.clone()).or_default();
        if cut_all {
            table.rows.clear();
        }
        let mut minus = false;
        loop {
            self.skip_ws();
            match self.s.peek() {
                None => return,
                Some(b'}') => {
                    self.s.bump();
                    return;
                }
                Some(b'{') => {
                    if let Some(kind) = self.parse_table_meta() {
                        self.m.tables.get_mut(&key).unwrap().kind = Some(kind);
                    }
                }
                Some(b'[') => {
                    let rk = self.parse_row(&scope);
                    self.member(&key, rk, minus);
                    minus = false;
                }
                Some(b'-') => {
                    self.s.bump();
                    minus = true;
                }
                Some(b'@') => self.parse_group(),
                Some(c) if is_hex(c) || c == b'^' => {
                    let t = self.read_token();
                    let rk = self.parse_oid(&t, &scope);
                    self.member(&key, rk, minus);
                    minus = false;
                }
                Some(_) => self.s.bump(),
            }
        }
    }

    fn member(&mut self, table: &RowKey, row: RowKey, remove: bool) {
        let t = self.m.tables.get_mut(table).unwrap();
        if remove {
            t.rows.remove(&row);
        } else {
            t.rows.insert(row);
        }
    }

    /// Parses `{(k^XX:c)(s=9)...}`; returns the table kind if present.
    fn parse_table_meta(&mut self) -> Option<String> {
        self.s.bump(); // '{'
        let mut kind = None;
        loop {
            self.skip_ws();
            match self.s.peek() {
                None => return kind,
                Some(b'}') => {
                    self.s.bump();
                    return kind;
                }
                Some(b'(') => {
                    self.s.bump();
                    let raw = s(&self.read_value());
                    if let Some(rest) = raw.strip_prefix('k') {
                        let rest = rest.trim();
                        if let Some(r) = rest.strip_prefix('^') {
                            let id: String = r.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                            kind = Some(self.col_name(&id.to_ascii_uppercase()));
                        } else if let Some(r) = rest.strip_prefix('=') {
                            kind = Some(r.to_string());
                        }
                    }
                }
                Some(b'[') => {
                    self.parse_row("");
                }
                Some(_) => self.s.bump(),
            }
        }
    }

    fn parse_row(&mut self, default_scope: &str) -> RowKey {
        self.s.bump(); // '['
        self.skip_ws();
        let cut_all = self.s.peek() == Some(b'-');
        if cut_all {
            self.s.bump();
        }
        let tok = self.read_token();
        let key = self.parse_oid(&tok, default_scope);
        let mut cells = self.m.rows.remove(&key).unwrap_or_default();
        if cut_all {
            cells.clear();
        }
        let mut cut_next = false;
        loop {
            self.skip_ws();
            match self.s.peek() {
                None => break,
                Some(b']') => {
                    self.s.bump();
                    break;
                }
                Some(b'[') => {
                    // row meta: skip
                    while let Some(c) = self.s.next() {
                        if c == b']' {
                            break;
                        }
                    }
                }
                Some(b'-') => {
                    self.s.bump();
                    cut_next = true;
                }
                Some(b'(') => {
                    self.s.bump();
                    let (col, val) = self.parse_cell();
                    if cut_next {
                        cells.remove(&col);
                    } else if KEEP_COLS.contains(&col.as_str()) {
                        cells.insert(col, val);
                    }
                    cut_next = false;
                }
                Some(_) => self.s.bump(),
            }
        }
        self.m.rows.insert(key.clone(), cells);
        key
    }

    /// After '(' — returns (column name, value). Consumes ')'.
    fn parse_cell(&mut self) -> (String, Vec<u8>) {
        let col = if self.s.peek() == Some(b'^') {
            self.s.bump();
            let id = self.read_hex();
            self.col_name(&id)
        } else {
            let mut out = Vec::new();
            while let Some(c) = self.s.peek() {
                if c == b'=' || c == b'^' || c == b')' {
                    break;
                }
                out.push(c);
                self.s.bump();
            }
            s(&out)
        };
        let val = match self.s.peek() {
            Some(b'=') => {
                self.s.bump();
                self.read_value()
            }
            Some(b'^') => {
                self.s.bump();
                let id = self.read_hex();
                while let Some(c) = self.s.next() {
                    if c == b')' {
                        break;
                    }
                }
                self.m.val_atoms.get(&id).cloned().unwrap_or_default()
            }
            Some(b')') => {
                self.s.bump();
                Vec::new()
            }
            _ => Vec::new(),
        };
        (col, val)
    }

    /// Handles a transaction group `@$${ID{@ ... @$$}ID}@`.
    ///
    /// Since we can't look ahead in a stream, the group body is collected
    /// into memory and only parsed once we know it was committed. Aborted
    /// (`@$$}~~}@`) and unterminated groups are discarded.
    fn parse_group(&mut self) {
        if self.s.starts(b"@$${") {
            self.s.skip(4);
            // group id, up to "{@"
            loop {
                if self.s.starts(b"{@") {
                    self.s.skip(2);
                    break;
                }
                if self.s.next().is_none() {
                    return;
                }
            }
            // body, up to "@$$}"
            let mut body = Vec::new();
            loop {
                if self.s.starts(b"@$$}") {
                    self.s.skip(4);
                    break;
                }
                match self.s.next() {
                    Some(c) => body.push(c),
                    None => return, // unterminated: discard
                }
            }
            // terminator: "ID}@" (commit) or "~~}@" (abort)
            let mut term = Vec::new();
            loop {
                if self.s.starts(b"}@") {
                    self.s.skip(2);
                    break;
                }
                match self.s.next() {
                    Some(c) => term.push(c),
                    None => break,
                }
            }
            if term != b"~~" {
                // Committed: parse the body against the same Mork state.
                let _ = Parser::new(&body[..], self.m).run();
            }
        } else if self.s.starts(b"@$$}") {
            // stray end marker
            self.s.skip(4);
            while !self.s.starts(b"}@") {
                if self.s.next().is_none() {
                    return;
                }
            }
            self.s.skip(2);
        } else {
            self.s.bump();
        }
    }
}

pub struct Report {
    pub total: usize,
    pub unread: Vec<(RowKey, String)>,
}

fn hex_u32(v: Option<&Vec<u8>>) -> u32 {
    v.and_then(|v| u32::from_str_radix(s(v).trim(), 16).ok()).unwrap_or(0)
}

pub fn analyze(m: &Mork) -> Report {
    // Message rows = members of the msgs table(s). Fall back to every row in
    // the msgs scope if no such table is found.
    let mut keys: BTreeSet<&RowKey> = m
        .tables
        .iter()
        .filter(|(k, t)| k.0 == MSGS_SCOPE || t.kind.as_deref() == Some(MSGS_KIND))
        .flat_map(|(_, t)| t.rows.iter())
        .collect();
    if keys.is_empty() {
        keys = m.rows.keys().filter(|k| k.0 == MSGS_SCOPE).collect();
    }

    let mut total = 0;
    let mut unread = Vec::new();
    for k in keys {
        let Some(cells) = m.rows.get(k) else { continue };
        let flags = hex_u32(cells.get("flags"));
        if flags & MSG_FLAG_EXPUNGED != 0 {
            continue;
        }
        total += 1;
        if flags & MSG_FLAG_READ == 0 {
            let subj = cells.get("subject").map(|v| s(v)).unwrap_or_default();
            unread.push((k.clone(), subj));
        }
    }
    unread.sort_by_key(|((_, id), _)| u64::from_str_radix(id, 16).unwrap_or(u64::MAX));
    Report { total, unread }
}
