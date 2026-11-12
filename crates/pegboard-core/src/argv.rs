//! Validation of the agent-browser command line an agent supplies, by an allow-list grammar.
//!
//! agent-browser 0.42.0 reads its global flags (`--session`, `--cdp`, `--profile`, ...) from
//! anywhere on the command line, lets a later occurrence override pegboard's own, and takes
//! the word after a value flag as its value even when that word is `--` (after which it reads
//! flags again). So nothing is passed on unless the grammar below declares it: a known
//! command as the first word, then only that form's flags (with their verified arity) and
//! positional arguments of a declared kind. Every arity was checked against the real parser
//! (see the Task 7 report). Anything else is refused with the offending word named.
//!
//! The rules that keep a word from being read as a global flag:
//! - a word starting with '-' must be a declared flag of the form (a negative number in a
//!   numeric position excepted);
//! - a flag value or positional argument never starts with '-', except text after `--`;
//! - `--` is accepted only where the form says the rest is literal text (`fill`, `type`: the
//!   only commands that honour it), right before that text. The global parser stops there.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use pegboard_protocol::{codes, RpcError};

use crate::proxy::filter::normalize_url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    Run(Vec<String>),
    TabNew { url: String },
}

/// What a positional argument or a flag value may be.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// CSS/XPath selector or `@ref`.
    Selector,
    /// A selector that cannot be mistaken for an image path (`screenshot` tells its two
    /// optional arguments apart by the file extension).
    ShotSelector,
    /// One word of free text.
    Text,
    /// A JavaScript expression: free text that the log masks like `eval`'s.
    Expr,
    /// Judged and normalized like the proxy does (`normalize_url`).
    Url,
    /// A key or combination such as `Enter` or `Control+a`.
    Key,
    /// A finite decimal, possibly negative.
    Number,
    /// `t<N>`, a label, or a 32-hex CDP target id.
    TabRef,
    /// A bare file name with one of these extensions, placed in the session's files dir.
    File(&'static [&'static str]),
    Choice(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy)]
enum Arity {
    /// No value.
    Switch,
    /// Exactly one value.
    Value(Kind),
    /// A global boolean flag: agent-browser takes a following `true`/`false` as its value.
    OptionalBool,
}

struct Flag {
    names: &'static [&'static str],
    arity: Arity,
}

struct Arg {
    kind: Kind,
    required: bool,
    /// The value is a secret (a cookie's or storage entry's value, a dialog's answer): the
    /// action log never keeps it.
    secret: bool,
}

/// What a form does with typed text, which the action log must never record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextUse {
    /// No typed text.
    No,
    /// The words of the rest are typed into (or chosen in) the focused element; the log
    /// hides them when that element is sensitive.
    Typed,
    /// A single key press: typed text when the key is one printable character (or
    /// `Shift+` one); named keys and shortcuts are not.
    Key,
}

/// One accepted form of a command.
struct Form {
    /// Command and subcommand words, matched literally.
    words: &'static [&'static str],
    args: &'static [Arg],
    /// Further words of this kind after `args` (joined into text, or a list of values).
    rest: Option<Kind>,
    rest_required: bool,
    flags: &'static [Flag],
    /// `--` may come right before the rest (literal text, may start with '-').
    literal_rest: bool,
    /// When no `File` argument is given, pegboard names one with this extension, so the
    /// CLI never falls back to a directory of its own.
    default_file: Option<&'static str>,
    text: TextUse,
}

const fn req(kind: Kind) -> Arg {
    Arg {
        kind,
        required: true,
        secret: false,
    }
}

const fn opt(kind: Kind) -> Arg {
    Arg {
        kind,
        required: false,
        secret: false,
    }
}

const fn secret(arg: Arg) -> Arg {
    Arg {
        secret: true,
        ..arg
    }
}

const fn switch(names: &'static [&'static str]) -> Flag {
    Flag {
        names,
        arity: Arity::Switch,
    }
}

const fn value(names: &'static [&'static str], kind: Kind) -> Flag {
    Flag {
        names,
        arity: Arity::Value(kind),
    }
}

const fn form(words: &'static [&'static str], args: &'static [Arg]) -> Form {
    Form {
        words,
        args,
        rest: None,
        rest_required: false,
        flags: &[],
        literal_rest: false,
        default_file: None,
        text: TextUse::No,
    }
}

const fn with_text(f: Form, text: TextUse) -> Form {
    Form { text, ..f }
}

const fn with_flags(f: Form, flags: &'static [Flag]) -> Form {
    Form { flags, ..f }
}

const fn with_rest(f: Form, kind: Kind, required: bool) -> Form {
    Form {
        rest: Some(kind),
        rest_required: required,
        ..f
    }
}

use Kind::*;

const SEL: &[Arg] = &[req(Selector)];
const IMAGE: &[&str] = &["png", "jpg", "jpeg"];
const FIND_ACTIONS: Kind = Choice(&["click", "fill", "check", "hover", "text"]);
const FIND_FLAGS: &[Flag] = &[value(&["--name"], Text), switch(&["--exact"])];
const MOUSE_BUTTON: Kind = Choice(&["left", "right", "middle"]);
const SNAPSHOT_FLAGS: &[Flag] = &[
    switch(&["-i", "--interactive"]),
    switch(&["-u", "--urls"]),
    switch(&["-c", "--compact"]),
    value(&["-d", "--depth"], Number),
    value(&["-s", "--selector"], Selector),
    switch(&["--delta"]),
    switch(&["--full"]),
];
const COOKIE_FLAGS: &[Flag] = &[
    value(&["--url"], Url),
    value(&["--domain"], Text),
    value(&["--path"], Text),
    switch(&["--http-only"]),
    switch(&["--secure"]),
    value(&["--same-site"], Choice(&["Strict", "Lax", "None"])),
    value(&["--expires"], Number),
];
const WAIT_FLAGS: &[Flag] = &[
    value(
        &["--state"],
        Choice(&["visible", "hidden", "attached", "detached"]),
    ),
    value(&["--url"], Text),
    value(&["--text"], Text),
    value(
        &["--load"],
        Choice(&["load", "domcontentloaded", "networkidle"]),
    ),
    value(&["--fn"], Expr),
    value(&["--timeout"], Number),
];

/// The phase 1 command set. Left out on purpose, besides everything pegboard owns:
/// `cookies clear` (`Network.clearBrowserCookies`) and `set media|geo|offline` (Emulation
/// and network-condition methods) need CDP methods the proxy does not allow; `read`,
/// `flow`, `diff`, `upload`, `download`, `network route|har`, `trace`, `profiler`, `inspect`,
/// `webmcp`, `clipboard`, `react`, `pushstate` reach outside the session (local files, the
/// CLI's own network access, an unauthenticated DevTools proxy).
const GRAMMAR: &[Form] = &[
    form(&["open"], &[req(Url)]),
    form(&["back"], &[]),
    form(&["forward"], &[]),
    form(&["reload"], &[]),
    with_flags(form(&["snapshot"], &[]), SNAPSHOT_FLAGS),
    form(&["get", "url"], &[]),
    form(&["get", "title"], &[]),
    form(&["get", "text"], SEL),
    form(&["get", "html"], SEL),
    form(&["get", "value"], SEL),
    form(&["get", "attr"], &[req(Selector), req(Text)]),
    form(&["get", "count"], SEL),
    form(&["get", "box"], SEL),
    form(&["get", "styles"], SEL),
    form(&["is", "visible"], SEL),
    form(&["is", "enabled"], SEL),
    form(&["is", "checked"], SEL),
    with_flags(
        form(&["click"], SEL),
        &[switch(&["--new-tab"]), switch(&["--human"])],
    ),
    form(&["dblclick"], SEL),
    form(&["hover"], SEL),
    form(&["focus"], SEL),
    form(&["check"], SEL),
    form(&["uncheck"], SEL),
    form(&["scrollintoview"], SEL),
    form(&["highlight"], SEL),
    Form {
        literal_rest: true,
        ..with_text(with_rest(form(&["fill"], SEL), Text, true), TextUse::Typed)
    },
    Form {
        literal_rest: true,
        flags: &[switch(&["--clear"]), value(&["--delay"], Number)],
        ..with_text(with_rest(form(&["type"], SEL), Text, true), TextUse::Typed)
    },
    with_text(form(&["press"], &[req(Key)]), TextUse::Key),
    with_text(
        with_rest(form(&["keyboard", "type"], &[]), Text, true),
        TextUse::Typed,
    ),
    with_text(
        with_rest(form(&["keyboard", "inserttext"], &[]), Text, true),
        TextUse::Typed,
    ),
    with_text(
        with_rest(form(&["select"], SEL), Text, true),
        TextUse::Typed,
    ),
    with_flags(
        form(
            &["scroll"],
            &[opt(Choice(&["up", "down", "left", "right"])), opt(Number)],
        ),
        &[value(&["-s", "--selector"], Selector)],
    ),
    with_flags(
        form(&["drag"], &[req(Selector), req(Selector)]),
        &[switch(&["--human"])],
    ),
    with_flags(
        form(&["mouse", "move"], &[req(Number), req(Number)]),
        &[
            value(&["--duration"], Number),
            value(&["--steps"], Number),
            switch(&["--human"]),
            value(&["--seed"], Number),
        ],
    ),
    form(&["mouse", "down"], &[opt(MOUSE_BUTTON)]),
    form(&["mouse", "up"], &[opt(MOUSE_BUTTON)]),
    form(&["mouse", "wheel"], &[req(Number), opt(Number)]),
    find(&["find", "role"], &[req(Text), opt(FIND_ACTIONS)]),
    find(&["find", "text"], &[req(Text), opt(FIND_ACTIONS)]),
    find(&["find", "label"], &[req(Text), opt(FIND_ACTIONS)]),
    find(&["find", "placeholder"], &[req(Text), opt(FIND_ACTIONS)]),
    find(&["find", "alt"], &[req(Text), opt(FIND_ACTIONS)]),
    find(&["find", "title"], &[req(Text), opt(FIND_ACTIONS)]),
    find(&["find", "testid"], &[req(Text), opt(FIND_ACTIONS)]),
    find(&["find", "first"], &[req(Selector), opt(FIND_ACTIONS)]),
    find(&["find", "last"], &[req(Selector), opt(FIND_ACTIONS)]),
    find(
        &["find", "nth"],
        &[req(Number), req(Selector), opt(FIND_ACTIONS)],
    ),
    // A number waits that long; anything else is a selector.
    with_flags(form(&["wait"], &[opt(Selector)]), WAIT_FLAGS),
    with_rest(form(&["eval"], &[]), Text, true),
    Form {
        flags: &[
            switch(&["--full", "-f"]),
            Flag {
                names: &["--annotate"],
                arity: Arity::OptionalBool,
            },
        ],
        default_file: Some("png"),
        ..form(&["screenshot"], &[opt(ShotSelector), opt(File(IMAGE))])
    },
    form(&["pdf"], &[req(File(&["pdf"]))]),
    form(&["tab"], &[opt(TabRef)]),
    form(&["tab", "list"], &[]),
    form(&["tab", "new"], &[opt(Url)]),
    form(&["tab", "close"], &[opt(TabRef)]),
    form(&["frame"], SEL),
    form(&["dialog", "accept"], &[secret(opt(Text))]),
    form(&["dialog", "dismiss"], &[]),
    form(&["dialog", "status"], &[]),
    form(&["cookies"], &[]),
    form(&["cookies", "get"], &[]),
    with_flags(
        form(&["cookies", "set"], &[req(Text), secret(req(Text))]),
        COOKIE_FLAGS,
    ),
    form(&["storage", "local"], &[opt(Text)]),
    form(&["storage", "local", "get"], &[opt(Text)]),
    form(
        &["storage", "local", "set"],
        &[req(Text), secret(req(Text))],
    ),
    form(&["storage", "local", "clear"], &[]),
    form(&["storage", "session"], &[opt(Text)]),
    form(&["storage", "session", "get"], &[opt(Text)]),
    form(
        &["storage", "session", "set"],
        &[req(Text), secret(req(Text))],
    ),
    form(&["storage", "session", "clear"], &[]),
    with_flags(form(&["console"], &[]), &[switch(&["--clear"])]),
    with_flags(form(&["errors"], &[]), &[switch(&["--clear"])]),
    with_flags(
        form(&["network", "requests"], &[]),
        &[
            switch(&["--clear"]),
            value(&["--filter"], Text),
            value(&["--type"], Text),
            value(&["--method"], Text),
            value(&["--status"], Text),
        ],
    ),
    with_flags(
        form(&["a11y"], &[opt(Url)]),
        &[
            value(&["--tags"], Text),
            value(&["-s", "--selector"], Selector),
        ],
    ),
    form(&["vitals"], &[opt(Url)]),
    form(
        &["set", "viewport"],
        &[req(Number), req(Number), opt(Number)],
    ),
];

const fn find(words: &'static [&'static str], args: &'static [Arg]) -> Form {
    Form {
        flags: FIND_FLAGS,
        ..with_text(with_rest(form(words, args), Text, false), TextUse::Typed)
    }
}

fn rejected(message: impl Into<String>) -> RpcError {
    RpcError {
        code: codes::ARGV_REJECTED.to_string(),
        message: message.into(),
        data: None,
    }
}

/// Validates a user-supplied agent-browser command line (without `--session`, `--cdp`,
/// `--json`) and returns the words to run. File names are rewritten to absolute paths in
/// `files_dir` (the session's files directory, which must be absolute), and URLs to the
/// normalized string the proxy will judge.
pub fn check(argv: &[String], files_dir: &Path) -> Result<Checked, RpcError> {
    if !files_dir.is_absolute() {
        return Err(RpcError {
            code: codes::INTERNAL.to_string(),
            message: "the session files directory must be absolute".into(),
            data: None,
        });
    }
    let Some(command) = argv.first() else {
        return Err(rejected("empty command line"));
    };
    if let Some(bad) = argv.iter().find(|w| w.contains('\0')) {
        return Err(rejected(format!("{bad:?} contains a NUL character")));
    }
    let form = form_of(argv).ok_or_else(|| {
        let known = GRAMMAR.iter().any(|f| f.words[0] == command.as_str());
        let what = match argv.get(1) {
            Some(sub) if known => format!("{command} {sub}"),
            _ => command.clone(),
        };
        rejected(format!("'{what}' is not supported in pegboard phase 1"))
    })?;
    let words = parse(form, &argv[form.words.len()..], files_dir)?.words;
    if form.words == ["tab", "new"] {
        let url = words
            .get(2)
            .cloned()
            .unwrap_or_else(|| "about:blank".to_string());
        return Ok(Checked::TabNew { url });
    }
    Ok(Checked::Run(words))
}

/// Where the secret words of a command line are, by the grammar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextAt {
    /// Words typed into (or chosen in) a page element: `fill`, `type`, `select`,
    /// `keyboard type`, `keyboard inserttext`, `find ... fill`, `press` of one character.
    /// They are logged only when the element they went to is known to be harmless.
    pub typed: Vec<usize>,
    /// Values that are never logged: the value of `cookies set` and `storage ... set`,
    /// the answer of `dialog accept`.
    pub always: Vec<usize>,
    /// Words that are URLs (the log keeps them without userinfo, query and fragment).
    pub urls: Vec<usize>,
    /// Words that are JavaScript expressions other than `eval`'s (`wait --fn`).
    pub expr: Vec<usize>,
    /// Where typed text goes.
    pub destination: Option<TextDestination>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextDestination {
    /// The element this selector (or `@ref`) names.
    Selector(String),
    /// Whatever has the focus.
    Focused,
    /// A locator that cannot be asked about as a selector (`find ...`).
    Unknown,
}

/// The positions in `argv` (a command line as the caller gave it) of text the log must not
/// keep as it is, as the grammar knows them. Empty for a command line the grammar does not
/// accept.
pub fn text_at(argv: &[String]) -> TextAt {
    let Some(form) = form_of(argv) else {
        return TextAt::default();
    };
    // Only the shape matters here, so any absolute directory will do.
    match parse(form, &argv[form.words.len()..], Path::new("/")) {
        Ok(parsed) => parsed.text,
        Err(_) => TextAt::default(),
    }
}

/// Typed positions only (see `TextAt::typed`).
pub fn typed_positions(argv: &[String]) -> Vec<usize> {
    text_at(argv).typed
}

/// Typed and always-secret positions, ascending.
pub fn text_positions(argv: &[String]) -> Vec<usize> {
    let at = text_at(argv);
    let mut all: Vec<usize> = at.typed.into_iter().chain(at.always).collect();
    all.sort_unstable();
    all
}

/// A `press` key that is not typed text: a named key (`Enter`, `Tab`, arrows, `F1`..`F12`,
/// ...) possibly with modifiers, or a single letter or digit with `Control` or `Meta`.
/// Anything else the grammar accepts (`a`, `Shift+a`, `KeyA`, `Space`, `Alt+a`, ...) may
/// put a character into the page.
fn is_typed_key(key: &str) -> bool {
    const NAMED: &[&str] = &[
        "Enter",
        "Tab",
        "Escape",
        "Backspace",
        "Delete",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Insert",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
    ];
    let parts: Vec<&str> = key.split('+').collect();
    let (last, earlier) = parts.split_last().expect("split yields one part at least");
    let shortcut = last.len() == 1
        && last.bytes().all(|b| b.is_ascii_alphanumeric())
        && earlier.iter().any(|m| *m == "Control" || *m == "Meta");
    !(NAMED.contains(last) || shortcut)
}

/// Whether `word` is the first word of some command form (a word the log may show for a
/// refused command line; any other first word may be a secret, e.g. a session handle).
pub fn is_command_word(word: &str) -> bool {
    GRAMMAR.iter().any(|f| f.words[0] == word)
}

fn form_of(argv: &[String]) -> Option<&'static Form> {
    GRAMMAR
        .iter()
        .filter(|f| argv.len() >= f.words.len() && f.words.iter().zip(argv).all(|(a, b)| a == b))
        .max_by_key(|f| f.words.len())
}

struct Parsed {
    words: Vec<String>,
    text: TextAt,
}

/// Parses the words after the command against the form; returns the full command line and
/// the positions of its secret text (the same indices in the input and the output: words
/// are never added or dropped, except a default file name appended at the end).
fn parse(form: &Form, words: &[String], files_dir: &Path) -> Result<Parsed, RpcError> {
    let name = form.words.join(" ");
    let mut out: Vec<String> = form.words.iter().map(|w| w.to_string()).collect();
    let mut typed = Vec::new();
    let mut always = Vec::new();
    let mut urls = Vec::new();
    let mut expr = Vec::new();
    let mut selector_at = None;
    let mut next_arg = 0;
    let mut file_given = false;
    let mut rest = 0;
    let mut literal = false;
    let mut i = 0;
    while i < words.len() {
        let word = &words[i];
        i += 1;
        if literal {
            if form.text == TextUse::Typed {
                typed.push(out.len());
            }
            out.push(word.clone());
            rest += 1;
            continue;
        }
        if word == "--" {
            let in_place =
                form.literal_rest && rest == 0 && form.args[next_arg..].iter().all(|a| !a.required);
            if !in_place {
                return Err(rejected(format!(
                    "'--' is only accepted right before the text of `fill` or `type`, not in `{name}`"
                )));
            }
            literal = true;
            out.push(word.clone());
            continue;
        }
        if word.starts_with('-') && !is_number(word) {
            let flag = form
                .flags
                .iter()
                .find(|f| f.names.contains(&word.as_str()))
                .ok_or_else(|| rejected(format!("'{word}' is not an option of `{name}`")))?;
            out.push(word.clone());
            match flag.arity {
                Arity::Switch => {}
                Arity::OptionalBool => {
                    if let Some(v) = words.get(i).filter(|v| *v == "true" || *v == "false") {
                        out.push(v.clone());
                        i += 1;
                    }
                }
                Arity::Value(kind) => {
                    let Some(v) = words.get(i) else {
                        return Err(rejected(format!("'{word}' needs a value in `{name}`")));
                    };
                    i += 1;
                    // Not even a negative number: a value never looks like a flag.
                    if v.starts_with('-') {
                        return Err(rejected(format!(
                            "invalid value {v:?} for '{word}' in `{name}`: must not start with '-'"
                        )));
                    }
                    let v = accept(kind, v, files_dir).map_err(|why| {
                        rejected(format!(
                            "invalid value {v:?} for '{word}' in `{name}`: {why}"
                        ))
                    })?;
                    match kind {
                        Url => urls.push(out.len()),
                        Expr => expr.push(out.len()),
                        _ => {}
                    }
                    out.push(v);
                }
            }
            continue;
        }
        // A positional argument: the next declared one it fits (an optional one it does not
        // fit is skipped), else part of the rest.
        let mut placed = None;
        let mut why = String::from("unexpected argument");
        while next_arg < form.args.len() {
            let arg = &form.args[next_arg];
            next_arg += 1;
            match accept(arg.kind, word, files_dir) {
                Ok(v) => {
                    if arg.secret {
                        always.push(out.len());
                    }
                    if matches!(arg.kind, Url) {
                        urls.push(out.len());
                    }
                    if matches!(arg.kind, Selector) && selector_at.is_none() {
                        selector_at = Some(out.len());
                    }
                    if form.text == TextUse::Key && is_typed_key(&v) {
                        typed.push(out.len());
                    }
                    file_given |= matches!(arg.kind, File(_));
                    placed = Some(v);
                    break;
                }
                Err(e) if arg.required => {
                    return Err(rejected(format!(
                        "invalid argument {word:?} for `{name}`: {e}"
                    )));
                }
                Err(e) => why = e,
            }
        }
        let value = match (placed, form.rest) {
            (Some(v), _) => v,
            (None, Some(kind)) => {
                rest += 1;
                if form.text == TextUse::Typed {
                    typed.push(out.len());
                }
                accept(kind, word, files_dir)
                    .map_err(|e| rejected(format!("invalid argument {word:?} for `{name}`: {e}")))?
            }
            (None, None) => {
                return Err(rejected(format!("{word:?} in `{name}`: {why}")));
            }
        };
        out.push(value);
    }
    if let Some(arg) = form.args[next_arg..].iter().find(|a| a.required) {
        return Err(rejected(format!(
            "`{name}` is missing its {} argument",
            kind_name(arg.kind)
        )));
    }
    if form.rest_required && rest == 0 {
        return Err(rejected(format!("`{name}` is missing its text")));
    }
    if let (Some(ext), false) = (form.default_file, file_given) {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let file = files_dir.join(format!("shot-{millis}.{ext}"));
        out.push(file.display().to_string());
    }
    let destination = if typed.is_empty() {
        None
    } else if form.words[0] == "find" {
        Some(TextDestination::Unknown)
    } else if let Some(i) = selector_at {
        Some(TextDestination::Selector(out[i].clone()))
    } else {
        Some(TextDestination::Focused)
    };
    Ok(Parsed {
        words: out,
        text: TextAt {
            typed,
            always,
            urls,
            expr,
            destination,
        },
    })
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Selector | ShotSelector => "selector",
        Text => "text",
        Expr => "expression",
        Url => "url",
        Key => "key",
        Number => "number",
        TabRef => "tab",
        File(_) => "file name",
        Choice(_) => "choice",
    }
}

fn is_number(w: &str) -> bool {
    let digits = w.strip_prefix('-').unwrap_or(w);
    let (int, frac) = digits.split_once('.').unwrap_or((digits, "1"));
    let all_digits =
        |s: &str| !s.is_empty() && s.len() <= 15 && s.bytes().all(|b| b.is_ascii_digit());
    all_digits(int) && all_digits(frac)
}

fn is_image_name(w: &str) -> bool {
    let lower = w.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".webp"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// Judges one word as `kind`; returns the word to pass on.
fn accept(kind: Kind, w: &str, files_dir: &Path) -> Result<String, String> {
    if w.starts_with('-') && !matches!(kind, Number) {
        return Err("must not start with '-'".into());
    }
    let ok = |b: bool, why: &str| {
        if b {
            Ok(w.to_string())
        } else {
            Err(why.to_string())
        }
    };
    match kind {
        Selector => ok(!w.is_empty(), "empty selector"),
        ShotSelector => ok(
            !w.is_empty() && !is_image_name(w),
            "not a selector, and not a bare .png/.jpg/.jpeg file name",
        ),
        Text | Expr => Ok(w.to_string()),
        Url => normalize_url(w)
            .ok_or_else(|| "only http(s) URLs with a host, data: URLs and about:blank".into()),
        Key => ok(
            !w.is_empty()
                && w.len() <= 64
                && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+'),
            "not a key name such as Enter or Control+a",
        ),
        Number => ok(is_number(w), "not a number"),
        TabRef => ok(
            is_label(w) || (w.len() == 32 && w.bytes().all(|b| b.is_ascii_hexdigit())),
            "not a tab id (t1), label or target id",
        ),
        File(exts) => {
            let ext_ok = w.rsplit_once('.').is_some_and(|(_, e)| exts.contains(&e));
            if !is_file_name(w) || !ext_ok {
                return Err(format!(
                    "a bare file name (letters, digits, '.', '_', '-'; no path) ending in .{}",
                    exts.join(" or .")
                ));
            }
            Ok(files_dir.join(w).display().to_string())
        }
        Choice(options) => ok(
            options.contains(&w),
            &format!("one of {}", options.join(", ")),
        ),
    }
}

fn is_label(w: &str) -> bool {
    let mut b = w.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphabetic())
        && w.len() <= 64
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$`
fn is_file_name(w: &str) -> bool {
    let mut b = w.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && w.len() <= 100
        && b.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: &str = "/home/p/sessions/s_abcdefgh/files";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn run(v: &[&str]) -> Result<Vec<String>, RpcError> {
        match check(&s(v), Path::new(FILES))? {
            Checked::Run(words) => Ok(words),
            other => panic!("{v:?}: {other:?}"),
        }
    }

    fn refused(v: &[&str]) -> RpcError {
        let e = check(&s(v), Path::new(FILES)).expect_err(&format!("{v:?} was accepted"));
        assert_eq!(e.code, "argv_rejected", "{v:?}");
        e
    }

    #[test]
    fn earlier_passing_cases_still_pass() {
        for a in [
            &["snapshot", "-i"][..],
            &["click", "@e1"],
            &["get", "url"],
            &["fill", "#q", "hello"],
            &["wait", "#x", "--state", "hidden"],
            &["wait", "200"],
            &["mouse", "wheel", "-50", "0"],
            &["find", "role", "button", "click", "--name", "Add"],
            &["tab", "t1"],
            &["tab", "close"],
            &["cookies", "set", "n", "v", "--same-site", "Lax"],
            &["storage", "local", "set", "k", "v"],
            &["type", "#q", "--clear", "--delay", "10", "--", "--x"],
        ] {
            assert_eq!(run(a).unwrap(), s(a), "{a:?}");
        }
        assert_eq!(
            run(&["open", "https://a.test"]).unwrap(),
            s(&["open", "https://a.test/"])
        );
    }

    #[test]
    fn c1_double_dash_after_value_flag_is_rejected() {
        for a in [
            &["get", "url", "--user-agent", "--", "--session", "x"][..],
            &["snapshot", "--max-output", "--", "--cdp", "9222"],
            &["click", "#a", "--", "--profile", "Default"],
            &["snapshot", "-s", "--", "--session", "x"],
            &["wait", "#x", "--state", "--", "--cdp", "1"],
        ] {
            refused(a);
        }
    }

    #[test]
    fn double_dash_only_where_text_is_expected() {
        assert_eq!(
            run(&["fill", "#q", "--", "--cdp"]).unwrap(),
            s(&["fill", "#q", "--", "--cdp"])
        );
        refused(&["click", "--", "#a"]);
        refused(&["fill", "--", "#q", "x"]);
        refused(&["fill", "#q", "a", "--", "b"]);
        refused(&["fill", "#q", "--"]);
        // keyboard and eval keep `--` as text, so it is not offered there.
        refused(&["keyboard", "type", "--", "-x"]);
        refused(&["eval", "--", "-1"]);
    }

    #[test]
    fn unknown_flags_are_rejected_for_every_allowed_command() {
        for form in GRAMMAR {
            for bad in [
                "--definitely-not-a-flag",
                "--session=x",
                "--cdp=1",
                "-p",
                "--debug",
                "--headers",
                "--init-script",
                "--content-boundaries",
                "--allow-file-access",
                "--session-name",
                "--capture",
                "--json",
                "--cdp",
                "--session",
                "--profile",
                "--state=x",
                "-ic",
            ] {
                let mut argv: Vec<&str> = form.words.to_vec();
                argv.push(bad);
                let e = refused(&argv);
                assert!(e.message.contains(bad), "{argv:?}: {}", e.message);
            }
        }
    }

    #[test]
    fn not_supported_commands_are_rejected() {
        for c in [
            "read",
            "flow",
            "inspect",
            "trace",
            "profiler",
            "webmcp",
            "diff",
            "upload",
            "download",
            "batch",
            "session",
            "close",
            "stream",
            "record",
            "connect",
            "mcp",
            "state",
            "auth",
            "plugin",
            "profiles",
            "clipboard",
            "react",
            "pushstate",
            "chat",
            "install",
            "upgrade",
            "dashboard",
            "skills",
            "confirm",
            "deny",
            "doctor",
            "goto",
            "navigate",
            "key",
        ] {
            let e = refused(&[c, "x"]);
            assert!(e.message.contains("not supported"), "{c}: {}", e.message);
        }
        for a in [
            &["get", "cdp-url"][..],
            &["network", "route", "*"],
            &["network", "har", "start"],
            &["network", "request", "1"],
            &["keyboard", "down", "Shift"],
            &["set", "headers", "{}"],
            &["set", "credentials", "a", "b"],
            // These need CDP methods the proxy does not allow (see spike-findings A2).
            &["set", "media", "dark"],
            &["set", "geo", "1", "2"],
            &["set", "offline", "on"],
            &["cookies", "clear"],
            &["open"],
            &["--session", "x", "get", "url"],
            &[],
        ] {
            refused(a);
        }
    }

    #[test]
    fn file_names_are_confined() {
        for a in [
            &["screenshot", "../x.png"][..],
            &["screenshot", "/tmp/x.png"],
            &["screenshot", "x.png\u{0}"],
            &["screenshot", ".x.png"],
            &["screenshot", "x.png", "y.png"],
            &["pdf", "a/b.pdf"],
            &["pdf", ".hidden"],
            &["pdf", "report.png"],
            &["pdf", "~/r.pdf"],
            &["pdf"],
        ] {
            refused(a);
        }
        assert_eq!(
            run(&["pdf", "report.pdf"]).unwrap(),
            s(&["pdf", &format!("{FILES}/report.pdf")])
        );
        assert_eq!(
            run(&["screenshot", "--full", "page.png"]).unwrap(),
            s(&["screenshot", "--full", &format!("{FILES}/page.png")])
        );
        // An element shot: selector first, then the file pegboard names.
        let shot = run(&["screenshot", "#main"]).unwrap();
        assert_eq!(shot[..2], s(&["screenshot", "#main"]));
        assert!(shot[2].starts_with(&format!("{FILES}/shot-")) && shot[2].ends_with(".png"));
        assert_eq!(shot.len(), 3);
        // No name: pegboard picks one, so the CLI never uses a directory of its own.
        let shot = run(&["screenshot"]).unwrap();
        assert!(shot[1].starts_with(&format!("{FILES}/shot-")), "{shot:?}");
    }

    #[test]
    fn file_reading_forms_are_rejected() {
        for a in [
            &["cookies", "set", "--curl", "x"][..],
            &["network", "har", "stop", "x"],
            &["wait", "--download", "x"],
            &["eval", "--stdin"],
            &["open", "https://a.test", "--init-script", "x.js"],
            &["screenshot", "--screenshot-dir", "d"],
            &["diff", "snapshot", "--baseline", "x"],
        ] {
            refused(a);
        }
    }

    #[test]
    fn urls_are_validated_for_open_and_tab_new() {
        for url in [
            "file:///etc/hosts",
            "chrome://settings",
            "\u{a0}https:/../../sw.js",
            "example.com",
        ] {
            refused(&["open", url]);
            refused(&["tab", "new", url]);
            refused(&["a11y", url]);
        }
        assert_eq!(
            run(&["open", "  HTTPS://Example.com "]).unwrap(),
            s(&["open", "https://example.com/"])
        );
        assert_eq!(
            check(
                &s(&["tab", "new", "  HTTPS://Example.com "]),
                Path::new(FILES)
            )
            .unwrap(),
            Checked::TabNew {
                url: "https://example.com/".into()
            }
        );
        assert_eq!(
            check(&s(&["tab", "new"]), Path::new(FILES)).unwrap(),
            Checked::TabNew {
                url: "about:blank".into()
            }
        );
        refused(&["tab", "new", "--label", "docs", "https://a.test"]);
        refused(&["cookies", "set", "n", "v", "--url", "file:///x"]);
    }

    #[test]
    fn flag_values_starting_with_dash_are_rejected_unless_text() {
        for a in [
            &["snapshot", "-d", "-1"][..],
            &["snapshot", "-s", "-x"],
            &["wait", "--text", "--annotate"],
            &["find", "role", "button", "--name", "-x"],
            &["cookies", "set", "n", "v", "--domain", "-x"],
            &["dialog", "accept", "-x"],
            &["select", "#s", "-a"],
            &["fill", "#q", "-x"],
            &["press", "-"],
        ] {
            refused(a);
        }
        // Negative numbers are numbers, not flags.
        assert!(run(&["mouse", "wheel", "-50"]).is_ok());
        refused(&["mouse", "wheel", "-5x"]);
        // Literal text after `--` may start with '-'.
        assert!(run(&["type", "#q", "--", "-x", "--session"]).is_ok());
    }

    #[test]
    fn global_boolean_takes_an_optional_value() {
        let shot = run(&["screenshot", "--annotate", "false"]).unwrap();
        assert_eq!(shot[..3], s(&["screenshot", "--annotate", "false"]));
        assert!(shot[3].ends_with(".png"));
    }

    #[test]
    fn missing_or_extra_arguments_are_rejected() {
        for a in [
            &["click"][..],
            &["fill", "#q"],
            &["get", "attr", "#a"],
            &["get", "url", "x"],
            &["dialog", "accept", "a", "b"],
            &["mouse", "down", "side"],
            &["scroll", "sideways"],
            &["tab", "close", "../x"],
            &["press", "Control a"],
        ] {
            refused(a);
        }
    }

    fn sample(kind: Kind) -> String {
        match kind {
            Selector | ShotSelector => "#a".into(),
            Text | Expr => "q".into(),
            Url => "https://a.test/".into(),
            Key => "Enter".into(),
            Number => "1".into(),
            TabRef => "t1".into(),
            File(exts) => format!("a.{}", exts[0]),
            Choice(options) => options[0].into(),
        }
    }

    /// The command line of a form with every declared argument and `rest` as its rest.
    fn full_line(f: &Form, rest: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = f.words.iter().map(|w| w.to_string()).collect();
        v.extend(f.args.iter().map(|a| sample(a.kind)));
        v.extend(rest.iter().map(|w| w.to_string()));
        v
    }

    #[test]
    fn every_form_that_types_text_has_its_text_positions_known() {
        let mut typed = 0;
        for f in GRAMMAR.iter().filter(|f| f.text == TextUse::Typed) {
            typed += 1;
            let line = full_line(f, &["SECRETX", "SECRETY"]);
            let n = line.len();
            assert!(check(&line, Path::new(FILES)).is_ok(), "{line:?}");
            assert_eq!(text_positions(&line), vec![n - 2, n - 1], "{line:?}");
            if f.literal_rest {
                let mut line = full_line(f, &["--", "-SECRETX"]);
                let n = line.len();
                assert!(check(&line, Path::new(FILES)).is_ok(), "{line:?}");
                assert_eq!(text_positions(&line), vec![n - 1], "{line:?}");
                line.truncate(n);
            }
        }
        assert!(typed >= 14, "only {typed} typing forms");
        for f in GRAMMAR.iter().filter(|f| f.args.iter().any(|a| a.secret)) {
            let line = full_line(f, &[]);
            let at = text_at(&line);
            assert!(at.typed.is_empty(), "{line:?}");
            assert_eq!(at.always, vec![line.len() - 1], "{line:?}");
        }
        // Where typed text goes.
        let at = |v: &[&str]| text_at(&s(v)).destination;
        assert_eq!(
            at(&["type", "--clear", "#pw", "x"]),
            Some(TextDestination::Selector("#pw".into()))
        );
        assert_eq!(
            at(&["fill", "@e3", "x"]),
            Some(TextDestination::Selector("@e3".into()))
        );
        assert_eq!(
            at(&["keyboard", "type", "x"]),
            Some(TextDestination::Focused)
        );
        assert_eq!(at(&["press", "a"]), Some(TextDestination::Focused));
        assert_eq!(
            at(&["find", "label", "L", "fill", "x"]),
            Some(TextDestination::Unknown)
        );
        assert_eq!(at(&["click", "#a"]), None);
    }

    /// A new form that takes free text must be classified: either its text is hidden, or it
    /// is listed here as text that is not typed into the page.
    #[test]
    fn free_text_forms_are_classified() {
        const NOT_TYPED: &[&str] = &[
            "eval",
            "get attr",
            "find role",
            "find text",
            "find label",
            "find placeholder",
            "find alt",
            "find title",
            "find testid",
            "cookies set",
            "storage local",
            "storage local get",
            "storage local set",
            "storage session",
            "storage session get",
            "storage session set",
            "network requests",
            "wait",
            "a11y",
        ];
        for f in GRAMMAR {
            let free = matches!(f.rest, Some(Text))
                || f.args.iter().any(|a| matches!(a.kind, Text))
                || f.flags
                    .iter()
                    .any(|fl| matches!(fl.arity, Arity::Value(Text)));
            let name = f.words.join(" ");
            if !free || f.text != TextUse::No || f.args.iter().any(|a| a.secret) {
                continue;
            }
            // `find role` and the like take a query text first, but their rest (after the
            // action) is typed: they are `Typed`, so they cannot be here.
            assert!(
                NOT_TYPED.contains(&name.as_str()),
                "`{name}` takes free text: hide it (TextUse) or list it as not typed"
            );
        }
        for f in GRAMMAR {
            if matches!(f.rest, Some(Text)) {
                let name = f.words.join(" ");
                assert!(
                    f.text == TextUse::Typed || name == "eval",
                    "`{name}` has a free-text rest that is not hidden"
                );
            }
        }
    }
}
