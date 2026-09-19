//! The `PINYIN` value: one syllable packed into a `u16`.
//!
//! The layout is the one used by `WFLing-seaer/pinyinparser`. Three disjoint
//! fields, each of which carries a *variant-select bit* above its base value
//! rather than a dense index:
//!
//! ```text
//! bit 15      声母 variant   ch/sh/zh and the special H/R/M/N
//! bit 14..8   韵母           bits 14..13 variant, bits 12..8 base
//! bit  7..5   声调
//! bit  4..0   声母 base
//! ```
//!
//! A base value occupies the low bits of its field and the variant bit sits
//! above it, so masking the variant off collapses a variant onto its base:
//! `ch & 0x001F == c`, and `zh & 0x001F == z`. That is the whole point of the
//! layout — "首字母" and "通配" are a mask away, not a table lookup.
//!
//! The 韵母 is **phonemic, not orthographic**. `chi` is `ch` + `ri`, `zi` is
//! `z` + `ii`, `ju` is `j` + `v`, `liu` is `l` + `iou`, `yan` is `y` + `ian`.
//! `y` and `w` are independent 声母, not variants of the zero initial.
//!
//! Because the fields sit in separate bits, a query is a masked compare:
//!
//! ```text
//! 声母 is p        value & 0x801F == 0x000E
//! 韵母 is ang      value & 0x7F00 == 0x0600
//! 平声 (阴平阳平)   value & 0x00C0 == 0x0080
//! 仄声 (上声去声)   value & 0x00C0 == 0x00C0
//! ```
//!
//! The 平/仄 test is the one the reference calls out: 声调 is stored as
//! `0x80, 0xA0, 0xC0, 0xE0` for 阴平/阳平/上声/去声, so the top two bits
//! separate the two classes in a single mask.

use crate::pinyin_data::{FINALS, INITIALS, SPELLINGS, SYLLABLES};

// --- field geometry --------------------------------------------------------

/// 声母 field: the whole base plus its variant-select bit.
pub const INITIAL_MASK: u16 = 0x801F;
/// 韵母 field.
pub const FINAL_MASK: u16 = 0x7F00;
/// 声调 field.
pub const TONE_MASK: u16 = 0x00E0;
/// 声母 and 韵母, i.e. everything but the 声调.
pub const SYLLABLE_MASK: u16 = 0xFF1F;

// --- the three fields ------------------------------------------------------

macro_rules! field {
    ($name:ident, $mask:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub struct $name(u16);

        impl $name {
            /// Wrap a raw field value, dropping anything outside the field.
            pub const fn from_bits(bits: u16) -> Self {
                Self(bits & $mask)
            }

            /// The raw field value, as it sits in the packed syllable.
            pub const fn bits(self) -> u16 {
                self.0
            }
        }
    };
}

field!(
    Initial,
    INITIAL_MASK,
    "声母, including the zero initial and the variant-select forms."
);
field!(
    Final,
    FINAL_MASK,
    "韵母. Phonemic, so `chi` is ch + ri rather than ch + i."
);
field!(
    Tone,
    TONE_MASK,
    "声调. `missing` means the syllable was written without one."
);

impl Initial {
    /// 零声母: the syllable starts with a vowel.
    pub const NUL: Initial = Initial(0x0002);
    /// The pseudo-initial `r`, which only ever appears as `ri`.
    pub const R: Initial = Initial(0x8010);
}

impl Tone {
    /// No tone given.
    pub const MISSING: Tone = Tone(0x0000);
    /// Tone explicitly wildcarded.
    pub const UNSPEC: Tone = Tone(0x0020);
    /// 轻声.
    pub const NEUTRAL: Tone = Tone(0x0060);
    /// 阴平.
    pub const T1: Tone = Tone(0x0080);
    /// 阳平.
    pub const T2: Tone = Tone(0x00A0);
    /// 上声.
    pub const T3: Tone = Tone(0x00C0);
    /// 去声.
    pub const T4: Tone = Tone(0x00E0);

    /// The digit this tone is written with, if it is one of 1..5.
    pub const fn digit(self) -> Option<u8> {
        match self.0 {
            0x0060 => Some(5),
            0x0080 => Some(1),
            0x00A0 => Some(2),
            0x00C0 => Some(3),
            0x00E0 => Some(4),
            _ => None,
        }
    }
}

// --- reading a packed syllable ---------------------------------------------

/// The 声调 of a packed syllable.
pub fn tone_of(value: u16) -> Tone {
    Tone::from_bits(value)
}

/// The packed syllable with its 声调 dropped.
pub fn syllable_of(value: u16) -> u16 {
    value & SYLLABLE_MASK
}

/// Pack the three fields into one syllable.
pub fn encode(initial: Initial, final_: Final, tone: Tone) -> u16 {
    initial.bits() | final_.bits() | tone.bits()
}

// --- spelling ---------------------------------------------------------------

/// The canonical toneless spelling of a packed syllable, e.g. `zhong`.
pub fn plain(value: u16) -> &'static str {
    let key = syllable_of(value);
    SPELLINGS
        .binary_search_by_key(&key, |(k, _)| *k)
        .map(|i| SPELLINGS[i].1)
        .unwrap_or("")
}

/// The canonical spelling with its tone, e.g. `zhōng`.
///
/// Where a tone has no precomposed character the digit form is used instead,
/// so `ê` in 上声 renders as `ê3`.
pub fn render(value: u16) -> String {
    let spelling = plain(value);
    let tone = tone_of(value);
    if matches!(tone, Tone::MISSING | Tone::UNSPEC) {
        return spelling.to_string();
    }
    match marked(spelling, tone) {
        Some(marked) => marked,
        None => match tone.digit() {
            Some(d) => format!("{spelling}{d}"),
            None => spelling.to_string(),
        },
    }
}

/// Apply a tone mark to a spelling, if there is a character for it.
fn marked(spelling: &str, tone: Tone) -> Option<String> {
    // The syllabic nasals carry the mark on the nasal itself.
    if let Some(base) = match spelling {
        "m" => Some('m'),
        "n" => Some('n'),
        _ => None,
    } {
        let marked = match (base, tone) {
            ('m', Tone::T2) => 'ḿ',
            ('n', Tone::T2) => 'ń',
            ('n', Tone::T3) => 'ň',
            ('n', Tone::T4) => 'ǹ',
            _ => return None,
        };
        return Some(marked.to_string());
    }

    // `ê` has precomposed forms for 阳平 and 去声 only.
    if spelling == "ê" {
        return match tone {
            Tone::T2 => Some("ế".to_string()),
            Tone::T4 => Some("ề".to_string()),
            _ => None,
        };
    }

    // `hm`, `hng` and `ng` have no precomposed tone forms at all.
    if matches!(spelling, "hm" | "hng" | "ng") {
        return None;
    }

    // Otherwise the mark goes on `a`, else `o`, else `e`, else the last
    // vowel — which is what makes `iu` mark the `u` and `ui` mark the `i`.
    let chars: Vec<char> = spelling.chars().collect();
    let target = chars
        .iter()
        .position(|c| *c == 'a')
        .or_else(|| chars.iter().position(|c| *c == 'o'))
        .or_else(|| chars.iter().position(|c| *c == 'e'))
        .or_else(|| {
            chars
                .iter()
                .rposition(|c| matches!(c, 'i' | 'u' | 'ü'))
        })?;

    let source = chars[target];
    let marked = mark_vowel(source, tone)?;
    Some(
        chars
            .iter()
            .enumerate()
            .map(|(i, c)| if i == target { marked } else { *c })
            .collect(),
    )
}

/// The tone-marked form of a single vowel.
fn mark_vowel(vowel: char, tone: Tone) -> Option<char> {
    Some(match (vowel, tone) {
        ('a', Tone::T1) => 'ā',
        ('a', Tone::T2) => 'á',
        ('a', Tone::T3) => 'ǎ',
        ('a', Tone::T4) => 'à',
        ('e', Tone::T1) => 'ē',
        ('e', Tone::T2) => 'é',
        ('e', Tone::T3) => 'ě',
        ('e', Tone::T4) => 'è',
        ('i', Tone::T1) => 'ī',
        ('i', Tone::T2) => 'í',
        ('i', Tone::T3) => 'ǐ',
        ('i', Tone::T4) => 'ì',
        ('o', Tone::T1) => 'ō',
        ('o', Tone::T2) => 'ó',
        ('o', Tone::T3) => 'ǒ',
        ('o', Tone::T4) => 'ò',
        ('u', Tone::T1) => 'ū',
        ('u', Tone::T2) => 'ú',
        ('u', Tone::T3) => 'ǔ',
        ('u', Tone::T4) => 'ù',
        ('ü', Tone::T1) => 'ǖ',
        ('ü', Tone::T2) => 'ǘ',
        ('ü', Tone::T3) => 'ǚ',
        ('ü', Tone::T4) => 'ǜ',
        _ => return None,
    })
}

/// A tone-marked vowel back to its plain form and the tone it carried.
fn demark(ch: char) -> Option<(char, Tone)> {
    let (base, tone) = match ch {
        'ā' => ('a', Tone::T1),
        'á' => ('a', Tone::T2),
        'ǎ' => ('a', Tone::T3),
        'à' => ('a', Tone::T4),
        'ē' => ('e', Tone::T1),
        'é' => ('e', Tone::T2),
        'ě' => ('e', Tone::T3),
        'è' => ('e', Tone::T4),
        'ī' => ('i', Tone::T1),
        'í' => ('i', Tone::T2),
        'ǐ' => ('i', Tone::T3),
        'ì' => ('i', Tone::T4),
        'ō' => ('o', Tone::T1),
        'ó' => ('o', Tone::T2),
        'ǒ' => ('o', Tone::T3),
        'ò' => ('o', Tone::T4),
        'ū' => ('u', Tone::T1),
        'ú' => ('u', Tone::T2),
        'ǔ' => ('u', Tone::T3),
        'ù' => ('u', Tone::T4),
        'ǖ' => ('ü', Tone::T1),
        'ǘ' => ('ü', Tone::T2),
        'ǚ' => ('ü', Tone::T3),
        'ǜ' => ('ü', Tone::T4),
        'ń' => ('n', Tone::T2),
        'ň' => ('n', Tone::T3),
        'ǹ' => ('n', Tone::T4),
        'ḿ' => ('m', Tone::T2),
        'ế' => ('ê', Tone::T2),
        'ề' => ('ê', Tone::T4),
        _ => return None,
    };
    Some((base, tone))
}

/// The combining tone marks, which have no precomposed form on `m`, `n` or
/// `ê`: U+0304 macron, U+0301 acute, U+030C caron, U+0300 grave.
fn combining_tone(ch: char) -> Option<Tone> {
    Some(match ch {
        '\u{0304}' => Tone::T1,
        '\u{0301}' => Tone::T2,
        '\u{030C}' => Tone::T3,
        '\u{0300}' => Tone::T4,
        _ => return None,
    })
}

/// A data error in a `VARCHAR` -> `PINYIN` cast.
#[derive(Debug)]
pub struct ParseError {
    pub input: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid pinyin syllable: '{}'", self.input)
    }
}

impl std::error::Error for ParseError {}

/// Split a written syllable into its toneless spelling and its tone.
///
/// Accepts `zhong`, `zhōng` and `zhong1`, treats `v` as `ü`, and strips the
/// combining marks used for the nasals. A tone given twice is rejected rather
/// than guessed at.
fn split_tone(input: &str) -> Result<(String, Option<Tone>), ParseError> {
    let fail = || ParseError {
        input: input.to_string(),
    };

    let lowered = input.trim().to_lowercase();

    // A trailing digit is a tone. 0 is the 声调 wildcard, which is meaningful
    // in a pattern but never in a value, so callers decide what to do with it.
    let (body, digit) = match lowered.as_bytes().last() {
        Some(b'0'..=b'9') => {
            let digit = lowered.as_bytes()[lowered.len() - 1] - b'0';
            if digit > 5 {
                return Err(fail());
            }
            (&lowered[..lowered.len() - 1], Some(digit))
        }
        _ => (lowered.as_str(), None),
    };

    let mut spelling = String::with_capacity(body.len());
    let mut marked_tone = None;
    for ch in body.chars() {
        if let Some((base, tone)) = demark(ch) {
            if marked_tone.is_some() {
                // Two tonal marks on one syllable is not something we can read.
                return Err(fail());
            }
            marked_tone = Some(tone);
            spelling.push(base);
        } else if let Some(tone) = combining_tone(ch) {
            if marked_tone.is_some() {
                return Err(fail());
            }
            marked_tone = Some(tone);
        } else {
            // `v` is the keyboard stand-in for `ü`.
            spelling.push(if ch == 'v' { 'ü' } else { ch });
        }
    }

    let tone = match (marked_tone, digit) {
        (Some(_), Some(_)) => return Err(fail()),
        (Some(t), None) => Some(t),
        (None, Some(0)) => Some(Tone::UNSPEC),
        (None, Some(d)) => Some(match d {
            1 => Tone::T1,
            2 => Tone::T2,
            3 => Tone::T3,
            4 => Tone::T4,
            _ => Tone::NEUTRAL,
        }),
        (None, None) => None,
    };

    Ok((spelling, tone))
}

/// Look up a toneless written syllable, returning its 声母 and 韵母.
pub fn lookup(spelling: &str) -> Option<(Initial, Final)> {
    SYLLABLES
        .binary_search_by(|(name, _, _)| (*name).cmp(spelling))
        .ok()
        .map(|i| {
            let (_, initial, final_) = SYLLABLES[i];
            (initial, final_)
        })
}

/// Parse a written syllable into the packed `u16`.
pub fn parse(input: &str) -> Result<u16, ParseError> {
    let (spelling, tone) = split_tone(input)?;
    let (initial, final_) = lookup(&spelling).ok_or_else(|| ParseError {
        input: input.to_string(),
    })?;
    Ok(encode(initial, final_, tone.unwrap_or(Tone::MISSING)))
}

// --- matching ---------------------------------------------------------------

/// A wildcard glyph. These are the ones the reference defines for querying.
pub const WILDCARD_ANY: char = '?';
/// Wildcards 声母 and 韵母 but not 声调.
pub const WILDCARD_SYLLABLE: char = '*';
/// The zero initial.
pub const ZERO_INITIAL: char = '/';
/// The special initial `R`.
pub const SPECIAL_R: char = '&';

const GLYPHS: [char; 4] = [WILDCARD_ANY, WILDCARD_SYLLABLE, ZERO_INITIAL, SPECIAL_R];

/// A compiled `pinyin_match` pattern.
///
/// Every pattern reduces to one masked compare on the packed syllable, which is
/// what the layout is for: a constraint on 声母 is a test of `0x801F`, on 韵母
/// of `0x7F00`, on 声调 of `0x00E0`, and leaving a field out simply drops it
/// from the mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Match when `value & mask == expect`.
    Mask { mask: u16, expect: u16 },
    /// A pattern that names nothing legal, and so matches nothing.
    Never,
}

impl Pattern {
    /// Match a packed syllable.
    pub fn matches(&self, value: u16) -> bool {
        match self {
            Pattern::Mask { mask, expect } => value & mask == *expect,
            Pattern::Never => false,
        }
    }
}

/// The 韵母 a written final names, following the same phonemic spelling rules
/// the value parser uses.
fn final_from_spelling(spelling: &str) -> Option<Final> {
    let phonemic = match spelling {
        "iu" => "iou",
        "ui" => "uei",
        "in" => "ien",
        "un" => "uen",
        "ing" => "ieng",
        "ie" => "ieh",
        "ê" => "eh",
        "ü" => "v",
        "üe" => "veh",
        "üan" => "van",
        "ün" => "ven",
        other => other,
    };
    FINALS
        .iter()
        .find(|(name, _)| *name == phonemic)
        .map(|(_, f)| *f)
}

/// The written 声母 a pattern starts with, and the remainder after it.
///
/// Longest match wins, so `zh` beats `z`; the zero initial is not written and
/// so is never returned here.
fn split_written_initial(spelling: &str) -> Option<(Initial, &str)> {
    const WRITTEN: [&str; 23] = [
        "zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x",
        "r", "z", "c", "s", "y", "w",
    ];
    for name in WRITTEN {
        if let Some(rest) = spelling.strip_prefix(name) {
            let initial = INITIALS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, i)| *i)?;
            return Some((initial, rest));
        }
    }
    None
}

/// Compile a match pattern.
///
/// The syntax is the reference's: a pattern is a syllable written the same way
/// a value is, except that any part of it may be replaced by a wildcard glyph,
/// and a part that is simply not written is a wildcard too. So the pattern only
/// ever constrains what it actually says.
///
/// ```text
/// p?       声母 p, 韵母 and 声调 free        value & 0x801F == 0x000E
/// p        同 p?                            value & 0x801F == 0x000E
/// ?ang     韵母 ang, 声母 and 声调 free      value & 0x7F00 == 0x0600
/// zhong    声母 and 韵母 of zhong           value & 0xFF1F == 0x8E16
/// zhong1   ... and 阴平                     value & 0xFFFF == 0x8E96
/// *        任意音节                         value & 0 == 0
/// /        零声母                           value & 0x801F == 0x0002
/// &        伪声母 R                         value & 0x801F == 0x8010
/// ??0      任意音节, 声调 free              value & 0 == 0
/// ```
///
/// Note that `zh` constrains the 声母 to the variant, so `z?` does not match
/// `zha`: the variant-select bit is part of the 声母 field, so a pattern cannot
/// ask for `z` and `zh` at once. A 声调 *class* is the same story — 平声 is
/// `(v & 0x00C0) == 0x0080`, which no single pattern reaches. Both are available
/// from SQL as a masked compare on `value::USMALLINT`.
pub fn compile_pattern(pattern: &str) -> Pattern {
    let (body, tone) = match split_tone(pattern) {
        Ok(parts) => parts,
        Err(_) => return Pattern::Never,
    };

    let mut mask = 0u16;
    let mut expect = 0u16;

    if let Some(tone) = tone {
        // `0` is the explicit 声调 wildcard; leaving the tone out means the
        // same thing, so neither adds to the mask.
        if tone != Tone::UNSPEC {
            mask |= TONE_MASK;
            expect |= tone.bits();
        }
    }

    // A bare tone digit constrains only the 声调, so it is a complete pattern.
    if body.is_empty() {
        return Pattern::Mask { mask, expect };
    }

    // A pattern with no glyphs is a plain syllable, or failing that a bare
    // 声母 such as `p`, or a bare 韵母 such as `ang`.
    let has_glyph = GLYPHS.iter().any(|g| body.contains(*g));
    if !has_glyph {
        if let Some((initial, final_)) = lookup(&body) {
            mask |= INITIAL_MASK | FINAL_MASK;
            expect |= initial.bits() | final_.bits();
            return Pattern::Mask { mask, expect };
        }
        // Only a *whole* 声母 counts: a pattern that resolves to an initial and
        // then trails off into something unrecognisable is a typo, not a
        // filter. Without this `zzz` would quietly match every `z` syllable.
        if let Some((initial, "")) = split_written_initial(&body) {
            mask |= INITIAL_MASK;
            expect |= initial.bits();
            return Pattern::Mask { mask, expect };
        }
        if let Some(final_) = final_from_spelling(&body) {
            mask |= FINAL_MASK;
            expect |= final_.bits();
            return Pattern::Mask { mask, expect };
        }
        return Pattern::Never;
    }

    // Otherwise walk the glyphs. The 声母 slot is filled first, then the 韵母;
    // a slot nothing claims stays free.
    let mut rest = body.as_str();

    if let Some(r) = rest.strip_prefix(ZERO_INITIAL) {
        mask |= INITIAL_MASK;
        expect |= Initial::NUL.bits();
        rest = r;
    } else if let Some(r) = rest.strip_prefix(SPECIAL_R) {
        mask |= INITIAL_MASK;
        expect |= Initial::R.bits();
        rest = r;
    } else if rest.starts_with(WILDCARD_SYLLABLE) {
        // `*` covers 声母 and 韵母 in one go.
        rest = &rest[1..];
        if !rest.is_empty() {
            return Pattern::Never;
        }
    } else if let Some(r) = rest.strip_prefix(WILDCARD_ANY) {
        rest = r;
    } else if let Some((initial, r)) = split_written_initial(rest) {
        mask |= INITIAL_MASK;
        expect |= initial.bits();
        rest = r;
    }

    // Whatever is left addresses the 韵母.
    let rest = rest.strip_prefix(WILDCARD_ANY).unwrap_or(rest);
    match rest {
        "" | "*" => Pattern::Mask { mask, expect },
        other => match final_from_spelling(other) {
            Some(final_) => Pattern::Mask {
                mask: mask | FINAL_MASK,
                expect: expect | final_.bits(),
            },
            None => Pattern::Never,
        },
    }
}

// --- matching a sequence of syllables ---------------------------------------

/// One element of a multi-syllable pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Elem {
    /// A single syllable the value must match.
    One(Pattern),
    /// Any number of syllables, including none.
    Any,
}

/// Compile a whitespace-separated multi-syllable pattern.
///
/// The value is a sequence of syllables and the pattern is a sequence of
/// single-syllable patterns, matched element by element. `*` is one syllable and
/// `**` is any number of them:
///
/// ```text
/// zhong guo       exactly two syllables, 中 then 国
/// *   *           exactly two syllables, both free
/// p   *   *       three syllables, the first with 声母 p
/// **              anything, including nothing
/// zhong ** guo    中 ... 国, with any number of syllables in between
/// ```
///
/// So `*` means the same thing here as it does in a one-syllable pattern — one
/// free syllable — and `**` is that idea widened to a run of them. `?` still
/// means one syllable with everything free, which is what a lone `*` says too;
/// the two differ only in the one-syllable form, where `*` covers 声母 and 韵母
/// but `?` covers 声调 as well.
///
/// A pattern with no elements (the empty string) matches only an empty
/// sequence. Returns `None` if any element names nothing legal.
pub fn compile_sequence(pattern: &str) -> Option<Vec<Elem>> {
    let mut elems = Vec::new();
    for token in pattern.split_whitespace() {
        if token == "**" {
            // Consecutive runs are one run; `** **` is just `**`.
            if elems.last() != Some(&Elem::Any) {
                elems.push(Elem::Any);
            }
            continue;
        }
        let compiled = compile_pattern(token);
        if compiled == Pattern::Never {
            return None;
        }
        elems.push(Elem::One(compiled));
    }
    Some(elems)
}

/// Match a sequence of syllables against a compiled multi-syllable pattern.
///
/// A `**` is greedy but gives its syllables back on demand: the match keeps the
/// position of the last one and how much it had swallowed, and when a later
/// element fails, backs up and lets it swallow one more. Linear in the number of
/// elements for the patterns people actually write.
pub fn match_sequence(elems: &[Elem], values: &[u16]) -> bool {
    let (mut e, mut v) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_at = 0usize;

    while v < values.len() {
        let advanced = match elems.get(e) {
            Some(Elem::One(pattern)) if pattern.matches(values[v]) => {
                e += 1;
                v += 1;
                true
            }
            Some(Elem::Any) => {
                star = Some(e);
                star_at = v;
                e += 1;
                true
            }
            _ => false,
        };
        if advanced {
            continue;
        }
        match star {
            Some(star_index) => {
                star_at += 1;
                e = star_index + 1;
                v = star_at;
            }
            None => return false,
        }
    }

    // Whatever is left has to be `*`, which matches the empty remainder.
    elems[e..].iter().all(|elem| *elem == Elem::Any)
}
