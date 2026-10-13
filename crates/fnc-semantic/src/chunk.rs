//! Chunking (SPEC ch.5 §3.1 "分块器"): file names with their two parent folders (noise
//! such as UUIDs, hashes and timestamps removed), text split at paragraph/sentence
//! boundaries into ≤ `max_tokens` pieces with overlap, and code in fixed line windows.

/// A chunk of a document's text: byte range plus the text to embed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    /// Byte offset in the source text.
    pub start: usize,
    /// Byte length.
    pub len: usize,
}

impl Piece {
    /// The chunk's text in `text`.
    pub fn of<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start..self.start + self.len]
    }
}

fn is_noise_token(t: &str) -> bool {
    let digits = t.chars().filter(char::is_ascii_digit).count();
    let hex = t.chars().all(|c| c.is_ascii_hexdigit());
    // Timestamps and counters (20260712, 1234567), hashes (a3f9c0e1…), UUID parts.
    (digits == t.len() && t.len() >= 5) || (hex && t.len() >= 8 && digits >= 2)
}

/// Words of a file name with separators and noise removed: `IMG_20260712_1234.jpg` →
/// `IMG 1234 jpg`, `2026Q2_budget_final_v3.xlsx` → `2026Q2 budget final v3 xlsx`.
pub fn clean_name(name: &str) -> String {
    name.split(['_', '-', '.', ' ', '+', '(', ')'])
        .filter(|t| !t.is_empty() && !is_noise_token(t))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `true` if a name carries no words worth embedding (only digits, hashes, punctuation).
pub fn is_noise_name(name: &str) -> bool {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    !clean_name(stem).chars().any(char::is_alphabetic)
}

/// The text of a file-name chunk: `parent2\parent1\name` cleaned, or `None` for noise
/// names (SPEC ch.5 §6.7).
pub fn name_text(path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    let name = *parts.last()?;
    if is_noise_name(name) {
        return None;
    }
    let mut text: Vec<String> = parts.iter().rev().skip(1).take(2).rev().map(|p| clean_name(p)).collect();
    text.retain(|t| !t.is_empty() && !t.ends_with(':'));
    text.push(clean_name(name));
    Some((name.to_string(), text.join(" \\ ")))
}

/// Sentence-ish boundaries: after blank lines, line breaks, and sentence punctuation
/// (Latin and CJK). Returns the end offsets of the pieces.
fn sentences(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let end = i + c.len_utf8();
        let boundary = match c {
            '\n' | '。' | '！' | '？' | '；' => true,
            '.' | '!' | '?' | ';' => it.peek().is_none_or(|(_, n)| n.is_whitespace()),
            _ => false,
        };
        if boundary {
            ends.push(end);
        }
    }
    if ends.last() != Some(&text.len()) && !text.is_empty() {
        ends.push(text.len());
    }
    ends
}

/// Splits `text` into pieces of at most `max_tokens` tokens (counted by `count`), cut at
/// sentence boundaries, consecutive pieces overlapping by about `overlap` tokens; at most
/// `max_chunks` pieces. Whitespace-only pieces are skipped.
pub fn text_chunks(
    text: &str,
    max_tokens: usize,
    overlap: usize,
    max_chunks: usize,
    count: &dyn Fn(&str) -> usize,
) -> Vec<Piece> {
    let ends = sentences(text);
    // (start, end, tokens) of every sentence; over-long sentences are cut by characters.
    let mut units: Vec<(usize, usize, usize)> = Vec::new();
    let mut start = 0;
    for end in ends {
        let s = &text[start..end];
        let n = count(s);
        if n <= max_tokens {
            units.push((start, end, n));
        } else {
            // Hard split into roughly equal parts on char boundaries.
            let parts = n.div_ceil(max_tokens);
            let step = s.len().div_ceil(parts);
            let mut a = start;
            while a < end {
                let mut b = (a + step).min(end);
                while !text.is_char_boundary(b) {
                    b += 1;
                }
                units.push((a, b, count(&text[a..b])));
                a = b;
            }
        }
        start = end;
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < units.len() && out.len() < max_chunks {
        let (a, mut b, mut tokens) = (units[i].0, units[i].1, units[i].2);
        let mut j = i + 1;
        while j < units.len() && tokens + units[j].2 <= max_tokens {
            tokens += units[j].2;
            b = units[j].1;
            j += 1;
        }
        if !text[a..b].trim().is_empty() {
            out.push(Piece { start: a, len: b - a });
        }
        if j >= units.len() {
            break;
        }
        // Step back over trailing sentences worth `overlap` tokens for the next piece.
        let mut k = j;
        let mut back = 0;
        while k > i + 1 && back + units[k - 1].2 <= overlap {
            back += units[k - 1].2;
            k -= 1;
        }
        i = k;
    }
    out
}

/// Code in windows of `lines` lines, overlapping by `overlap` lines (SPEC ch.5 §3.1).
pub fn code_chunks(text: &str, lines: usize, overlap: usize, max_chunks: usize) -> Vec<Piece> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1).filter(|&i| i < text.len()));
    let step = lines.saturating_sub(overlap).max(1);
    let mut out = Vec::new();
    let mut l = 0;
    while l < starts.len() && out.len() < max_chunks {
        let a = starts[l];
        let b = starts.get(l + lines).copied().unwrap_or(text.len());
        if !text[a..b].trim().is_empty() {
            out.push(Piece { start: a, len: b - a });
        }
        if l + lines >= starts.len() {
            break;
        }
        l += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn names_lose_noise() {
        assert_eq!(clean_name("IMG_20260712_1234.jpg"), "IMG 1234 jpg");
        assert_eq!(clean_name("2026Q2_budget_final_v3.xlsx"), "2026Q2 budget final v3 xlsx");
        assert_eq!(clean_name("report-3f2a9c1be0d4.pdf"), "report pdf");
        assert!(is_noise_name("20260712_123456.jpg"));
        assert!(is_noise_name("a3f9c0e1d2b4c5f6.bin"));
        assert!(!is_noise_name("季度报告.docx"));
        let (title, text) = name_text(r"C:\Users\me\Documents\Finance\2026Q2_budget.xlsx").unwrap();
        assert_eq!(title, "2026Q2_budget.xlsx");
        assert_eq!(text, r"Documents \ Finance \ 2026Q2 budget xlsx");
        assert_eq!(name_text("D:/x.txt").unwrap().1, "x txt");
        assert!(name_text(r"C:\DCIM\20260712_1234.jpg").is_none());
    }

    #[test]
    fn text_is_cut_at_sentences_with_overlap() {
        let text = "One two three. Four five six. Seven eight nine. Ten eleven twelve.";
        let p = text_chunks(text, 6, 3, 10, &words);
        let got: Vec<&str> = p.iter().map(|p| p.of(text).trim()).collect();
        assert_eq!(
            got,
            vec![
                "One two three. Four five six.",
                "Four five six. Seven eight nine.",
                "Seven eight nine. Ten eleven twelve."
            ]
        );
        assert_eq!(text_chunks(text, 100, 10, 10, &words).len(), 1);
        assert_eq!(text_chunks(text, 6, 3, 2, &words).len(), 2, "max_chunks");
    }

    #[test]
    fn cjk_and_long_sentences() {
        let text = "第一句话。第二句话！第三句话？";
        let chars = |s: &str| s.chars().count();
        let p = text_chunks(text, 10, 0, 10, &chars);
        assert_eq!(p.iter().map(|p| p.of(text)).collect::<Vec<_>>(), vec!["第一句话。第二句话！", "第三句话？"]);
        let long = "word ".repeat(50);
        let p = text_chunks(&long, 10, 0, 100, &words);
        assert!(p.len() >= 5 && p.iter().all(|x| words(x.of(&long)) <= 11));
        assert!(text_chunks("   \n\n  ", 10, 0, 10, &words).is_empty());
    }

    #[test]
    fn code_windows() {
        let code: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let p = code_chunks(&code, 80, 10, 32);
        assert_eq!(p.len(), 3);
        assert!(p[0].of(&code).starts_with("line 0\n") && p[0].of(&code).ends_with("line 79\n"));
        assert!(p[1].of(&code).starts_with("line 70\n"));
        assert!(p[2].of(&code).ends_with("line 199\n"));
        assert_eq!(code_chunks("fn a() {}", 80, 10, 32).len(), 1);
    }
}
