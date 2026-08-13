//! Secret scrubbing for content entering the LLM context.
//!
//! Self-made lightweight scrubber: curated high-signal token patterns
//! (regex-lite, linear-time) plus a Shannon-entropy gate for generic
//! `password`/`secret`/`api_key` assignments. Chosen over the
//! `secrets_scanner` crate, whose 222-rule compilation stalls the
//! first read for ~13 seconds and ballooned the test suite 1.6s ->
//! 14.9s. Best effort by design — the sensitive-path read block and
//! the approval gate are the hard lines.
//!
//! Performance: marker detection is a single LUT-gated byte pass and
//! the generic-assignment scrub is anchored on `:`/`=` bytes, so a
//! 10MB read stays far below the 200ms latency bound.

use regex_lite::Regex;

/// High-signal token prefixes; no entropy gate needed.
fn token_patterns() -> &'static [Regex] {
    static PATTERNS: std::sync::OnceLock<Vec<Regex>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"sk-[A-Za-z0-9_-]{16,}",     // OpenAI/DeepSeek
            r"sk-ant-[A-Za-z0-9_-]{16,}", // Anthropic
            r"AKIA[0-9A-Z]{16}",          // AWS access key id
            r"ghp_[A-Za-z0-9]{20,}",      // GitHub personal token
            r"github_pat_[A-Za-z0-9_]{20,}",
            r"glpat-[A-Za-z0-9_-]{16,}",     // GitLab
            r"xox[baprs]-[A-Za-z0-9-]{10,}", // Slack
        ]
        .iter()
        .map(|p| Regex::new(p).expect("static pattern compiles"))
        .collect()
    })
}

const TOKEN_MARKERS: [&str; 8] =
    ["sk-", "AKIA", "ghp_", "github_pat_", "glpat-", "xox", "-----BEGIN", "PRIVATE KEY"];
const GENERIC_MARKERS: [&str; 6] = ["password", "passwd", "secret", "token", "api_key", "api-key"];

/// Shannon entropy of a string, in bits per byte (0.0..=8.0).
fn shannon(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let len = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// Result of the single LUT-gated byte pass over the content.
struct MarkerScan {
    has_block: bool,
    has_token: bool,
    has_generic: bool,
    /// Positions of `:`/`=` bytes (candidates for generic assignments).
    seps: Vec<usize>,
}

/// One byte pass reporting every marker class plus the separator
/// positions — the only full-content scan in the scrub pipeline.
///
/// The LUT maps each byte to a per-marker bitmask (bit i = marker i),
/// so a hit position only compares against the markers that can
/// actually start there — no marker sweep per candidate byte.
fn scan(text: &str) -> MarkerScan {
    let mut lut = [0u16; 256];
    let case = |b: u8| [b.to_ascii_lowercase() as usize, b.to_ascii_uppercase() as usize];
    for (bit, m) in TOKEN_MARKERS.iter().enumerate() {
        for idx in case(m.as_bytes()[0]) {
            lut[idx] |= 1u16 << bit;
        }
    }
    for (n, m) in GENERIC_MARKERS.iter().enumerate() {
        for idx in case(m.as_bytes()[0]) {
            lut[idx] |= 1u16 << (TOKEN_MARKERS.len() + n);
        }
    }
    let bytes = text.as_bytes();
    let mut marker_hits = [false; TOKEN_MARKERS.len() + GENERIC_MARKERS.len()];
    let mut seps = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' || b == b':' {
            seps.push(i);
        }
        let mut flags = lut[b as usize];
        while flags != 0 {
            let bit = flags.trailing_zeros() as usize;
            flags &= flags - 1;
            let marker = if bit < TOKEN_MARKERS.len() {
                TOKEN_MARKERS[bit]
            } else {
                GENERIC_MARKERS[bit - TOKEN_MARKERS.len()]
            };
            let window = &bytes[i..(i + marker.len()).min(bytes.len())];
            let matched = if bit < TOKEN_MARKERS.len() {
                window == marker.as_bytes()
            } else {
                window.eq_ignore_ascii_case(marker.as_bytes())
            };
            if matched {
                marker_hits[bit] = true;
            }
        }
    }
    let begin = marker_hits[6];
    let private = marker_hits[7];
    let has_token = marker_hits[..TOKEN_MARKERS.len()].iter().any(|&h| h);
    let has_generic = marker_hits[TOKEN_MARKERS.len()..].iter().any(|&h| h);
    MarkerScan { has_block: begin && private, has_token, has_generic, seps }
}

/// Replace every detected secret in `text` with `[REDACTED]`. Also
/// collapses PEM private-key blocks into a single marker line.
pub fn scrub_secrets(text: &str) -> String {
    let scan = scan(text);
    // Generic redaction first: its byte offsets refer to the original
    // text, so it must run before any other pass shifts the string.
    let mut scrubbed =
        if scan.has_generic { scrub_generic(text, &scan.seps) } else { text.to_string() };
    if scan.has_block {
        scrubbed = scrub_block(&scrubbed);
    }
    if scan.has_token {
        for pattern in token_patterns() {
            let matches: Vec<String> =
                pattern.find_iter(&scrubbed).map(|m| m.as_str().to_string()).collect();
            for matched in matches {
                scrubbed = scrubbed.replace(&matched, "[REDACTED]");
            }
        }
    }
    scrubbed
}

/// Generic `marker [spaces] [:=] [spaces] "value"` scrub over the
/// separator positions collected by [`scan`], redacting only
/// high-entropy quoted values.
fn scrub_generic(text: &str, seps: &[usize]) -> String {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &sep in seps {
        if let Some((start, end)) = redact_range(text, sep) {
            ranges.push((start, end));
        }
    }
    if ranges.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + ranges.len() * 9);
    let mut cursor = 0;
    for (start, end) in ranges {
        out.push_str(&text[cursor..start]);
        out.push_str("[REDACTED]");
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Given a `:`/`=` at `sep`, check whether the word before it is a
/// generic marker and the quoted value after it is high-entropy; when
/// so, return the value's byte range to redact.
fn redact_range(text: &str, sep: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    // Quick forward shape check first: most separators are not
    // assignments to quoted values, and bailing in two byte checks
    // keeps the per-separator cost negligible.
    let mut i = sep + 1;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let quote = *bytes.get(i)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let mut word_end = sep;
    while word_end > 0 && (bytes[word_end - 1] == b' ' || bytes[word_end - 1] == b'\t') {
        word_end -= 1;
    }
    let mut matched = false;
    for marker in GENERIC_MARKERS {
        if word_end >= marker.len()
            && text[word_end - marker.len()..word_end].eq_ignore_ascii_case(marker)
        {
            matched = true;
            break;
        }
    }
    if !matched {
        return None;
    }
    let value_start = i + 1;
    let end_offset = text[value_start..].find(quote as char)?;
    let value = &text[value_start..value_start + end_offset];
    if value.len() < 8 || shannon(value) < 3.5 {
        return None;
    }
    Some((value_start, value_start + end_offset))
}

/// Collapse `-----BEGIN ... PRIVATE KEY-----` ... `-----END ...-----`
/// blocks into one marker line (multi-line spans are easier handled
/// here than with a single regex).
fn scrub_block(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_block = false;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("-----BEGIN") && trimmed.contains("PRIVATE KEY-----") {
            in_block = true;
            out.push_str("[REDACTED PRIVATE KEY BLOCK]\n");
            continue;
        }
        if in_block {
            if trimmed.starts_with("-----END") {
                in_block = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Does the path (or any of its components) match a sensitive-path
/// pattern? Patterns support `*` (any run of characters) and `?`
/// (one character); the file name and the full relative path are both
/// checked, so `.ssh/id_rsa` matches the `id_rsa*` pattern and
/// `sub/.env` matches `.env*`.
pub fn is_sensitive_path(path: &str, patterns: &[String]) -> bool {
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    patterns.iter().any(|p| glob_match(p, &name) || glob_match(p, path))
}

/// Minimal wildcard matcher: `*` matches any run (including `/`),
/// `?` matches exactly one character.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Is this content binary (not fit for the LLM context)? NUL bytes or
/// invalid UTF-8 mark it; valid UTF-8 text (including CJK) passes.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0u8) || std::str::from_utf8(bytes).is_err()
}
