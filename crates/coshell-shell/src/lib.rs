//! Conservative command analysis and redaction. This crate does not execute commands.
//!
//! This is not a shell interpreter. In particular, local source text cannot prove
//! what an unknown remote alias, function, variable or executable will do.
use sha2::{Digest, Sha256};

pub const RULE_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Safe,
    Dangerous,
    Unknown,
}

/// Byte offsets into the original UTF-8 input, with an exclusive end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskAssessment {
    pub level: RiskLevel,
    pub categories: Vec<&'static str>,
    pub rule_version: &'static str,
    pub display_spans: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redaction {
    pub text: String,
    /// Byte offsets into the original input, never the secret contents.
    pub spans: Vec<Span>,
    /// SHA-256 of the redacted text, never of the original input.
    pub digest: String,
}

#[derive(Debug)]
struct Word {
    value: String,
    span: Span,
    dynamic: bool,
}

#[derive(Debug)]
enum Token {
    Word(Word),
    Separator,
    Redirect { output: bool },
}

struct Lexed {
    tokens: Vec<Token>,
    substitutions: Vec<String>,
    uncertain: bool,
}

fn lex(input: &str) -> Lexed {
    let mut result = Lexed {
        tokens: Vec::new(),
        substitutions: Vec::new(),
        uncertain: false,
    };
    let mut chars = input.char_indices().peekable();
    let mut current = String::new();
    let mut start = None;
    let mut dynamic = false;
    let mut quote = None;
    let mut escaped = false;
    let flush = |end: usize,
                 result: &mut Lexed,
                 current: &mut String,
                 start: &mut Option<usize>,
                 dynamic: &mut bool| {
        if let Some(begin) = start.take() {
            result.tokens.push(Token::Word(Word {
                value: std::mem::take(current),
                span: Span { start: begin, end },
                dynamic: *dynamic,
            }));
            *dynamic = false;
        }
    };
    while let Some((i, ch)) = chars.next() {
        if escaped {
            if ch == '\n' {
                result.uncertain = true;
            }
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            start.get_or_insert(i);
            escaped = true;
            continue;
        }
        if ch == '!' && quote != Some('\'') {
            result.uncertain = true;
        }
        if ch == '\'' && quote != Some('"') || ch == '"' && quote != Some('\'') {
            start.get_or_insert(i);
            quote = if quote == Some(ch) { None } else { Some(ch) };
            continue;
        }
        if quote != Some('\'') && ch == '$' {
            start.get_or_insert(i);
            dynamic = true;
            if chars.peek().is_some_and(|(_, c)| *c == '(') {
                chars.next();
                let inner_start = i + 2;
                let mut depth = 1usize;
                let mut end = None;
                for (j, c) in chars.by_ref() {
                    if c == '(' {
                        depth += 1;
                    }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(j);
                            break;
                        }
                    }
                }
                if let Some(j) = end {
                    result.substitutions.push(input[inner_start..j].to_owned());
                } else {
                    result.uncertain = true;
                }
            }
            current.push('$');
            continue;
        }
        if quote != Some('\'') && ch == '`' {
            start.get_or_insert(i);
            dynamic = true;
            result.uncertain = true;
            let inner_start = i + 1;
            let mut end = None;
            for (j, c) in chars.by_ref() {
                if c == '`' {
                    end = Some(j);
                    break;
                }
            }
            if let Some(j) = end {
                result.substitutions.push(input[inner_start..j].to_owned());
            }
            current.push('`');
            continue;
        }
        if quote.is_none() {
            if ch.is_whitespace() {
                flush(i, &mut result, &mut current, &mut start, &mut dynamic);
                if ch == '\n' {
                    result.tokens.push(Token::Separator);
                }
                continue;
            }
            if matches!(ch, ';' | '|' | '&' | '(' | ')') {
                flush(i, &mut result, &mut current, &mut start, &mut dynamic);
                if matches!(ch, '|' | '&') && chars.peek().is_some_and(|(_, next)| *next == ch) {
                    chars.next();
                }
                if matches!(ch, '(' | ')') {
                    result.uncertain = true;
                }
                result.tokens.push(Token::Separator);
                continue;
            }
            if matches!(ch, '>' | '<') {
                flush(i, &mut result, &mut current, &mut start, &mut dynamic);
                if chars.peek().is_some_and(|(_, next)| *next == ch) {
                    chars.next();
                }
                result.tokens.push(Token::Redirect { output: ch == '>' });
                result.uncertain = true;
                continue;
            }
            if matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '~') {
                dynamic = true;
            }
            if ch == '#' && start.is_none() {
                result.uncertain = true;
                break;
            }
        }
        start.get_or_insert(i);
        current.push(ch);
    }
    if quote.is_some() || escaped {
        result.uncertain = true;
    }
    flush(
        input.len(),
        &mut result,
        &mut current,
        &mut start,
        &mut dynamic,
    );
    result
}

fn inspect(
    words: &[&Word],
    categories: &mut Vec<&'static str>,
    spans: &mut Vec<Span>,
    uncertain: &mut bool,
    trusted: &[&str],
    depth: usize,
) {
    if words.is_empty() {
        *uncertain = true;
        return;
    }
    let mut i = 0;
    while i < words.len() && words[i].value.contains('=') && !words[i].value.starts_with('-') {
        i += 1;
    }
    if i > 0 {
        *uncertain = true;
    }
    if i == words.len() {
        return;
    }
    // `command -v/-V` inspects names; it does not run the following command.
    // Other `command` forms execute the remaining words.
    if words[i].value == "command" {
        *uncertain = true;
        i += 1;
        if i < words.len() && matches!(words[i].value.as_str(), "-v" | "-V") {
            return;
        }
        if i < words.len() && words[i].value == "--" {
            i += 1;
        }
    }
    while i < words.len()
        && matches!(
            words[i].value.rsplit('/').next(),
            Some("sudo" | "doas" | "env")
        )
    {
        let wrapper = words[i].value.rsplit('/').next().unwrap_or("");
        *uncertain = true;
        i += 1;
        while i < words.len() && words[i].value.starts_with('-') {
            if words[i].value == "--" {
                i += 1;
                break;
            }
            let takes_value = matches!(
                (wrapper, words[i].value.as_str()),
                ("env", "-u" | "--unset" | "-C" | "--chdir")
                    | (
                        "sudo" | "doas",
                        "-u" | "--user"
                            | "-g"
                            | "--group"
                            | "-h"
                            | "--host"
                            | "-p"
                            | "--prompt"
                            | "-C"
                            | "--close-from"
                            | "-T"
                            | "--command-timeout"
                            | "-r"
                            | "--role"
                            | "-t"
                            | "--type"
                    )
            );
            i += 1;
            if takes_value && i < words.len() {
                i += 1;
            }
        }
        while i < words.len() && words[i].value.contains('=') && !words[i].value.starts_with('-') {
            i += 1;
        }
    }
    if i == words.len() {
        *uncertain = true;
        return;
    }
    let cmd = words[i].value.rsplit('/').next().unwrap_or("");
    let args = &words[i + 1..];
    // These commands can execute other commands. Keep the overall result
    // conservative, and inspect the embedded command using original spans.
    if depth < 8 {
        let nested_start = match cmd {
            "time" => Some(
                args.iter()
                    .position(|w| !w.value.starts_with('-'))
                    .unwrap_or(args.len()),
            ),
            "xargs" => {
                let mut pos = 0;
                while pos < args.len() && args[pos].value.starts_with('-') {
                    let option = args[pos].value.as_str();
                    pos += 1;
                    if option == "--" {
                        break;
                    }
                    if matches!(
                        option,
                        "-a" | "--arg-file"
                            | "-d"
                            | "--delimiter"
                            | "-E"
                            | "-I"
                            | "-L"
                            | "-n"
                            | "-P"
                            | "-s"
                    ) {
                        pos = (pos + 1).min(args.len());
                    }
                }
                Some(pos)
            }
            "find" => args
                .iter()
                .position(|w| matches!(w.value.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
                .map(|pos| pos + 1),
            _ => None,
        };
        if let Some(start) = nested_start {
            *uncertain = true;
            if start < args.len() {
                let end = if cmd == "find" {
                    args[start..]
                        .iter()
                        .position(|w| matches!(w.value.as_str(), ";" | "+"))
                        .map_or(args.len(), |offset| start + offset)
                } else {
                    args.len()
                };
                inspect(
                    &args[start..end],
                    categories,
                    spans,
                    uncertain,
                    trusted,
                    depth + 1,
                );
            }
        }
    }
    let has_recursive = args.iter().take_while(|w| w.value != "--").any(|w| {
        w.value == "--recursive"
            || w.value.starts_with('-')
                && !w.value.starts_with("--")
                && w.value.chars().any(|c| c == 'r' || c == 'R')
    });
    let root_operand = args.iter().any(|word| {
        if !word.value.starts_with('/') || word.dynamic {
            return false;
        }
        let mut depth = 0usize;
        for component in word.value.split('/') {
            match component {
                "" | "." => {}
                ".." => depth = depth.saturating_sub(1),
                _ => depth += 1,
            }
        }
        depth == 0
    });
    let category = match cmd {
        "rm" if has_recursive || root_operand => Some("recursive_delete"),
        "find" if args.iter().any(|w| w.value == "-delete") => Some("recursive_delete"),
        c if c == "mkfs" || c.starts_with("mkfs.") || matches!(c, "mkswap" | "wipefs") => {
            Some("format_disk")
        }
        "dd" if args.iter().any(|w| w.value.starts_with("of=/dev/")) => Some("overwrite_disk"),
        "fdisk" | "sfdisk" | "parted" | "gdisk" | "sgdisk" => Some("partition_change"),
        "shutdown" | "reboot" | "poweroff" | "halt" => Some("shutdown"),
        "systemctl"
            if args
                .iter()
                .any(|w| matches!(w.value.as_str(), "reboot" | "poweroff" | "halt")) =>
        {
            Some("shutdown")
        }
        "chmod" | "chown" | "chgrp" if has_recursive => Some("recursive_permissions"),
        _ => None,
    };
    if let Some(category) = category {
        if !categories.contains(&category) {
            categories.push(category);
        }
        spans.push(Span {
            start: words[i].span.start,
            end: words.last().map_or(words[i].span.end, |w| w.span.end),
        });
    }
    if words[i].dynamic
        || args.iter().any(|w| w.dynamic)
        || cmd == "eval"
        || !trusted.contains(&words[i].value.as_str())
        || !matches!(
            cmd,
            "pwd" | "whoami" | "uname" | "uptime" | "df" | "free" | "ls" | "echo" | "cat"
        )
    {
        *uncertain = true;
    }
    if matches!(cmd, "sh" | "bash" | "dash" | "zsh" | "ksh" | "eval") {
        *uncertain = true;
        let script_pos = if cmd == "eval" {
            Some(0)
        } else {
            args.iter()
                .position(|w| {
                    w.value.starts_with('-') && !w.value.starts_with("--") && w.value.contains('c')
                })
                .map(|pos| pos + 1)
        };
        if let Some(pos) = script_pos {
            if let Some(script) = args.get(pos) {
                let script_words = if cmd == "eval" {
                    &args[pos..]
                } else {
                    &args[pos..pos + 1]
                };
                if script_words.iter().all(|word| !word.dynamic) && depth < 8 {
                    let source = script_words
                        .iter()
                        .map(|word| word.value.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let nested = assess_inner(&source, trusted, depth + 1);
                    for category in nested.categories {
                        if !categories.contains(&category) {
                            categories.push(category);
                        }
                    }
                    if nested.level == RiskLevel::Dangerous {
                        spans.push(Span {
                            start: script.span.start,
                            end: script_words
                                .last()
                                .map_or(script.span.end, |word| word.span.end),
                        });
                    }
                }
            }
        }
    }
}

fn assess_inner(input: &str, trusted: &[&str], depth: usize) -> RiskAssessment {
    let lexed = lex(input);
    let mut uncertain = lexed.uncertain || lexed.tokens.is_empty();
    let mut categories = Vec::new();
    let mut spans = Vec::new();
    let mut words = Vec::new();
    let mut redirect = None;
    for token in &lexed.tokens {
        match token {
            Token::Word(word) if redirect.is_some() => {
                if redirect == Some(true) && word.value.starts_with("/dev/") {
                    categories.push("overwrite_disk");
                    spans.push(word.span.clone());
                }
                redirect = None;
            }
            Token::Word(word) => words.push(word),
            Token::Separator => {
                inspect(
                    &words,
                    &mut categories,
                    &mut spans,
                    &mut uncertain,
                    trusted,
                    depth,
                );
                words.clear();
            }
            Token::Redirect { output } => {
                redirect = Some(*output);
            }
        }
    }
    inspect(
        &words,
        &mut categories,
        &mut spans,
        &mut uncertain,
        trusted,
        depth,
    );
    if redirect.is_some() {
        uncertain = true;
    }
    for script in &lexed.substitutions {
        uncertain = true;
        if depth < 8 {
            let nested = assess_inner(script, trusted, depth + 1);
            for category in nested.categories {
                if !categories.contains(&category) {
                    categories.push(category);
                }
            }
            if nested.level == RiskLevel::Dangerous {
                spans.push(Span {
                    start: 0,
                    end: input.len(),
                });
            }
        }
    }
    RiskAssessment {
        level: if !categories.is_empty() {
            RiskLevel::Dangerous
        } else if uncertain {
            RiskLevel::Unknown
        } else {
            RiskLevel::Safe
        },
        categories,
        rule_version: RULE_VERSION,
        display_spans: spans,
    }
}

/// Assess without assuming anything about the remote shell's aliases or functions.
pub fn assess(input: &str) -> RiskAssessment {
    assess_inner(input, &[], 0)
}

/// Only use trusted names after the caller has independently established their
/// identity in the *current* remote shell. A name is not intrinsically trusted.
pub fn assess_with_trusted_commands(input: &str, trusted: &[&str]) -> RiskAssessment {
    assess_inner(input, trusted, 0)
}

/// Redact the whole command by default. Arbitrary arguments can contain secrets
/// without markers, so only recognized, static inspection forms are retained.
pub fn redact(input: &str) -> Redaction {
    let lower = input.to_ascii_lowercase();
    let lexed = lex(input);
    // Only preserve literal, fixed context. Unknown arguments or syntax may
    // themselves contain credentials, so every other form is fully redacted.
    if !lexed.uncertain && lexed.substitutions.is_empty() {
        if let [Token::Word(command), Token::Word(option), Token::Word(secret)] =
            lexed.tokens.as_slice()
        {
            if !command.dynamic
                && !option.dynamic
                && !secret.dynamic
                && command.value == "curl"
                && matches!(option.value.as_str(), "--user" | "--proxy-user")
            {
                let text = format!("curl {} [REDACTED]", option.value);
                return Redaction {
                    digest: format!("{:x}", Sha256::digest(text.as_bytes())),
                    text,
                    spans: vec![secret.span.clone()],
                };
            }
        }
    }
    // Arbitrary commands and shell syntax can carry secrets without a standard
    // marker. Preserve text only for a narrow set of simple inspection commands.
    let is_simple_inspection = !lexed.uncertain
        && lexed.substitutions.is_empty()
        && lexed
            .tokens
            .iter()
            .all(|t| matches!(t, Token::Word(w) if !w.dynamic))
        && match lexed.tokens.as_slice() {
            [Token::Word(w)] => matches!(
                w.value.as_str(),
                "ls" | "pwd" | "whoami" | "uname" | "uptime" | "df" | "free"
            ),
            [Token::Word(command), rest @ ..] if command.value == "ls" => {
                rest.iter().all(|t| match t {
                    Token::Word(w) if w.value.starts_with('-') && !w.value.starts_with("--") => w
                        .value[1..]
                        .chars()
                        .all(|c| matches!(c, 'a' | 'A' | 'l' | 'h' | 't' | 'r' | 'S' | '1')),
                    Token::Word(w) => matches!(w.value.as_str(), "/tmp" | "/" | "." | ".."),
                    _ => false,
                })
            }
            _ => false,
        };
    let sensitive = !is_simple_inspection
        || [
            "password",
            "passwd",
            "passphrase",
            "--pass",
            "token",
            "secret",
            "api_key",
            "api-key",
            "apikey",
            "access-key",
            "access_key",
            "private key",
            "authorization",
            "bearer ",
            "-p",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        || (lower.contains("://") && lower.contains('@'));
    let text = if sensitive {
        "[REDACTED]".to_owned()
    } else {
        input.to_owned()
    };
    let spans = if sensitive {
        vec![Span {
            start: 0,
            end: input.len(),
        }]
    } else {
        Vec::new()
    };
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    Redaction {
        text,
        spans,
        digest,
    }
}
