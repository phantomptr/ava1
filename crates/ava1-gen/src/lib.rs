//! Generates the AVA1 codecs from a schema (SPEC.md §3): the protocol's own
//! (`schema/ava1.toml`, the core) and an application's extension schema, whose messages ride
//! in the core's `RpcRequest`/`RpcResponse` bodies with the application's own method numbers.
//!
//! An extension's code builds on the core's: in Rust it is a module of the application's crate
//! (`use ava1::wire::...`); in C it is `ava1_<name>_gen.{h,c}`, which includes `ava1_gen.h`.
//! Constants and types keep the `AVA1_` / `ava1_` prefix either way, so an application's names
//! read like the protocol's.
use serde::Deserialize;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    #[serde(default, rename = "const")]
    pub consts: Vec<Const>,
    #[serde(default, rename = "message")]
    pub messages: Vec<Msg>,
    #[serde(default, rename = "struct")]
    pub structs: Vec<Msg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Const {
    pub name: String,
    pub ty: String,
    pub value: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Msg {
    pub name: String,
    #[serde(default, rename = "type")]
    pub frame_type: Option<u8>,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub ext: Vec<Ext>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    #[serde(default)]
    pub of: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ext {
    pub tag: u16,
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ty {
    U8,
    U16,
    U32,
    U64,
    B16,
    B32,
    Bytes,
    Str,
    Records,
}

/// Rust keywords (strict, reserved, and 2018+): a field with one of these names is
/// emitted as a raw identifier.
const RUST_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn",
    "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let",
    "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return",
    "static", "struct", "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use",
    "virtual", "where", "while", "yield",
];
/// Keywords that cannot be raw identifiers: refused as field names.
const RUST_UNRAWABLE: &[&str] = &["self", "super", "crate"];
/// C keywords (C11) and the names the C runtime's types use: suffixed with `_`.
const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
    "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
    "union", "unsigned", "void", "volatile", "while", "bool", "true", "false", "asm", "errno",
];
/// Type and item names the generated Rust refers to unqualified: a message with one of
/// these names would shadow it.
const RUST_RESERVED_TYPES: &[&str] = &[
    "Self",
    "Option",
    "Some",
    "None",
    "Result",
    "Ok",
    "Err",
    "Vec",
    "String",
    "Box",
    "Default",
    "Message",
    "FrameMessage",
    "Reader",
    "Writer",
    "DecodeError",
    "EncodeError",
    "SplitMix",
];
/// Items the generated Rust defines itself.
const RUST_RESERVED_CONSTS: &[&str] = &["ALL"];

/// A field name as the generated Rust spells it.
fn rid(n: &str) -> String {
    if RUST_KEYWORDS.contains(&n) {
        format!("r#{n}")
    } else {
        n.to_string()
    }
}

/// A field name as the generated C spells it.
fn cid(n: &str) -> String {
    if C_KEYWORDS.contains(&n) {
        format!("{n}_")
    } else {
        n.to_string()
    }
}

fn is_snake(n: &str) -> bool {
    !n.is_empty()
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub fn parse(src: &str) -> Result<Schema, String> {
    let mut s: Schema = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut const_names = HashSet::new();
    for c in &s.consts {
        let max = match c.ty.as_str() {
            "u8" => u64::from(u8::MAX),
            "u16" => u64::from(u16::MAX),
            "u32" => u64::from(u32::MAX),
            "u64" => u64::MAX,
            _ => return Err(format!("const {}: bad ty {}", c.name, c.ty)),
        };
        if c.value > max {
            return Err(format!(
                "const {}: {} does not fit {}",
                c.name, c.value, c.ty
            ));
        }
        let screaming = c
            .name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase())
            && c.name
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_');
        if !screaming {
            return Err(format!("const {}: const names are SCREAMING_SNAKE", c.name));
        }
        // AVA1_TYPE_* is the C namespace of frame types; ALL is generated.
        if c.name.starts_with("TYPE_") || RUST_RESERVED_CONSTS.contains(&c.name.as_str()) {
            return Err(format!("const {}: the name is reserved", c.name));
        }
        if !const_names.insert(c.name.clone()) {
            return Err(format!("const {}: name used twice", c.name));
        }
    }
    let mut names = HashSet::new();
    for m in s.messages.iter().chain(&s.structs) {
        let camel = m
            .name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
            && m.name.chars().all(|c| c.is_ascii_alphanumeric());
        if !camel {
            return Err(format!("{}: message names are CamelCase", m.name));
        }
        if RUST_RESERVED_TYPES.contains(&m.name.as_str()) {
            return Err(format!(
                "{}: the name would shadow a type the generated code uses",
                m.name
            ));
        }
        if !names.insert(m.name.clone()) {
            return Err(format!("{}: name used twice", m.name));
        }
        // Two messages may differ in Rust yet collide in C (`FooBar` / `Foo_bar` cannot
        // happen with CamelCase, but `AB` and `A_b`-style splits can): compare C names.
        if !names.insert(format!("c:{}", snake(&m.name))) {
            return Err(format!(
                "{}: its C name ava1_{} is used twice",
                m.name,
                snake(&m.name)
            ));
        }
        let mut fields = HashSet::new();
        for n in m
            .fields
            .iter()
            .map(|f| &f.name)
            .chain(m.ext.iter().map(|e| &e.name))
        {
            if !is_snake(n) {
                return Err(format!("{}.{n}: field names are snake_case", m.name));
            }
            if RUST_UNRAWABLE.contains(&n.as_str()) {
                return Err(format!("{}.{n}: not usable as a field name", m.name));
            }
            if !fields.insert(n.clone()) {
                return Err(format!("{}.{n}: field used twice", m.name));
            }
        }
        // Names the C struct adds next to the declared ones must not collide with them.
        let mut c_names: HashSet<String> = HashSet::new();
        let mut c_add = |n: String| -> Result<(), String> {
            if n == "unused_" || !c_names.insert(n.clone()) {
                return Err(format!(
                    "{}: C member `{n}` would be declared twice",
                    m.name
                ));
            }
            Ok(())
        };
        for (n, ty) in m
            .fields
            .iter()
            .map(|f| (&f.name, f.ty))
            .chain(m.ext.iter().map(|e| (&e.name, e.ty)))
        {
            c_add(cid(n))?;
            if matches!(ty, Ty::Bytes | Ty::Str) {
                c_add(format!("{}_len", cid(n)))?;
            }
            if matches!(ty, Ty::Records) {
                c_add(format!("{}_len", cid(n)))?;
                c_add(format!("{}_count", cid(n)))?;
            }
        }
        for e in &m.ext {
            c_add(format!("has_{}", e.name))?;
        }
        let mut tags = HashSet::new();
        for e in &m.ext {
            if e.tag == 0 || !tags.insert(e.tag) {
                return Err(format!("{}: ext tag {} is 0 or repeated", m.name, e.tag));
            }
        }
    }
    let struct_names: HashSet<&str> = s.structs.iter().map(|st| st.name.as_str()).collect();
    for m in s.messages.iter().chain(&s.structs) {
        for f in &m.fields {
            match (f.ty, f.of.as_deref()) {
                (Ty::Records, Some(of)) if of == m.name => {
                    return Err(format!(
                        "{}.{}: records cannot list its own type",
                        m.name, f.name
                    ))
                }
                (Ty::Records, Some(of)) if struct_names.contains(of) => {}
                (Ty::Records, Some(of)) => {
                    return Err(format!(
                        "{}.{}: records of unknown struct {of}",
                        m.name, f.name
                    ))
                }
                (Ty::Records, None) => {
                    return Err(format!("{}.{}: records needs `of`", m.name, f.name))
                }
                (_, Some(_)) => {
                    return Err(format!("{}.{}: `of` is only for records", m.name, f.name))
                }
                (_, None) => {}
            }
        }
        if let Some(e) = m.ext.iter().find(|e| e.ty == Ty::Records) {
            return Err(format!(
                "{}.{}: records cannot be an extension",
                m.name, e.name
            ));
        }
    }
    let mut types = HashSet::new();
    for m in &s.messages {
        let t = m
            .frame_type
            .ok_or_else(|| format!("{}: a message needs a type", m.name))?;
        if t >= 0x80 {
            return Err(format!(
                "{}: type {t:#04x} is in the reserved range",
                m.name
            ));
        }
        if !types.insert(t) {
            return Err(format!("{}: type {t:#04x} is used twice", m.name));
        }
    }
    if let Some(st) = s.structs.iter().find(|st| st.frame_type.is_some()) {
        return Err(format!("{}: a struct has no frame type", st.name));
    }
    for m in s.messages.iter_mut().chain(s.structs.iter_mut()) {
        m.ext.sort_by_key(|e| e.tag);
    }
    Ok(s)
}

fn rust_ty(t: Ty, of: Option<&str>) -> String {
    match t {
        Ty::U8 => "u8".into(),
        Ty::U16 => "u16".into(),
        Ty::U32 => "u32".into(),
        Ty::U64 => "u64".into(),
        Ty::B16 => "[u8; 16]".into(),
        Ty::B32 => "[u8; 32]".into(),
        Ty::Bytes => "Vec<u8>".into(),
        Ty::Str => "String".into(),
        Ty::Records => format!("Vec<{}>", of.unwrap_or("()")),
    }
}

fn rust_put(t: Ty, e: &str) -> String {
    match t {
        Ty::U8 => format!("w.u8({e});"),
        Ty::U16 => format!("w.u16({e});"),
        Ty::U32 => format!("w.u32({e});"),
        Ty::U64 => format!("w.u64({e});"),
        Ty::B16 | Ty::B32 => format!("w.fixed(&{e});"),
        Ty::Bytes => format!("w.bytes(&{e})?;"),
        Ty::Str => format!("w.str(&{e})?;"),
        Ty::Records => format!("w.records(&{e})?;"),
    }
}

fn rust_put_ext(t: Ty) -> &'static str {
    match t {
        Ty::U8 => "w.u8(*v); Ok(())",
        Ty::U16 => "w.u16(*v); Ok(())",
        Ty::U32 => "w.u32(*v); Ok(())",
        Ty::U64 => "w.u64(*v); Ok(())",
        Ty::B16 | Ty::B32 => "w.fixed(v); Ok(())",
        Ty::Bytes => "w.bytes(v)",
        Ty::Str => "w.str(v)",
        Ty::Records => "unreachable!()",
    }
}

fn rust_get(t: Ty, r: &str, of: Option<&str>) -> String {
    match t {
        Ty::U8 => format!("{r}.u8()?"),
        Ty::U16 => format!("{r}.u16()?"),
        Ty::U32 => format!("{r}.u32()?"),
        Ty::U64 => format!("{r}.u64()?"),
        Ty::B16 => format!("{r}.fixed::<16>()?"),
        Ty::B32 => format!("{r}.fixed::<32>()?"),
        Ty::Bytes => format!("{r}.bytes()?"),
        Ty::Str => format!("{r}.str()?"),
        Ty::Records => format!("{r}.records::<{}>()?", of.unwrap_or("()")),
    }
}

fn rust_sample(t: Ty, of: Option<&str>) -> String {
    match t {
        Ty::U8 => "rng.next_u64() as u8".into(),
        Ty::U16 => "rng.next_u64() as u16".into(),
        Ty::U32 => "rng.next_u64() as u32".into(),
        Ty::U64 => "rng.next_u64()".into(),
        Ty::B16 => "{ let mut a = [0u8; 16]; rng.fill(&mut a); a }".into(),
        Ty::B32 => "{ let mut a = [0u8; 32]; rng.fill(&mut a); a }".into(),
        Ty::Bytes => {
            "{ let n = rng.below(41) as usize; let mut v = vec![0u8; n]; rng.fill(&mut v); v }"
                .into()
        }
        Ty::Str => "rng.ascii(20)".into(),
        Ty::Records => format!(
            "vec![{}::default(); rng.below(3) as usize]",
            of.unwrap_or("()")
        ),
    }
}

fn emit_rust_msg(o: &mut String, m: &Msg) {
    let _ = writeln!(
        o,
        "#[derive(Debug, Clone, PartialEq, Eq, Default)]\npub struct {} {{",
        m.name
    );
    for f in &m.fields {
        let _ = writeln!(
            o,
            "    pub {}: {},",
            rid(&f.name),
            rust_ty(f.ty, f.of.as_deref())
        );
    }
    for e in &m.ext {
        let _ = writeln!(
            o,
            "    pub {}: Option<{}>,",
            rid(&e.name),
            rust_ty(e.ty, None)
        );
    }
    o.push_str("}\n\n");
    let _ = writeln!(
        o,
        "impl Message for {} {{\n    const NAME: &'static str = \"{}\";\n",
        m.name, m.name
    );
    o.push_str("    fn encode_into(&self, w: &mut Writer) -> Result<(), EncodeError> {\n");
    for f in &m.fields {
        let _ = writeln!(
            o,
            "        {}",
            rust_put(f.ty, &format!("self.{}", rid(&f.name)))
        );
    }
    if m.ext.is_empty() {
        o.push_str("        w.u16(0);\n");
    } else {
        o.push_str("        let mut ext_n: u16 = 0;\n");
        for e in &m.ext {
            let _ = writeln!(
                o,
                "        if self.{}.is_some() {{ ext_n += 1; }}",
                rid(&e.name)
            );
        }
        o.push_str("        w.u16(ext_n);\n");
        for e in &m.ext {
            let _ = writeln!(
                o,
                "        if let Some(v) = &self.{} {{ w.ext({}, |w| {{ {} }})?; }}",
                rid(&e.name),
                e.tag,
                rust_put_ext(e.ty)
            );
        }
    }
    o.push_str("        Ok(())\n    }\n\n");
    o.push_str("    fn decode(b: &[u8]) -> Result<Self, DecodeError> {\n        let mut r = Reader::new(b);\n");
    let assigns = !m.fields.is_empty() || !m.ext.is_empty();
    let _ = writeln!(
        o,
        "        let {}m = Self::default();",
        if assigns { "mut " } else { "" }
    );
    for f in &m.fields {
        let _ = writeln!(
            o,
            "        m.{} = {};",
            rid(&f.name),
            rust_get(f.ty, "r", f.of.as_deref())
        );
    }
    o.push_str("        let ext_n = r.u16()?;\n        for _ in 0..ext_n {\n            let tag = r.u16()?;\n            let len = r.u32()? as usize;\n            let v = r.take(len)?;\n");
    if m.ext.is_empty() {
        o.push_str("            let _ = (tag, v);\n");
    } else {
        o.push_str("            match tag {\n");
        for e in &m.ext {
            let _ = writeln!(
                o,
                "                {} => {{\n                    if m.{}.is_some() {{ return Err(DecodeError::DupExt({})); }}\n                    let mut vr = Reader::new(v);\n                    m.{} = Some({});\n                    vr.finish()?;\n                }}",
                e.tag, rid(&e.name), e.tag, rid(&e.name), rust_get(e.ty, "vr", None)
            );
        }
        o.push_str("                _ => {}\n            }\n");
    }
    o.push_str("        }\n        r.finish()?;\n        Ok(m)\n    }\n}\n\n");
    if let Some(t) = m.frame_type {
        let _ = writeln!(
            o,
            "impl FrameMessage for {} {{\n    const TYPE: u8 = {t:#04x};\n}}\n",
            m.name
        );
    }
}

/// What one run generates: the core's codecs, or an application's extension of them.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// For the "do not edit" banner: the schema the files come from and how to remake them.
    pub source: &'a str,
    pub regenerate: &'a str,
    /// None for the core; `Some("app")` names an extension (`ava1_app_gen.h`,
    /// `ava1_app_message_names`, `ava1_app_roundtrip`).
    pub ext: Option<&'a str>,
}

impl Target<'_> {
    fn banner(&self, open: &str, cont: &str, close: &str) -> String {
        format!(
            "{open}Generated by ava1-gen from {}. Do not edit:\n{cont}{} after changing the schema.{close}\n",
            self.source, self.regenerate
        )
    }
    fn c_prefix(&self) -> String {
        match self.ext {
            Some(n) => format!("ava1_{n}_"),
            None => "ava1_".into(),
        }
    }
    fn c_header(&self) -> String {
        match self.ext {
            Some(n) => format!("ava1_{n}_gen.h"),
            None => "ava1_gen.h".into(),
        }
    }
}

/// The core's own target (`cargo run -p ava1-gen` in this repository).
pub const CORE: Target<'static> = Target {
    source: "schema/ava1.toml",
    regenerate: "run `cargo run -p ava1-gen`",
    ext: None,
};

pub fn emit_rust(s: &Schema, t: &Target) -> String {
    let mut o = t.banner("// ", "// ", "");
    o.push_str("#![allow(clippy::field_reassign_with_default, clippy::single_match, clippy::match_single_binding)]\n\n");
    let wire = if t.ext.is_some() {
        "ava1::wire"
    } else {
        "crate::wire"
    };
    let frame = if s.messages.is_empty() {
        ""
    } else {
        "FrameMessage, "
    };
    let _ = writeln!(
        o,
        "use {wire}::{{DecodeError, EncodeError, {frame}Message, Reader, SplitMix, Writer}};\n"
    );
    for c in &s.consts {
        let _ = writeln!(o, "pub const {}: {} = {};", c.name, c.ty, c.value);
    }
    o.push('\n');
    let all: Vec<&Msg> = s.messages.iter().chain(&s.structs).collect();
    for m in &all {
        emit_rust_msg(&mut o, m);
    }
    // An extension's helpers live in `conformance`, so a module that re-exports the core's
    // codecs and the extension's together has one ALL / sample / roundtrip of each.
    if t.ext.is_some() {
        o.push_str(
            "/// The extension's conformance helpers.\npub mod conformance {\nuse super::*;\n",
        );
    }
    o.push_str(
        "/// Every message and struct, by name (conformance tests).\npub const ALL: &[&str] = &[",
    );
    for m in &all {
        let _ = write!(o, "\"{}\", ", m.name);
    }
    o.push_str("];\n\n#[doc(hidden)]\npub fn sample(name: &str, rng: &mut SplitMix) -> Option<Vec<u8>> {\n    match name {\n");
    for m in &all {
        let _ = write!(o, "        \"{}\" => {} {{", m.name, m.name);
        for f in &m.fields {
            let _ = write!(
                o,
                " {}: {},",
                rid(&f.name),
                rust_sample(f.ty, f.of.as_deref())
            );
        }
        for e in &m.ext {
            let _ = write!(
                o,
                " {}: if rng.below(2) == 1 {{ Some({}) }} else {{ None }},",
                rid(&e.name),
                rust_sample(e.ty, None)
            );
        }
        o.push_str(" }.to_bytes().ok(),\n");
    }
    o.push_str("        _ => None,\n    }\n}\n\n#[doc(hidden)]\npub fn roundtrip(name: &str, bytes: &[u8]) -> Option<Result<Vec<u8>, String>> {\n    fn rt<M: Message>(b: &[u8]) -> Result<Vec<u8>, String> {\n        M::decode(b).map_err(|e| e.to_string())?.to_bytes().map_err(|e| e.to_string())\n    }\n    Some(match name {\n");
    for m in &all {
        let _ = writeln!(o, "        \"{}\" => rt::<{}>(bytes),", m.name, m.name);
    }
    o.push_str("        _ => return None,\n    })\n}\n");
    if t.ext.is_some() {
        o.push_str("}\n");
    }
    o
}

fn snake(name: &str) -> String {
    let mut s = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                s.push('_');
            }
            s.push(ch.to_ascii_lowercase());
        } else {
            s.push(ch);
        }
    }
    s
}

fn c_decl(t: Ty, n: &str) -> String {
    match t {
        Ty::U8 => format!("    uint8_t {n};\n"),
        Ty::U16 => format!("    uint16_t {n};\n"),
        Ty::U32 => format!("    uint32_t {n};\n"),
        Ty::U64 => format!("    uint64_t {n};\n"),
        Ty::B16 => format!("    uint8_t {n}[16];\n"),
        Ty::B32 => format!("    uint8_t {n}[32];\n"),
        Ty::Bytes => format!("    const uint8_t *{n};\n    uint32_t {n}_len;\n"),
        Ty::Str => format!("    const uint8_t *{n};\n    uint16_t {n}_len;\n"),
        Ty::Records => {
            format!("    const uint8_t *{n};\n    uint32_t {n}_len;\n    uint32_t {n}_count;\n")
        }
    }
}

fn c_put(t: Ty, n: &str) -> String {
    match t {
        Ty::U8 => format!("ava1_w_u8(w, m->{n});"),
        Ty::U16 => format!("ava1_w_u16(w, m->{n});"),
        Ty::U32 => format!("ava1_w_u32(w, m->{n});"),
        Ty::U64 => format!("ava1_w_u64(w, m->{n});"),
        Ty::B16 => format!("ava1_w_fixed(w, m->{n}, 16);"),
        Ty::B32 => format!("ava1_w_fixed(w, m->{n}, 32);"),
        Ty::Bytes | Ty::Records => format!("ava1_w_bytes(w, m->{n}, m->{n}_len);"),
        Ty::Str => format!("ava1_w_str(w, m->{n}, m->{n}_len);"),
    }
}

fn c_get(t: Ty, r: &str, n: &str, of: Option<&str>) -> String {
    match t {
        Ty::U8 => format!("m->{n} = ava1_r_u8({r});"),
        Ty::U16 => format!("m->{n} = ava1_r_u16({r});"),
        Ty::U32 => format!("m->{n} = ava1_r_u32({r});"),
        Ty::U64 => format!("m->{n} = ava1_r_u64({r});"),
        Ty::B16 => format!("ava1_r_fixed({r}, m->{n}, 16);"),
        Ty::B32 => format!("ava1_r_fixed({r}, m->{n}, 32);"),
        Ty::Bytes => format!("m->{n} = ava1_r_bytes({r}, &m->{n}_len);"),
        Ty::Str => format!("m->{n} = ava1_r_str({r}, &m->{n}_len);"),
        Ty::Records => format!(
            "m->{n} = ava1_r_bytes({r}, &m->{n}_len);\n    if (!({r})->err) {{\n        int rc = ava1_{of}_count(m->{n}, m->{n}_len, &m->{n}_count);\n        if (rc != 0) return rc;\n    }}",
            of = snake(of.unwrap_or("x"))
        ),
    }
}

pub fn emit_c_header(s: &Schema, t: &Target) -> String {
    let mut o = t.banner("/* ", " * ", " */");
    let guard = t.c_header().replace('.', "_").to_uppercase();
    let base = if t.ext.is_some() {
        "ava1_gen.h"
    } else {
        "ava1_wire.h"
    };
    let _ = write!(
        o,
        "#ifndef {guard}\n#define {guard}\n\n#include <stddef.h>\n#include <stdint.h>\n\n#include \"{base}\"\n\n"
    );
    for c in &s.consts {
        // ULL: every constant has one type wide enough for a u64 value, whatever the
        // target's int and long are.
        let _ = writeln!(o, "#define AVA1_{} {}ULL", c.name, c.value);
    }
    o.push('\n');
    for m in &s.messages {
        let _ = writeln!(
            o,
            "#define AVA1_TYPE_{} {:#04x}u",
            snake(&m.name).to_uppercase(),
            m.frame_type.unwrap_or(0)
        );
    }
    o.push('\n');
    for m in s.structs.iter().chain(&s.messages) {
        let sn = snake(&m.name);
        o.push_str("typedef struct {\n");
        for f in &m.fields {
            o.push_str(&c_decl(f.ty, &cid(&f.name)));
        }
        for e in &m.ext {
            let _ = writeln!(o, "    int has_{};", e.name);
            o.push_str(&c_decl(e.ty, &cid(&e.name)));
        }
        if m.fields.is_empty() && m.ext.is_empty() {
            o.push_str("    uint8_t unused_;\n");
        }
        let _ = writeln!(o, "}} ava1_{sn}_t;\n");
        let _ = writeln!(
            o,
            "int ava1_{sn}_encode(const ava1_{sn}_t *m, ava1_w_t *w);"
        );
        let _ = writeln!(
            o,
            "int ava1_{sn}_decode(const uint8_t *buf, size_t len, ava1_{sn}_t *m);\n"
        );
        if m.frame_type.is_none() {
            let _ = writeln!(
                o,
                "int ava1_{sn}_append(ava1_w_t *blob, const ava1_{sn}_t *m);"
            );
            let _ = writeln!(o, "int ava1_{sn}_next(ava1_r_t *it, ava1_{sn}_t *out);");
            let _ = writeln!(
                o,
                "int ava1_{sn}_count(const uint8_t *p, uint32_t len, uint32_t *count);\n"
            );
        }
    }
    let px = t.c_prefix();
    let _ = writeln!(
        o,
        "extern const char *const {px}message_names[];\nextern const size_t {px}message_count;\n"
    );
    o.push_str(
        "/* Decode `in` as message `name`, then re-encode it into `out` (conformance tests). */\n",
    );
    let _ = writeln!(
        o,
        "int {px}roundtrip(const char *name, const uint8_t *in, size_t in_len, uint8_t *out, size_t cap,\n                   size_t *out_len);\n\n#endif"
    );
    o
}

pub fn emit_c_source(s: &Schema, t: &Target) -> String {
    let mut o = t.banner("/* ", " * ", " */");
    let _ = write!(
        o,
        "#include \"{}\"\n\n#include <string.h>\n\n",
        t.c_header()
    );
    let px = t.c_prefix();
    let all: Vec<&Msg> = s.structs.iter().chain(&s.messages).collect();
    for m in &all {
        let sn = snake(&m.name);
        let _ = writeln!(
            o,
            "int ava1_{sn}_encode(const ava1_{sn}_t *m, ava1_w_t *w) {{"
        );
        if m.fields.is_empty() && m.ext.is_empty() {
            o.push_str("    (void)m;\n");
        }
        if !m.ext.is_empty() {
            o.push_str("    uint16_t ext_n = 0;\n");
        }
        for f in &m.fields {
            let _ = writeln!(o, "    {}", c_put(f.ty, &cid(&f.name)));
        }
        if m.ext.is_empty() {
            o.push_str("    ava1_w_u16(w, 0);\n");
        } else {
            for e in &m.ext {
                let _ = writeln!(o, "    if (m->has_{}) ext_n++;", e.name);
            }
            o.push_str("    ava1_w_u16(w, ext_n);\n");
            for e in &m.ext {
                let _ = writeln!(
                    o,
                    "    if (m->has_{n}) {{\n        size_t at = ava1_w_ext_begin(w, {t});\n        {put}\n        ava1_w_ext_end(w, at);\n    }}",
                    n = e.name,
                    t = e.tag,
                    put = c_put(e.ty, &cid(&e.name))
                );
            }
        }
        o.push_str("    return w->err;\n}\n\n");

        let _ = writeln!(
            o,
            "int ava1_{sn}_decode(const uint8_t *buf, size_t len, ava1_{sn}_t *m) {{"
        );
        o.push_str("    ava1_r_t r;\n    uint16_t ext_n, i;\n    memset(m, 0, sizeof(*m));\n    ava1_r_init(&r, buf, len);\n");
        for f in &m.fields {
            let _ = writeln!(
                o,
                "    {}",
                c_get(f.ty, "&r", &cid(&f.name), f.of.as_deref())
            );
        }
        o.push_str("    ext_n = ava1_r_u16(&r);\n    for (i = 0; i < ext_n && !r.err; i++) {\n");
        if m.ext.is_empty() {
            o.push_str("        uint32_t vlen;\n        (void)ava1_r_u16(&r);\n        vlen = ava1_r_u32(&r);\n        (void)ava1_r_take(&r, vlen);\n");
        } else {
            o.push_str("        uint16_t tag = ava1_r_u16(&r);\n        uint32_t vlen = ava1_r_u32(&r);\n        const uint8_t *v = ava1_r_take(&r, vlen);\n        ava1_r_t vr;\n        int rc;\n        if (r.err) break;\n        ava1_r_init(&vr, v, vlen);\n        switch (tag) {\n");
            for e in &m.ext {
                let _ = writeln!(
                    o,
                    "        case {t}:\n            if (m->has_{n}) return AVA1_E_DUP_EXT;\n            m->has_{n} = 1;\n            {get}\n            break;",
                    t = e.tag,
                    n = e.name,
                    get = c_get(e.ty, "&vr", &cid(&e.name), None)
                );
            }
            o.push_str("        default:\n            continue;\n        }\n        rc = ava1_r_finish(&vr);\n        if (rc != 0) return rc;\n");
        }
        o.push_str("    }\n    return ava1_r_finish(&r);\n}\n\n");
        if m.frame_type.is_none() {
            let _ = writeln!(
                o,
                "int ava1_{sn}_append(ava1_w_t *blob, const ava1_{sn}_t *m) {{\n    size_t at = ava1_w_len_begin(blob);\n    int rc = ava1_{sn}_encode(m, blob);\n    if (rc != 0) return rc;\n    ava1_w_len_end(blob, at);\n    return blob->err;\n}}\n"
            );
            let _ = writeln!(
                o,
                "int ava1_{sn}_next(ava1_r_t *it, ava1_{sn}_t *out) {{\n    uint32_t n;\n    const uint8_t *p;\n    int rc;\n    if (it->err) return it->err;\n    if (it->pos == it->len) return 0;\n    n = ava1_r_u32(it);\n    p = ava1_r_take(it, n);\n    if (it->err) return it->err;\n    rc = ava1_{sn}_decode(p, n, out);\n    return rc != 0 ? rc : 1;\n}}\n"
            );
            let _ = writeln!(
                o,
                "int ava1_{sn}_count(const uint8_t *p, uint32_t len, uint32_t *count) {{\n    ava1_r_t it;\n    ava1_{sn}_t tmp;\n    int rc;\n    *count = 0;\n    ava1_r_init(&it, p, len);\n    while ((rc = ava1_{sn}_next(&it, &tmp)) == 1) (*count)++;\n    return rc;\n}}\n"
            );
        }
    }
    let _ = writeln!(o, "const char *const {px}message_names[] = {{");
    for m in &all {
        let _ = writeln!(o, "    \"{}\",", m.name);
    }
    let _ = writeln!(o, "}};\nconst size_t {px}message_count = {};\n", all.len());
    let _ = writeln!(o, "int {px}roundtrip(const char *name, const uint8_t *in, size_t in_len, uint8_t *out, size_t cap,\n                   size_t *out_len) {{\n    ava1_w_t w;\n    int rc;\n    ava1_w_init(&w, out, cap);\n    *out_len = 0;");
    for (i, m) in all.iter().enumerate() {
        let sn = snake(&m.name);
        let _ = writeln!(
            o,
            "    {kw}if (strcmp(name, \"{n}\") == 0) {{\n        ava1_{sn}_t m;\n        rc = ava1_{sn}_decode(in, in_len, &m);\n        if (rc == 0) rc = ava1_{sn}_encode(&m, &w);\n    }}",
            kw = if i == 0 { "" } else { "else " },
            n = m.name
        );
    }
    o.push_str("    else {\n        return AVA1_E_PROTO;\n    }\n    *out_len = w.len;\n    return rc;\n}\n");
    o
}

/// The core schema this generator checks extensions against.
pub const CORE_SCHEMA: &str = include_str!("core_schema.toml");

/// The core's generated files, under this repository's root.
pub fn outputs(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let src = std::fs::read_to_string(root.join("schema/ava1.toml"))
        .map_err(|e| format!("read schema: {e}"))?;
    let s = parse(&src)?;
    Ok(vec![
        (root.join("crates/ava1/src/gen.rs"), emit_rust(&s, &CORE)),
        (root.join("c/gen/ava1_gen.h"), emit_c_header(&s, &CORE)),
        (root.join("c/gen/ava1_gen.c"), emit_c_source(&s, &CORE)),
        (root.join("crates/ava1-gen/src/core_schema.toml"), src),
    ])
}

/// An application's extension: its schema checked against the core's (no name or constant may
/// be defined twice, and an extension cannot add frame types: those belong to the protocol),
/// then its Rust module and its C pair.
pub fn extension_outputs(
    ext_schema: &str,
    t: &Target,
    rust_out: &Path,
    c_dir: &Path,
) -> Result<Vec<(PathBuf, String)>, String> {
    let name = t.ext.ok_or("an extension needs a name")?;
    if !is_snake(name) {
        return Err(format!("extension name `{name}` must be snake_case"));
    }
    // The core schema travels inside the crate (a published crate cannot reach the repository
    // root); `outputs` keeps the copy identical to schema/ava1.toml.
    let core = parse(CORE_SCHEMA)?;
    let s = parse(ext_schema)?;
    if let Some(m) = s.messages.first() {
        return Err(format!(
            "{}: an extension defines structs, not frame messages (frame types are the protocol's)",
            m.name
        ));
    }
    let taken: HashSet<&str> = core
        .consts
        .iter()
        .map(|c| c.name.as_str())
        .chain(
            core.messages
                .iter()
                .chain(&core.structs)
                .map(|m| m.name.as_str()),
        )
        .collect();
    for n in s
        .consts
        .iter()
        .map(|c| c.name.as_str())
        .chain(s.structs.iter().map(|m| m.name.as_str()))
    {
        if taken.contains(n) {
            return Err(format!("{n} is already defined by the AVA1 core schema"));
        }
    }
    Ok(vec![
        (rust_out.to_path_buf(), emit_rust(&s, t)),
        (
            c_dir.join(format!("ava1_{name}_gen.h")),
            emit_c_header(&s, t),
        ),
        (
            c_dir.join(format!("ava1_{name}_gen.c")),
            emit_c_source(&s, t),
        ),
    ])
}
