# duck-pinyin

A DuckDB extension that adds a **`PINYIN`** column type and **`pinyin_match`**,
for filtering Chinese text by how it sounds rather than how it is written.

`PINYIN` is an alias of `USMALLINT`, so a syllable costs the two bytes an integer
already cost, and there is no separate storage path to keep working. Filtering
is one masked compare per value: the 声母, 韵母 and 声调 are separate bit fields,
so a pattern like `p?` or `?ang` compiles to a mask and a constant.

- No DuckDB build required — built against the C extension API
- No C or C++ — pure Rust
- No hanzi table: `VARCHAR -> PINYIN` parses pinyin, not characters

## Building

Building is a two-step process. First:

```shell
make configure
```

This sets up a Python venv with DuckDB and its test runner, and works out the
platform you are compiling for. It needs Python 3 with `venv`, `make`, and git;
the submodule has to be present, so clone with `--recurse-submodules`.

Then:

```shell
make debug      # or `make release` for an optimized build
```

This delegates to cargo and turns the resulting shared library into a loadable
extension by appending a binary footer, written to
`build/debug/extension/pinyin/pinyin.duckdb_extension`.

## Running it

Local extensions have to be loaded unsigned:

```sh
duckdb -unsigned
```

```sql
LOAD './build/release/extension/pinyin/pinyin.duckdb_extension';

CREATE TABLE t(s PINYIN);
INSERT INTO t VALUES ('zhong1'), ('guó'), ('shǎng'), (NULL);

-- 韵母 ang, 声母 and 声调 free
SELECT s::VARCHAR, s::USMALLINT FROM t WHERE pinyin_match(s, '?ang');
-- shǎng	34513
```

## The `PINYIN` column type

`PINYIN` is a column type for **one syllable per value**, packed into the 2 bytes
a `USMALLINT` occupies. It is registered as an alias of `USMALLINT`, so it costs
nothing over storing the integer yourself, and the packing is a pure bit layout
you can query with ordinary bitwise SQL.

```sql
CREATE TABLE t(s PINYIN);
INSERT INTO t VALUES ('zhong1'), ('zhōng'), ('pīng'), (NULL);

SELECT s, s::USMALLINT FROM t;
```

A value is written in any of the usual ways and all three agree:
`'zhong'` (no tone), `'zhong1'` (digit) and `'zhōng'` (tone mark). A syllable
written with no tone is *not* the same value as a toned one — it leaves the 声调
field at 未指定.

### Bit layout

The layout follows [`WFLing-seaer/pinyinparser`](https://github.com/WFLing-seaer/pinyinparser):

```
bit 15      声母 variant-select   (turns c/s/z into ch/sh/zh, and selects H/R/M/N)
bit 14..13  韵母 variant-select
bit 12..8   韵母 base
bit  7..5   声调
bit  4..0   声母 base
```

which gives these masks:

| field    | mask       | meaning                                                                                                                         |
| -------- | ---------- | ------------------------------------------------------------------------------------------------------------------------------- |
| 声母     | `0x801F` | base plus the variant bit, so`ch` is one value, not `c` + something                                                         |
| 韵母     | `0x7F00` |                                                                                                                                 |
| 声调     | `0x00E0` | `0x00` no tone written, `0x20` an explicit `0`, `0x60` 轻声, `0x80` 阴平, `0xA0` 阳平, `0xC0` 上声, `0xE0` 去声 |
| syllable | `0xFF1F` | everything but the 声调                                                                                                         |

The split is **phonemic, not orthographic**: `chi` is ch·ri rather than ch·i,
`zi` is z·ii, `ju` is j·v, `liu` is l·iou, `yan` is y·ian. `y` and `w` are
independent 声母, not variants of the zero initial.

Because the 声调 codes are ordered so that 阴平 and 阳平 share a prefix,
**平声 is one masked compare** and needs no dedicated function:

```sql
-- 平声 (阴平 + 阳平); 192 = 0x00C0, 128 = 0x0080
SELECT * FROM t WHERE (s::USMALLINT & 192) = 128;
-- 仄声 (上声 + 去声)
SELECT * FROM t WHERE (s::USMALLINT & 192) = 192;
```

### `PINYIN[]`

An array of syllables is just `LIST(PINYIN)`, so it comes for free — no separate
type is registered. It has its own `pinyin_match` overload, below.

```sql
SELECT ['zhong1', 'guo2']::PINYIN[]::VARCHAR;             -- [zhōng, guó]
SELECT unnest(['zhong1', 'guo2']::PINYIN[])::VARCHAR;     -- zhōng, then guó
SELECT list_filter(['zhong1', 'guo2', 'shǎng']::PINYIN[],
                   lambda s: pinyin_match(s, 'zh'))::VARCHAR;   -- [zhōng]
```

### Rendering takes an explicit cast

`PINYIN -> VARCHAR` is registered **without** an implicit cost, so `SELECT s`
prints the integer a `PINYIN` is stored as, and `'zhōng'` needs `s::VARCHAR`.
That is deliberate: `PINYIN` is an alias of `USMALLINT`, so an implicit cast to
`VARCHAR` would compete with DuckDB's own numeric one and could change how a
plain `USMALLINT` renders. The cast is still there for `::` and for
`TRY_CAST(... AS VARCHAR)`, and it is the inverse of the parse:

```sql
SELECT 'zhong1'::PINYIN::VARCHAR;    -- zhōng
SELECT 'liu2'::PINYIN::VARCHAR;      -- liú    (tone mark on the o)
SELECT 'zhong'::PINYIN::VARCHAR;     -- zhong  (no tone written, none shown)
```

Tone marks are placed by the 汉语拼音 rule, on the first of `a > o > e` and
otherwise on the last vowel.

### `pinyin_match(PINYIN, VARCHAR) -> BOOLEAN`

A pattern is a syllable written the same way a value is, except that any part of
it may be replaced by a wildcard — and **a part that is simply not written is a
wildcard too**. Matching is one masked compare on the packed value, so a filter
costs a load and an `and`.

| pattern       | means                        | mask                   |
| ------------- | ---------------------------- | ---------------------- |
| `p?`, `p` | 声母 p, everything else free | `& 0x801F == 0x000E` |
| `?ang`      | 韵母 ang, 声母 and 声调 free | `& 0x7F00 == 0x0600` |
| `zhong`     | 声母 and 韵母 of`zhong`    | `& 0xFF1F == 0x8E16` |
| `zhong1`    | ... and 阴平                 | `& 0xFFFF == 0x8E96` |
| `1`         | any syllable in 阴平         | `& 0x00E0 == 0x0080` |
| `?`, `*`  | any syllable                 | matches all            |
| `/`         | 零声母                       | `& 0x801F == 0x0002` |
| `&`         | 伪声母 R — see below        | `& 0x801F == 0x8010` |

```sql
SELECT * FROM t WHERE pinyin_match(s, 'p?');    -- 声母 p
SELECT * FROM t WHERE pinyin_match(s, '?1');    -- 阴平
SELECT * FROM t WHERE pinyin_match(s, '?ang');  -- 韵母 ang
```

The first parameter is declared as the `PINYIN` type itself, so a `PINYIN` value
and a `VARCHAR` column both bind through the type's own parse:

```sql
SELECT pinyin_match('zhong1'::PINYIN, 'zhong');   -- true
SELECT pinyin_match(v, 'zhong') FROM t(v);        -- v is VARCHAR
```

### `pinyin_match(PINYIN[], VARCHAR) -> BOOLEAN`

The same function over a **list** of syllables, for filtering whole phrases. The
pattern is the syllables separated by spaces, matched element by element:

| pattern          | means                                     |
| ---------------- | ----------------------------------------- |
| `zhong guo`    | exactly two syllables, 中 then 国         |
| `zhong ** guo` | 中, then any number of syllables, then 国 |
| `zhong **`     | starts with 中                            |
| `p **`         | starts with a 声母-p syllable             |
| `* *`          | exactly two syllables, both free          |

`**` is the only thing that is not a single syllable: it stands for any number
of them, **including none**, so `zhong ** guo` matches both `[中, 国]` and
`[中, 人, 民, 国]`. `*` keeps the meaning it has in a one-syllable pattern —
one free syllable — so a bare `*` is a phrase of exactly one.

```sql
SELECT pinyin_match(s, 'zhong ** guo') FROM phrases(s);
SELECT pinyin_match(s, 'p **') FROM phrases(s);   -- starts with a p syllable
```

### What the first argument may be

With both overloads registered, the two are different types, so a **bare string
literal** does not say which is meant and DuckDB will not guess:

```sql
SELECT pinyin_match('zhong1', 'zhong');          -- Binder Error
SELECT pinyin_match('zhong1'::PINYIN, 'zhong');  -- true
SELECT pinyin_match(['zhong1']::PINYIN[], '**'); -- true
```

Every column and every value with a type still binds on its own: a `PINYIN`
column, a `PINYIN[]` column, and a `VARCHAR` column all reach the right overload.

A `USMALLINT` column does **not** bind implicitly — an arbitrary integer has not
been parsed into a syllable, so it takes an explicit cast. An integer literal
folds at bind time and does bind.

A `USMALLINT[]` column *does* bind. Parquet and every other round trip keep the
storage but drop the alias, so a `PINYIN[]` written out and read back is a
`USMALLINT[]`; an implicit `USMALLINT[] -> PINYIN[]` cast re-tags it so such a
column still reaches the array overload without a hand-written `::PINYIN[]`. The
cast moves nothing — the u16s are untouched — and it is registered only at the
list level, so the scalar rule above is unaffected.

NULL on either side gives NULL, as does a NULL *element* inside the list: there
is no syllable there to have matched. A pattern that names nothing legal raises
`Invalid pinyin pattern` rather than quietly matching nothing, and a value that
is not a syllable raises `Invalid pinyin syllable`. `**` in the one-syllable
overload is such a pattern — a single value can never be a run of syllables.

Three things worth knowing:

- A 声调 **class** cannot be said by a single pattern, because 平声 spans two
  codes. Reach for `s::USMALLINT & 192` instead (see above).
- A 声母 with the variant bit ignored — "`z` or `zh`" — likewise: `s::USMALLINT & 31` is the base letter, so `(s::USMALLINT & 31) = 22` covers both `z` and
  `zh`, and `= 4` covers both `c` and `ch`. A pattern cannot, because the
  variant-select bit is part of the 声母 field.
- `&` is the reference's 伪声母 R, which exists for the erhua tail `-r` (`huar`
  → h·ua + r). A `PINYIN` value is strictly **one** syllable, so nothing ever
  carries that initial and `&` currently matches nothing. It is parsed, and kept,
  because it is the reference's glyph and would be the right spelling if erhua
  were ever added.

## Where the table comes from

`src/pinyin_data.rs` is generated and **must not be edited by hand**:

```shell
python tools/gen_pinyin_table.py tools/pinyin-data-0.15.0.txt src/pinyin_data.rs
```

It is built from [`mozillazg/pinyin-data`](https://github.com/mozillazg/pinyin-data)
(MIT), which supplies the *inventory* of 426 syllables. The bit layout and the
orthography-to-phoneme split follow
[`WFLing-seaer/pinyinparser`](https://github.com/WFLing-seaer/pinyinparser) —
which carries no license, so no part of it is copied. Only the encoding rules
are, reimplemented in `src/pinyin.rs`; `tools/gen_pinyin_table.py` states them.

## Testing

`test/sql/pinyin.test` is a SQLLogicTest, the same format DuckDB's own tests
use, run through the DuckDB Python client that `make configure` installed.

```shell
make test_release     # or `make test_debug`
```

The suite is the only spec for this extension, so it is written to be read. Its
sections follow the order the type is described above — the type itself,
encoding, rendering, casts, `pinyin_match`, what the first argument may be, the
array overload, nulls, invalid patterns, the round trip through Parquet — and
every block says what it is pinning down rather than just what it expects.

`make test_*` does **not** rebuild — it runs whatever is already in
`build/<profile>/`. Run `make debug` or `make release` first, or the suite
tests a binary older than the source.

### Version switching

To test against a different DuckDB, throw away the configure step first:

```shell
make clean_all
DUCKDB_TEST_VERSION=v1.3.2 make configure
make debug
make test_debug
```
