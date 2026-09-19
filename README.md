# DuckDB Rust extension template
This is an **experimental** template for Rust based extensions based on the C Extension API of DuckDB. The goal is to
turn this eventually into a stable basis for pure-Rust DuckDB extensions that can be submitted to the Community extensions
repository

Features:
- No DuckDB build required
- No C++ or C code required
- CI/CD chain preconfigured
- (Coming soon) Works with community extensions

## Cloning

Clone the repo with submodules

```shell
git clone --recurse-submodules <repo>
```

## Dependencies
In principle, these extensions can be compiled with the Rust toolchain alone. However, this template relies on some additional
tooling to make life a little easier and to be able to share CI/CD infrastructure with extension templates for other languages:

- Python3
- Python3-venv
- [Make](https://www.gnu.org/software/make)
- Git

Installing these dependencies will vary per platform:
- For Linux, these come generally pre-installed or are available through the distro-specific package manager.
- For MacOS, [homebrew](https://formulae.brew.sh/).
- For Windows, [chocolatey](https://community.chocolatey.org/).

## Building
After installing the dependencies, building is a two-step process. Firstly run:
```shell
make configure
```
This will ensure a Python venv is set up with DuckDB and DuckDB's test runner installed. Additionally, depending on configuration,
DuckDB will be used to determine the correct platform for which you are compiling.

Then, to build the extension run:
```shell
make debug
```
This delegates the build process to cargo, which will produce a shared library in `target/debug/<shared_lib_name>`. After this step,
a script is run to transform the shared library into a loadable extension by appending a binary footer. The resulting extension is written
to the `build/debug` directory.

To create optimized release binaries, simply run `make release` instead.

### Running the extension
To run the extension code, start `duckdb` with `-unsigned` flag. This will allow you to load the local extension file.

```sh
duckdb -unsigned
```

After loading the extension by the file path, you can use the functions provided by the extension. This template registers
the `rusty_echo()` scalar function and the `rusty_quack()` table function.

```sql
LOAD './build/debug/extension/rusty_quack/rusty_quack.duckdb_extension';
SELECT rusty_echo('Jane');
```

```
┌─────────────────────┐
│ rusty_echo('Jane')  │
│       varchar       │
├─────────────────────┤
│ 🐤 Jane 🦀 Jane     │
└─────────────────────┘
```

```sql
SELECT * FROM rusty_quack('Jane');
```

```
┌─────────────────────┐
│       column0       │
│       varchar       │
├─────────────────────┤
│ Rusty Quack Jane 🐥 │
└─────────────────────┘
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

| field | mask | meaning |
|---|---|---|
| 声母 | `0x801F` | base plus the variant bit, so `ch` is one value, not `c` + something |
| 韵母 | `0x7F00` | |
| 声调 | `0x00E0` | `0x00` no tone written, `0x20` an explicit `0`, `0x60` 轻声, `0x80` 阴平, `0xA0` 阳平, `0xC0` 上声, `0xE0` 去声 |
| syllable | `0xFF1F` | everything but the 声调 |

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
SELECT ['zhong1', 'guo2']::PINYIN[];        -- [zhōng, guó]
SELECT unnest(['zhong1', 'guo2']::PINYIN[]);
SELECT list_filter(['zhong1', 'guo2', 'shǎng']::PINYIN[], lambda s: pinyin_match(s, 'zh'));
```

### `pinyin_match(PINYIN, VARCHAR) -> BOOLEAN`

A pattern is a syllable written the same way a value is, except that any part of
it may be replaced by a wildcard — and **a part that is simply not written is a
wildcard too**. Matching is one masked compare on the packed value, so a filter
costs a load and an `and`.

| pattern | means | mask |
|---|---|---|
| `p?`, `p` | 声母 p, everything else free | `& 0x801F == 0x000E` |
| `?ang` | 韵母 ang, 声母 and 声调 free | `& 0x7F00 == 0x0600` |
| `zhong` | 声母 and 韵母 of `zhong` | `& 0xFF1F == 0x8E16` |
| `zhong1` | ... and 阴平 | `& 0xFFFF == 0x8E96` |
| `1` | any syllable in 阴平 | `& 0x00E0 == 0x0080` |
| `?`, `*` | any syllable | matches all |
| `/` | 零声母 | `& 0x801F == 0x0002` |
| `&` | 伪声母 R — see below | `& 0x801F == 0x8010` |

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

| pattern | means |
|---|---|
| `zhong guo` | exactly two syllables, 中 then 国 |
| `zhong ** guo` | 中, then any number of syllables, then 国 |
| `zhong **` | starts with 中 |
| `p **` | starts with a 声母-p syllable |
| `* *` | exactly two syllables, both free |

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

NULL on either side gives NULL, as does a NULL *element* inside the list: there
is no syllable there to have matched. A pattern that names nothing legal raises
`Invalid pinyin pattern` rather than quietly matching nothing, and a value that
is not a syllable raises `Invalid pinyin syllable`. `**` in the one-syllable
overload is such a pattern — a single value can never be a run of syllables.

Three things worth knowing:

- A 声调 **class** cannot be said by a single pattern, because 平声 spans two
  codes. Reach for `s::USMALLINT & 192` instead (see above).
- A 声母 with the variant bit ignored — "`z` or `zh`" — likewise: `s::USMALLINT
  & 31` is the base letter, so `(s::USMALLINT & 31) = 22` covers both `z` and
  `zh`, and `= 4` covers both `c` and `ch`. A pattern cannot, because the
  variant-select bit is part of the 声母 field.
- `&` is the reference's 伪声母 R, which exists for the erhua tail `-r` (`huar`
  → h·ua + r). A `PINYIN` value is strictly **one** syllable, so nothing ever
  carries that initial and `&` currently matches nothing. It is parsed, and kept,
  because it is the reference's glyph and would be the right spelling if erhua
  were ever added.

### Where the table comes from

`src/pinyin_data.rs` is generated and **must not be edited by hand**:

```shell
python tools/gen_pinyin_table.py tools/pinyin-data-0.15.0.txt src/pinyin_data.rs
```

It is built from [`mozillazg/pinyin-data`](https://github.com/mozillazg/pinyin-data)
(MIT), which supplies the *inventory* of 426 syllables. The bit layout and the
orthography-to-phoneme split follow `WFLing-seaer/pinyinparser`. That repository
carries no license, so no part of it is copied — only the encoding rules, which
are reimplemented here. See `tools/gen_pinyin_table.py` for the rules themselves.

## Testing
This extension uses the DuckDB Python client for testing. This should be automatically installed in the `make configure` step.
The tests themselves are written in the SQLLogicTest format, just like most of DuckDB's tests. A sample test can be found in
`test/sql/<extension_name>.test`. To run the tests using the *debug* build:

```shell
make test_debug
```

or for the *release* build:
```shell
make test_release
```

### Version switching
Testing with different DuckDB versions is really simple:

First, run
```
make clean_all
```
to ensure the previous `make configure` step is deleted.

Then, run
```
DUCKDB_TEST_VERSION=v1.3.2 make configure
```
to select a different duckdb version to test with

Finally, build and test with
```
make debug
make test_debug
```

### Known issues
This is a bit of a footgun, but the extensions produced by this template may (or may not) be broken on windows on python3.11
with the following error on extension load:
```shell
IO Error: Extension '<name>.duckdb_extension' could not be loaded: The specified module could not be found
```
This was resolved by using python 3.12
