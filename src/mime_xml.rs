//! The XML that update-mime-database reads and writes, as libxml2 (2.15) does it:
//!
//! - `parse`: what xmlParseFile builds for the package files (namespaces, character and
//!   predefined entity references, comments, blanks kept, the internal DTD subset's default
//!   `xmlns` and its non-CDATA attribute normalization, its attribute defaults for
//!   xmlGetNsProp). Everything else (other entities, CDATA sections, processing instructions in
//!   the document element, external DTDs, other encodings, anything libxml2 would report as an
//!   error) is `Unsupported`, and the caller runs update-mime-database itself.
//! - `copy_info`: what xmlDocCopyNode into a type's document changes, with update-mime-database's
//!   removal of the copy's namespace declarations: the copy itself is serialized from the parsed
//!   tree.
//! - `dump_copy`: xmlSaveFormatFileEnc(…, "utf-8", 1) of such a copy.
//!
//! A `Tree` is flat: nodes, attributes and namespace bindings in arrays, linked by index, with
//! names, values and text as spans of the input, or of one buffer for what references and
//! normalization change. Its arrays are allocated once, from counts of the input.

#[derive(Debug)]
pub struct Unsupported(pub String);

macro_rules! unsupported {
    ($($t:tt)*) => { return Err(Unsupported(format!($($t)*))) };
}

type R<T> = Result<T, Unsupported>;

pub const XML_NS: &[u8] = b"http://www.w3.org/XML/1998/namespace";
pub const NONE: u32 = u32::MAX;
/// `AttrRec::ns` of xml:*
pub const NS_XML: u32 = u32::MAX - 1;
/// Spans with this bit are in `Tree::extra`
const EXTRA: u32 = 1 << 31;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Span {
    pub start: u32,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Element,
    Text,
    Comment,
}

#[derive(Clone, Copy, Debug)]
pub struct NodeRec {
    pub kind: Kind,
    pub first_child: u32,
    pub next: u32,
    /// An element's qualified name, the content of text and comments
    pub span: Span,
    /// The prefix's length in the qualified name, 0 without
    pub prefix_len: u32,
    /// Index in `nss`, or NONE
    pub ns: u32,
    /// Ranges (start, len) in `nss` and `attrs`
    pub ns_defs: (u32, u32),
    pub attrs: (u32, u32),
}

#[derive(Clone, Copy, Debug)]
pub struct NsRec {
    pub prefix: Option<Span>,
    pub href: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct AttrRec {
    pub prefix: Option<Span>,
    pub name: Span,
    /// Index in `nss`, NS_XML or NONE
    pub ns: u32,
    pub value: Span,
}

struct Decl {
    element: Span,
    attr: Span,
    cdata: bool,
    default: Option<Span>,
}

pub struct Tree {
    input: Vec<u8>,
    extra: Vec<u8>,
    pub nodes: Vec<NodeRec>,
    pub attrs: Vec<AttrRec>,
    pub nss: Vec<NsRec>,
    pub root: u32,
    decls: Vec<Decl>,
}

impl Tree {
    pub fn get(&self, s: Span) -> &[u8] {
        if s.start & EXTRA != 0 {
            let start = (s.start & !EXTRA) as usize;
            &self.extra[start..start + s.len as usize]
        } else {
            &self.input[s.start as usize..(s.start + s.len) as usize]
        }
    }

    pub fn qname(&self, n: u32) -> &[u8] {
        self.get(self.nodes[n as usize].span)
    }

    pub fn local(&self, n: u32) -> &[u8] {
        let node = &self.nodes[n as usize];
        let q = self.get(node.span);
        if node.prefix_len == 0 {
            q
        } else {
            &q[node.prefix_len as usize + 1..]
        }
    }

    pub fn in_ns(&self, n: u32, href: &[u8]) -> bool {
        let ns = self.nodes[n as usize].ns;
        ns != NONE && self.get(self.nss[ns as usize].href) == href
    }

    pub fn children(&self, n: u32) -> impl Iterator<Item = u32> + '_ {
        crate::mime_xml::linked(self.nodes[n as usize].first_child, move |c| {
            self.nodes[c as usize].next
        })
    }

    pub fn elements(&self, n: u32) -> impl Iterator<Item = u32> + '_ {
        self.children(n)
            .filter(|&c| self.nodes[c as usize].kind == Kind::Element)
    }

    fn attrs_of(&self, n: u32) -> &[AttrRec] {
        let (s, l) = self.nodes[n as usize].attrs;
        &self.attrs[s as usize..(s + l) as usize]
    }

    fn ns_defs_of(&self, n: u32) -> &[NsRec] {
        let (s, l) = self.nodes[n as usize].ns_defs;
        &self.nss[s as usize..(s + l) as usize]
    }

    /// xmlGetNsProp(node, name, NULL): the attribute without namespace, or the DTD's default.
    pub fn prop(&self, n: u32, name: &[u8]) -> Option<&[u8]> {
        if let Some(a) = self
            .attrs_of(n)
            .iter()
            .find(|a| a.ns == NONE && self.get(a.name) == name)
        {
            return Some(self.get(a.value));
        }
        self.dtd_default(self.qname(n), name, None)
    }

    /// The xml:lang attribute itself
    pub fn lang_attr_span(&self, n: u32) -> Option<Span> {
        self.attrs_of(n)
            .iter()
            .find(|a| a.ns == NS_XML && self.get(a.name) == b"lang")
            .map(|a| a.value)
    }

    /// xmlGetNsProp(node, "lang", XML_NS), with the DTD's default
    pub fn lang(&self, n: u32) -> Option<&[u8]> {
        match self.lang_attr_span(n) {
            Some(s) => Some(self.get(s)),
            None => self.dtd_default(self.qname(n), b"lang", Some(b"xml")),
        }
    }

    fn dtd_default(&self, element: &[u8], attr: &[u8], prefix: Option<&[u8]>) -> Option<&[u8]> {
        self.decls.iter().find_map(|d| {
            let name = self.get(d.attr);
            let matches = match prefix {
                None => name == attr,
                Some(p) => {
                    name.len() == p.len() + 1 + attr.len()
                        && name.starts_with(p)
                        && name[p.len()] == b':'
                        && name.ends_with(attr)
                }
            };
            (matches && self.get(d.element) == element).then_some(())?;
            d.default.map(|v| self.get(v))
        })
    }
}

/// The elements of a list in an array, from `first`, linked by `next`, up to NONE
pub fn linked(first: u32, next: impl Fn(u32) -> u32) -> impl Iterator<Item = u32> {
    let mut cur = first;
    std::iter::from_fn(move || {
        if cur == NONE {
            return None;
        }
        let item = cur;
        cur = next(cur);
        Some(item)
    })
}

// --- Parsing ---

struct Parser<'a> {
    t: &'a mut Tree,
    i: usize,
    /// In-scope namespace bindings (indices in `nss`), innermost last
    scope: Vec<u32>,
    /// Attributes of the elements being parsed: (qualified name, value)
    raw_attrs: Vec<(Span, Span)>,
    scratch: Vec<u8>,
}

/// Parse a file's content. Allocations: the normalized input (only with CRs), and the tree's
/// arrays, sized from counts of '<' and '='.
pub fn parse(data: Vec<u8>) -> R<Tree> {
    if std::str::from_utf8(&data).is_err() {
        unsupported!("not UTF-8");
    }
    // End-of-line handling (XML 1.0, 2.11)
    let input = if data.contains(&b'\r') {
        let mut t = Vec::with_capacity(data.len());
        let mut k = 0;
        while k < data.len() {
            if data[k] == b'\r' {
                t.push(b'\n');
                if data.get(k + 1) == Some(&b'\n') {
                    k += 1;
                }
            } else {
                t.push(data[k]);
            }
            k += 1;
        }
        t
    } else {
        data
    };
    let (lt, eq) = check_chars(&input)?;
    if input.len() >= EXTRA as usize {
        unsupported!("file too large");
    }
    let len = input.len();
    let mut tree = Tree {
        input,
        extra: Vec::with_capacity(len / 8 + 64),
        nodes: Vec::with_capacity(2 * lt + 1),
        attrs: Vec::with_capacity(eq),
        nss: Vec::with_capacity(16),
        root: NONE,
        decls: Vec::new(),
    };
    let mut p = Parser {
        t: &mut tree,
        i: 0,
        scope: Vec::with_capacity(16),
        raw_attrs: Vec::with_capacity(64),
        scratch: Vec::with_capacity(256),
    };
    if p.starts(b"\xef\xbb\xbf") {
        p.i = 3;
    }
    p.xml_decl()?;
    p.misc()?;
    if p.starts(b"<!DOCTYPE") {
        p.doctype()?;
        p.misc()?;
        // By element, for `decls_of`
        let t = &mut *p.t;
        let mut decls = std::mem::take(&mut t.decls);
        decls.sort_by(|a, b| t.get(a.element).cmp(t.get(b.element)));
        t.decls = decls;
    }
    if !p.starts(b"<") {
        unsupported!("no document element");
    }
    let root = p.element()?;
    p.misc()?;
    if p.i != p.t.input.len() {
        unsupported!("content after the document element");
    }
    tree.root = root;
    Ok(tree)
}

/// Only XML 1.0 characters: libxml2 rejects the others. (Valid UTF-8 has no surrogates, so
/// these are the C0 controls and U+FFFE, U+FFFF.) Also counts '<' and '=', for capacities.
fn check_chars(s: &[u8]) -> R<(usize, usize)> {
    let (mut lt, mut eq) = (0, 0);
    for (i, &c) in s.iter().enumerate() {
        match c {
            b'<' => lt += 1,
            b'=' => eq += 1,
            b'\t' | b'\n' | b'\r' => {}
            0..0x20 => unsupported!("character 0x{c:02x} at {i}"),
            0xef if s.get(i + 1) == Some(&0xbf) && matches!(s.get(i + 2), Some(0xbe | 0xbf)) => {
                unsupported!("character U+FFFE or U+FFFF at {i}")
            }
            _ => {}
        }
    }
    Ok((lt, eq))
}

fn is_xml_char(c: u32) -> bool {
    matches!(c, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b':' || c >= 0x80
}

fn is_name_char(c: u8) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == b'-' || c == b'.'
}

fn span(start: usize, end: usize) -> Span {
    Span {
        start: start as u32,
        len: (end - start) as u32,
    }
}

impl Parser<'_> {
    fn peek(&self) -> u8 {
        self.t.input.get(self.i).copied().unwrap_or(0)
    }

    fn starts(&self, s: &[u8]) -> bool {
        self.t.input[self.i..].starts_with(s)
    }

    fn expect(&mut self, s: &[u8]) -> R<()> {
        if !self.starts(s) {
            unsupported!("expected {:?} at {}", String::from_utf8_lossy(s), self.i);
        }
        self.i += s.len();
        Ok(())
    }

    fn skip_space(&mut self) -> bool {
        let start = self.i;
        while self.i < self.t.input.len() && is_space(self.t.input[self.i]) {
            self.i += 1;
        }
        self.i > start
    }

    fn name(&mut self) -> R<Span> {
        let start = self.i;
        if !is_name_start(self.peek()) {
            unsupported!("expected a name at {}", self.i);
        }
        while self.i < self.t.input.len() && is_name_char(self.t.input[self.i]) {
            self.i += 1;
        }
        Ok(span(start, self.i))
    }

    /// Up to (and past) `end`, returning what's before it.
    fn until(&mut self, end: &[u8]) -> R<Span> {
        let Some(pos) = self.t.input[self.i..]
            .windows(end.len())
            .position(|w| w == end)
        else {
            unsupported!("unterminated construct");
        };
        let s = span(self.i, self.i + pos);
        self.i += pos + end.len();
        Ok(s)
    }

    fn xml_decl(&mut self) -> R<()> {
        if !(self.starts(b"<?xml") && is_space(self.t.input.get(self.i + 5).copied().unwrap_or(0)))
        {
            return Ok(());
        }
        self.i += 5;
        let decl = self.until(b"?>")?;
        // version, encoding, standalone; only UTF-8
        let text = std::str::from_utf8(self.t.get(decl)).unwrap();
        let mut rest = text.trim();
        let mut first = true;
        while !rest.is_empty() {
            let Some(eq) = rest.find('=') else {
                unsupported!("XML declaration");
            };
            let key = rest[..eq].trim();
            let after = rest[eq + 1..].trim_start();
            let quote = after.chars().next().unwrap_or(' ');
            if quote != '"' && quote != '\'' {
                unsupported!("XML declaration");
            }
            let Some(close) = after[1..].find(quote) else {
                unsupported!("XML declaration");
            };
            let value = &after[1..1 + close];
            match key {
                "version" if value == "1.0" && first => {}
                "encoding" if !first && value.eq_ignore_ascii_case("utf-8") => {}
                "standalone" if !first && (value == "yes" || value == "no") => {}
                _ => unsupported!("XML declaration: {key}={value}"),
            }
            first = false;
            rest = after[close + 2..].trim_start();
        }
        if first {
            unsupported!("XML declaration without version");
        }
        Ok(())
    }

    /// Comments, processing instructions and blanks outside the document element.
    fn misc(&mut self) -> R<()> {
        loop {
            self.skip_space();
            if self.starts(b"<!--") {
                self.comment()?;
            } else if self.starts(b"<?") {
                self.i += 2;
                let target = self.name()?;
                if self.t.get(target).eq_ignore_ascii_case(b"xml") {
                    unsupported!("misplaced XML declaration");
                }
                self.until(b"?>")?;
            } else {
                return Ok(());
            }
        }
    }

    fn comment(&mut self) -> R<Span> {
        self.expect(b"<!--")?;
        let s = self.until(b"-->")?;
        let content = self.t.get(s);
        if content.windows(2).any(|w| w == b"--") || content.ends_with(b"-") {
            unsupported!("'--' in a comment");
        }
        Ok(s)
    }

    fn doctype(&mut self) -> R<()> {
        self.expect(b"<!DOCTYPE")?;
        if !self.skip_space() {
            unsupported!("DOCTYPE");
        }
        self.name()?;
        self.skip_space();
        if self.peek() == b'[' {
            self.i += 1;
            self.int_subset()?;
            self.skip_space();
        }
        if self.peek() != b'>' {
            unsupported!("external DTD");
        }
        self.i += 1;
        Ok(())
    }

    fn int_subset(&mut self) -> R<()> {
        loop {
            self.skip_space();
            if self.peek() == b']' {
                self.i += 1;
                return Ok(());
            } else if self.starts(b"<!--") {
                self.comment()?;
            } else if self.starts(b"<?") {
                self.until(b"?>")?;
            } else if self.starts(b"<!ELEMENT") {
                // No quotes in element declarations
                let decl = self.until(b">")?;
                if self.t.get(decl).contains(&b'%') {
                    unsupported!("parameter entities");
                }
            } else if self.starts(b"<!ATTLIST") {
                self.attlist()?;
            } else {
                unsupported!("DTD declaration at {}", self.i);
            }
        }
    }

    fn attlist(&mut self) -> R<()> {
        self.expect(b"<!ATTLIST")?;
        self.skip_space();
        let element = self.name()?;
        loop {
            let space = self.skip_space();
            if self.peek() == b'>' {
                self.i += 1;
                return Ok(());
            }
            if !space {
                unsupported!("ATTLIST");
            }
            let attr = self.name()?;
            self.skip_space();
            let cdata = if self.starts(b"CDATA") {
                self.i += 5;
                true
            } else if self.peek() == b'(' {
                self.until(b")")?;
                false
            } else {
                let t = self.name()?;
                let t = self.t.get(t);
                if t == b"NOTATION" {
                    self.skip_space();
                    self.expect(b"(")?;
                    self.until(b")")?;
                } else if !matches!(
                    t,
                    b"ID"
                        | b"IDREF"
                        | b"IDREFS"
                        | b"ENTITY"
                        | b"ENTITIES"
                        | b"NMTOKEN"
                        | b"NMTOKENS"
                ) {
                    unsupported!("attribute type");
                }
                false
            };
            self.skip_space();
            let default = if self.starts(b"#REQUIRED") {
                self.i += 9;
                None
            } else if self.starts(b"#IMPLIED") {
                self.i += 8;
                None
            } else {
                if self.starts(b"#FIXED") {
                    self.i += 6;
                    self.skip_space();
                }
                let (v, had_ref) = self.att_value()?;
                Some(if cdata {
                    v
                } else if had_ref {
                    unsupported!("reference in a non-CDATA default");
                } else {
                    self.normalize_space(v)
                })
            };
            let name = self.t.get(attr);
            if name.starts_with(b"xmlns:") {
                unsupported!("defaulted namespace prefix");
            }
            let (e, a) = (self.t.get(element), name);
            if self
                .t
                .decls
                .iter()
                .any(|d| self.t.get(d.element) == e && self.t.get(d.attr) == a)
            {
                unsupported!("attribute declared twice");
            }
            self.t.decls.push(Decl {
                element,
                attr,
                cdata,
                default,
            });
        }
    }

    /// A quoted attribute value with references replaced and whitespace normalized (CDATA), and
    /// whether it contained a reference. A span of the input where nothing changed.
    fn att_value(&mut self) -> R<(Span, bool)> {
        let quote = self.peek();
        if quote != b'"' && quote != b'\'' {
            unsupported!("attribute value");
        }
        self.i += 1;
        let start = self.i;
        let Some(len) = self.t.input[start..].iter().position(|&c| c == quote) else {
            unsupported!("attribute value");
        };
        let raw = &self.t.input[start..start + len];
        if raw.contains(&b'<') {
            unsupported!("'<' in an attribute value");
        }
        if !raw
            .iter()
            .any(|&c| matches!(c, b'&' | b'\t' | b'\n' | b'\r'))
        {
            self.i = start + len + 1;
            return Ok((span(start, start + len), false));
        }
        let out_start = self.t.extra.len();
        let mut had_ref = false;
        while self.i < start + len {
            let c = self.t.input[self.i];
            self.i += 1;
            if c == b'&' {
                had_ref = true;
                self.reference()?;
                if self.i > start + len {
                    unsupported!("attribute value");
                }
            } else if matches!(c, b'\t' | b'\n' | b'\r') {
                self.t.extra.push(b' ');
            } else {
                self.t.extra.push(c);
            }
        }
        self.i = start + len + 1;
        Ok((self.extra_span(out_start), had_ref))
    }

    fn extra_span(&self, start: usize) -> Span {
        Span {
            start: start as u32 | EXTRA,
            len: (self.t.extra.len() - start) as u32,
        }
    }

    /// After a '&': a character or predefined entity reference, appended to `extra`.
    fn reference(&mut self) -> R<()> {
        let body = self.until(b";")?;
        let b = self.t.get(body);
        let c = match b {
            b"lt" => '<' as u32,
            b"gt" => '>' as u32,
            b"amp" => '&' as u32,
            b"apos" => '\'' as u32,
            b"quot" => '"' as u32,
            [b'#', b'x', hex @ ..]
                if !hex.is_empty() && hex.len() <= 8 && hex.iter().all(u8::is_ascii_hexdigit) =>
            {
                u32::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap()
            }
            [b'#', dec @ ..]
                if !dec.is_empty() && dec.len() <= 9 && dec.iter().all(u8::is_ascii_digit) =>
            {
                std::str::from_utf8(dec).unwrap().parse().unwrap()
            }
            _ => unsupported!("entity reference &{};", String::from_utf8_lossy(b)),
        };
        if !is_xml_char(c) {
            unsupported!("character reference to U+{c:04X}");
        }
        let mut buf = [0; 4];
        let s = char::from_u32(c).unwrap().encode_utf8(&mut buf);
        self.t.extra.extend_from_slice(s.as_bytes());
        Ok(())
    }

    /// Normalization of non-CDATA attribute values: no leading or trailing spaces, runs of
    /// spaces collapsed.
    fn normalize_space(&mut self, v: Span) -> Span {
        self.scratch.clear();
        let mut scratch = std::mem::take(&mut self.scratch);
        for part in self
            .t
            .get(v)
            .split(|&c| c == b' ')
            .filter(|p| !p.is_empty())
        {
            if !scratch.is_empty() {
                scratch.push(b' ');
            }
            scratch.extend_from_slice(part);
        }
        let start = self.t.extra.len();
        self.t.extra.extend_from_slice(&scratch);
        self.scratch = scratch;
        self.extra_span(start)
    }

    /// (prefix length, valid)
    fn prefix_len(&self, name: Span) -> R<u32> {
        let n = self.t.get(name);
        match n.iter().position(|&c| c == b':') {
            None => Ok(0),
            Some(p) => {
                if p == 0 || p + 1 == n.len() || n[p + 1..].contains(&b':') {
                    unsupported!("name {:?}", String::from_utf8_lossy(n));
                }
                Ok(p as u32)
            }
        }
    }

    fn resolve(&self, prefix: Option<&[u8]>) -> u32 {
        for &ns in self.scope.iter().rev() {
            let p = self.t.nss[ns as usize].prefix.map(|s| self.t.get(s));
            if p == prefix {
                return ns;
            }
        }
        NONE
    }

    /// The declarations of an element's attributes: a range of the sorted `decls`
    fn decls_of(&self, element: &[u8]) -> (usize, usize) {
        let d = &self.t.decls;
        let start = d.partition_point(|x| self.t.get(x.element) < element);
        let end = start + d[start..].partition_point(|x| self.t.get(x.element) == element);
        (start, end)
    }

    fn decl(&self, (start, end): (usize, usize), attr: &[u8]) -> Option<&Decl> {
        self.t.decls[start..end]
            .iter()
            .find(|d| self.t.get(d.attr) == attr)
    }

    fn push_node(&mut self, kind: Kind, s: Span) -> u32 {
        self.t.nodes.push(NodeRec {
            kind,
            first_child: NONE,
            next: NONE,
            span: s,
            prefix_len: 0,
            ns: NONE,
            ns_defs: (0, 0),
            attrs: (0, 0),
        });
        (self.t.nodes.len() - 1) as u32
    }

    fn element(&mut self) -> R<u32> {
        self.expect(b"<")?;
        let qname = self.name()?;
        let decls = self.decls_of(self.t.get(qname));
        let attrs_mark = self.raw_attrs.len();
        loop {
            let space = self.skip_space();
            match self.peek() {
                b'/' | b'>' => break,
                _ if !space => unsupported!("attributes"),
                _ => {}
            }
            let name = self.name()?;
            self.skip_space();
            self.expect(b"=")?;
            self.skip_space();
            let (mut value, had_ref) = self.att_value()?;
            if let Some(d) = self.decl(decls, self.t.get(name))
                && !d.cdata
            {
                if had_ref {
                    unsupported!("reference in a non-CDATA attribute");
                }
                value = self.normalize_space(value);
            }
            let n = self.t.get(name);
            if self.raw_attrs[attrs_mark..]
                .iter()
                .any(|(a, _)| self.t.get(*a) == n)
            {
                unsupported!("duplicate attribute");
            }
            self.raw_attrs.push((name, value));
        }

        // Namespace declarations
        let scope_mark = self.scope.len();
        let defs_start = self.t.nss.len();
        let mut has_default = false;
        for k in attrs_mark..self.raw_attrs.len() {
            let (name, value) = self.raw_attrs[k];
            let n = self.t.get(name);
            if n == b"xmlns" {
                if value.len == 0 {
                    unsupported!("xmlns=\"\"");
                }
                has_default = true;
                self.t.nss.push(NsRec {
                    prefix: None,
                    href: value,
                });
            } else if n.starts_with(b"xmlns:") {
                let p = &n[6..];
                if value.len == 0 || p == b"xml" || p == b"xmlns" || p.contains(&b':') {
                    unsupported!("namespace declaration");
                }
                let prefix = Span {
                    start: name.start + 6,
                    len: name.len - 6,
                };
                self.t.nss.push(NsRec {
                    prefix: Some(prefix),
                    href: value,
                });
            }
        }
        // A default xmlns from the DTD, unless it's already in effect
        if !has_default
            && let Some(d) = self.decl(decls, b"xmlns")
            && let Some(default) = d.default
        {
            let current = self.resolve(None);
            if current == NONE
                || self.t.get(self.t.nss[current as usize].href) != self.t.get(default)
            {
                self.t.nss.push(NsRec {
                    prefix: None,
                    href: default,
                });
            }
        }
        let defs_end = self.t.nss.len();
        self.scope.extend((defs_start..defs_end).map(|n| n as u32));

        let prefix_len = self.prefix_len(qname)?;
        let ns = if prefix_len == 0 {
            self.resolve(None)
        } else {
            let p = &self.t.get(qname)[..prefix_len as usize];
            if p == b"xml" {
                unsupported!("element in the xml namespace");
            }
            match self.resolve(Some(p)) {
                NONE => unsupported!("unbound prefix"),
                ns => ns,
            }
        };
        let attrs_start = self.t.attrs.len();
        for k in attrs_mark..self.raw_attrs.len() {
            let (name, value) = self.raw_attrs[k];
            let n = self.t.get(name);
            if n == b"xmlns" || n.starts_with(b"xmlns:") {
                continue;
            }
            let plen = self.prefix_len(name)?;
            let (prefix, local, ans) = if plen == 0 {
                (None, name, NONE)
            } else {
                let p = &n[..plen as usize];
                let ans = if p == b"xml" {
                    NS_XML
                } else {
                    match self.resolve(Some(p)) {
                        NONE => unsupported!("unbound prefix"),
                        ns => ns,
                    }
                };
                (
                    Some(Span {
                        start: name.start,
                        len: plen,
                    }),
                    Span {
                        start: name.start + plen + 1,
                        len: name.len - plen - 1,
                    },
                    ans,
                )
            };
            if ans != NONE {
                let href = |ns: u32| {
                    if ns == NS_XML {
                        XML_NS
                    } else {
                        self.t.get(self.t.nss[ns as usize].href)
                    }
                };
                let l = self.t.get(local);
                if self.t.attrs[attrs_start..]
                    .iter()
                    .any(|a| a.ns != NONE && href(a.ns) == href(ans) && self.t.get(a.name) == l)
                {
                    unsupported!("duplicate attribute");
                }
            }
            self.t.attrs.push(AttrRec {
                prefix,
                name: local,
                ns: ans,
                value,
            });
        }
        self.raw_attrs.truncate(attrs_mark);

        let node = self.push_node(Kind::Element, qname);
        {
            let n = &mut self.t.nodes[node as usize];
            n.prefix_len = prefix_len;
            n.ns = ns;
            n.ns_defs = (defs_start as u32, (defs_end - defs_start) as u32);
            n.attrs = (
                attrs_start as u32,
                (self.t.attrs.len() - attrs_start) as u32,
            );
        }
        if self.starts(b"/>") {
            self.i += 2;
        } else {
            self.expect(b">")?;
            self.content(node)?;
            self.expect(b"</")?;
            let end = self.name()?;
            if self.t.get(end) != self.t.get(qname) {
                unsupported!("mismatched end tag");
            }
            self.skip_space();
            self.expect(b">")?;
        }
        self.scope.truncate(scope_mark);
        Ok(node)
    }

    fn content(&mut self, parent: u32) -> R<()> {
        let mut last = NONE;
        let mut link = |p: &mut Self, child: u32| {
            if last == NONE {
                p.t.nodes[parent as usize].first_child = child;
            } else {
                p.t.nodes[last as usize].next = child;
            }
            last = child;
        };
        // The text node being collected: a span of the input, or from `extra_start` in `extra`
        let mut text: Option<Span> = None;
        let mut extra_start: Option<usize> = None;
        let flush = |p: &mut Self,
                     text: &mut Option<Span>,
                     extra_start: &mut Option<usize>|
         -> Option<u32> {
            let s = match (text.take(), extra_start.take()) {
                (_, Some(start)) => p.extra_span(start),
                (Some(s), None) => s,
                (None, None) => return None,
            };
            (s.len > 0).then(|| p.push_node(Kind::Text, s))
        };
        loop {
            if self.i >= self.t.input.len() {
                unsupported!("unterminated element");
            }
            let c = self.t.input[self.i];
            if c == b'<' {
                if let Some(n) = flush(self, &mut text, &mut extra_start) {
                    link(self, n);
                }
                if self.starts(b"</") {
                    return Ok(());
                } else if self.starts(b"<!--") {
                    let s = self.comment()?;
                    let n = self.push_node(Kind::Comment, s);
                    link(self, n);
                } else if self.starts(b"<![CDATA[") {
                    unsupported!("CDATA section");
                } else if self.starts(b"<?") {
                    unsupported!("processing instruction");
                } else if self.starts(b"<!") {
                    unsupported!("markup declaration in content");
                } else {
                    let n = self.element()?;
                    link(self, n);
                }
            } else if c == b'&' {
                if extra_start.is_none() {
                    extra_start = Some(self.t.extra.len());
                    if let Some(s) = text.take() {
                        let t = &mut *self.t;
                        t.extra.extend_from_slice(
                            &t.input[s.start as usize..(s.start + s.len) as usize],
                        );
                    }
                }
                self.i += 1;
                self.reference()?;
            } else {
                let run = self.t.input[self.i..]
                    .iter()
                    .position(|&b| b == b'<' || b == b'&')
                    .unwrap_or(self.t.input.len() - self.i);
                let chunk = span(self.i, self.i + run);
                if self.t.get(chunk).windows(3).any(|w| w == b"]]>") {
                    unsupported!("']]>' in content");
                }
                if extra_start.is_some() {
                    let t = &mut *self.t;
                    t.extra.extend_from_slice(&t.input[self.i..self.i + run]);
                } else {
                    text = Some(chunk);
                }
                self.i += run;
            }
        }
    }
}

// --- Copies ---

/// A namespace declaration a copy gets on its root: (prefix, href), spans of the tree
pub type AddedNs = (Option<Span>, Span);

/// A frame of the new tree during xmlDocCopyNode: its definitions (in `nss`), the
/// definitions added (for the root, in `added`), and its namespace (prefix, href).
pub struct Frame {
    defs: (u32, u32),
    ns: Option<(Option<Span>, Span)>,
}

/// What xmlDocCopyNode(node, doc, 1) and update-mime-database's hack (a copy in the default
/// `free_ns` namespace loses its declarations) change: whether the copy's root keeps no
/// declarations, and the ones xmlDocCopyNode adds to it (pushed to `added`) for namespaces
/// declared outside of the copy. `frames`: scratch space.
pub fn copy_info(
    t: &Tree,
    node: u32,
    free_ns: &[u8],
    frames: &mut Vec<Frame>,
    added: &mut Vec<AddedNs>,
) -> R<bool> {
    frames.clear();
    let added_start = added.len();
    copy_element(t, node, frames, added, added_start)?;
    let ns = t.nodes[node as usize].ns;
    let clear = ns != NONE
        && t.nss[ns as usize].prefix.is_none()
        && t.get(t.nss[ns as usize].href) == free_ns;
    Ok(clear)
}

fn copy_element(
    t: &Tree,
    node: u32,
    frames: &mut Vec<Frame>,
    added: &mut Vec<AddedNs>,
    added_start: usize,
) -> R<()> {
    let n = &t.nodes[node as usize];
    frames.push(Frame {
        defs: n.ns_defs,
        ns: None,
    });
    if n.ns != NONE {
        let src = t.nss[n.ns as usize];
        let prefix = src.prefix.map(|s| t.get(s));
        let found = search(t, frames, &added[added_start..], prefix);
        let ns = match found {
            Some(ns) => ns,
            None => {
                // Defined outside of the copy: declared on the copy's root
                added.push((src.prefix, src.href));
                (src.prefix, src.href)
            }
        };
        frames.last_mut().unwrap().ns = Some(ns);
    }
    // xmlCopyProp: an attribute's namespace by its prefix in the new tree (the element's own
    // namespace doesn't count), or declared on the copy's root
    for a in t.attrs_of(node) {
        if a.ns == NONE || a.ns == NS_XML {
            continue;
        }
        let src = t.nss[a.ns as usize];
        let prefix = src.prefix.map(|s| t.get(s));
        let last = frames.len() - 1;
        let own = frames[last].ns.take();
        let found = search(t, frames, &added[added_start..], prefix);
        frames[last].ns = own;
        match found {
            Some((_, href)) if t.get(href) == t.get(src.href) => {}
            Some(_) => unsupported!("attribute prefix bound to another namespace in the copy"),
            None => added.push((src.prefix, src.href)),
        }
    }
    for c in t.children(node) {
        if t.nodes[c as usize].kind == Kind::Element {
            copy_element(t, c, frames, added, added_start)?;
        }
    }
    frames.pop();
    Ok(())
}

/// xmlSearchNs in the new tree: the innermost definition of `prefix` (the root's include the
/// added ones), also matching the namespaces of ancestors.
fn search(
    t: &Tree,
    frames: &[Frame],
    added: &[AddedNs],
    prefix: Option<&[u8]>,
) -> Option<(Option<Span>, Span)> {
    let last = frames.len() - 1;
    for (depth, f) in frames.iter().enumerate().rev() {
        let (s, l) = f.defs;
        if let Some(d) = t.nss[s as usize..(s + l) as usize]
            .iter()
            .find(|d| d.prefix.map(|p| t.get(p)) == prefix)
        {
            return Some((d.prefix, d.href));
        }
        if depth == 0
            && let Some(d) = added.iter().find(|d| d.0.map(|p| t.get(p)) == prefix)
        {
            return Some(*d);
        }
        if depth != last
            && let Some((p, h)) = f.ns
            && p.map(|p| t.get(p)) == prefix
        {
            return Some((p, h));
        }
    }
    None
}

// --- Serialization ---

const INDENT_NR: usize = 30; // MAX_INDENT (60) / strlen("  ")

/// xmlEscapeTab (text) or xmlEscapeTabAttr, for the UTF-8 output encoding (non-ASCII as is).
pub fn escape(out: &mut Vec<u8>, s: &[u8], attr: bool) {
    let mut start = 0;
    for (i, &c) in s.iter().enumerate() {
        let repl: &[u8] = match c {
            b'&' => b"&amp;",
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            b'\r' => b"&#13;",
            b'"' if attr => b"&quot;",
            b'\t' if attr => b"&#9;",
            b'\n' if attr => b"&#10;",
            b'\t' | b'\n' => continue,
            0x00..=0x1f => b"&#xFFFD;",
            _ => continue,
        };
        out.extend_from_slice(&s[start..i]);
        out.extend_from_slice(repl);
        start = i + 1;
    }
    out.extend_from_slice(&s[start..]);
}

pub fn indent(out: &mut Vec<u8>, level: usize) {
    for _ in 0..level.min(INDENT_NR) {
        out.extend_from_slice(b"  ");
    }
}

fn ns_def(out: &mut Vec<u8>, prefix: Option<&[u8]>, href: &[u8]) {
    out.extend_from_slice(b" xmlns");
    if let Some(p) = prefix {
        out.push(b':');
        out.extend_from_slice(p);
    }
    out.extend_from_slice(b"=\"");
    escape(out, href, true);
    out.push(b'"');
}

/// xmlNodeDumpOutputInternal with format = 1 of a copy's root (`added`: None when the hack
/// removed its declarations), at `level` below the document's root element.
pub fn dump_copy(out: &mut Vec<u8>, t: &Tree, node: u32, added: Option<&[AddedNs]>, level: usize) {
    dump(out, t, node, Some(added), level, true);
}

/// `root_defs`: for a copy's root, what replaces its own declarations
fn dump(
    out: &mut Vec<u8>,
    t: &Tree,
    node: u32,
    root_defs: Option<Option<&[AddedNs]>>,
    level: usize,
    format: bool,
) {
    let n = &t.nodes[node as usize];
    match n.kind {
        Kind::Text => escape(out, t.get(n.span), false),
        Kind::Comment => {
            if format {
                indent(out, level);
            }
            out.extend_from_slice(b"<!--");
            out.extend_from_slice(t.get(n.span));
            out.extend_from_slice(b"-->");
        }
        Kind::Element => {
            if format {
                indent(out, level);
            }
            let qname = t.get(n.span);
            out.push(b'<');
            out.extend_from_slice(qname);
            match root_defs {
                Some(None) => {}
                _ => {
                    for d in t.ns_defs_of(node) {
                        ns_def(out, d.prefix.map(|p| t.get(p)), t.get(d.href));
                    }
                    if let Some(Some(added)) = root_defs {
                        for (p, h) in added {
                            ns_def(out, p.map(|p| t.get(p)), t.get(*h));
                        }
                    }
                }
            }
            for a in t.attrs_of(node) {
                out.push(b' ');
                if let Some(p) = a.prefix {
                    out.extend_from_slice(t.get(p));
                    out.push(b':');
                }
                out.extend_from_slice(t.get(a.name));
                out.extend_from_slice(b"=\"");
                escape(out, t.get(a.value), true);
                out.push(b'"');
            }
            if n.first_child == NONE {
                out.extend_from_slice(b"/>");
                return;
            }
            let child_format = format
                && !t
                    .children(node)
                    .any(|c| t.nodes[c as usize].kind == Kind::Text);
            out.push(b'>');
            if child_format {
                out.push(b'\n');
            }
            for c in t.children(node) {
                dump(out, t, c, None, level + 1, child_format);
                if child_format {
                    out.push(b'\n');
                }
            }
            if child_format {
                indent(out, level);
            }
            out.extend_from_slice(b"</");
            out.extend_from_slice(qname);
            out.push(b'>');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREE: &[u8] = b"http://www.freedesktop.org/standards/shared-mime-info";

    #[test]
    fn parse_and_copy() {
        let t = parse(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE mime-info [
<!ATTLIST mime-info xmlns CDATA #FIXED "http://www.freedesktop.org/standards/shared-mime-info">
<!ATTLIST glob weight CDATA "50">
<!ATTLIST x t (a | b) #IMPLIED>
]>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="text/plain">
    <comment xml:lang="de">Text &amp; &#x41;</comment>
    <glob pattern="*.txt"/>
    <x t="  a  " xmlns:k="urn:k"><k:y/></x>
  </mime-type>
</mime-info>"#
                .to_vec(),
        )
        .unwrap();
        let ty = t.elements(t.root).next().unwrap();
        let mut it = t.elements(ty);
        let comment = it.next().unwrap();
        assert_eq!(t.lang(comment).unwrap(), b"de");
        let glob = it.next().unwrap();
        assert_eq!(t.prop(glob, b"weight").unwrap(), b"50");
        let x = it.next().unwrap();
        assert_eq!(t.prop(x, b"t").unwrap(), b"a");
        let (mut frames, mut added) = (Vec::new(), Vec::new());
        assert!(copy_info(&t, comment, FREE, &mut frames, &mut added).unwrap());
        assert!(copy_info(&t, x, FREE, &mut frames, &mut added).unwrap());
        assert_eq!(added.len(), 2); // the default namespace, for each
        let mut out = Vec::new();
        dump_copy(&mut out, &t, comment, None, 1);
        out.push(b'\n');
        dump_copy(&mut out, &t, x, None, 1);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "  <comment xml:lang=\"de\">Text &amp; A</comment>\n  <x t=\"a\">\n    <k:y/>\n  </x>"
        );
    }

    #[test]
    fn rejects() {
        assert!(parse(b"<a>&foo;</a>".to_vec()).is_err());
        assert!(parse(b"<a><![CDATA[x]]></a>".to_vec()).is_err());
        assert!(parse(b"<a><b></a>".to_vec()).is_err());
        assert!(parse(b"<k:a/>".to_vec()).is_err());
    }
}
