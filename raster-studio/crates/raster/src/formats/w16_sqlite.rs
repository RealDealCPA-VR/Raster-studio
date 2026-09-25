//! W16-L: a bounded, read-only SQLite table reader: enough of the file
//! format (<https://www.sqlite.org/fileformat.html>) to list the tables and
//! read a table's rows, blobs included, from a database held in memory.
//! Clip Studio Paint stores its document in one (see [`super::clip`]).
//!
//! Only table b-trees are walked (interior `0x05` and leaf `0x0D` pages,
//! with overflow chains); indexes, WAL files and free lists are never
//! needed. Every page is visited at most once per walk, the tree depth is
//! capped, every payload is checked against a byte cap before it is
//! allocated, and every offset is checked before it is followed, so a
//! damaged or hostile database errors instead of looping or panicking.

use std::collections::HashSet;

use super::super::malformed;
use crate::codec::CodecError;

const MAGIC: &[u8] = b"SQLite format 3\0";
/// The deepest table b-tree walked; real trees are a handful of levels.
const MAX_DEPTH: usize = 40;

/// One column value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    /// The value as text, when it is text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The value as an integer, when it is one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// The value as a blob, when it is one.
    pub fn as_blob(&self) -> Option<&[u8]> {
        match self {
            Value::Blob(b) => Some(b),
            _ => None,
        }
    }
}

/// A table named in `sqlite_master`.
#[derive(Debug, Clone)]
pub struct Table {
    pub name: String,
    pub root: u32,
    /// The table's column names, from its `CREATE TABLE` statement.
    pub columns: Vec<String>,
}

/// A database in memory.
pub struct Db<'a> {
    bytes: &'a [u8],
    page_size: usize,
    usable: usize,
    pages: usize,
    what: &'a str,
}

/// `true` when `head` starts with the SQLite header string.
pub fn looks_like_sqlite(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

fn varint(b: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let byte = *b.get(at + i)?;
        if i == 8 {
            return Some(((v << 8) | u64::from(byte), 9));
        }
        v = (v << 7) | u64::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

impl<'a> Db<'a> {
    /// Open `bytes` as a database; `what` names the containing format in
    /// errors.
    pub fn open(bytes: &'a [u8], what: &'a str) -> Result<Self, CodecError> {
        if bytes.len() < 100 || !looks_like_sqlite(bytes) {
            return Err(malformed(what, "its database is not an SQLite file"));
        }
        let raw = usize::from(u16::from_be_bytes([bytes[16], bytes[17]]));
        let page_size = if raw == 1 { 65536 } else { raw };
        if page_size < 512 || !page_size.is_power_of_two() {
            return Err(malformed(what, "its database declares a bad page size"));
        }
        let reserved = usize::from(bytes[20]);
        let usable = page_size.saturating_sub(reserved);
        if usable < 480 {
            return Err(malformed(what, "its database reserves too much of a page"));
        }
        Ok(Db {
            bytes,
            page_size,
            usable,
            pages: bytes.len() / page_size,
            what,
        })
    }

    fn bad(&self, why: &str) -> CodecError {
        malformed(self.what, format!("its database is damaged: {why}"))
    }

    fn page(&self, n: u32) -> Result<&'a [u8], CodecError> {
        let n = n as usize;
        if n == 0 || n > self.pages {
            return Err(self.bad("a page number points past the file"));
        }
        let at = (n - 1) * self.page_size;
        Ok(&self.bytes[at..at + self.page_size])
    }

    /// Every table in `sqlite_master`.
    pub fn tables(&self) -> Result<Vec<Table>, CodecError> {
        let mut out = Vec::new();
        for row in self.rows(1, 1 << 20)? {
            let is_table = row.first().and_then(Value::as_text) == Some("table");
            let name = row.get(1).and_then(Value::as_text);
            let root = row.get(3).and_then(Value::as_int);
            let sql = row.get(4).and_then(Value::as_text).unwrap_or("");
            if let (true, Some(name), Some(root)) = (is_table, name, root) {
                if let Ok(root) = u32::try_from(root) {
                    out.push(Table {
                        name: name.to_string(),
                        root,
                        columns: columns_of(sql),
                    });
                }
            }
        }
        Ok(out)
    }

    /// The table called `name` (case-insensitively).
    pub fn table(&self, name: &str) -> Result<Option<Table>, CodecError> {
        Ok(self
            .tables()?
            .into_iter()
            .find(|t| t.name.eq_ignore_ascii_case(name)))
    }

    /// Every row of the table rooted at page `root`, each payload at most
    /// `cap` bytes and all of them together at most `cap * 4`.
    pub fn rows(&self, root: u32, cap: u64) -> Result<Vec<Vec<Value>>, CodecError> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut budget = cap.saturating_mul(4);
        self.walk(root, 0, cap, &mut budget, &mut seen, &mut out)?;
        Ok(out)
    }

    fn walk(
        &self,
        n: u32,
        depth: usize,
        cap: u64,
        budget: &mut u64,
        seen: &mut HashSet<u32>,
        out: &mut Vec<Vec<Value>>,
    ) -> Result<(), CodecError> {
        if depth > MAX_DEPTH {
            return Err(self.bad("the table tree is too deep"));
        }
        if !seen.insert(n) {
            return Err(self.bad("a page is linked twice"));
        }
        let page = self.page(n)?;
        let h = if n == 1 { 100 } else { 0 };
        let kind = *page.get(h).ok_or_else(|| self.bad("a page is empty"))?;
        let cells = usize::from(u16::from_be_bytes([page[h + 3], page[h + 4]]));
        let header = match kind {
            0x0D => 8,
            0x05 => 12,
            _ => return Err(self.bad("a table page has an unknown type")),
        };
        for i in 0..cells {
            let at = h + header + i * 2;
            let ptr = page
                .get(at..at + 2)
                .map(|s| usize::from(u16::from_be_bytes([s[0], s[1]])))
                .ok_or_else(|| self.bad("a cell pointer runs past its page"))?;
            if kind == 0x05 {
                let child = page
                    .get(ptr..ptr + 4)
                    .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
                    .ok_or_else(|| self.bad("an interior cell runs past its page"))?;
                self.walk(child, depth + 1, cap, budget, seen, out)?;
            } else {
                let payload = self.leaf_payload(page, ptr, cap, seen)?;
                *budget = budget.checked_sub(payload.len() as u64).ok_or_else(|| {
                    CodecError::LimitExceeded(format!(
                        "the {} database holds more than the import limit allows",
                        self.what
                    ))
                })?;
                out.push(self.record(&payload)?);
            }
        }
        if kind == 0x05 {
            let right = u32::from_be_bytes([page[h + 8], page[h + 9], page[h + 10], page[h + 11]]);
            self.walk(right, depth + 1, cap, budget, seen, out)?;
        }
        Ok(())
    }

    fn leaf_payload(
        &self,
        page: &[u8],
        at: usize,
        cap: u64,
        seen: &mut HashSet<u32>,
    ) -> Result<Vec<u8>, CodecError> {
        let (size, a) = varint(page, at).ok_or_else(|| self.bad("a cell is cut short"))?;
        let (_rowid, b) = varint(page, at + a).ok_or_else(|| self.bad("a cell is cut short"))?;
        if size > cap {
            return Err(CodecError::LimitExceeded(format!(
                "a {} database record is larger than {cap} bytes",
                self.what
            )));
        }
        let p = size as usize;
        let u = self.usable;
        let x = u - 35;
        let local = if p <= x {
            p
        } else {
            let m = ((u - 12) * 32 / 255) - 23;
            let k = m + ((p - m) % (u - 4));
            if k <= x {
                k
            } else {
                m
            }
        };
        let start = at + a + b;
        let mut out = Vec::with_capacity(p.min(1 << 20));
        out.extend_from_slice(
            page.get(start..start + local)
                .ok_or_else(|| self.bad("a record runs past its page"))?,
        );
        if local < p {
            let mut next = page
                .get(start + local..start + local + 4)
                .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
                .ok_or_else(|| self.bad("an overflow pointer runs past its page"))?;
            while out.len() < p {
                if !seen.insert(next) {
                    return Err(self.bad("an overflow chain loops"));
                }
                let ov = self.page(next)?;
                next = u32::from_be_bytes([ov[0], ov[1], ov[2], ov[3]]);
                let take = (p - out.len()).min(u - 4);
                out.extend_from_slice(&ov[4..4 + take]);
            }
        }
        Ok(out)
    }

    fn record(&self, payload: &[u8]) -> Result<Vec<Value>, CodecError> {
        let (hsize, n) = varint(payload, 0).ok_or_else(|| self.bad("a record header is cut"))?;
        let hsize = usize::try_from(hsize).unwrap_or(usize::MAX);
        if hsize > payload.len() {
            return Err(self.bad("a record header runs past its record"));
        }
        let mut types = Vec::new();
        let mut at = n;
        while at < hsize {
            let (t, k) = varint(payload, at).ok_or_else(|| self.bad("a record header is cut"))?;
            types.push(t);
            at += k;
        }
        let mut body = hsize;
        let mut out = Vec::with_capacity(types.len());
        for t in types {
            let len = match t {
                0 | 8 | 9 | 10 | 11 => 0,
                1 => 1,
                2 => 2,
                3 => 3,
                4 => 4,
                5 => 6,
                6 | 7 => 8,
                t => usize::try_from((t - 12) / 2).unwrap_or(usize::MAX),
            };
            let bytes = body
                .checked_add(len)
                .and_then(|end| payload.get(body..end))
                .ok_or_else(|| self.bad("a value runs past its record"))?;
            body += len;
            out.push(match t {
                0 | 10 | 11 => Value::Null,
                8 => Value::Int(0),
                9 => Value::Int(1),
                1..=6 => {
                    let mut v: i64 = if bytes[0] & 0x80 != 0 { -1 } else { 0 };
                    for b in bytes {
                        v = (v << 8) | i64::from(*b);
                    }
                    Value::Int(v)
                }
                7 => {
                    let mut a = [0u8; 8];
                    a.copy_from_slice(bytes);
                    Value::Float(f64::from_be_bytes(a))
                }
                t if t % 2 == 0 => Value::Blob(bytes.to_vec()),
                _ => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
            });
        }
        Ok(out)
    }
}

/// The column names of a `CREATE TABLE` statement, in order.
pub fn columns_of(sql: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (sql.find('('), sql.rfind(')')) else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    let body = &sql[open + 1..close];
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in body.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&body[start..]);
    parts
        .into_iter()
        .filter_map(|p| {
            let word = p.split_whitespace().next()?;
            let keyword = word.split('(').next().unwrap_or(word);
            let upper = keyword.to_ascii_uppercase();
            if ["PRIMARY", "UNIQUE", "CHECK", "FOREIGN", "CONSTRAINT"].contains(&upper.as_str()) {
                return None;
            }
            Some(
                word.trim_matches(|c| matches!(c, '"' | '`' | '[' | ']' | '\''))
                    .to_string(),
            )
        })
        .collect()
}

/// A test-only SQLite writer: one row per leaf page, an interior root over
/// several leaves, overflow chains for long records; page size 512.
#[cfg(test)]
pub(crate) mod writer {
    use super::Value;

    const PAGE: usize = 512;

    fn varint(mut v: u64) -> Vec<u8> {
        if v > 0x00FF_FFFF_FFFF_FFFF {
            let mut out = vec![0u8; 9];
            out[8] = v as u8;
            v >>= 8;
            for i in (0..8).rev() {
                out[i] = ((v & 0x7F) as u8) | 0x80;
                v >>= 7;
            }
            return out;
        }
        let mut groups = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            groups.push(((v & 0x7F) as u8) | 0x80);
            v >>= 7;
        }
        groups.reverse();
        groups
    }

    fn record(values: &[Value]) -> Vec<u8> {
        let mut types = Vec::new();
        let mut body = Vec::new();
        for v in values {
            match v {
                Value::Null => types.extend(varint(0)),
                Value::Int(i) => {
                    types.extend(varint(6));
                    body.extend_from_slice(&i.to_be_bytes());
                }
                Value::Float(f) => {
                    types.extend(varint(7));
                    body.extend_from_slice(&f.to_be_bytes());
                }
                Value::Text(s) => {
                    types.extend(varint(s.len() as u64 * 2 + 13));
                    body.extend_from_slice(s.as_bytes());
                }
                Value::Blob(b) => {
                    types.extend(varint(b.len() as u64 * 2 + 12));
                    body.extend_from_slice(b);
                }
            }
        }
        // The header size counts itself; one byte suffices for these tests.
        let mut out = varint(types.len() as u64 + 1);
        out.extend(types);
        out.extend(body);
        out
    }

    /// A database with `tables` (name, `CREATE TABLE` sql, rows).
    pub fn build(tables: &[(&str, &str, Vec<Vec<Value>>)]) -> Vec<u8> {
        let mut pages: Vec<Vec<u8>> = vec![vec![0u8; PAGE]];
        let mut master_rows = Vec::new();
        for (name, sql, rows) in tables {
            let mut leaves = Vec::new();
            for (i, row) in rows.iter().enumerate() {
                let payload = record(row);
                let leaf = leaf_page(&mut pages, &payload, i as u64 + 1, 0);
                leaves.push(leaf);
            }
            let root = if leaves.len() == 1 {
                leaves[0]
            } else {
                let mut p = vec![0u8; PAGE];
                p[0] = 0x05;
                let cells = leaves.len() - 1;
                p[3..5].copy_from_slice(&(cells as u16).to_be_bytes());
                let right = *leaves.last().unwrap();
                p[8..12].copy_from_slice(&right.to_be_bytes());
                let mut content = PAGE;
                for (i, leaf) in leaves[..cells].iter().enumerate() {
                    let mut cell = leaf.to_be_bytes().to_vec();
                    cell.extend(varint(i as u64 + 1));
                    content -= cell.len();
                    p[content..content + cell.len()].copy_from_slice(&cell);
                    p[12 + i * 2..14 + i * 2].copy_from_slice(&(content as u16).to_be_bytes());
                }
                pages.push(p);
                pages.len() as u32
            };
            master_rows.push(vec![
                Value::Text("table".into()),
                Value::Text(name.to_string()),
                Value::Text(name.to_string()),
                Value::Int(i64::from(root)),
                Value::Text(sql.to_string()),
            ]);
        }
        // Page 1: the header and sqlite_master as one leaf.
        let mut p1 = vec![0u8; PAGE];
        p1[..16].copy_from_slice(b"SQLite format 3\0");
        p1[16..18].copy_from_slice(&(PAGE as u16).to_be_bytes());
        p1[18] = 1;
        p1[19] = 1;
        p1[21] = 64;
        p1[22] = 32;
        p1[23] = 32;
        p1[100] = 0x0D;
        p1[103..105].copy_from_slice(&(master_rows.len() as u16).to_be_bytes());
        let mut content = PAGE;
        for (i, row) in master_rows.iter().enumerate() {
            let payload = record(row);
            let mut cell = varint(payload.len() as u64);
            cell.extend(varint(i as u64 + 1));
            cell.extend(payload);
            assert!(cell.len() < 300, "the test master row must fit locally");
            content -= cell.len();
            p1[content..content + cell.len()].copy_from_slice(&cell);
            p1[108 + i * 2..110 + i * 2].copy_from_slice(&(content as u16).to_be_bytes());
        }
        p1[105..107].copy_from_slice(&(content as u16).to_be_bytes());
        pages[0] = p1;
        let count = pages.len() as u32;
        pages[0][28..32].copy_from_slice(&count.to_be_bytes());
        pages.concat()
    }

    /// Append a leaf page holding one record (and its overflow pages);
    /// return its page number.
    fn leaf_page(pages: &mut Vec<Vec<u8>>, payload: &[u8], rowid: u64, reserved: usize) -> u32 {
        let u = PAGE - reserved;
        let p = payload.len();
        let x = u - 35;
        let local = if p <= x {
            p
        } else {
            let m = ((u - 12) * 32 / 255) - 23;
            let k = m + ((p - m) % (u - 4));
            if k <= x {
                k
            } else {
                m
            }
        };
        pages.push(vec![0u8; PAGE]);
        let leaf_no = pages.len();
        let mut cell = varint(p as u64);
        cell.extend(varint(rowid));
        cell.extend_from_slice(&payload[..local]);
        if local < p {
            let mut rest = &payload[local..];
            let first = pages.len() + 1;
            cell.extend_from_slice(&(first as u32).to_be_bytes());
            while !rest.is_empty() {
                let take = rest.len().min(u - 4);
                let mut ov = vec![0u8; PAGE];
                let next = if take < rest.len() {
                    pages.len() as u32 + 2
                } else {
                    0
                };
                ov[..4].copy_from_slice(&next.to_be_bytes());
                ov[4..4 + take].copy_from_slice(&rest[..take]);
                pages.push(ov);
                rest = &rest[take..];
            }
        }
        let leaf = &mut pages[leaf_no - 1];
        leaf[0] = 0x0D;
        leaf[3..5].copy_from_slice(&1u16.to_be_bytes());
        let content = PAGE - cell.len();
        leaf[5..7].copy_from_slice(&(content as u16).to_be_bytes());
        leaf[8..10].copy_from_slice(&(content as u16).to_be_bytes());
        leaf[content..].copy_from_slice(&cell);
        leaf_no as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_rows_blobs_and_overflow_chains_read_back() {
        let big: Vec<u8> = (0..3000u32).map(|i| (i * 7) as u8).collect();
        let db = writer::build(&[
            (
                "Things",
                "CREATE TABLE Things (_PW_ID INTEGER PRIMARY KEY, Name TEXT, Data BLOB, UNIQUE(Name))",
                vec![
                    vec![Value::Null, Value::Text("a".into()), Value::Blob(big.clone())],
                    vec![Value::Null, Value::Text("b".into()), Value::Int(-5)],
                    vec![Value::Null, Value::Text("c".into()), Value::Float(1.5)],
                ],
            ),
            (
                "Other",
                "CREATE TABLE Other (x)",
                vec![vec![Value::Int(42)]],
            ),
        ]);
        let db = Db::open(&db, "test").unwrap();
        let tables = db.tables().unwrap();
        assert_eq!(tables.len(), 2);
        let things = db.table("things").unwrap().unwrap();
        assert_eq!(things.columns, ["_PW_ID", "Name", "Data"]);
        let rows = db.rows(things.root, 1 << 20).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][2], Value::Blob(big));
        assert_eq!(rows[1][2], Value::Int(-5));
        assert_eq!(rows[2][2], Value::Float(1.5));
        // A record past the cap is refused before it is assembled.
        assert!(matches!(
            db.rows(things.root, 100),
            Err(CodecError::LimitExceeded(_))
        ));
    }

    #[test]
    fn a_damaged_database_errors_and_never_panics() {
        let big: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let file = writer::build(&[(
            "T",
            "CREATE TABLE T (a, b)",
            vec![
                vec![Value::Blob(big), Value::Int(1)],
                vec![Value::Text("x".into()), Value::Null],
            ],
        )]);
        for cut in 0..file.len() {
            if let Ok(db) = Db::open(&file[..cut], "t") {
                if let Ok(tables) = db.tables() {
                    for t in tables {
                        let _ = db.rows(t.root, 1 << 20);
                    }
                }
            }
        }
        for i in 0..file.len() {
            for flip in [0xFFu8, 0x01, 0x80] {
                let mut bad = file.clone();
                bad[i] ^= flip;
                if let Ok(db) = Db::open(&bad, "t") {
                    if let Ok(tables) = db.tables() {
                        for t in tables {
                            let _ = db.rows(t.root, 1 << 20);
                        }
                    }
                }
            }
        }
    }

    /// Checked by hand against a database the real SQLite wrote: set
    /// `W16_SQLITE_FIXTURE` to a file holding a table `T(a, b BLOB)`.
    #[test]
    fn reads_a_real_sqlite_file_when_one_is_given() {
        let Ok(path) = std::env::var("W16_SQLITE_FIXTURE") else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let db = Db::open(&bytes, "real").unwrap();
        let t = db.table("T").unwrap().unwrap();
        let rows = db.rows(t.root, 1 << 26).unwrap();
        assert_eq!(rows.len(), 500);
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r[0], Value::Int(i as i64));
            let blob = r[1].as_blob().unwrap();
            assert_eq!(blob.len(), 1000 + i * 37);
            assert!(blob
                .iter()
                .enumerate()
                .all(|(j, b)| *b == ((i + j) % 251) as u8));
        }
    }
}
