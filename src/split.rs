//! Split presentation rows and exact allowed source streams without losing bytes.
//! A continuation is an ordered fragment, never advertised as an applicable patch.
use crate::model::{Continuation, FileDiff, Row, Span};
use base64::{engine::general_purpose::STANDARD, Engine};

fn cut(bytes: &[u8]) -> usize {
    let mut mid = bytes.len() / 2;
    if let Ok(s) = std::str::from_utf8(bytes) {
        while mid > 0 && !s.is_char_boundary(mid) {
            mid -= 1;
        }
        if mid == 0 {
            mid = s.chars().next().map_or(0, char::len_utf8);
        }
    }
    mid
}
fn spans(values: &[Span]) -> (Vec<Span>, Vec<Span>) {
    let total: usize = values.iter().map(|s| s.text.len()).sum();
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut consumed = 0;
    for span in values {
        let mut n = (total / 2).saturating_sub(consumed).min(span.text.len());
        while !span.text.is_char_boundary(n) {
            n -= 1;
        }
        if n > 0 {
            let mut s = span.clone();
            s.text = span.text[..n].into();
            left.push(s);
        }
        if n < span.text.len() {
            let mut s = span.clone();
            s.text = span.text[n..].into();
            right.push(s);
        }
        consumed += span.text.len();
    }
    (left, right)
}
fn rows(values: &[Row]) -> (Vec<Row>, Vec<Row>) {
    if values.len() != 1 {
        return (
            values[..values.len() / 2].to_vec(),
            values[values.len() / 2..].to_vec(),
        );
    }
    let row = &values[0];
    let (a, b) = spans(&row.left);
    let (c, d) = spans(&row.right);
    let mut left = row.clone();
    left.left = a;
    left.right = c;
    let mut right = row.clone();
    right.left = b;
    right.right = d;
    let empty = |r: &Row| r.left.is_empty() && r.right.is_empty();
    (
        if empty(&left) { vec![] } else { vec![left] },
        if empty(&right) { vec![] } else { vec![right] },
    )
}

pub fn begin(file: &FileDiff) -> FileDiff {
    let mut f = file.clone();
    let old = STANDARD.decode(&f.old_bytes).unwrap_or_default();
    let new = STANDARD.decode(&f.new_bytes).unwrap_or_default();
    f.continuation = Some(Continuation {
        part: 1,
        total: 1,
        old_range: (0, old.len()),
        new_range: (0, new.len()),
        old_sha256: crate::hash(&old),
        new_sha256: crate::hash(&new),
    });
    f
}
pub fn bisect(file: &FileDiff) -> Option<(FileDiff, FileDiff)> {
    let old = STANDARD.decode(&file.old_bytes).ok()?;
    let new = STANDARD.decode(&file.new_bytes).ok()?;
    let (oc, nc) = (cut(&old), cut(&new));
    let (lr, rr) = rows(&file.rows);
    let weight = |rows: &[Row], a: usize, b: usize| {
        rows.iter()
            .map(|r| {
                r.left
                    .iter()
                    .chain(&r.right)
                    .map(|s| s.text.len())
                    .sum::<usize>()
                    + 1
            })
            .sum::<usize>()
            + a
            + b
    };
    let original = weight(&file.rows, old.len(), new.len());
    if weight(&lr, oc, nc) >= original || weight(&rr, old.len() - oc, new.len() - nc) >= original {
        return None;
    }
    let mut left = file.clone();
    let mut right = file.clone();
    left.rows = lr;
    right.rows = rr;
    left.old_bytes = STANDARD.encode(&old[..oc]);
    left.new_bytes = STANDARD.encode(&new[..nc]);
    right.old_bytes = STANDARD.encode(&old[oc..]);
    right.new_bytes = STANDARD.encode(&new[nc..]);
    let base = file.continuation.as_ref()?;
    left.continuation.as_mut()?.old_range.1 = base.old_range.0 + oc;
    left.continuation.as_mut()?.new_range.1 = base.new_range.0 + nc;
    right.continuation.as_mut()?.old_range.0 = base.old_range.0 + oc;
    right.continuation.as_mut()?.new_range.0 = base.new_range.0 + nc;
    Some((left, right))
}
