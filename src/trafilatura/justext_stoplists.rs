//! `justext_stoplists` — Stage 5a: vendored jusText language stoplists.
//!
//! Source: `justext` package (`justext/stoplists/*.txt`) — 100 languages,
//! newline-delimited UTF-8 word lists used by `justext.core.classify_paragraphs`
//! (paragraph good/bad classification) and consumed in the Rust port by
//! Stage 5c.
//!
//! # Python source (line-cites)
//!
//! - `justext.utils.get_stoplist(language)` at `justext/utils.py:51-63` —
//!   reads `stoplists/<language>.txt`, splits on lines, decodes UTF-8,
//!   lowercases each word, returns a `frozenset`. Raises `ValueError`
//!   on unknown language.
//! - `justext.utils.get_stoplists()` at `justext/utils.py:37-48` —
//!   `os.listdir`s the `stoplists/` directory, strips `.txt`, returns a
//!   `frozenset` of language names.
//!
//! # Faithful divergences (documented)
//!
//! 1. **Return type**: Python returns `frozenset[str]`; Rust returns
//!    `&'static [String]` (lazily populated `Vec<String>` borrowed from a
//!    `OnceLock`). The set-vs-slice distinction is not load-bearing — the
//!    Stage 5c paragraph classifier uses membership tests (`contains`),
//!    which both shapes support. We pick `Vec<String>` because the
//!    `.lower()` step (utils.py:63) requires OWNED strings, which forbids
//!    a zero-copy `&'static [&'static str]` view over `include_str!`'d
//!    data (most stoplists have mixed case in source).
//! 2. **Unknown language**: Python raises `ValueError`; Rust returns an
//!    empty slice. Stage 5c's caller (`classify_paragraphs`) will check
//!    `is_empty()` and fall back; this is a more idiomatic Rust shape
//!    than panicking, and the Python ValueError is a fail-fast signal
//!    that maps cleanly to "no classification possible" in the cascade.
//!    The companion `get_stoplists()` lets a caller pre-check.
//! 3. **Lazy initialization**: Each language's parsed `Vec<String>` is
//!    populated on first access via a per-language `OnceLock`. The matched
//!    Python call site (pkgutil.get_data, decode, splitlines, lower,
//!    frozenset) runs on every call; we cache it. This is a performance
//!    refinement, not a behavioural change.
//!
//! # Implementation choice: `OnceLock<Vec<String>>` (not `lazy_static`)
//!
//! - `OnceLock` is in the standard library (Rust 1.70+) — no new
//!   dependency, matching `readability_fork.rs`'s precedent for its six
//!   regex slots.
//! - `Vec<String>` (not `Vec<&'static str>`) because Python lowercases
//!   each word, which produces new strings for the 94/100 stoplists that
//!   contain uppercase content. Storing owned strings avoids a
//!   per-call `to_lowercase` and keeps `get_stoplist` zero-cost after
//!   first access.
//! - Per-language `OnceLock` slots (rather than a single
//!   `OnceLock<HashMap<&str, Vec<String>>>`) so unused languages never
//!   parse their data — most consumers will only ever touch a handful.

use std::sync::OnceLock;

/// All vendored language names, in sorted order. Matches the Python
/// `get_stoplists()` return value (a `frozenset` of language names) but
/// flattened to a slice for cheap iteration.
///
/// Python source: `justext/utils.py:37-48`.
pub const LANGUAGES: &[&str] = &[
    "Afrikaans",
    "Albanian",
    "Arabic",
    "Aragonese",
    "Armenian",
    "Aromanian",
    "Asturian",
    "Azerbaijani",
    "Basque",
    "Belarusian",
    "Belarusian_Taraskievica",
    "Bengali",
    "Bishnupriya_Manipuri",
    "Bosnian",
    "Breton",
    "Bulgarian",
    "Catalan",
    "Cebuano",
    "Chuvash",
    "Croatian",
    "Czech",
    "Danish",
    "Dutch",
    "English",
    "Esperanto",
    "Estonian",
    "Finnish",
    "French",
    "Galician",
    "Georgian",
    "German",
    "Greek",
    "Gujarati",
    "Haitian",
    "Hebrew",
    "Hindi",
    "Hungarian",
    "Icelandic",
    "Ido",
    "Igbo",
    "Indonesian",
    "Irish",
    "Italian",
    "Javanese",
    "Kannada",
    "Kazakh",
    "Korean",
    "Kurdish",
    "Kyrgyz",
    "Latin",
    "Latvian",
    "Lithuanian",
    "Lombard",
    "Low_Saxon",
    "Luxembourgish",
    "Macedonian",
    "Malay",
    "Malayalam",
    "Maltese",
    "Marathi",
    "Neapolitan",
    "Nepali",
    "Newar",
    "Norwegian_Bokmal",
    "Norwegian_Nynorsk",
    "Occitan",
    "Persian",
    "Piedmontese",
    "Polish",
    "Portuguese",
    "Quechua",
    "Romanian",
    "Russian",
    "Samogitian",
    "Serbian",
    "Serbo_Croatian",
    "Sicilian",
    "Simple_English",
    "Slovak",
    "Slovenian",
    "Spanish",
    "Sundanese",
    "Swahili",
    "Swedish",
    "Tagalog",
    "Tamil",
    "Telugu",
    "Turkish",
    "Turkmen",
    "Ukrainian",
    "Urdu",
    "Uzbek",
    "Vietnamese",
    "Volapuk",
    "Walloon",
    "Waray_Waray",
    "Welsh",
    "West_Frisian",
    "Western_Panjabi",
    "Yoruba",
];

/// Returns the list of all available language names.
///
/// Python source: `justext.utils.get_stoplists` at `justext/utils.py:37-48`.
/// Python returns a `frozenset`; Rust returns the underlying `&'static [&str]`
/// (see [`LANGUAGES`]).
pub fn get_stoplists() -> &'static [&'static str] {
    LANGUAGES
}

/// Returns the lowercased, line-split stoplist for `language`, or an
/// empty slice if the language is not vendored.
///
/// Python source: `justext.utils.get_stoplist` at `justext/utils.py:51-63`.
/// Faithful divergences:
///  - Unknown language → `&[]` (not `ValueError`), per module-level rationale.
///  - Returns `&[String]` (not `frozenset[str]`); slice may contain
///    duplicates if the source file did (Python's `frozenset` would
///    collapse them, but Stage 5c only uses membership tests).
///
/// First-access latency: O(N) split + lowercase of the language's
/// stoplist file; subsequent calls return the cached slice unchanged.
///
/// # Language naming
///
/// Names match Python jusText's filename convention — capitalized
/// language name with underscores for spaces (e.g. `"English"`,
/// `"French"`, `"Norwegian_Bokmal"`, `"Simple_English"`), NOT ISO 639-1
/// codes. See [`LANGUAGES`] for the full set.
pub fn get_stoplist(language: &str) -> &'static [String] {
    let slot = match language {
        "Afrikaans" => &SLOT_AFRIKAANS,
        "Albanian" => &SLOT_ALBANIAN,
        "Arabic" => &SLOT_ARABIC,
        "Aragonese" => &SLOT_ARAGONESE,
        "Armenian" => &SLOT_ARMENIAN,
        "Aromanian" => &SLOT_AROMANIAN,
        "Asturian" => &SLOT_ASTURIAN,
        "Azerbaijani" => &SLOT_AZERBAIJANI,
        "Basque" => &SLOT_BASQUE,
        "Belarusian" => &SLOT_BELARUSIAN,
        "Belarusian_Taraskievica" => &SLOT_BELARUSIAN_TARASKIEVICA,
        "Bengali" => &SLOT_BENGALI,
        "Bishnupriya_Manipuri" => &SLOT_BISHNUPRIYA_MANIPURI,
        "Bosnian" => &SLOT_BOSNIAN,
        "Breton" => &SLOT_BRETON,
        "Bulgarian" => &SLOT_BULGARIAN,
        "Catalan" => &SLOT_CATALAN,
        "Cebuano" => &SLOT_CEBUANO,
        "Chuvash" => &SLOT_CHUVASH,
        "Croatian" => &SLOT_CROATIAN,
        "Czech" => &SLOT_CZECH,
        "Danish" => &SLOT_DANISH,
        "Dutch" => &SLOT_DUTCH,
        "English" => &SLOT_ENGLISH,
        "Esperanto" => &SLOT_ESPERANTO,
        "Estonian" => &SLOT_ESTONIAN,
        "Finnish" => &SLOT_FINNISH,
        "French" => &SLOT_FRENCH,
        "Galician" => &SLOT_GALICIAN,
        "Georgian" => &SLOT_GEORGIAN,
        "German" => &SLOT_GERMAN,
        "Greek" => &SLOT_GREEK,
        "Gujarati" => &SLOT_GUJARATI,
        "Haitian" => &SLOT_HAITIAN,
        "Hebrew" => &SLOT_HEBREW,
        "Hindi" => &SLOT_HINDI,
        "Hungarian" => &SLOT_HUNGARIAN,
        "Icelandic" => &SLOT_ICELANDIC,
        "Ido" => &SLOT_IDO,
        "Igbo" => &SLOT_IGBO,
        "Indonesian" => &SLOT_INDONESIAN,
        "Irish" => &SLOT_IRISH,
        "Italian" => &SLOT_ITALIAN,
        "Javanese" => &SLOT_JAVANESE,
        "Kannada" => &SLOT_KANNADA,
        "Kazakh" => &SLOT_KAZAKH,
        "Korean" => &SLOT_KOREAN,
        "Kurdish" => &SLOT_KURDISH,
        "Kyrgyz" => &SLOT_KYRGYZ,
        "Latin" => &SLOT_LATIN,
        "Latvian" => &SLOT_LATVIAN,
        "Lithuanian" => &SLOT_LITHUANIAN,
        "Lombard" => &SLOT_LOMBARD,
        "Low_Saxon" => &SLOT_LOW_SAXON,
        "Luxembourgish" => &SLOT_LUXEMBOURGISH,
        "Macedonian" => &SLOT_MACEDONIAN,
        "Malay" => &SLOT_MALAY,
        "Malayalam" => &SLOT_MALAYALAM,
        "Maltese" => &SLOT_MALTESE,
        "Marathi" => &SLOT_MARATHI,
        "Neapolitan" => &SLOT_NEAPOLITAN,
        "Nepali" => &SLOT_NEPALI,
        "Newar" => &SLOT_NEWAR,
        "Norwegian_Bokmal" => &SLOT_NORWEGIAN_BOKMAL,
        "Norwegian_Nynorsk" => &SLOT_NORWEGIAN_NYNORSK,
        "Occitan" => &SLOT_OCCITAN,
        "Persian" => &SLOT_PERSIAN,
        "Piedmontese" => &SLOT_PIEDMONTESE,
        "Polish" => &SLOT_POLISH,
        "Portuguese" => &SLOT_PORTUGUESE,
        "Quechua" => &SLOT_QUECHUA,
        "Romanian" => &SLOT_ROMANIAN,
        "Russian" => &SLOT_RUSSIAN,
        "Samogitian" => &SLOT_SAMOGITIAN,
        "Serbian" => &SLOT_SERBIAN,
        "Serbo_Croatian" => &SLOT_SERBO_CROATIAN,
        "Sicilian" => &SLOT_SICILIAN,
        "Simple_English" => &SLOT_SIMPLE_ENGLISH,
        "Slovak" => &SLOT_SLOVAK,
        "Slovenian" => &SLOT_SLOVENIAN,
        "Spanish" => &SLOT_SPANISH,
        "Sundanese" => &SLOT_SUNDANESE,
        "Swahili" => &SLOT_SWAHILI,
        "Swedish" => &SLOT_SWEDISH,
        "Tagalog" => &SLOT_TAGALOG,
        "Tamil" => &SLOT_TAMIL,
        "Telugu" => &SLOT_TELUGU,
        "Turkish" => &SLOT_TURKISH,
        "Turkmen" => &SLOT_TURKMEN,
        "Ukrainian" => &SLOT_UKRAINIAN,
        "Urdu" => &SLOT_URDU,
        "Uzbek" => &SLOT_UZBEK,
        "Vietnamese" => &SLOT_VIETNAMESE,
        "Volapuk" => &SLOT_VOLAPUK,
        "Walloon" => &SLOT_WALLOON,
        "Waray_Waray" => &SLOT_WARAY_WARAY,
        "Welsh" => &SLOT_WELSH,
        "West_Frisian" => &SLOT_WEST_FRISIAN,
        "Western_Panjabi" => &SLOT_WESTERN_PANJABI,
        "Yoruba" => &SLOT_YORUBA,
        _ => return &[],
    };

    let raw = match language {
        "Afrikaans" => include_str!("justext_stoplists/Afrikaans.txt"),
        "Albanian" => include_str!("justext_stoplists/Albanian.txt"),
        "Arabic" => include_str!("justext_stoplists/Arabic.txt"),
        "Aragonese" => include_str!("justext_stoplists/Aragonese.txt"),
        "Armenian" => include_str!("justext_stoplists/Armenian.txt"),
        "Aromanian" => include_str!("justext_stoplists/Aromanian.txt"),
        "Asturian" => include_str!("justext_stoplists/Asturian.txt"),
        "Azerbaijani" => include_str!("justext_stoplists/Azerbaijani.txt"),
        "Basque" => include_str!("justext_stoplists/Basque.txt"),
        "Belarusian" => include_str!("justext_stoplists/Belarusian.txt"),
        "Belarusian_Taraskievica" => {
            include_str!("justext_stoplists/Belarusian_Taraskievica.txt")
        }
        "Bengali" => include_str!("justext_stoplists/Bengali.txt"),
        "Bishnupriya_Manipuri" => include_str!("justext_stoplists/Bishnupriya_Manipuri.txt"),
        "Bosnian" => include_str!("justext_stoplists/Bosnian.txt"),
        "Breton" => include_str!("justext_stoplists/Breton.txt"),
        "Bulgarian" => include_str!("justext_stoplists/Bulgarian.txt"),
        "Catalan" => include_str!("justext_stoplists/Catalan.txt"),
        "Cebuano" => include_str!("justext_stoplists/Cebuano.txt"),
        "Chuvash" => include_str!("justext_stoplists/Chuvash.txt"),
        "Croatian" => include_str!("justext_stoplists/Croatian.txt"),
        "Czech" => include_str!("justext_stoplists/Czech.txt"),
        "Danish" => include_str!("justext_stoplists/Danish.txt"),
        "Dutch" => include_str!("justext_stoplists/Dutch.txt"),
        "English" => include_str!("justext_stoplists/English.txt"),
        "Esperanto" => include_str!("justext_stoplists/Esperanto.txt"),
        "Estonian" => include_str!("justext_stoplists/Estonian.txt"),
        "Finnish" => include_str!("justext_stoplists/Finnish.txt"),
        "French" => include_str!("justext_stoplists/French.txt"),
        "Galician" => include_str!("justext_stoplists/Galician.txt"),
        "Georgian" => include_str!("justext_stoplists/Georgian.txt"),
        "German" => include_str!("justext_stoplists/German.txt"),
        "Greek" => include_str!("justext_stoplists/Greek.txt"),
        "Gujarati" => include_str!("justext_stoplists/Gujarati.txt"),
        "Haitian" => include_str!("justext_stoplists/Haitian.txt"),
        "Hebrew" => include_str!("justext_stoplists/Hebrew.txt"),
        "Hindi" => include_str!("justext_stoplists/Hindi.txt"),
        "Hungarian" => include_str!("justext_stoplists/Hungarian.txt"),
        "Icelandic" => include_str!("justext_stoplists/Icelandic.txt"),
        "Ido" => include_str!("justext_stoplists/Ido.txt"),
        "Igbo" => include_str!("justext_stoplists/Igbo.txt"),
        "Indonesian" => include_str!("justext_stoplists/Indonesian.txt"),
        "Irish" => include_str!("justext_stoplists/Irish.txt"),
        "Italian" => include_str!("justext_stoplists/Italian.txt"),
        "Javanese" => include_str!("justext_stoplists/Javanese.txt"),
        "Kannada" => include_str!("justext_stoplists/Kannada.txt"),
        "Kazakh" => include_str!("justext_stoplists/Kazakh.txt"),
        "Korean" => include_str!("justext_stoplists/Korean.txt"),
        "Kurdish" => include_str!("justext_stoplists/Kurdish.txt"),
        "Kyrgyz" => include_str!("justext_stoplists/Kyrgyz.txt"),
        "Latin" => include_str!("justext_stoplists/Latin.txt"),
        "Latvian" => include_str!("justext_stoplists/Latvian.txt"),
        "Lithuanian" => include_str!("justext_stoplists/Lithuanian.txt"),
        "Lombard" => include_str!("justext_stoplists/Lombard.txt"),
        "Low_Saxon" => include_str!("justext_stoplists/Low_Saxon.txt"),
        "Luxembourgish" => include_str!("justext_stoplists/Luxembourgish.txt"),
        "Macedonian" => include_str!("justext_stoplists/Macedonian.txt"),
        "Malay" => include_str!("justext_stoplists/Malay.txt"),
        "Malayalam" => include_str!("justext_stoplists/Malayalam.txt"),
        "Maltese" => include_str!("justext_stoplists/Maltese.txt"),
        "Marathi" => include_str!("justext_stoplists/Marathi.txt"),
        "Neapolitan" => include_str!("justext_stoplists/Neapolitan.txt"),
        "Nepali" => include_str!("justext_stoplists/Nepali.txt"),
        "Newar" => include_str!("justext_stoplists/Newar.txt"),
        "Norwegian_Bokmal" => include_str!("justext_stoplists/Norwegian_Bokmal.txt"),
        "Norwegian_Nynorsk" => include_str!("justext_stoplists/Norwegian_Nynorsk.txt"),
        "Occitan" => include_str!("justext_stoplists/Occitan.txt"),
        "Persian" => include_str!("justext_stoplists/Persian.txt"),
        "Piedmontese" => include_str!("justext_stoplists/Piedmontese.txt"),
        "Polish" => include_str!("justext_stoplists/Polish.txt"),
        "Portuguese" => include_str!("justext_stoplists/Portuguese.txt"),
        "Quechua" => include_str!("justext_stoplists/Quechua.txt"),
        "Romanian" => include_str!("justext_stoplists/Romanian.txt"),
        "Russian" => include_str!("justext_stoplists/Russian.txt"),
        "Samogitian" => include_str!("justext_stoplists/Samogitian.txt"),
        "Serbian" => include_str!("justext_stoplists/Serbian.txt"),
        "Serbo_Croatian" => include_str!("justext_stoplists/Serbo_Croatian.txt"),
        "Sicilian" => include_str!("justext_stoplists/Sicilian.txt"),
        "Simple_English" => include_str!("justext_stoplists/Simple_English.txt"),
        "Slovak" => include_str!("justext_stoplists/Slovak.txt"),
        "Slovenian" => include_str!("justext_stoplists/Slovenian.txt"),
        "Spanish" => include_str!("justext_stoplists/Spanish.txt"),
        "Sundanese" => include_str!("justext_stoplists/Sundanese.txt"),
        "Swahili" => include_str!("justext_stoplists/Swahili.txt"),
        "Swedish" => include_str!("justext_stoplists/Swedish.txt"),
        "Tagalog" => include_str!("justext_stoplists/Tagalog.txt"),
        "Tamil" => include_str!("justext_stoplists/Tamil.txt"),
        "Telugu" => include_str!("justext_stoplists/Telugu.txt"),
        "Turkish" => include_str!("justext_stoplists/Turkish.txt"),
        "Turkmen" => include_str!("justext_stoplists/Turkmen.txt"),
        "Ukrainian" => include_str!("justext_stoplists/Ukrainian.txt"),
        "Urdu" => include_str!("justext_stoplists/Urdu.txt"),
        "Uzbek" => include_str!("justext_stoplists/Uzbek.txt"),
        "Vietnamese" => include_str!("justext_stoplists/Vietnamese.txt"),
        "Volapuk" => include_str!("justext_stoplists/Volapuk.txt"),
        "Walloon" => include_str!("justext_stoplists/Walloon.txt"),
        "Waray_Waray" => include_str!("justext_stoplists/Waray_Waray.txt"),
        "Welsh" => include_str!("justext_stoplists/Welsh.txt"),
        "West_Frisian" => include_str!("justext_stoplists/West_Frisian.txt"),
        "Western_Panjabi" => include_str!("justext_stoplists/Western_Panjabi.txt"),
        "Yoruba" => include_str!("justext_stoplists/Yoruba.txt"),
        _ => unreachable!("language match arm above guarded this"),
    };

    slot.get_or_init(|| parse_stoplist(raw)).as_slice()
}

/// Parses a raw stoplist file (newline-delimited words) into a `Vec<String>`
/// matching Python `frozenset(w.decode("utf8").lower() for w in stopwords.splitlines())`
/// at `justext/utils.py:63`.
///
/// Note: we do NOT deduplicate. Python's `frozenset` collapses duplicates;
/// our slice may include them. Membership tests behave identically, which
/// is all Stage 5c's `classify_paragraphs` uses (see
/// `justext.core.classify_paragraphs:160-167`).
fn parse_stoplist(raw: &'static str) -> Vec<String> {
    raw.lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.to_lowercase())
        .collect()
}

// ===========================================================================
// Per-language OnceLock slots
// ===========================================================================
//
// One slot per vendored language. Populated on first call to
// `get_stoplist(language)`; subsequent calls return the cached slice.
// Unused languages never parse their data.

static SLOT_AFRIKAANS: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ALBANIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ARABIC: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ARAGONESE: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ARMENIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_AROMANIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ASTURIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_AZERBAIJANI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_BASQUE: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_BELARUSIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_BELARUSIAN_TARASKIEVICA: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_BENGALI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_BISHNUPRIYA_MANIPURI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_BOSNIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_BRETON: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_BULGARIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_CATALAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_CEBUANO: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_CHUVASH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_CROATIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_CZECH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_DANISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_DUTCH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ENGLISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ESPERANTO: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ESTONIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_FINNISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_FRENCH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_GALICIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_GEORGIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_GERMAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_GREEK: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_GUJARATI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_HAITIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_HEBREW: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_HINDI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_HUNGARIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ICELANDIC: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_IDO: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_IGBO: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_INDONESIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_IRISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ITALIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_JAVANESE: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_KANNADA: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_KAZAKH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_KOREAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_KURDISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_KYRGYZ: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_LATIN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_LATVIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_LITHUANIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_LOMBARD: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_LOW_SAXON: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_LUXEMBOURGISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_MACEDONIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_MALAY: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_MALAYALAM: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_MALTESE: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_MARATHI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_NEAPOLITAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_NEPALI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_NEWAR: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_NORWEGIAN_BOKMAL: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_NORWEGIAN_NYNORSK: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_OCCITAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_PERSIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_PIEDMONTESE: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_POLISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_PORTUGUESE: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_QUECHUA: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_ROMANIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_RUSSIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SAMOGITIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SERBIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SERBO_CROATIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SICILIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SIMPLE_ENGLISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SLOVAK: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SLOVENIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SPANISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SUNDANESE: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SWAHILI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_SWEDISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_TAGALOG: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_TAMIL: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_TELUGU: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_TURKISH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_TURKMEN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_UKRAINIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_URDU: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_UZBEK: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_VIETNAMESE: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_VOLAPUK: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_WALLOON: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_WARAY_WARAY: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_WELSH: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_WEST_FRISIAN: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_WESTERN_PANJABI: OnceLock<Vec<String>> = OnceLock::new();
static SLOT_YORUBA: OnceLock<Vec<String>> = OnceLock::new();

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// `get_stoplist("English")` returns a non-empty list with the expected
    /// magnitude. The vendored English stoplist has 503 lines (with
    /// duplicates) yielding > 100 unique entries.
    #[test]
    fn get_stoplist_english_returns_nonempty() {
        let english = get_stoplist("English");
        assert!(
            english.len() > 100,
            "English stoplist should have > 100 entries (got {})",
            english.len()
        );
        // Spot-check well-known English stopwords (Python lowercases all).
        assert!(english.iter().any(|w| w == "the"));
        assert!(english.iter().any(|w| w == "and"));
    }

    /// Unknown language returns an empty slice (faithful divergence from
    /// Python's `ValueError`; documented at module header).
    #[test]
    fn get_stoplist_unknown_language_returns_empty() {
        assert!(get_stoplist("Klingon").is_empty());
        assert!(get_stoplist("").is_empty());
        // ISO 639-1 codes are NOT supported — name convention is the
        // capitalized full name (per `justext.utils.get_stoplist`).
        assert!(get_stoplist("en").is_empty());
        assert!(get_stoplist("fr").is_empty());
    }

    /// French stoplist contains the documented set of articles + prepositions.
    /// Verifies the language-name → file routing AND the lowercase
    /// transformation (the French source file has mixed case).
    #[test]
    fn get_stoplist_french_has_known_words() {
        let french = get_stoplist("French");
        let expected: &[&str] = &["le", "la", "les", "de"];
        for word in expected {
            assert!(
                french.iter().any(|w| w == *word),
                "French stoplist missing expected word {word:?}"
            );
        }
    }

    /// `get_stoplists()` returns the 100 vendored language names.
    #[test]
    fn get_stoplists_returns_all_languages() {
        let langs = get_stoplists();
        assert_eq!(
            langs.len(),
            100,
            "expected 100 vendored stoplists, got {}",
            langs.len()
        );
        // Spot-check the boundary entries.
        assert_eq!(langs.first(), Some(&"Afrikaans"));
        assert_eq!(langs.last(), Some(&"Yoruba"));
        // Spot-check a multi-word name (underscore convention).
        assert!(langs.contains(&"Simple_English"));
        assert!(langs.contains(&"Norwegian_Bokmal"));
    }

    /// Every name returned by `get_stoplists()` must yield a non-empty
    /// `get_stoplist()` result. Catches: a typo in the LANGUAGES array,
    /// a missing vendored file, an unwired match arm.
    #[test]
    fn get_stoplist_consistent_with_get_stoplists() {
        for &lang in get_stoplists() {
            let words = get_stoplist(lang);
            assert!(
                !words.is_empty(),
                "language {lang:?} is in get_stoplists() but get_stoplist() returned empty"
            );
        }
    }
}
