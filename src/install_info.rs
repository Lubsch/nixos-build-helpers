//! Replacement for the loop of nixos/modules/misc/documentation.nix that runs texinfo's
//! install-info once per Info file in system.path (one process each, ~1.7 ms):
//!
//!     nixos-build-helpers install-info INSTALL-INFO DIR-FILE INFO-FILE...
//!
//! does what `for f in INFO-FILE...; do INSTALL-INFO "$f" DIR-FILE; done` does, in one process.
//! It's a port of what install-info.c (texinfo 7.2) does without options, line by line, quirks
//! included, so that the dir file is the same byte for byte. Whatever it doesn't reproduce runs
//! INSTALL-INFO itself, on the dir file as made so far: compressed or very short Info files,
//! input install-info would reject (it then fails the same way), NUL bytes, an existing dir file,
//! a locale or INSTALL_INFO_NO_INDENT in the environment, and entries whose order would depend on
//! glibc's qsort.

use std::ffi::{OsStr, OsString, c_char};
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail};

/// What ensure_dirfile_exists() writes, untranslated (the build has no locale).
const DIR_TEMPLATE: &[u8] = concat!(
    "This is the file .../info/dir, which contains the\n",
    "topmost node of the Info hierarchy, called (dir)Top.\n",
    "The first time you invoke Info you start off looking at this node.\n",
    "\x1f\n",
    "File: dir,\tNode: Top\tThis is the top of the INFO tree\n",
    "\n",
    "  This (the Directory node) gives a menu of major topics.\n",
    "  Typing \"q\" exits, \"H\" lists all Info commands, \"d\" returns here,\n",
    "  \"h\" gives a primer for first-timers,\n",
    "  \"mEmacs<Return>\" visits the Emacs manual, etc.\n",
    "\n",
    "  In Emacs, you can click mouse button 2 on a menu item or cross reference\n",
    "  to select it.\n",
    "\n",
    "* Menu:\n",
)
.as_bytes();

const TAB_WIDTH: usize = 8;

pub fn run(args: Vec<OsString>) -> Result<()> {
    let [install_info, dir_file, files @ ..] = &args[..] else {
        bail!("usage: install-info INSTALL-INFO DIR-FILE INFO-FILE...");
    };
    let real = Real {
        install_info,
        dir_file: Path::new(dir_file),
    };
    if files.is_empty() {
        return Ok(());
    }

    // The dir file as install-info would have written it so far.
    let mut dir: Option<Vec<u8>> = None;
    let whole_loop = fs::symlink_metadata(real.dir_file).is_ok() || !plain_environment();
    for file in files {
        let port = if whole_loop {
            None
        } else {
            let dir_data = dir.get_or_insert_with(|| DIR_TEMPLATE.to_vec());
            install(dir_data, file)
        };
        match port {
            Some(Outcome::Updated(new)) => dir = Some(new),
            Some(Outcome::NoEntries) => {
                eprintln!(
                    "install-info: warning: no info dir entry in `{}'",
                    file.to_string_lossy()
                )
            }
            None => {
                if std::env::var_os("NIXOS_BUILD_HELPERS_DEBUG").is_some() {
                    eprintln!("install-info: running {:?} for {file:?}", real.install_info);
                }
                dir = Some(real.install(dir.as_deref(), file)?)
            }
        }
    }
    if let Some(dir) = dir {
        fs::write(real.dir_file, dir).with_context(|| format!("writing {:?}", real.dir_file))?;
    }
    Ok(())
}

/// install-info itself, for a file the port doesn't handle.
struct Real<'a> {
    install_info: &'a OsStr,
    dir_file: &'a Path,
}

impl Real<'_> {
    fn install(&self, dir: Option<&[u8]>, file: &OsStr) -> Result<Vec<u8>> {
        if let Some(dir) = dir {
            fs::write(self.dir_file, dir)?;
        }
        let status = Command::new(self.install_info)
            .arg(file)
            .arg(self.dir_file)
            .status()
            .with_context(|| format!("running {:?}", self.install_info))?;
        if !status.success() {
            // Like the loop with `set -e`: install-info has printed why. Exit as bash reports it.
            use std::os::unix::process::ExitStatusExt as _;
            std::process::exit(status.code().unwrap_or(128 + status.signal().unwrap_or(0)));
        }
        fs::read(self.dir_file).with_context(|| format!("reading {:?}", self.dir_file))
    }
}

/// Without a locale (install-info calls setlocale(LC_ALL, "")) or install-info's own variable.
fn plain_environment() -> bool {
    let set =
        |v: &str| std::env::var_os(v).is_some_and(|x| !x.is_empty() && x != "C" && x != "POSIX");
    ![
        "LC_ALL",
        "LC_CTYPE",
        "LC_COLLATE",
        "LC_MESSAGES",
        "LANG",
        "LANGUAGE",
    ]
    .iter()
    .any(|v| set(v))
        && std::env::var_os("INSTALL_INFO_NO_INDENT").is_none()
}

enum Outcome {
    Updated(Vec<u8>),
    /// "no info dir entry": install-info exits without writing the dir file.
    NoEntries,
}

/// One run of install-info: `None` where the port doesn't reproduce it.
fn install(dir_data: &[u8], file: &OsStr) -> Option<Outcome> {
    let input = fs::read(file).ok()?;
    if is_compressed(&input)? {
        return None;
    }
    let input = CText::without_lines(input);
    let (sections, mut entries) = parse_input(&input)?;
    if entries.is_empty() {
        return Some(Outcome::NoEntries);
    }
    for e in &mut entries {
        reformat_entry(e, &sections)?;
    }

    let dir = CText::new(dir_data.to_vec());
    let mut state = DirState::new(&dir);
    state.add_entries(sections, &entries)?;
    Some(Outcome::Updated(state.output(&entries)?))
}

/// open_possibly_compressed_file()'s check of the first 13 bytes: `None` if there are fewer
/// (install-info then fails).
fn is_compressed(d: &[u8]) -> Option<bool> {
    if d.len() < 13 {
        return None;
    }
    Some(
        d.starts_with(b"\x1f\x8b")
            || d.starts_with(b"\xfd7zXZ\0")
            || d.starts_with(b"BZh")
            || d.starts_with(b"BZ0")
            || d.starts_with(b"LZIP\x01")
            || d[9..13] == [0; 4]
            || d[5..13] == [0xff; 8]
            || d.starts_with(b"\xffLZMA\0"),
    )
}

/// A file's data with a terminating NUL, as readfile() returns it, and its lines (findlines()).
struct CText {
    data: Vec<u8>,
    lines: Vec<Line>,
}

#[derive(Clone, Copy)]
struct Line {
    start: usize,
    size: usize,
}

/// findlines(): the lines of `data`, the last one without a newline if it has none.
fn lines_of(data: &[u8]) -> impl Iterator<Item = Line> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= data.len() {
            return None;
        }
        let size = data[start..]
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(data.len() - start);
        let line = Line { start, size };
        start += size + 1;
        Some(line)
    })
}

impl CText {
    fn new(data: Vec<u8>) -> Self {
        let mut t = Self::without_lines(data);
        t.lines = lines_of(&t.data[..t.data.len() - 1]).collect();
        t
    }

    /// For an Info file, whose lines parse_input() goes through once.
    fn without_lines(mut data: Vec<u8>) -> Self {
        data.push(0);
        Self {
            data,
            lines: Vec::new(),
        }
    }

    fn at(&self, i: usize) -> u8 {
        self.data.get(i).copied().unwrap_or(0)
    }

    fn line(&self, l: Line) -> &[u8] {
        &self.data[l.start..l.start + l.size]
    }

    /// `!strncmp(lit, data + pos, n)`
    fn strncmp_eq(&self, lit: &[u8], pos: usize, n: usize) -> bool {
        for k in 0..n {
            let a = lit.get(k).copied().unwrap_or(0);
            if a != self.at(pos + k) {
                return false;
            }
            if a == 0 {
                return true;
            }
        }
        true
    }

    /// The start of line i + 1, or None for the NULL past the last line.
    fn next_start(&self, i: usize) -> Option<usize> {
        self.lines.get(i + 1).map(|l| l.start)
    }

    /// The start of the line after `l`, or None past the last line.
    fn start_after(&self, l: Line) -> Option<usize> {
        let next = l.start + l.size + 1;
        (next < self.data.len() - 1).then_some(next)
    }
}

/// A section an entry goes to (struct spec_section), in a list linked through `next`.
struct SpecSection {
    name: Vec<u8>,
    next: Option<usize>,
    missing: bool,
}

/// The sections of an Info file, as parse_input() leaves them.
struct Sections {
    all: Vec<SpecSection>,
    head: Option<usize>,
}

impl Sections {
    /// The sections from `head` up to (excluding) `tail`.
    fn chain(&self, head: Option<usize>, tail: Option<usize>) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(head, |&s| self.all[s].next).take_while(move |&s| Some(s) != tail)
    }
}

struct Entry {
    text: Vec<u8>,
    sections_head: Option<usize>,
    sections_tail: Option<usize>,
}

impl Entry {
    fn wants(&self, sections: &Sections, name: &[u8]) -> bool {
        sections
            .chain(self.sections_head, self.sections_tail)
            .any(|s| sections.all[s].name == name)
    }
}

/// parse_input() without options. Entries in file order (install-info's list is reversed).
fn parse_input(t: &CText) -> Option<(Sections, Vec<Entry>)> {
    const SECTION: &[u8] = b"INFO-DIR-SECTION ";
    let mut s = Sections {
        all: Vec::new(),
        head: None,
    };
    let mut entries = Vec::new();
    let (mut head, mut tail, mut reset_tail) = (None, None, false);
    let mut start_of_entry: Option<usize> = None;
    for l in lines_of(&t.data[..t.data.len() - 1]) {
        let line = t.line(l);
        if t.strncmp_eq(SECTION, l.start, SECTION.len()) {
            let name = t.data[l.start + SECTION.len()..l.start + l.size].to_vec();
            if name.contains(&0) {
                return None;
            }
            s.all.push(SpecSection {
                name,
                next: s.head,
                missing: true,
            });
            if reset_tail {
                tail = s.head;
                reset_tail = false;
            }
            s.head = Some(s.all.len() - 1);
            head = s.head;
        } else if line == b"START-INFO-DIR-ENTRY" {
            if s.head.is_none() {
                s.all.push(SpecSection {
                    name: b"Miscellaneous".to_vec(),
                    next: None,
                    missing: true,
                });
                s.head = Some(s.all.len() - 1);
                head = s.head;
            }
            reset_tail = true;
            if start_of_entry.is_some() {
                return None; // fatal: START-INFO-DIR-ENTRY without matching END-INFO-DIR-ENTRY
            }
            start_of_entry = t.start_after(l);
        } else if let Some(start) = start_of_entry {
            let end = line == b"END-INFO-DIR-ENTRY";
            if (t.strncmp_eq(b"* ", l.start, 2) && l.start > start) || end {
                let text = t.data[start..l.start].to_vec();
                if text.contains(&0) {
                    return None;
                }
                entries.push(Entry {
                    text,
                    sections_head: head,
                    sections_tail: tail,
                });
                start_of_entry = if end { None } else { Some(l.start) };
            }
        }
    }
    if start_of_entry.is_some() {
        return None; // fatal
    }
    if s.head.is_none() {
        // main(): no sections at all: "Miscellaneous"
        s.all.push(SpecSection {
            name: b"Miscellaneous".to_vec(),
            next: None,
            missing: true,
        });
        s.head = Some(0);
    }
    Some((s, entries))
}

fn c_isspace(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// reformat_new_entries() with the default alignment and width, for one entry. `None` where
/// install-info overflows its buffer (it's built with _FORTIFY_SOURCE, which aborts).
fn reformat_entry(e: &mut Entry, sections: &Sections) -> Option<()> {
    let Some((name, desc)) = split_entry(&e.text) else {
        return Some(()); // format_entry() leaves it alone without a description
    };
    let utilities = sections
        .chain(e.sections_head, e.sections_tail)
        .any(|s| sections.all[s].name == b"Individual utilities");
    let (calign, align, width) = if utilities {
        (49, 51, 79)
    } else {
        (33, 35, 79)
    };
    let text = format_entry(name, &desc, calign, align, width);
    // format_entry()'s xmalloc, which it fills with strcat(), and the terminating NUL
    let allocated = width + (desc.len() + width) / (width - align) * width * 2;
    if text.len() + 1 > allocated {
        return None;
    }
    e.text = text;
    Some(())
}

/// split_entry(): the name (up to the first period followed by whitespace) and the description.
/// `None` where it finds no description.
fn split_entry(entry: &[u8]) -> Option<(&[u8], Vec<u8>)> {
    let period = (0..entry.len())
        .find(|&i| entry[i] == b'.' && matches!(entry.get(i + 1), Some(b' ' | b'\t' | b'\n')))?;
    let name = &entry[..=period];
    let mut desc = Vec::new();
    let mut p = period + 1;
    while p < entry.len() {
        while p < entry.len() && c_isspace(entry[p]) {
            p += 1;
        }
        if p == entry.len() {
            continue;
        }
        if let Some(nl) = entry[p..].iter().position(|&c| c == b'\n').map(|n| p + n) {
            let length = nl - p;
            desc.extend_from_slice(&entry[p..nl]);
            p = nl + 1;
            if length > 1 && p < entry.len() {
                let c = entry[nl - 1];
                if c == b'.' {
                    desc.extend_from_slice(b"  ");
                } else if !c_isspace(c) {
                    desc.push(b' ');
                }
            }
        } else {
            desc.extend_from_slice(&entry[p..]);
            p = entry.len();
        }
    }
    desc.push(b'\n');
    Some((name, desc))
}

fn adjust_column(column: usize, c: u8) -> usize {
    match c {
        0x08 => column.saturating_sub(1),
        b'\r' => 0,
        b'\t' => column + TAB_WIDTH - column % TAB_WIDTH,
        _ => column + 1,
    }
}

/// format_entry(), quirks included: the indentation of continuation lines doesn't count
/// towards the width.
fn format_entry(name: &[u8], desc: &[u8], calign: usize, align: usize, width: usize) -> Vec<u8> {
    let mut out = name.to_vec();
    let mut column = name.len();
    let desc_len = desc.len();
    if name.len() > calign - 2 {
        if desc_len > 1 {
            out.push(b'\n');
            out.resize(out.len() + calign - 1, b' ');
            column = calign - 1;
        }
    } else {
        for _ in 0..calign - name.len() - 1 {
            if desc_len <= 2 {
                break;
            }
            column = adjust_column(column, b' ');
            out.push(b' ');
        }
    }

    let mut line_out: Vec<u8> = Vec::new();
    if desc_len > 2 {
        for &c in desc {
            if c == b'\n' {
                line_out.push(c);
                out.append(&mut line_out);
                column = 0;
                continue;
            }
            loop {
                // rescan:
                column = adjust_column(column, c);
                if column <= width {
                    line_out.push(c);
                    break;
                }
                if let Some(blank) = line_out.iter().rposition(|&b| b == b' ' || b == b'\t') {
                    let logical_end = blank + 1;
                    out.extend_from_slice(&line_out[..logical_end]);
                    out.push(b'\n');
                    out.resize(out.len() + align - 1, b' ');
                    line_out.drain(..logical_end);
                    column = line_out.iter().fold(0, |col, &b| adjust_column(col, b));
                    continue;
                }
                if line_out.is_empty() {
                    line_out.push(c);
                    break;
                }
                line_out.push(b'\n');
                out.append(&mut line_out);
                column = 0;
            }
        }
    }
    if desc_len <= 2 {
        out.push(b'\n');
    }
    out.append(&mut line_out);
    out
}

/// A node of the dir file (struct node), in file order (install-info's list is reversed).
struct Node {
    name: Option<Vec<u8>>,
    end_line: usize,
    menu_start: Option<usize>,
    sections: Vec<MenuSection>,
}

struct MenuSection {
    name: Vec<u8>,
    /// One-based: the line after the section's title
    start_line: usize,
    end_line: usize,
}

/// The dir file being updated: its lines, nodes, and what to add before each line.
struct DirState<'a> {
    dir: &'a CText,
    nodes: Vec<Node>,
    delete: Vec<bool>,
    /// Per line, including the one past the last: entry indices (a vector of
    /// `n_entries_to_add` slots), and sections
    add_entries_before: Vec<Option<Vec<Option<usize>>>>,
    add_sections_before: Vec<Vec<usize>>,
    sections: Option<Sections>,
}

impl<'a> DirState<'a> {
    fn new(dir: &'a CText) -> Self {
        let n = dir.lines.len();
        Self {
            nodes: parse_dir_file(dir),
            dir,
            delete: vec![false; n + 1],
            add_entries_before: vec![None; n + 1],
            add_sections_before: vec![Vec::new(); n + 1],
            sections: None,
        }
    }

    fn first_byte(&self, i: usize) -> u8 {
        self.dir.lines.get(i).map_or(0, |l| self.dir.at(l.start))
    }

    fn line(&self, i: usize) -> &'a [u8] {
        self.dir.line(self.dir.lines[i])
    }

    /// main()'s two loops that decide where entries and new sections go.
    fn add_entries(&mut self, mut sections: Sections, entries: &[Entry]) -> Option<()> {
        let n_entries = entries.len();
        // Entries in install-info's list order
        let order: Vec<usize> = (0..n_entries).rev().collect();
        for ni in (0..self.nodes.len()).rev() {
            for si in 0..self.nodes[ni].sections.len() {
                let (start_line, name) = {
                    let s = &self.nodes[ni].sections[si];
                    (s.start_line, s.name.clone())
                };
                let mut i = self.nodes[ni].sections[si].end_line;
                while i > start_line && self.dir.lines[i - 1].size == 0 {
                    i -= 1;
                }
                self.nodes[ni].sections[si].end_line = i;
                let end_line = i;

                let Some(spec) = sections
                    .chain(sections.head, None)
                    .find(|&s| sections.all[s].name == name)
                else {
                    continue;
                };
                sections.all[spec].missing = false;
                let mut add_at_line = end_line;
                for &e in &order {
                    let entry = &entries[e];
                    if !entry.wants(&sections, &name) {
                        continue;
                    }
                    for i in (start_line - 1..end_line).rev() {
                        if self.first_byte(i) == b'*'
                            && menu_line_equal(&entry.text, self.line(i))
                            && !self.delete[i]
                        {
                            self.delete[i] = true;
                            for j in i + 1..end_line {
                                if self.first_byte(j) == b'*' {
                                    break;
                                }
                                self.delete[j] = true;
                            }
                        }
                        if self.first_byte(i) == b'*' && menu_line_lessp(&entry.text, self.line(i))
                        {
                            add_at_line = i;
                        }
                    }
                    self.insert_entry_here(e, add_at_line, entries)?;
                }
            }
        }

        // New sections, in the first Top node
        if let Some(top) = self
            .nodes
            .iter()
            .position(|n| n.name.as_deref() == Some(b"Top"))
        {
            let top = &self.nodes[top];
            let specs: Vec<usize> = sections.chain(sections.head, None).collect();
            for spec in specs {
                if !sections.all[spec].missing {
                    continue;
                }
                let mut target = None;
                let mut prev: Option<&MenuSection> = None;
                for section in &top.sections {
                    if mbsncasecmp(&sections.all[spec].name, &section.name, usize::MAX) < 0 {
                        target = Some(match prev {
                            Some(p) => p.end_line,
                            None => top.sections[0].start_line.checked_sub(2)?,
                        });
                        break;
                    }
                    prev = Some(section);
                }
                let target = target.unwrap_or(top.end_line);
                self.add_sections_before[target].push(spec);
            }
        }
        self.sections = Some(sections);
        Some(())
    }

    fn insert_entry_here(&mut self, e: usize, line: usize, entries: &[Entry]) -> Option<()> {
        let n = entries.len();
        let slots = self.add_entries_before[line].get_or_insert_with(|| vec![None; n]);
        let i = (0..n).find(|&i| match slots[i] {
            None => true,
            Some(other) => menu_line_lessp(&entries[e].text, &entries[other].text),
        })?; // install-info aborts
        if slots[i].is_some() {
            slots.pop();
            slots.insert(i, Some(e));
        } else {
            slots[i] = Some(e);
        }
        Some(())
    }

    /// output_dirfile()
    fn output(mut self, entries: &[Entry]) -> Option<Vec<u8>> {
        let mut sections = self.sections.take()?;
        let n_lines = self.dir.lines.len();
        let mut out = Vec::with_capacity(self.dir.data.len() + 4096);
        for i in 0..=n_lines {
            if let Some(slots) = &self.add_entries_before[i] {
                for e in slots.iter().map_while(|s| *s) {
                    out.extend_from_slice(&entries[e].text);
                }
            }
            if !self.add_sections_before[i].is_empty() {
                let mut specs = std::mem::take(&mut self.add_sections_before[i]);
                specs.sort_by(|&a, &b| sections.all[a].name.cmp(&sections.all[b].name));
                let sorted = sort_entries(entries, &sections)?;
                for spec in specs {
                    if !sections.all[spec].missing {
                        continue;
                    }
                    if i >= 1 && self.dir.lines[i - 1].size > 0 {
                        out.push(b'\n');
                    }
                    out.extend_from_slice(&sections.all[spec].name);
                    out.push(b'\n');
                    sections.all[spec].missing = false;
                    let name = sections.all[spec].name.clone();
                    for &e in &sorted {
                        if entries[e].wants(&sections, &name) {
                            out.extend_from_slice(&entries[e].text);
                        }
                    }
                }
            }
            if i < n_lines && !self.delete[i] {
                out.extend_from_slice(self.line(i));
                out.push(b'\n');
            }
        }
        Some(out)
    }
}

/// parse_dir_file()
fn parse_dir_file(t: &CText) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut node_header = false;
    for (i, &l) in t.lines.iter().enumerate() {
        if node_header {
            let mut j = 0;
            while j < l.size {
                if t.strncmp_eq(b"Node:", l.start + j, 5) {
                    j += 5;
                    while matches!(t.at(l.start + j), b' ' | b'\t') {
                        j += 1;
                    }
                    let mut end = j;
                    while !matches!(t.at(l.start + end), 0 | b',' | b'\n' | b'\t') {
                        end += 1;
                    }
                    if let Some(node) = nodes.last_mut() {
                        node.name = Some(t.data[l.start + j..l.start + end].to_vec());
                    }
                }
                j += 1;
            }
            node_header = false;
        }

        let first = t.at(l.start);
        if first == 0x1f {
            if let Some(node) = nodes.last_mut() {
                node.end_line = i;
                if let Some(s) = node.sections.last_mut() {
                    s.end_line = i;
                }
            }
            nodes.push(Node {
                name: None,
                end_line: 0,
                menu_start: None,
                sections: Vec::new(),
            });
            node_header = true;
        }

        if let Some(node) = nodes.last_mut() {
            if t.strncmp_eq(b"* Menu:", l.start, 7) {
                node.menu_start = t.next_start(i);
            }
            if node.menu_start.is_some() && !matches!(first, b'\n' | b'*' | b' ' | b'\t') {
                if let Some(s) = node.sections.last_mut() {
                    s.end_line = i;
                }
                node.sections.push(MenuSection {
                    name: t.line(l).to_vec(),
                    start_line: i + 1,
                    end_line: 0,
                });
            }
        }
    }
    let n = t.lines.len();
    if let Some(node) = nodes.last_mut() {
        node.end_line = n;
        if let Some(s) = node.sections.last_mut() {
            s.end_line = n;
        }
    }
    nodes
}

/// `char` comparisons, signed where C's char is.
fn cc(b: u8) -> c_char {
    b as c_char
}

/// menu_line_lessp(): whether the item name of `a` is less than that of `b`.
fn menu_line_lessp(a: &[u8], b: &[u8]) -> bool {
    for (&x, &y) in a.iter().zip(b) {
        if x == b':' && y != b':' {
            return true;
        }
        if y == b':' && x != b':' {
            return false;
        }
        if cc(x) < cc(y) {
            return true;
        }
        if cc(x) > cc(y) {
            return false;
        }
    }
    false
}

/// menu_line_equal()
fn menu_line_equal(a: &[u8], b: &[u8]) -> bool {
    for (&x, &y) in a.iter().zip(b) {
        if x == b':' && y == b':' {
            return true;
        }
        if x != y {
            return false;
        }
    }
    true
}

/// mbsncasecmp() in the C locale (mbscasecmp() with n = usize::MAX)
fn mbsncasecmp(a: &[u8], b: &[u8], n: usize) -> i32 {
    for k in 0..n {
        let x = a.get(k).copied().unwrap_or(0).to_ascii_lowercase();
        let y = b.get(k).copied().unwrap_or(0).to_ascii_lowercase();
        if x != y {
            return x as i32 - y as i32;
        }
        if x == 0 {
            return 0;
        }
    }
    0
}

/// The entries sorted with compare_entries_text(). That isn't consistent for two names equal but
/// for case, of the same length (each is less than the other), so qsort decides their order.
/// Every other entry compares the same with both, and a new section prints only its own entries,
/// so their order only matters when both go to one section: `None` then.
fn sort_entries(entries: &[Entry], sections: &Sections) -> Option<Vec<usize>> {
    let key = |e: usize| {
        let t = &entries[e].text;
        t.iter().position(|&c| c == b':').unwrap_or(t.len())
    };
    let compare = |a: usize, b: usize| {
        let (la, lb) = (key(a), key(b));
        match mbsncasecmp(&entries[a].text, &entries[b].text, la.min(lb)) {
            0 if la <= lb => -1,
            0 => 1,
            r => r,
        }
    };
    let n = entries.len();
    for a in 0..n {
        for b in a + 1..n {
            if compare(a, b) < 0
                && compare(b, a) < 0
                && sections
                    .chain(entries[a].sections_head, entries[a].sections_tail)
                    .any(|s| entries[b].wants(sections, &sections.all[s].name))
            {
                return None;
            }
        }
    }
    let mut order: Vec<usize> = (0..n).rev().collect();
    order.sort_by(|&a, &b| compare(a, b).cmp(&0));
    Some(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let t = CText::new(b"a\n\nbc".to_vec());
        let l: Vec<_> = t.lines.iter().map(|l| t.line(*l).to_vec()).collect();
        assert_eq!(l, [b"a".to_vec(), b"".to_vec(), b"bc".to_vec()]);
        assert_eq!(CText::new(b"a\n".to_vec()).lines.len(), 1);
    }

    #[test]
    fn split() {
        let (name, desc) =
            split_entry(b"* Sed: (sed).                   Stream EDitor.\n").unwrap();
        assert_eq!(name, b"* Sed: (sed).");
        assert_eq!(desc, b"Stream EDitor.\n");
        assert!(split_entry(b"* No description: (x)\n").is_none());
        let (_, desc) = split_entry(b"* A: (a). One.\n  Two\n  three.\n").unwrap();
        assert_eq!(desc, b"One.  Two three.\n");
    }

    #[test]
    fn format() {
        let e = format_entry(b"* Sed: (sed).", b"Stream EDitor.\n", 33, 35, 79);
        assert_eq!(e, b"* Sed: (sed).                   Stream EDitor.\n");
    }
}
