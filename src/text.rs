//! Markdown rendering of AX attributed text, matching how the original shows text values.
use crate::ax::{self, Cf};
use core_foundation_sys::attributed_string::*;
use core_foundation_sys::base::{CFRange, CFTypeRef};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use std::ffi::c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCopyParameterizedAttributeValue(
        e: ax::AXRef,
        a: CFTypeRef,
        p: CFTypeRef,
        v: *mut CFTypeRef,
    ) -> i32;
    fn AXValueCreate(t: u32, v: *const c_void) -> CFTypeRef;
}

/// Markdown text plus, for every visible character, (byte offset in `text`, UTF-16 offset in the plain string).
pub struct Markdown {
    pub text: String,
    map: Vec<(usize, usize)>,
}

#[derive(PartialEq, Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    link: Option<String>,
}

fn style(attrs: CFDictionaryRef) -> Style {
    let get = |k: &str| {
        let v = unsafe { CFDictionaryGetValue(attrs, ax::cfstr(k).0) };
        (!v.is_null()).then_some(v)
    };
    let font = get("AXFont")
        .map(|f| unsafe { CFDictionaryGetValue(f as _, ax::cfstr("AXFontName").0) })
        .filter(|n| !n.is_null())
        .and_then(ax::to_string)
        .unwrap_or_default();
    let on = |k: &str| get(k).and_then(ax::to_string).is_some_and(|v| v != "0" && v != "off");
    Style {
        bold: ["Bold", "Black", "Heavy", "Semibold"]
            .iter()
            .any(|w| font.contains(w)),
        italic: font.contains("Italic") || font.contains("Oblique"),
        underline: on("AXUnderline"),
        strike: on("AXStrikethrough"),
        link: get("AXLink").and_then(|l| ax::attr_str(l, "AXURL")),
    }
}

/// Plain text as Markdown with an identity offset map (for elements without attributes).
pub fn markdown_of(text: String) -> Markdown {
    let mut map = vec![];
    let mut u = 0;
    for (b, c) in text.char_indices() {
        map.push((b, u));
        u += c.len_utf16();
    }
    Markdown { text, map }
}

pub fn markdown(e: ax::AXRef) -> Option<Markdown> {
    let n: isize = ax::attr_str(e, "AXNumberOfCharacters")?.parse().ok()?;
    let range = CFRange {
        location: 0,
        length: n,
    };
    let param = Cf(unsafe { AXValueCreate(4, &range as *const _ as _) });
    let mut v: CFTypeRef = std::ptr::null();
    let key = ax::cfstr("AXAttributedStringForRange");
    if unsafe { AXUIElementCopyParameterizedAttributeValue(e, key.0, param.0, &mut v) } != 0
        || v.is_null()
    {
        return ax::attr_str(e, "AXValue").map(markdown_of);
    }
    let s = Cf(v);
    let units: Vec<u16> = ax::to_string(unsafe { CFAttributedStringGetString(s.0 as _) } as _)
        .unwrap_or_default()
        .encode_utf16()
        .collect();
    // Runs of (style, plain UTF-16 start, end), with equal neighbours merged.
    let mut runs: Vec<(Style, usize, usize)> = vec![];
    let mut loc = 0;
    while loc < units.len() {
        let mut r = CFRange {
            location: 0,
            length: 0,
        };
        let attrs = unsafe { CFAttributedStringGetAttributes(s.0 as _, loc as isize, &mut r) };
        let st = style(attrs);
        let end = ((r.location + r.length) as usize).clamp(loc + 1, units.len());
        match runs.last_mut() {
            Some(last) if last.0 == st => last.2 = end,
            _ => runs.push((st, loc, end)),
        }
        loc = end;
    }
    let mut md = Markdown {
        text: String::new(),
        map: vec![],
    };
    for (st, start, end) in runs {
        let text = String::from_utf16_lossy(&units[start..end]);
        // Markers must hug the text, so surrounding whitespace stays outside them.
        let core = text.trim();
        let lead = text.len() - text.trim_start().len();
        let (open, close) = markers(&st);
        let emit = |md: &mut Markdown, s: &str, mut u: usize| {
            for (b, c) in s.char_indices() {
                md.map.push((md.text.len() + b, u));
                u += c.len_utf16();
            }
            md.text.push_str(s);
        };
        if core.is_empty() || open.is_empty() {
            emit(&mut md, &text, start);
            continue;
        }
        emit(&mut md, &text[..lead], start);
        md.text.push_str(&open);
        emit(&mut md, core, start + text[..lead].encode_utf16().count());
        md.text.push_str(&close);
        let tail = &text[lead + core.len()..];
        emit(&mut md, tail, end - tail.encode_utf16().count());
    }
    Some(md)
}

fn markers(st: &Style) -> (String, String) {
    let em = match (st.bold, st.italic) {
        (true, true) => "***",
        (true, false) => "**",
        (false, true) => "*",
        _ => "",
    };
    let (mut open, mut close) = (em.to_string(), em.to_string());
    if let Some(url) = &st.link {
        open.insert(0, '[');
        close += &format!("]({})", ax::short_url(url));
    }
    if st.strike {
        (open, close) = (format!("~~{open}"), format!("{close}~~"));
    }
    if st.underline {
        (open, close) = (format!("<u>{open}"), format!("{close}</u>"));
    }
    (open, close)
}

impl Markdown {
    /// Plain UTF-16 range covered by Markdown bytes `start..end`, skipping formatting markers.
    pub fn plain_range(&self, start: usize, end: usize) -> (usize, usize) {
        let first = self.map.iter().find(|(b, _)| *b >= start);
        let last = self.map.iter().rev().find(|(b, _)| *b < end);
        let total = self.map.last().map_or(0, |&(b, u)| {
            u + self.text[b..].chars().next().map_or(0, char::len_utf16)
        });
        let lo = first.map_or(total, |m| m.1);
        let hi = last.map_or(lo, |&(b, u)| {
            u + self.text[b..].chars().next().map_or(0, char::len_utf16)
        });
        (lo, hi.max(lo))
    }
}
