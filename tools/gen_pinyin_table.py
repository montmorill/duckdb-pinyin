"""Generate the phonemic pinyin table for src/pinyin_data.rs.

Dev-time only: reads pinyin.txt (MIT, mozillazg/pinyin-data) and emits the
static table checked into src/pinyin_data.rs. Not part of the build.

The bit layout is the one used by WFLing-seaer/pinyinparser: a syllable is a
u16 split into three disjoint fields, each of which carries a variant-select
bit above a base value rather than a dense index.

    v & 0x801F   声母  bit 15 selects the variant (ch/sh/zh/H/R/M/N),
                       bits 4..0 are the base letter
    v & 0x7F00   韵母  bits 14..13 select the variant, bits 12..8 the base
    v & 0x00E0   声调  bits 7..5

Only the *inventory* of syllables comes from pinyin-data; the enum values and
the orthography-to-phoneme split are ours. The split is phonemic, not
orthographic: `chi` is ch + ri, `ju` is j + v, `liu` is l + iou, `yan` is y +
ian. y and w are independent 声母 here, not variants of the zero initial.

    python tools/gen_pinyin_table.py tools/pinyin-data-0.15.0.txt src/pinyin_data.rs
"""
import sys
from collections import OrderedDict

# --- wire format -----------------------------------------------------------
# Base values occupy the low bits of the field; the variant-select bits sit
# above them, so masking a variant bit off collapses ch->c, sh->s, zh->z.

INITIALS = OrderedDict([
    ("missing", 0x0000), ("unspec", 0x0001), ("nul", 0x0002),
    ("b", 0x0003), ("c", 0x0004), ("d", 0x0005), ("f", 0x0006),
    ("g", 0x0007), ("h", 0x0008), ("j", 0x0009), ("k", 0x000A),
    ("l", 0x000B), ("m", 0x000C), ("n", 0x000D), ("p", 0x000E),
    ("q", 0x000F), ("r", 0x0010), ("s", 0x0011), ("t", 0x0012),
    ("w", 0x0013), ("x", 0x0014), ("y", 0x0015), ("z", 0x0016),
    ("ch", 0x8004), ("sh", 0x8011), ("zh", 0x8016),
    ("H", 0x8008), ("R", 0x8010), ("M", 0x800C), ("N", 0x800D),
])

FINALS = OrderedDict([
    ("missing", 0x0000), ("unspec", 0x0100), ("nul", 0x0200),
    ("a", 0x0300), ("ai", 0x0400), ("an", 0x0500), ("ang", 0x0600),
    ("ao", 0x0700), ("e", 0x0800), ("ei", 0x0900), ("en", 0x0A00),
    ("eng", 0x0B00), ("er", 0x0C00), ("i", 0x0D00), ("ong", 0x0E00),
    ("ou", 0x0F00), ("u", 0x1000), ("uo", 0x1100), ("veh", 0x1200),
    ("o", 0x1400), ("v", 0x1500), ("ieng", 0x1600), ("ng", 0x1700),
    ("m", 0x2200), ("n", 0x4200), ("hm", 0x6200), ("hng", 0x3700),
    ("eh", 0x2900), ("ii", 0x2D00), ("ri", 0x4D00),
    ("ia", 0x2300), ("ian", 0x2500), ("iang", 0x2600), ("iao", 0x2700),
    ("ieh", 0x2800), ("ien", 0x2A00), ("iong", 0x2E00), ("iou", 0x2F00),
    ("ua", 0x4300), ("uai", 0x4400), ("uan", 0x4500), ("uang", 0x4600),
    ("uei", 0x4900), ("uen", 0x4A00), ("ueng", 0x4B00),
    ("van", 0x6500), ("ven", 0x6A00),
])

# 声调 needs no table: there are only seven values and they are written out as
# constants on `Tone` in src/pinyin.rs. This is the mapping of record, and it is
# what those constants are checked against.
TONES = OrderedDict([
    ("missing", 0x0000), ("unspec", 0x0020), ("nul", 0x0040),
    ("t5", 0x0060), ("t1", 0x0080), ("t2", 0x00A0),
    ("t3", 0x00C0), ("t4", 0x00E0),
])

# --- orthography -> phoneme ------------------------------------------------
# Written initials, longest first so that zh/ch/sh beat z/c/s/h.
WRITTEN_INITIALS = ["zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l",
                    "g", "k", "h", "j", "q", "x", "r", "z", "c", "s", "y", "w"]

# The two retroflex sibilants and the dental sibilants take a syllabic
# continuant rather than [i]: zhi is ch/zh + ri, zi is z + ii.
SIBILANT_I = {"z": "ii", "c": "ii", "s": "ii"}
RETROFLEX_I = {"zh": "ri", "ch": "ri", "sh": "ri", "r": "ri"}

# j/q/x are written with u where the phoneme is ü.
JQX_U = {"u": "v", "ue": "veh", "uan": "van", "un": "ven"}
JQX = {"j", "q", "x"}

# Abbreviations that apply after any initial.
GLOBAL = {
    "iu": "iou", "ui": "uei", "in": "ien", "un": "uen",
    "ing": "ieng", "ie": "ieh", "ê": "eh",
    "ü": "v", "üe": "veh", "üan": "van", "ün": "ven",
}

# After the pseudo-initials y/w the written final is a respelling of the
# phonemic one, so these are complete rewrites rather than edits.
Y_FINAL = {
    "i": "i", "a": "ia", "e": "ieh", "ao": "iao", "ou": "iou", "an": "ian",
    "in": "ien", "ang": "iang", "ing": "ieng", "ong": "iong", "o": "o",
    "u": "v", "ue": "veh", "uan": "van", "un": "ven",
}
W_FINAL = {
    "u": "u", "a": "ua", "o": "uo", "ai": "uai", "ei": "uei", "an": "uan",
    "en": "uen", "ang": "uang", "eng": "ueng",
    # wòng (𥥈, 𥦷) is a variant transcription of wèng with no separate
    # phonemic form; there is no `wong` token, so w + ong is what the
    # reference tokenizer yields.
    "ong": "ong",
}

# Syllables with no vowel at all. They are complete only in these spellings;
# a bare `m` or `n` is a lone 声母 and is not a syllable.
SPECIALS = {
    "ng": ("N", "ng"),
    "hm": ("H", "hm"),
    "hng": ("H", "hng"),
    "m": ("m", "m"),
    "n": ("n", "n"),
}

TONE_MARKS = {
    "ā": ("a", 1), "á": ("a", 2), "ǎ": ("a", 3), "à": ("a", 4),
    "ē": ("e", 1), "é": ("e", 2), "ě": ("e", 3), "è": ("e", 4),
    "ī": ("i", 1), "í": ("i", 2), "ǐ": ("i", 3), "ì": ("i", 4),
    "ō": ("o", 1), "ó": ("o", 2), "ǒ": ("o", 3), "ò": ("o", 4),
    "ū": ("u", 1), "ú": ("u", 2), "ǔ": ("u", 3), "ù": ("u", 4),
    "ǖ": ("ü", 1), "ǘ": ("ü", 2), "ǚ": ("ü", 3), "ǜ": ("ü", 4),
    "ń": ("n", 2), "ň": ("n", 3), "ǹ": ("n", 4), "ḿ": ("m", 2),
    "ế": ("ê", 2), "ề": ("ê", 4),
}

# `m̀`, `m̄`, `ê̄`, `ê̌` have no precomposed form, so they are written with a
# combining mark. Only the base letter matters for the toneless table.
COMBINING_TONES = {"̄", "́", "̌", "̀"}


def toneless(reading):
    """Strip tone marks and tone digits, lowercasing and folding v -> ü."""
    out = []
    for ch in reading.lower():
        if ch in TONE_MARKS:
            out.append(TONE_MARKS[ch][0])
        elif ch.isdigit() or ch in COMBINING_TONES:
            continue
        else:
            out.append("ü" if ch == "v" else ch)
    return "".join(out)


def split(syllable):
    """Split a written syllable into (声母 name, 韵母 name), phonemically."""
    if syllable in SPECIALS:
        return SPECIALS[syllable]

    for ini in WRITTEN_INITIALS:
        if syllable.startswith(ini) and len(syllable) > len(ini):
            rest = syllable[len(ini):]
            break
    else:
        # No written initial matches: the zero initial takes the whole string.
        ini, rest = "nul", syllable

    if ini in JQX and rest in JQX_U:
        return ini, JQX_U[rest]
    if ini in SIBILANT_I and rest == "i":
        return ini, SIBILANT_I[ini]
    if ini in RETROFLEX_I and rest == "i":
        return ini, RETROFLEX_I[ini]
    if ini == "y":
        return ini, Y_FINAL[rest]
    if ini == "w":
        return ini, W_FINAL[rest]
    return ini, GLOBAL.get(rest, rest)


def collect(path):
    raw = set()
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.split("#")[0].strip()
            if not line.startswith("U+"):
                continue
            _, _, readings = line.partition(":")
            for reading in readings.split(","):
                reading = reading.strip()
                if reading:
                    raw.add(toneless(reading))
    return raw


def main(path, dest):
    spellings = collect(path)
    spellings.update(SPECIALS)

    entries, problems = {}, []
    for syl in sorted(spellings):
        try:
            ini, fin = split(syl)
        except KeyError as exc:
            problems.append(f"  {syl!r}: no rule for final {exc}")
            continue
        if ini not in INITIALS:
            problems.append(f"  {syl!r}: unknown initial {ini!r}")
            continue
        if fin not in FINALS:
            problems.append(f"  {syl!r}: unknown final {fin!r}")
            continue
        entries[syl] = (INITIALS[ini], FINALS[fin])

    if problems:
        sys.exit("unmapped syllables:\n" + "\n".join(problems))

    # Two spellings colliding on one value would make rendering ambiguous.
    reverse = {}
    for syl, (ini, fin) in entries.items():
        reverse.setdefault(ini | fin, []).append(syl)
    collisions = {k: v for k, v in reverse.items() if len(v) > 1}
    if collisions:
        sys.exit("spellings collide on one value: " + repr(collisions))

    print(f"// syllables={len(entries)} initials={len(INITIALS)} "
          f"finals={len(FINALS)}", file=sys.stderr)

    out = ["//! Phonemic pinyin table.", "//!",
           "//! Generated from `pinyin-data` v0.15.0 (MIT,",
           "//! <https://github.com/mozillazg/pinyin-data>) by",
           "//! `tools/gen_pinyin_table.py`. Do not edit by hand.",
           "//!",
           "//! The bit layout and the orthography-to-phoneme split follow",
           "//! `WFLing-seaer/pinyinparser`; see that script for the rules.",
           "", "use crate::pinyin::{Final, Initial};", ""]

    # No 声调 table: the seven 声调 values are written out as constants on
    # `Tone`, and `split_tone` maps a digit straight to one of them.
    for name, table, ty, doc in (
        ("INITIALS", INITIALS, "Initial", "声母, including the zero initial and the "
                                          "variant-select forms."),
        ("FINALS", FINALS, "Final", "韵母. Phonemic, so `chi` is ch + ri, not ch + i."),
    ):
        out.append(f"/// {doc}")
        out.append(f"pub static {name}: &[(&str, {ty})] = &[")
        for key, val in table.items():
            out.append(f'    ("{key}", {ty}::from_bits(0x{val:04X})),')
        out.append("];")
        out.append("")

    out.append("/// (written syllable, 声母, 韵母), toneless, sorted for binary search.")
    out.append("pub static SYLLABLES: &[(&str, Initial, Final)] = &[")
    for syl, (ini, fin) in sorted(entries.items()):
        out.append(f'    ("{syl}", Initial::from_bits(0x{ini:04X}), '
                   f"Final::from_bits(0x{fin:04X})),")
    out.append("];")
    out.append("")

    out.append("/// (声母|韵母, written syllable), sorted by value, for rendering.")
    out.append("pub static SPELLINGS: &[(u16, &str)] = &[")
    for key, syls in sorted(reverse.items()):
        out.append(f'    (0x{key:04X}, "{syls[0]}"),')
    out.append("];")
    out.append("")

    with open(dest, "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
