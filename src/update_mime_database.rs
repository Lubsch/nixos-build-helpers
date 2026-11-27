//! Replacement for shared-mime-info's update-mime-database (2.4 to 2.5.1, which only differ in
//! their version), as system.path's postBuild runs it:
//!
//!     nixos-build-helpers update-mime-database UPDATE-MIME-DATABASE VERSION MIME-DIR
//!
//! writes the same files to MIME-DIR, byte for byte: a port of update-mime-database.cpp, with
//! glib's hash table (src/ghash.rs: its iteration order is in the output) and what it uses of
//! libxml2 (src/mime_xml.rs). Not its messages (the postBuild sends them to /dev/null). Anything
//! the port doesn't reproduce, it finds before writing anything, and runs
//! `UPDATE-MIME-DATABASE -V MIME-DIR` instead: input outside of what src/mime_xml.rs parses,
//! numbers out of range (update-mime-database checks errno without resetting it), fsync enabled,
//! a media directory that isn't one.
//!
//! Data is flat: records in arrays linked by u32 indices, strings as spans of a few buffers
//! (the parsed files, one arena, the output), with capacities from counts of the input.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::ghash::GHash;
use crate::mime_xml::{self, AddedNs, Frame, Kind, NONE, Span, Tree, Unsupported};

const FREE_NS: &[u8] = b"http://www.freedesktop.org/standards/shared-mime-info";
const NOGLOBS: &[u8] = b"__NOGLOBS__";
const NOMAGIC: &[u8] = b"__NOMAGIC__";
const CREATED: &[u8] = b"Created automatically by update-mime-database. DO NOT EDIT!";

const MEDIA_TYPES: [&str; 17] = [
    "all",
    "uri",
    "print",
    "text",
    "application",
    "image",
    "audio",
    "inode",
    "video",
    "message",
    "model",
    "multipart",
    "x-content",
    "x-epoc",
    "x-scheme-handler",
    "font",
    "chemical",
];

type R<T> = std::result::Result<T, Unsupported>;

macro_rules! unsupported {
    ($($t:tt)*) => { return Err(Unsupported(format!($($t)*))) };
}

pub fn run(args: Vec<OsString>) -> Result<()> {
    let [real, version, mime_dir] = &args[..] else {
        bail!("usage: update-mime-database UPDATE-MIME-DATABASE VERSION MIME-DIR");
    };
    let mut dir = mime_dir.as_bytes();
    while dir.len() > 1 && dir.ends_with(b"/") {
        dir = &dir[..dir.len() - 1];
    }
    let plan = match std::env::var_os("PKGSYSTEM_ENABLE_FSYNC") {
        Some(v) if atoi(v.as_bytes()) == 0 => plan(dir, version.as_bytes()),
        _ => Err(Unsupported("fsync enabled".into())),
    };
    match plan {
        Ok(out) => out.apply(),
        Err(Unsupported(why)) => {
            if std::env::var_os("NIXOS_BUILD_HELPERS_DEBUG").is_some() {
                eprintln!("update-mime-database: running {real:?}: {why}");
            }
            let status = Command::new(real)
                .arg("-V")
                .arg(OsStr::from_bytes(dir))
                .status()
                .with_context(|| format!("running {real:?}"))?;
            if !status.success() {
                use std::os::unix::process::ExitStatusExt as _;
                std::process::exit(status.code().unwrap_or(128 + status.signal().unwrap_or(0)));
            }
            Ok(())
        }
    }
}

// --- C's number parsing ---

/// C's atoi
fn atoi(s: &[u8]) -> i64 {
    strtol(s, 10).0 as i32 as i64
}

/// C's strtol: (value, end, overflowed), with LONG_MIN/LONG_MAX on overflow.
fn strtol(s: &[u8], base: u32) -> (i64, usize, bool) {
    let (v, end, overflow, neg) = strtoul_impl(s, base);
    if end == 0 {
        return (0, 0, false);
    }
    if neg {
        if overflow || v > (i64::MAX as u64) + 1 {
            (i64::MIN, end, true)
        } else {
            ((v as i64).wrapping_neg(), end, false)
        }
    } else if overflow || v > i64::MAX as u64 {
        (i64::MAX, end, true)
    } else {
        (v as i64, end, false)
    }
}

/// C's strtoul: (value, end, overflowed)
fn strtoul(s: &[u8], base: u32) -> (u64, usize, bool) {
    let (v, end, overflow, neg) = strtoul_impl(s, base);
    if overflow {
        (u64::MAX, end, true)
    } else if neg {
        (v.wrapping_neg(), end, false)
    } else {
        (v, end, false)
    }
}

/// Leading space, a sign, a base prefix and digits: (value, end, overflowed, negative). `end`
/// is 0 without digits.
fn strtoul_impl(s: &[u8], base: u32) -> (u64, usize, bool, bool) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let mut base = base;
    if (base == 0 || base == 16)
        && s.get(i) == Some(&b'0')
        && matches!(s.get(i + 1), Some(b'x' | b'X'))
        && s.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
    {
        i += 2;
        base = 16;
    } else if base == 0 {
        base = if s.get(i) == Some(&b'0') { 8 } else { 10 };
    }
    let start = i;
    let mut v: u64 = 0;
    let mut overflow = false;
    while i < s.len() {
        let Some(d) = (s[i] as char).to_digit(base) else {
            break;
        };
        match v
            .checked_mul(base as u64)
            .and_then(|x| x.checked_add(d as u64))
        {
            Some(x) => v = x,
            None => overflow = true,
        }
        i += 1;
    }
    if i == start {
        return (0, 0, false, false);
    }
    (v, i, overflow, neg)
}

// --- The database ---

/// (start, len) in `Db::arena` (or `Db::mdata`)
type Sp = (u32, u32);

/// A list in an array, linked by `next` indices
#[derive(Clone, Copy, Default)]
struct List {
    head: u32,
    tail: u32,
}

struct TypeRec {
    /// "media/subtype", in the arena
    name: Sp,
    slash: u32,
}

struct Glob {
    weight: i32,
    pattern: Sp,
    ty: u32,
    noglob: bool,
    case_sensitive: bool,
    next: u32,
}

struct MatchRec {
    range_start: i64,
    range_length: i32,
    word_size: i32,
    /// In `Db::mdata`
    data: Sp,
    mask: Option<Sp>,
    first_child: u32,
    next: u32,
}

struct Magic {
    priority: i32,
    ty: u32,
    first: u32,
    nomagic: bool,
}

struct TreeMatchRec {
    path: Sp,
    match_case: bool,
    executable: bool,
    non_empty: bool,
    ty: i32,
    mimetype: Option<Sp>,
    first_child: u32,
    next: u32,
}

struct TreeMagic {
    priority: i32,
    ty: u32,
    first: u32,
}

/// A node copied to a type's document: `node` of `trees[doc]`
struct Item {
    ty: u32,
    doc: u32,
    node: u32,
    /// The copy's root loses its namespace declarations
    clear: bool,
    /// Declarations xmlDocCopyNode added to the copy's root: (start, len) in `Db::added`
    added: (u32, u32),
    alive: bool,
    /// The next freedesktop.org <comment> of the type with the same xml:lang
    next_lang: u32,
}

/// The freedesktop.org <comment>s of each type by xml:lang, in output order (remove_old
/// replaces the first): open addressing, sized from the number of comments.
struct LangIndex {
    slots: Vec<LangEntry>,
    mask: usize,
}

#[derive(Clone, Copy)]
struct LangEntry {
    used: bool,
    ty: u32,
    doc: u32,
    lang: Option<Span>,
    list: List,
}

fn fnv(ty: u32, lang: Option<&[u8]>) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |b: u8| h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    ty.to_le_bytes().into_iter().for_each(&mut mix);
    match lang {
        None => mix(0),
        Some(l) => {
            mix(1);
            l.iter().for_each(|&b| mix(b));
        }
    }
    h
}

impl LangIndex {
    fn new(comments: usize) -> Self {
        let size = (comments * 2).next_power_of_two().max(16);
        let empty = LangEntry {
            used: false,
            ty: 0,
            doc: 0,
            lang: None,
            list: List {
                head: NONE,
                tail: NONE,
            },
        };
        Self {
            slots: vec![empty; size],
            mask: size - 1,
        }
    }

    /// The slot of (ty, lang), or the free one where it would go
    fn slot(&self, trees: &[Tree], ty: u32, lang: Option<&[u8]>) -> usize {
        let mut i = fnv(ty, lang) as usize & self.mask;
        loop {
            let e = &self.slots[i];
            if !e.used || (e.ty == ty && e.lang.map(|s| trees[e.doc as usize].get(s)) == lang) {
                return i;
            }
            i = (i + 1) & self.mask;
        }
    }
}

struct Db<'t> {
    trees: &'t [Tree],
    arena: Vec<u8>,
    types: Vec<TypeRec>,
    type_hash: GHash<u32>,
    globs: Vec<Glob>,
    globs_hash: GHash<List>,
    namespace_hash: GHash<u32>,
    magic: Vec<Magic>,
    matches: Vec<MatchRec>,
    mdata: Vec<u8>,
    tree_magic: Vec<TreeMagic>,
    tree_matches: Vec<TreeMatchRec>,
    /// The parents in subclass_hash's lists: (type, next)
    parents: Vec<(Sp, u32)>,
    subclass_hash: GHash<List>,
    alias_hash: GHash<u32>,
    icon_hash: GHash<Sp>,
    generic_icon_hash: GHash<Sp>,
    items: Vec<Item>,
    added: Vec<AddedNs>,
    langs: LangIndex,
    frames: Vec<Frame>,
    scratch: Vec<u8>,
}

/// A GError of update-mime-database: it ends the processing of a <mime-type>.
struct MimeError;
type M<T> = std::result::Result<T, MimeError>;

impl<'t> Db<'t> {
    fn new(trees: &'t [Tree]) -> Self {
        let (mut nodes, mut comments) = (0, 0);
        for t in trees {
            nodes += t.nodes.len();
            comments += (0..t.nodes.len() as u32)
                .filter(|&n| t.nodes[n as usize].kind == Kind::Element && t.local(n) == b"comment")
                .count();
        }
        Self {
            trees,
            arena: Vec::with_capacity(256 * 1024),
            types: Vec::with_capacity(2048),
            type_hash: GHash::new(64 * 1024),
            globs: Vec::with_capacity(4096),
            globs_hash: GHash::new(64 * 1024),
            namespace_hash: GHash::new(4096),
            magic: Vec::with_capacity(2048),
            matches: Vec::with_capacity(8192),
            mdata: Vec::with_capacity(128 * 1024),
            tree_magic: Vec::with_capacity(64),
            tree_matches: Vec::with_capacity(256),
            parents: Vec::with_capacity(1024),
            subclass_hash: GHash::new(64 * 1024),
            alias_hash: GHash::new(16 * 1024),
            icon_hash: GHash::new(4096),
            generic_icon_hash: GHash::new(64 * 1024),
            items: Vec::with_capacity(nodes),
            added: Vec::with_capacity(16),
            langs: LangIndex::new(comments),
            frames: Vec::with_capacity(16),
            scratch: Vec::with_capacity(1024),
        }
    }

    /// A parsed file, borrowed for as long as the trees (not the database)
    fn tree(&self, doc: u32) -> &'t Tree {
        let trees: &'t [Tree] = self.trees;
        &trees[doc as usize]
    }

    fn sp(&self, s: Sp) -> &[u8] {
        &self.arena[s.0 as usize..(s.0 + s.1) as usize]
    }

    fn intern(&mut self, s: &[u8]) -> Sp {
        let start = self.arena.len() as u32;
        self.arena.extend_from_slice(s);
        (start, s.len() as u32)
    }

    fn tname(&self, t: u32) -> &[u8] {
        self.sp(self.types[t as usize].name)
    }

    fn mdata(&self, s: Sp) -> &[u8] {
        &self.mdata[s.0 as usize..(s.0 + s.1) as usize]
    }

    /// get_type
    fn get_type(&mut self, name: &[u8]) -> M<u32> {
        let Some(slash) = name.iter().position(|&c| c == b'/') else {
            return Err(MimeError);
        };
        if name[slash + 1..].contains(&b'/') {
            return Err(MimeError);
        }
        if let Some(t) = self.type_hash.get(name) {
            return Ok(t);
        }
        let t = self.types.len() as u32;
        let name_sp = self.intern(name);
        self.types.push(TypeRec {
            name: name_sp,
            slash: slash as u32,
        });
        self.type_hash.insert(name, t);
        Ok(t)
    }

    fn load_source_file(&mut self, doc: u32) -> R<()> {
        let t = self.tree(doc);
        let root = t.root;
        if !t.in_ns(root, FREE_NS) || t.local(root) != b"mime-info" {
            return Ok(()); // a warning, the file is skipped
        }
        for node in t.elements(root) {
            if !(t.in_ns(node, FREE_NS) && t.local(node) == b"mime-type") {
                continue;
            }
            let Some(type_name) = t.prop(node, b"type") else {
                continue;
            };
            let Ok(ty) = self.get_type(type_name) else {
                continue;
            };
            let _ = self.load_type(doc, ty, node)?;
        }
        Ok(())
    }

    /// load_type: Err(MimeError) ends it, keeping what was done.
    fn load_type(&mut self, doc: u32, ty: u32, node: u32) -> R<M<()>> {
        let t = self.tree(doc);
        for field in t.elements(node) {
            if t.in_ns(field, FREE_NS) {
                match self.process_freedesktop_node(doc, ty, field)? {
                    Err(e) => return Ok(Err(e)),
                    Ok(false) => continue, // processed, not copied
                    Ok(true) => {}
                }
            }
            let added_start = self.added.len() as u32;
            let clear = mime_xml::copy_info(t, field, FREE_NS, &mut self.frames, &mut self.added)?;
            let added = (added_start, self.added.len() as u32 - added_start);
            self.remove_old(doc, ty, field);
            let item = self.items.len() as u32;
            self.items.push(Item {
                ty,
                doc,
                node: field,
                clear,
                added,
                alive: true,
                next_lang: NONE,
            });
            if t.in_ns(field, FREE_NS) && t.local(field) == b"comment" {
                // The copy's xml:lang (the output has no DTD)
                let lang = t.lang_attr_span(field);
                let i = self.langs.slot(self.trees, ty, lang.map(|s| t.get(s)));
                let e = &mut self.langs.slots[i];
                if !e.used {
                    *e = LangEntry {
                        used: true,
                        ty,
                        doc,
                        lang,
                        list: List {
                            head: NONE,
                            tail: NONE,
                        },
                    };
                }
                if e.list.head == NONE {
                    e.list = List {
                        head: item,
                        tail: item,
                    };
                } else {
                    let tail = e.list.tail;
                    e.list.tail = item;
                    self.items[tail as usize].next_lang = item;
                }
            }
        }
        Ok(Ok(()))
    }

    /// remove_old: a new <comment> replaces the first one with the same xml:lang.
    fn remove_old(&mut self, doc: u32, ty: u32, new: u32) {
        let t = self.tree(doc);
        if !t.in_ns(new, FREE_NS) || t.local(new) != b"comment" {
            return;
        }
        let i = self.langs.slot(self.trees, ty, t.lang(new));
        let e = &mut self.langs.slots[i];
        if e.used && e.list.head != NONE {
            let item = e.list.head as usize;
            e.list.head = self.items[item].next_lang;
            if e.list.head == NONE {
                e.list.tail = NONE;
            }
            self.items[item].alive = false;
        }
    }

    fn get_int_attribute(&self, doc: u32, node: u32, name: &[u8]) -> R<i32> {
        let Some(s) = self.tree(doc).prop(node, name) else {
            return Ok(50);
        };
        let (v, end, overflow) = strtol(s, 10);
        if overflow {
            unsupported!("number out of range");
        }
        let p = if s.is_empty() || end != s.len() {
            -1
        } else {
            v as i32
        };
        Ok(if (0..=100).contains(&p) { p } else { -1 })
    }

    fn get_boolean_attribute(&self, doc: u32, node: u32, name: &[u8]) -> bool {
        self.tree(doc).prop(node, name) == Some(b"true")
    }

    /// Append `new` to the list of `key` in `hash`; `set_next` links it after the old tail.
    fn append(hash: &mut GHash<List>, key: &[u8], new: u32, set_next: impl FnOnce(u32)) {
        match hash.get(key) {
            Some(List { head, tail }) => {
                set_next(tail);
                hash.insert(key, List { head, tail: new });
            }
            None => hash.insert(
                key,
                List {
                    head: new,
                    tail: new,
                },
            ),
        }
    }

    /// process_freedesktop_node: whether to copy the field to the output.
    fn process_freedesktop_node(&mut self, doc: u32, ty: u32, field: u32) -> R<M<bool>> {
        let t = self.tree(doc);
        let mut error = false;
        let mut copy = false;
        match t.local(field) {
            b"glob" => {
                let weight = self.get_int_attribute(doc, field, b"weight")?;
                let case_sensitive = self.get_boolean_attribute(doc, field, b"case-sensitive");
                if weight == -1 {
                    error = true;
                }
                match t.prop(field, b"pattern") {
                    Some(pattern) if !pattern.is_empty() => {
                        let pat = self.intern(pattern);
                        if !case_sensitive {
                            self.arena[pat.0 as usize..].make_ascii_lowercase();
                        }
                        let g = self.globs.len() as u32;
                        self.globs.push(Glob {
                            weight,
                            pattern: pat,
                            ty,
                            noglob: false,
                            case_sensitive,
                            next: NONE,
                        });
                        let key = &self.arena[pat.0 as usize..];
                        let globs = &mut self.globs;
                        Self::append(&mut self.globs_hash, key, g, |tail| {
                            globs[tail as usize].next = g
                        });
                        copy = true;
                    }
                    _ => error = true,
                }
            }
            b"glob-deleteall" => {
                let pat = self.intern(NOGLOBS);
                let g = self.globs.len() as u32;
                self.globs.push(Glob {
                    weight: 0,
                    pattern: pat,
                    ty,
                    noglob: true,
                    case_sensitive: false,
                    next: NONE,
                });
                let globs = &mut self.globs;
                Self::append(&mut self.globs_hash, NOGLOBS, g, |tail| {
                    globs[tail as usize].next = g
                });
                copy = true;
            }
            b"magic" => match self.magic_new(doc, field, ty)? {
                Some(m) => self.magic.push(m),
                None => error = true,
            },
            b"magic-deleteall" => {
                let data = self.push_mdata(NOMAGIC);
                let first = self.matches.len() as u32;
                self.matches.push(MatchRec {
                    range_start: 0,
                    range_length: 1,
                    word_size: 1,
                    data,
                    mask: None,
                    first_child: NONE,
                    next: NONE,
                });
                self.magic.push(Magic {
                    priority: 0,
                    ty,
                    first,
                    nomagic: true,
                });
            }
            b"treemagic" => match self.tree_magic_new(doc, field, ty)? {
                Some(m) => self.tree_magic.push(m),
                None => error = true,
            },
            b"comment" | b"acronym" | b"expanded-acronym" => copy = true,
            name @ (b"alias" | b"sub-class-of") => match t.prop(field, b"type") {
                Some(other) if other.contains(&b'/') => {
                    if name == b"alias" {
                        self.alias_hash.insert(other, ty);
                    } else {
                        let p = self.parents.len() as u32;
                        let sp = self.intern(other);
                        self.parents.push((sp, NONE));
                        let n = self.types[ty as usize].name;
                        let key = &self.arena[n.0 as usize..(n.0 + n.1) as usize];
                        let parents = &mut self.parents;
                        Self::append(&mut self.subclass_hash, key, p, |tail| {
                            parents[tail as usize].1 = p
                        });
                    }
                    copy = true;
                }
                _ => error = true,
            },
            b"root-XML" => match (t.prop(field, b"namespaceURI"), t.prop(field, b"localName")) {
                (Some(uri), Some(local))
                    if !(uri.is_empty() && local.is_empty())
                        && !uri.iter().chain(local).any(|&c| c == b' ' || c == b'\n') =>
                {
                    self.scratch.clear();
                    self.scratch.extend_from_slice(uri);
                    self.scratch.push(b' ');
                    self.scratch.extend_from_slice(local);
                    self.namespace_hash.insert(&self.scratch, ty);
                }
                _ => error = true,
            },
            name @ (b"generic-icon" | b"icon") => {
                if let Some(icon) = t.prop(field, b"name") {
                    let icon = self.intern(icon);
                    let n = self.types[ty as usize].name;
                    let key = &self.arena[n.0 as usize..(n.0 + n.1) as usize];
                    if name == b"icon" {
                        self.icon_hash.insert(key, icon);
                    } else {
                        self.generic_icon_hash.insert(key, icon);
                    }
                    copy = true;
                }
            }
            _ => {}
        }
        Ok(if error { Err(MimeError) } else { Ok(copy) })
    }

    fn push_mdata(&mut self, d: &[u8]) -> Sp {
        let start = self.mdata.len() as u32;
        self.mdata.extend_from_slice(d);
        (start, d.len() as u32)
    }

    fn magic_new(&mut self, doc: u32, node: u32, ty: u32) -> R<Option<Magic>> {
        let priority = self.get_int_attribute(doc, node, b"priority")?;
        if priority == -1 {
            return Ok(None);
        }
        match self.build_matches(doc, node)? {
            Ok(first) if first != NONE => Ok(Some(Magic {
                priority,
                ty,
                first,
                nomagic: false,
            })),
            _ => Ok(None), // an error, or "Incomplete <magic> element"
        }
    }

    /// build_matches: the first of the list (NONE if empty), Err(()) for a GError
    fn build_matches(&mut self, doc: u32, parent: u32) -> R<std::result::Result<u32, ()>> {
        let t = self.tree(doc);
        let (mut first, mut last) = (NONE, NONE);
        for node in t.elements(parent) {
            if !t.in_ns(node, FREE_NS) || t.local(node) != b"match" {
                return Ok(Err(()));
            }
            let Ok(m) = self.match_new(doc, node)? else {
                return Ok(Err(()));
            };
            let index = self.matches.len() as u32;
            self.matches.push(m);
            if last == NONE {
                first = index;
            } else {
                self.matches[last as usize].next = index;
            }
            last = index;
            let Ok(child) = self.build_matches(doc, node)? else {
                return Ok(Err(()));
            };
            self.matches[index as usize].first_child = child;
        }
        Ok(Ok(first))
    }

    /// One <match> (without its children), Err(()) for a GError
    fn match_new(&mut self, doc: u32, node: u32) -> R<std::result::Result<MatchRec, ()>> {
        let t = self.tree(doc);
        let mut m = MatchRec {
            range_start: 0,
            range_length: 1,
            word_size: 1,
            data: (0, 0),
            mask: None,
            first_child: NONE,
            next: NONE,
        };
        // match_offset
        let Some(offset) = t.prop(node, b"offset").filter(|o| !o.is_empty()) else {
            return Ok(Err(()));
        };
        let (start, end, overflow) = strtol(offset, 10);
        if overflow {
            unsupported!("number out of range");
        }
        m.range_start = start;
        if offset.get(end) == Some(&b':') && end + 1 < offset.len() && start >= 0 {
            let begin = &offset[end + 1..];
            let (last, end2, overflow) = strtol(begin, 10);
            if overflow {
                unsupported!("number out of range");
            }
            let last = last as i32;
            if end2 == begin.len() && last as i64 >= start {
                m.range_length = (last as i64 - start + 1) as i32;
            } else {
                return Ok(Err(()));
            }
        } else if end != offset.len() {
            return Ok(Err(()));
        }
        // match_word_size
        let Some(ty) = t.prop(node, b"type") else {
            return Ok(Err(()));
        };
        match ty {
            b"host16" => m.word_size = 2,
            b"host32" => m.word_size = 4,
            b"big16" | b"big32" | b"little16" | b"little32" | b"string" | b"byte" => {}
            _ => return Ok(Err(())),
        }
        // match_value_and_mask
        let mask = t.prop(node, b"mask");
        let Some(value) = t.prop(node, b"value").filter(|v| !v.is_empty()) else {
            return Ok(Err(()));
        };
        let has = |n: &[u8]| ty.windows(n.len()).any(|w| w == n);
        let data_start = self.mdata.len();
        let parsed = if has(b"16") {
            parse_int_value(&mut self.mdata, 2, value, mask, ty[0] != b'l')?
        } else if has(b"32") {
            parse_int_value(&mut self.mdata, 4, value, mask, ty[0] != b'l')?
        } else if ty == b"byte" {
            parse_int_value(&mut self.mdata, 1, value, mask, false)?
        } else {
            getstr(value, &mut self.mdata);
            let len = self.mdata.len() - data_start;
            match mask {
                None => Some(None),
                Some(mask) => parse_string_mask(&mut self.mdata, mask, len),
            }
        };
        let Some(mask_start) = parsed else {
            self.mdata.truncate(data_start);
            return Ok(Err(()));
        };
        let end = mask_start.unwrap_or(self.mdata.len());
        m.data = (data_start as u32, (end - data_start) as u32);
        m.mask = mask_start.map(|s| (s as u32, (self.mdata.len() - s) as u32));
        Ok(Ok(m))
    }

    fn tree_magic_new(&mut self, doc: u32, node: u32, ty: u32) -> R<Option<TreeMagic>> {
        let priority = self.get_int_attribute(doc, node, b"priority")?;
        if priority == -1 {
            return Ok(None);
        }
        Ok(self
            .build_tree_matches(doc, node)
            .ok()
            .map(|first| TreeMagic {
                priority,
                ty,
                first,
            }))
    }

    fn build_tree_matches(&mut self, doc: u32, parent: u32) -> std::result::Result<u32, ()> {
        let t = self.tree(doc);
        let (mut first, mut last) = (NONE, NONE);
        for node in t.elements(parent) {
            if !t.in_ns(node, FREE_NS) || t.local(node) != b"treematch" {
                return Err(());
            }
            let path = t.prop(node, b"path").ok_or(())?;
            let ty = match t.prop(node, b"type") {
                None => 0,
                Some(b"file") => 1,
                Some(b"directory") => 2,
                Some(b"link") => 3,
                Some(_) => return Err(()),
            };
            let path = self.intern(path);
            let mimetype = t.prop(node, b"mimetype").map(|m| self.intern(m));
            let m = TreeMatchRec {
                path,
                ty,
                executable: self.get_boolean_attribute(doc, node, b"executable"),
                match_case: self.get_boolean_attribute(doc, node, b"match-case"),
                non_empty: self.get_boolean_attribute(doc, node, b"non-empty"),
                mimetype,
                first_child: NONE,
                next: NONE,
            };
            let index = self.tree_matches.len() as u32;
            self.tree_matches.push(m);
            if last == NONE {
                first = index;
            } else {
                self.tree_matches[last as usize].next = index;
            }
            last = index;
            let child = self.build_tree_matches(doc, node)?;
            self.tree_matches[index as usize].first_child = child;
        }
        Ok(first)
    }
}

fn hextoint(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// getstr: C escapes, appended to `out`
fn getstr(s: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        i += 1;
        if c != b'\\' {
            out.push(c);
            continue;
        }
        let Some(&c) = s.get(i) else {
            return;
        };
        i += 1;
        match c {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b'b' => out.push(8),
            b't' => out.push(b'\t'),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut val = (c - b'0') as u32;
                if let Some(&d @ b'0'..=b'7') = s.get(i) {
                    val = (val << 3) | (d - b'0') as u32;
                    i += 1;
                    if let Some(&d @ b'0'..=b'7') = s.get(i) {
                        val = (val << 3) | (d - b'0') as u32;
                        i += 1;
                    }
                }
                out.push(val as u8);
            }
            b'x' => {
                let mut val = b'x' as u32;
                if let Some(d) = s.get(i).and_then(|&c| hextoint(c)) {
                    val = d as u32;
                    i += 1;
                    if let Some(d) = s.get(i).and_then(|&c| hextoint(c)) {
                        val = (val << 4) + d as u32;
                        i += 1;
                    }
                }
                out.push(val as u8);
            }
            _ => out.push(c),
        }
    }
}

/// parse_int_value, appending the value (and mask) to `out`: Some(mask start) or None for a
/// GError
fn parse_int_value(
    out: &mut Vec<u8>,
    bytes: usize,
    input: &[u8],
    mask: Option<&[u8]>,
    big_endian: bool,
) -> R<Option<Option<usize>>> {
    let (value, end, overflow) = strtoul(input, 0);
    if overflow {
        unsupported!("number out of range");
    }
    if end != input.len() {
        return Ok(None);
    }
    let push = |out: &mut Vec<u8>, v: u64| {
        for b in 0..bytes {
            let shift = (if big_endian { bytes - b - 1 } else { b }) * 8;
            out.push(((v >> shift) & 0xff) as u8);
        }
    };
    push(out, value);
    if (bytes == 1 && value & !0xff != 0) || (bytes == 2 && value & !0xffff != 0) {
        return Ok(None);
    }
    let Some(m) = mask else {
        return Ok(Some(None));
    };
    let (mv, end, overflow) = strtoul(m, 0);
    if overflow {
        unsupported!("number out of range");
    }
    if end != m.len() {
        return Ok(None);
    }
    let start = out.len();
    push(out, mv);
    Ok(Some(Some(start)))
}

/// parse_string_mask, appending the mask to `out`: Some(mask start, or None for len == 0
/// (g_return_val_if_fail)), or None for a GError
fn parse_string_mask(out: &mut Vec<u8>, mask: &[u8], len: usize) -> Option<Option<usize>> {
    if len == 0 {
        return Some(None);
    }
    if mask.first() != Some(&b'0') || mask.get(1) != Some(&b'x') {
        return None;
    }
    let start = out.len();
    out.resize(start + len, 0);
    for (i, &c) in mask[2..].iter().enumerate() {
        let c = hextoint(c)?;
        if i >= len * 2 {
            return None;
        }
        out[start + (i >> 1)] |= if i & 1 == 1 { c } else { c << 4 };
    }
    Some(Some(start))
}

// --- Output ---

/// The files to write, computed before writing anything: paths in `paths`, contents in `out`.
struct Plan {
    paths: Vec<u8>,
    out: Vec<u8>,
    /// Files of delete_old_types, media directories: ranges in `paths`
    delete: Vec<(u32, u32)>,
    dirs: Vec<(u32, u32)>,
    /// (path, content), in update-mime-database's order (a later one with the same path wins)
    files: Vec<((u32, u32), (u32, u32))>,
}

impl Plan {
    fn path(&mut self, parts: &[&[u8]]) -> (u32, u32) {
        let start = self.paths.len() as u32;
        for p in parts {
            self.paths.extend_from_slice(p);
        }
        (start, self.paths.len() as u32)
    }

    fn file(&mut self, path: (u32, u32), content_start: usize) {
        self.files
            .push((path, (content_start as u32, self.out.len() as u32)));
    }

    fn apply(self) -> Result<()> {
        let p =
            |r: (u32, u32)| Path::new(OsStr::from_bytes(&self.paths[r.0 as usize..r.1 as usize]));
        for &d in &self.delete {
            let _ = fs::remove_file(p(d));
        }
        for &d in &self.dirs {
            let _ = fs::DirBuilder::new().create(p(d));
        }
        let mut new = Vec::with_capacity(4096);
        for &(path, (s, e)) in &self.files {
            new.clear();
            new.extend_from_slice(&self.paths[path.0 as usize..path.1 as usize]);
            new.extend_from_slice(b".new");
            let new = Path::new(OsStr::from_bytes(&new));
            fs::write(new, &self.out[s as usize..e as usize])
                .with_context(|| format!("writing {new:?}"))?;
            fs::rename(new, p(path)).with_context(|| format!("renaming {new:?}"))?;
        }
        Ok(())
    }
}

/// path.extension() == ".xml"
fn has_xml_extension(name: &[u8]) -> bool {
    matches!(name.iter().rposition(|&c| c == b'.'), Some(p) if p != 0 && &name[p..] == b".xml")
}

fn io<T>(r: std::io::Result<T>) -> R<T> {
    r.map_err(|e| Unsupported(e.to_string()))
}

fn plan(mime_dir: &[u8], version: &[u8]) -> R<Plan> {
    let dir = Path::new(OsStr::from_bytes(mime_dir));
    let package_dir = dir.join("packages");
    if !dir.is_dir() || !package_dir.is_dir() {
        unsupported!("no {package_dir:?}");
    }
    // scan_source_dir: sorted, Override.xml last
    let mut files = Vec::with_capacity(64);
    let mut override_file = None;
    for entry in io(fs::read_dir(&package_dir))? {
        let entry = io(entry)?;
        let name = entry.file_name();
        if !has_xml_extension(name.as_bytes()) {
            continue;
        }
        if name == "Override.xml" {
            override_file = Some(entry.path());
        } else {
            files.push(entry.path());
        }
    }
    files.sort_by(|a, b| a.as_os_str().as_bytes().cmp(b.as_os_str().as_bytes()));
    files.extend(override_file);

    let mut trees = Vec::with_capacity(files.len());
    let mut input_bytes = 0;
    for f in &files {
        let data = fs::read(f).map_err(|e| Unsupported(format!("{f:?}: {e}")))?;
        input_bytes += data.len();
        trees.push(
            mime_xml::parse(data).map_err(|Unsupported(e)| Unsupported(format!("{f:?}: {e}")))?,
        );
    }
    let mut db = Db::new(&trees);
    for doc in 0..trees.len() as u32 {
        db.load_source_file(doc)?;
    }

    let mut plan = Plan {
        paths: Vec::with_capacity(db.types.len() * (2 * mime_dir.len() + 96) + 4096),
        out: Vec::with_capacity(input_bytes * 2 + 256 * 1024),
        delete: Vec::new(),
        dirs: Vec::with_capacity(db.types.len()),
        files: Vec::with_capacity(db.types.len() + 16),
    };

    // delete_old_types
    for media in MEDIA_TYPES {
        let media_dir = dir.join(media);
        if !media_dir.is_dir() {
            continue;
        }
        for entry in io(fs::read_dir(&media_dir))? {
            let name = io(entry)?.file_name();
            let name = name.as_bytes();
            if !has_xml_extension(name) {
                continue;
            }
            db.scratch.clear();
            db.scratch.extend_from_slice(media.as_bytes());
            db.scratch.push(b'/');
            db.scratch.extend_from_slice(&name[..name.len() - 4]);
            if !db.type_hash.contains(&db.scratch) {
                let path = plan.path(&[mime_dir, b"/", &db.scratch, b".xml"]);
                plan.delete.push(path);
            }
        }
    }

    // write_out_type, in the order of the types table: the items of each type, in order
    let n_types = db.types.len();
    let mut starts = vec![0u32; n_types + 1];
    for it in db.items.iter().filter(|i| i.alive) {
        starts[it.ty as usize + 1] += 1;
    }
    for i in 0..n_types {
        starts[i + 1] += starts[i];
    }
    let mut fill = starts.clone();
    let mut order = vec![0u32; starts[n_types] as usize];
    for (i, it) in db.items.iter().enumerate().filter(|(_, i)| i.alive) {
        order[fill[it.ty as usize] as usize] = i as u32;
        fill[it.ty as usize] += 1;
    }
    for (_, t) in db.type_hash.iter() {
        let ty = &db.types[t as usize];
        let name = db.sp(ty.name);
        let slash = ty.slash as usize;
        let media_start = plan.paths.len();
        let media_path = plan.path(&[mime_dir, b"/", &name[..slash]]);
        plan.paths[media_start + mime_dir.len() + 1..].make_ascii_lowercase();
        let media = OsStr::from_bytes(&plan.paths[media_path.0 as usize..media_path.1 as usize]);
        if fs::symlink_metadata(media).is_ok_and(|m| !m.is_dir()) {
            unsupported!("a media directory isn't a directory");
        }
        plan.dirs.push(media_path);
        let file_start = plan.paths.len();
        let path = plan.path(&[
            mime_dir,
            b"/",
            &name[..slash],
            b"/",
            &name[slash + 1..],
            b".xml",
        ]);
        plan.paths[file_start + mime_dir.len() + 1..].make_ascii_lowercase();

        let start = plan.out.len();
        let out = &mut plan.out;
        out.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<mime-type xmlns=\"");
        mime_xml::escape(out, FREE_NS, true);
        out.extend_from_slice(b"\" type=\"");
        mime_xml::escape(out, name, true);
        out.extend_from_slice(b"\">\n");
        mime_xml::indent(out, 1);
        out.extend_from_slice(b"<!--");
        out.extend_from_slice(CREATED);
        out.extend_from_slice(b"-->\n");
        for &i in &order[starts[t as usize] as usize..starts[t as usize + 1] as usize] {
            let it = &db.items[i as usize];
            let added = (!it.clear)
                .then(|| &db.added[it.added.0 as usize..(it.added.0 + it.added.1) as usize]);
            mime_xml::dump_copy(out, &trees[it.doc as usize], it.node, added, 1);
            out.push(b'\n');
        }
        out.extend_from_slice(b"</mime-type>\n");
        plan.file(path, start);
    }

    // globs, globs2
    let mut glob_list: Vec<u32> = Vec::with_capacity(db.globs.len());
    for (_, list) in db.globs_hash.iter() {
        let mut g = list.head;
        while g != NONE {
            glob_list.push(g);
            g = db.globs[g as usize].next;
        }
    }
    glob_list.sort_by(|&a, &b| {
        let (a, b) = (&db.globs[a as usize], &db.globs[b as usize]);
        if a.noglob || b.noglob {
            (b.noglob as i32 - a.noglob as i32).cmp(&0)
        } else {
            (b.weight - a.weight).cmp(&0)
        }
    });
    const HEADER: &[u8] = b"# This file was automatically generated by the\n# update-mime-database command. DO NOT EDIT!\n";
    for two in [false, true] {
        let start = plan.out.len();
        plan.out.extend_from_slice(HEADER);
        for &g in &glob_list {
            let glob = &db.globs[g as usize];
            let pattern = db.sp(glob.pattern);
            if pattern.contains(&b'\n') {
                continue;
            }
            let ty = db.tname(glob.ty);
            let out = &mut plan.out;
            if !two {
                out.extend_from_slice(ty);
                out.push(b':');
                out.extend_from_slice(pattern);
                out.push(b'\n');
                continue;
            }
            for cs in [true, false] {
                if cs && !glob.case_sensitive {
                    continue;
                }
                out.extend_from_slice(glob.weight.to_string().as_bytes());
                out.push(b':');
                out.extend_from_slice(ty);
                out.push(b':');
                out.extend_from_slice(pattern);
                if cs {
                    out.extend_from_slice(b":cs");
                }
                out.push(b'\n');
            }
        }
        let path = plan.path(&[mime_dir, if two { b"/globs2" } else { b"/globs" }]);
        plan.file(path, start);
    }

    // magic
    let mut magic = std::mem::take(&mut db.magic);
    magic.sort_by(|a, b| cmp_magic(&db, a, b));
    db.magic = magic;
    let start = plan.out.len();
    plan.out.extend_from_slice(b"MIME-Magic\0\n");
    for m in &db.magic {
        plan.out
            .extend_from_slice(format!("[{}:", m.priority).as_bytes());
        plan.out.extend_from_slice(db.tname(m.ty));
        plan.out.extend_from_slice(b"]\n");
        write_magic_children(&db, &mut plan.out, m.first, 0);
    }
    let path = plan.path(&[mime_dir, b"/magic"]);
    plan.file(path, start);

    // Sorted lines: written to `out`, then sorted into place
    let mut lines: Vec<(u32, u32)> = Vec::with_capacity(db.types.len() + 64);
    let finish_sorted = |plan: &mut Plan, lines: &mut Vec<(u32, u32)>, name: &[u8], body: usize| {
        let buf = &plan.out;
        lines.sort_by(|a, b| buf[a.0 as usize..a.1 as usize].cmp(&buf[b.0 as usize..b.1 as usize]));
        let unsorted_end = plan.out.len();
        for &(s, e) in lines.iter() {
            plan.out.extend_from_within(s as usize..e as usize);
        }
        plan.out.copy_within(unsorted_end.., body);
        plan.out.truncate(body + (unsorted_end - body));
        let path = plan.path(&[mime_dir, name]);
        plan.file(path, body);
        lines.clear();
    };

    // XMLnamespaces
    let body = plan.out.len();
    for (k, t) in db.namespace_hash.iter() {
        let s = plan.out.len() as u32;
        plan.out.extend_from_slice(k);
        plan.out.push(b' ');
        plan.out.extend_from_slice(db.tname(t));
        plan.out.push(b'\n');
        lines.push((s, plan.out.len() as u32));
    }
    finish_sorted(&mut plan, &mut lines, b"/XMLnamespaces", body);

    // subclasses
    let start = plan.out.len();
    for (k, list) in db.subclass_hash.iter() {
        let mut p = list.head;
        while p != NONE {
            plan.out.extend_from_slice(k);
            plan.out.push(b' ');
            plan.out.extend_from_slice(db.sp(db.parents[p as usize].0));
            plan.out.push(b'\n');
            p = db.parents[p as usize].1;
        }
    }
    let path = plan.path(&[mime_dir, b"/subclasses"]);
    plan.file(path, start);

    // aliases
    let body = plan.out.len();
    for (k, t) in db.alias_hash.iter() {
        let s = plan.out.len() as u32;
        plan.out.extend_from_slice(k);
        plan.out.push(b' ');
        plan.out.extend_from_slice(db.tname(t));
        plan.out.push(b'\n');
        lines.push((s, plan.out.len() as u32));
    }
    finish_sorted(&mut plan, &mut lines, b"/aliases", body);

    // types
    let body = plan.out.len();
    for (k, _) in db.type_hash.iter() {
        let s = plan.out.len() as u32;
        plan.out.extend_from_slice(k);
        plan.out.push(b'\n');
        lines.push((s, plan.out.len() as u32));
    }
    finish_sorted(&mut plan, &mut lines, b"/types", body);

    // generic-icons, icons
    for (name, hash) in [
        (b"/generic-icons".as_slice(), &db.generic_icon_hash),
        (b"/icons", &db.icon_hash),
    ] {
        let start = plan.out.len();
        for (k, v) in hash.iter() {
            plan.out.extend_from_slice(k);
            plan.out.push(b':');
            plan.out.extend_from_slice(db.sp(v));
            plan.out.push(b'\n');
        }
        let path = plan.path(&[mime_dir, name]);
        plan.file(path, start);
    }

    // treemagic
    let mut tree_magic = std::mem::take(&mut db.tree_magic);
    tree_magic.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| cmp_types(&db, a.ty, b.ty))
    });
    db.tree_magic = tree_magic;
    let start = plan.out.len();
    plan.out.extend_from_slice(b"MIME-TreeMagic\0\n");
    for m in &db.tree_magic {
        plan.out
            .extend_from_slice(format!("[{}:", m.priority).as_bytes());
        plan.out.extend_from_slice(db.tname(m.ty));
        plan.out.extend_from_slice(b"]\n");
        write_tree_magic_children(&db, &mut plan.out, m.first, 0);
    }
    let path = plan.path(&[mime_dir, b"/treemagic"]);
    plan.file(path, start);

    // mime.cache
    let start = plan.out.len();
    write_cache(&db, &mut plan.out)?;
    let path = plan.path(&[mime_dir, b"/mime.cache"]);
    plan.file(path, start);

    // version
    let start = plan.out.len();
    plan.out.extend_from_slice(version);
    plan.out.push(b'\n');
    let path = plan.path(&[mime_dir, b"/version"]);
    plan.file(path, start);

    // Not needed anymore: skip freeing them
    std::mem::forget(db);
    std::mem::forget(trees);
    Ok(plan)
}

fn cmp_types(db: &Db, a: u32, b: u32) -> std::cmp::Ordering {
    let (ta, tb) = (&db.types[a as usize], &db.types[b as usize]);
    let (na, nb) = (db.sp(ta.name), db.sp(tb.name));
    let (ma, sa) = na.split_at(ta.slash as usize);
    let (mb, sb) = nb.split_at(tb.slash as usize);
    ma.cmp(mb).then_with(|| sa[1..].cmp(&sb[1..]))
}

fn cmp_magic(db: &Db, a: &Magic, b: &Magic) -> std::cmp::Ordering {
    if a.nomagic || b.nomagic {
        return (b.nomagic as i32 - a.nomagic as i32).cmp(&0);
    }
    b.priority
        .cmp(&a.priority)
        .then_with(|| cmp_types(db, a.ty, b.ty))
}

fn write_magic_children(db: &Db, out: &mut Vec<u8>, first: u32, indent: i32) {
    let mut i = first;
    while i != NONE {
        let m = &db.matches[i as usize];
        if indent != 0 {
            out.extend_from_slice(format!("{indent}").as_bytes());
        }
        out.extend_from_slice(format!(">{}=", m.range_start).as_bytes());
        let data = db.mdata(m.data);
        // write16: nothing beyond 0xffff (g_return_if_fail)
        if data.len() <= 0xffff {
            out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        }
        out.extend_from_slice(data);
        if let Some(mask) = m.mask {
            out.push(b'&');
            out.extend_from_slice(db.mdata(mask));
        }
        if m.word_size != 1 {
            out.extend_from_slice(format!("~{}", m.word_size).as_bytes());
        }
        if m.range_length != 1 {
            out.extend_from_slice(format!("+{}", m.range_length).as_bytes());
        }
        out.push(b'\n');
        write_magic_children(db, out, m.first_child, indent + 1);
        i = m.next;
    }
}

fn write_tree_magic_children(db: &Db, out: &mut Vec<u8>, first: u32, indent: i32) {
    let mut i = first;
    while i != NONE {
        let m = &db.tree_matches[i as usize];
        if indent != 0 {
            out.extend_from_slice(format!("{indent}").as_bytes());
        }
        out.extend_from_slice(b">\"");
        out.extend_from_slice(db.sp(m.path));
        out.extend_from_slice(b"\"=");
        out.extend_from_slice(match m.ty {
            1 => b"file".as_slice(),
            2 => b"directory",
            3 => b"link",
            _ => b"any",
        });
        if m.match_case {
            out.extend_from_slice(b",match-case");
        }
        if m.executable {
            out.extend_from_slice(b",executable");
        }
        if m.non_empty {
            out.extend_from_slice(b",non-empty");
        }
        if let Some(t) = m.mimetype {
            out.push(b',');
            out.extend_from_slice(db.sp(t));
        }
        out.push(b'\n');
        write_tree_magic_children(db, out, m.first_child, indent + 1);
        i = m.next;
    }
}

// --- mime.cache ---

#[derive(PartialEq)]
enum GlobType {
    Literal,
    Simple,
    Full,
}

/// glob_type (it steps over UTF-8 characters, which doesn't change the result)
fn glob_type(glob: &[u8]) -> GlobType {
    let mut maybe_simple = false;
    for (i, &c) in glob.iter().enumerate() {
        if c == b'*' && i == 0 {
            maybe_simple = true;
        } else if matches!(c, b'\\' | b'[' | b'?' | b'*') {
            return GlobType::Full;
        }
    }
    if maybe_simple {
        GlobType::Simple
    } else {
        GlobType::Literal
    }
}

fn align4(n: u32) -> u32 {
    (n + 3) & !3
}

/// The suffix tree (SuffixEntry): children lists in an array
struct Suffix {
    character: u32,
    /// The glob's type, for leaves (character 0)
    ty: u32,
    weight: i32,
    flags: u32,
    first_child: u32,
    next: u32,
}

/// Where a list's link to its next entry is: its head, a node's `next`, a node's first child
#[derive(Clone, Copy)]
enum Link {
    Root,
    Next(u32),
    Child(u32),
}

struct Suffixes {
    nodes: Vec<Suffix>,
    first: u32,
}

impl Suffixes {
    fn get(&self, l: Link) -> u32 {
        match l {
            Link::Root => self.first,
            Link::Next(n) => self.nodes[n as usize].next,
            Link::Child(n) => self.nodes[n as usize].first_child,
        }
    }

    fn set(&mut self, l: Link, v: u32) {
        match l {
            Link::Root => self.first = v,
            Link::Next(n) => self.nodes[n as usize].next = v,
            Link::Child(n) => self.nodes[n as usize].first_child = v,
        }
    }

    /// Insert a node before what `link` points at
    fn insert_at(&mut self, link: Link, character: u32, ty: u32, weight: i32, flags: u32) -> u32 {
        let next = self.get(link);
        self.nodes.push(Suffix {
            character,
            ty,
            weight,
            flags,
            first_child: NONE,
            next,
        });
        let n = (self.nodes.len() - 1) as u32;
        self.set(link, n);
        n
    }

    /// insert_suffix into the list at `list`
    fn insert(&mut self, db: &Db, list: Link, suffix: &[u32], ty: u32, weight: i32, flags: u32) {
        // The entry for suffix[0], keeping the list sorted
        let c = suffix[0];
        let mut link = list;
        let entry = loop {
            let cur = self.get(link);
            if cur == NONE || self.nodes[cur as usize].character > c {
                break self.insert_at(link, c, NONE, 0, 0);
            }
            if self.nodes[cur as usize].character == c {
                break cur;
            }
            link = Link::Next(cur);
        };
        if suffix.len() > 1 {
            return self.insert(db, Link::Child(entry), &suffix[1..], ty, weight, flags);
        }
        // A leaf for the type, among the leading leaves (character 0)
        let mut link = Link::Child(entry);
        loop {
            let cur = self.get(link);
            if cur == NONE || self.nodes[cur as usize].character != 0 {
                self.insert_at(link, 0, ty, weight, flags);
                return;
            }
            let leaf = &mut self.nodes[cur as usize];
            if db.tname(leaf.ty) == db.tname(ty) {
                if leaf.weight < weight {
                    leaf.weight = weight;
                }
                return;
            }
            link = Link::Next(cur);
        }
    }

    fn children(&self, n: u32) -> impl Iterator<Item = u32> + '_ {
        let first = if n == NONE {
            self.first
        } else {
            self.nodes[n as usize].first_child
        };
        crate::mime_xml::linked(first, move |c| self.nodes[c as usize].next)
    }

    fn depth(&self, n: u32) -> u32 {
        self.children(n)
            .map(|c| self.depth(c) + 1)
            .max()
            .unwrap_or(0)
    }
}

struct Cache<'a> {
    out: &'a mut Vec<u8>,
    base: usize,
    strings: GHash<u32>,
}

impl Cache<'_> {
    fn card32(&mut self, n: u32) {
        self.out.extend_from_slice(&n.to_be_bytes());
    }

    fn offset(&self) -> u32 {
        (self.out.len() - self.base) as u32
    }

    fn data(&mut self, d: &[u8], terminator: bool) {
        self.out.extend_from_slice(d);
        let len = d.len() as u32 + terminator as u32;
        let padded = align4(len) as usize;
        self.out.resize(self.out.len() + padded - d.len(), 0);
    }

    /// The offset of a string of the pool
    fn string(&mut self, s: &[u8]) -> R<()> {
        match self.strings.get(s) {
            Some(o) if o != 0 => {
                self.card32(o);
                Ok(())
            }
            _ => unsupported!("missing string in mime.cache"),
        }
    }
}

fn glob_weight_and_flags(g: &Glob) -> u32 {
    (g.weight as u32 & 0xff) | if g.case_sensitive { 0x100 } else { 0 }
}

/// The keys of `hash` that pass `filter`, sorted (strcmp)
fn sorted_keys<V: Copy + Default>(hash: &GHash<V>, filter: impl Fn(&[u8]) -> bool) -> Vec<&[u8]> {
    let mut keys: Vec<&[u8]> = hash.iter().map(|(k, _)| k).filter(|k| filter(k)).collect();
    keys.sort();
    keys
}

fn write_cache(db: &Db, out: &mut Vec<u8>) -> R<()> {
    let base = out.len();
    out.resize(base + 44, 0);
    let mut c = Cache {
        out,
        base,
        strings: GHash::new(256 * 1024),
    };
    let tn = |t: u32| db.tname(t);
    let parents = |list: List| {
        crate::mime_xml::linked(list.head, |p| db.parents[p as usize].1)
            .map(|p| db.sp(db.parents[p as usize].0))
    };
    let globs_of = |list: List| {
        crate::mime_xml::linked(list.head, |g| db.globs[g as usize].next)
            .map(|g| &db.globs[g as usize])
    };

    // collect_strings
    for (k, t) in db.alias_hash.iter() {
        c.strings.insert(k, 0);
        c.strings.insert(tn(t), 0);
    }
    for (k, list) in db.subclass_hash.iter() {
        c.strings.insert(k, 0);
        for p in parents(list) {
            c.strings.insert(p, 0);
        }
    }
    for (k, list) in db.globs_hash.iter() {
        if glob_type(k) != GlobType::Simple {
            c.strings.insert(k, 0);
        }
        for g in globs_of(list) {
            c.strings.insert(tn(g.ty), 0);
        }
    }
    for m in &db.magic {
        c.strings.insert(tn(m.ty), 0);
    }
    for (k, t) in db.namespace_hash.iter() {
        c.strings.insert(tn(t), 0);
        match k.iter().position(|&b| b == b' ') {
            Some(p) => {
                c.strings.insert(&k[..p], 0);
                c.strings.insert(&k[p + 1..], 0);
            }
            None => c.strings.insert(k, 0),
        }
    }
    for hash in [&db.generic_icon_hash, &db.icon_hash] {
        for (k, v) in hash.iter() {
            c.strings.insert(k, 0);
            c.strings.insert(db.sp(v), 0);
        }
    }

    // write_strings, in foreach order (the keys copied first: their offsets go into the table)
    let mut keys = Vec::with_capacity(c.strings.len() * 32);
    let mut ranges = Vec::with_capacity(c.strings.len());
    for (k, _) in c.strings.iter() {
        ranges.push((keys.len(), keys.len() + k.len()));
        keys.extend_from_slice(k);
    }
    for &(s, e) in &ranges {
        let k = &keys[s..e];
        let o = c.offset();
        c.data(k, true);
        c.strings.insert(k, o);
    }

    // write_alias_cache
    let alias_offset = c.offset();
    let keys = sorted_keys(&db.alias_hash, |_| true);
    c.card32(keys.len() as u32);
    for k in keys {
        c.string(k)?;
        c.string(tn(db.alias_hash.get(k).unwrap()))?;
    }

    // write_parent_cache
    let parent_offset = c.offset();
    let keys = sorted_keys(&db.subclass_hash, |_| true);
    c.card32(keys.len() as u32);
    let mut lists_offset = parent_offset + 4 + keys.len() as u32 * 8;
    for &k in &keys {
        c.string(k)?;
        c.card32(lists_offset);
        lists_offset += 4 + 4 * parents(db.subclass_hash.get(k).unwrap()).count() as u32;
    }
    for &k in &keys {
        let list = db.subclass_hash.get(k).unwrap();
        c.card32(parents(list).count() as u32);
        for p in parents(list) {
            c.string(p)?;
        }
    }

    // write_literal_cache, write_glob_cache
    let write_globs = |c: &mut Cache, kind: GlobType| -> R<()> {
        let keys = sorted_keys(&db.globs_hash, |k| glob_type(k) == kind);
        c.card32(
            keys.iter()
                .map(|k| globs_of(db.globs_hash.get(k).unwrap()).count() as u32)
                .sum(),
        );
        for k in keys {
            for g in globs_of(db.globs_hash.get(k).unwrap()) {
                c.string(db.sp(g.pattern))?;
                c.string(tn(g.ty))?;
                c.card32(glob_weight_and_flags(g));
            }
        }
        Ok(())
    };
    let literal_offset = c.offset();
    write_globs(&mut c, GlobType::Literal)?;

    // write_suffix_cache: the tree, built in the order of globs_hash
    let suffix_offset = c.offset();
    let capacity: usize = db
        .globs_hash
        .iter()
        .filter(|(k, _)| glob_type(k) == GlobType::Simple)
        .map(|(k, list)| k.len() * globs_of(list).count() + 1)
        .sum();
    let mut sfx = Suffixes {
        nodes: Vec::with_capacity(capacity),
        first: NONE,
    };
    let mut chars: Vec<u32> = Vec::with_capacity(256);
    for (pattern, list) in db.globs_hash.iter() {
        if glob_type(pattern) != GlobType::Simple {
            continue;
        }
        let Ok(rest) = std::str::from_utf8(&pattern[1..]) else {
            continue;
        };
        chars.clear();
        chars.extend(rest.chars().rev().map(|ch| ch as u32));
        if chars.is_empty() {
            // g_utf8_to_ucs4("") and insert_suffix reads past its NUL
            unsupported!("glob \"*\"");
        }
        for g in globs_of(list) {
            let flags = if g.case_sensitive { 0x100 } else { 0 };
            sfx.insert(db, Link::Root, &chars, g.ty, g.weight, flags);
        }
    }
    let roots: Vec<u32> = sfx.children(NONE).collect();
    let entries_offset = suffix_offset + 8;
    let mut child_offset = entries_offset + 12 * roots.len() as u32;
    let max_depth = roots.iter().map(|&s| sfx.depth(s) + 1).max().unwrap_or(0);
    c.card32(roots.len() as u32);
    c.card32(entries_offset);
    fn write_entries(
        c: &mut Cache,
        db: &Db,
        sfx: &Suffixes,
        depth: u32,
        n: u32,
        child_offset: &mut u32,
    ) -> R<()> {
        if depth > 0 {
            for child in sfx.children(n) {
                write_entries(c, db, sfx, depth - 1, child, child_offset)?;
            }
            return Ok(());
        }
        let e = &sfx.nodes[n as usize];
        let children = sfx.children(n).count() as u32;
        c.card32(e.character);
        if e.character == 0 {
            c.string(db.tname(e.ty))?;
            c.card32((e.weight as u32 & 0xff) | e.flags);
        } else {
            c.card32(children);
            c.card32(*child_offset);
        }
        *child_offset += 12 * children;
        Ok(())
    }
    for d in 0..max_depth {
        for &s in &roots {
            write_entries(&mut c, db, &sfx, d, s, &mut child_offset)?;
        }
    }
    if child_offset != c.offset() {
        unsupported!("suffix tree size");
    }

    let glob_offset = c.offset();
    write_globs(&mut c, GlobType::Full)?;

    // write_magic_cache: the matches in the order collect_matches_list gives them
    let magic_offset = c.offset();
    let list = |first: u32| crate::mime_xml::linked(first, |m| db.matches[m as usize].next);
    let mut order: Vec<u32> = Vec::with_capacity(db.matches.len());
    fn collect(db: &Db, first: u32, order: &mut Vec<u32>) {
        let list = || crate::mime_xml::linked(first, |m| db.matches[m as usize].next);
        order.extend(list());
        for m in list() {
            collect(db, db.matches[m as usize].first_child, order);
        }
    }
    for m in &db.magic {
        collect(db, m.first, &mut order);
    }
    let mut index = vec![0u32; db.matches.len()];
    for (i, &m) in order.iter().enumerate() {
        index[m as usize] = i as u32;
    }
    // MAX(guint, long) into a guint
    let max_extent = order.iter().fold(0u32, |max, &m| {
        let m = &db.matches[m as usize];
        let extent = m.data.1 as i64 + m.range_start + m.range_length as i64;
        if extent > max as i64 {
            extent as u32
        } else {
            max
        }
    });
    let entries_offset = magic_offset + 12;
    c.card32(db.magic.len() as u32);
    c.card32(max_extent);
    c.card32(entries_offset);
    let matchlets_offset = entries_offset + 16 * db.magic.len() as u32;
    for m in &db.magic {
        c.card32(m.priority as u32);
        c.string(tn(m.ty))?;
        c.card32(list(m.first).count() as u32);
        c.card32(matchlets_offset + 32 * index[m.first as usize]);
    }
    let mut offset2 = matchlets_offset + 32 * order.len() as u32;
    for &m in &order {
        let m = &db.matches[m as usize];
        let len = m.data.1;
        c.card32(m.range_start as u32);
        c.card32(m.range_length as u32);
        c.card32(m.word_size as u32);
        c.card32(len);
        c.card32(offset2);
        offset2 = align4(offset2 + len);
        if m.mask.is_some() {
            c.card32(offset2);
            offset2 = align4(offset2 + len);
        } else {
            c.card32(0);
        }
        if m.first_child == NONE {
            c.card32(0);
            c.card32(0);
        } else {
            c.card32(list(m.first_child).count() as u32);
            c.card32(matchlets_offset + 32 * index[m.first_child as usize]);
        }
    }
    for &m in &order {
        let m = &db.matches[m as usize];
        for d in [Some(m.data), m.mask].into_iter().flatten() {
            c.data(db.mdata(d), false);
        }
    }

    // write_namespace_cache
    let namespace_offset = c.offset();
    let keys = sorted_keys(&db.namespace_hash, |_| true);
    c.card32(keys.len() as u32);
    for k in keys {
        let p = k.iter().position(|&b| b == b' ').unwrap();
        c.string(&k[..p])?;
        c.string(&k[p + 1..])?;
        c.string(tn(db.namespace_hash.get(k).unwrap()))?;
    }

    // write_icons_cache
    let mut icon_offsets = [0; 2];
    for (i, hash) in [&db.icon_hash, &db.generic_icon_hash]
        .into_iter()
        .enumerate()
    {
        icon_offsets[i] = c.offset();
        let keys = sorted_keys(hash, |_| true);
        c.card32(keys.len() as u32);
        for k in keys {
            c.string(k)?;
            c.string(db.sp(hash.get(k).unwrap()))?;
        }
    }

    // write_types_cache
    let type_offset = c.offset();
    let names = sorted_keys(&db.type_hash, |_| true);
    c.card32(names.len() as u32);
    let mut line = Vec::with_capacity(256);
    for n in names {
        // update-mime-database looks up the lines of `types`, with their newline: 0 unless a
        // string happens to end with one
        line.clear();
        line.extend_from_slice(n);
        line.push(b'\n');
        let o = c.strings.get(&line).unwrap_or(0);
        c.card32(o);
    }

    // write_header
    let offsets = [
        alias_offset,
        parent_offset,
        literal_offset,
        suffix_offset,
        glob_offset,
        magic_offset,
        namespace_offset,
        icon_offsets[0],
        icon_offsets[1],
        type_offset,
    ];
    let header = &mut c.out[base..base + 44];
    header[0..2].copy_from_slice(&1u16.to_be_bytes());
    header[2..4].copy_from_slice(&2u16.to_be_bytes());
    for (i, o) in offsets.iter().enumerate() {
        header[4 + 4 * i..8 + 4 * i].copy_from_slice(&o.to_be_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strtol_like_c() {
        assert_eq!(strtol(b"50", 10), (50, 2, false));
        assert_eq!(strtol(b" -3x", 10), (-3, 3, false));
        assert_eq!(strtol(b"x", 10), (0, 0, false));
        assert_eq!(strtoul(b"0x1F", 0), (31, 4, false));
        assert_eq!(strtoul(b"017", 0), (15, 3, false));
        assert_eq!(strtoul(b"0x", 0), (0, 1, false));
        assert_eq!(strtoul(b"-1", 0), (u64::MAX, 2, false));
        assert!(strtol(b"99999999999999999999", 10).2);
    }

    #[test]
    fn escapes() {
        let mut out = Vec::new();
        getstr(br"a\x41\101\n\q\", &mut out);
        assert_eq!(out, b"aAA\nq");
        out.clear();
        getstr(br"\xg", &mut out);
        assert_eq!(out, b"xg");
    }

    #[test]
    fn globs() {
        assert!(glob_type(b"*.txt") == GlobType::Simple);
        assert!(glob_type(b"README") == GlobType::Literal);
        assert!(glob_type(b"*.[ch]") == GlobType::Full);
        assert!(glob_type(b"a*") == GlobType::Full);
    }
}
