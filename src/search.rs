//! Fuzzy search: FTS5 trigram candidates → Rust re-rank (typo-tolerant).
use crate::error::AppError;
use rusqlite::{params_from_iter, Connection};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status { InUse, Low, UsedUp }

#[derive(Debug)]
pub struct Hit {
    pub id: i64, pub name: String, pub path: String, pub qty: i64, pub unit: String,
    pub in_use: i64, pub is_low: bool, pub is_used_up: bool, pub score: u32,
}

pub fn tokenize(q: &str) -> Vec<String> {
    q.split_whitespace()
        .map(|t| t.trim_start_matches('#').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

/// `"bat" OR "ate" OR …` across all tokens ≥ 3 chars. Quotes are escaped FTS5-style.
fn trigram_or(tokens: &[String]) -> String {
    let mut grams: Vec<String> = Vec::new();
    for t in tokens {
        let ch: Vec<char> = t.chars().collect();
        if ch.len() < 3 { continue; }
        for w in ch.windows(3) {
            let g: String = w.iter().collect();
            let g = format!("\"{}\"", g.replace('"', "\"\""));
            if !grams.contains(&g) { grams.push(g); }
        }
    }
    grams.join(" OR ")
}

fn score_token(t: &str, words: &[&str]) -> u32 {
    let mut best = 0;
    for w in words {
        if w.starts_with(t) { return 4; }
        if w.contains(t) { best = best.max(3); continue; }
        let len = t.chars().count();
        if len >= 4 {
            let lim = if len >= 6 { 2 } else { 1 };
            let prefix: String = w.chars().take(len + 1).collect();
            if strsim::levenshtein(t, &prefix).min(strsim::levenshtein(t, w)) <= lim { best = best.max(2); }
        }
    }
    best
}

pub fn search(c: &Connection, q: &str, tags: &[String], status: Option<Status>, limit: usize) -> Result<Vec<Hit>, AppError> {
    let tokens = tokenize(q);
    let mut where_: Vec<String> = Vec::new();
    let mut args: Vec<String> = Vec::new();

    if !tokens.is_empty() {
        let or = trigram_or(&tokens);
        if tokens.iter().all(|t| t.chars().count() >= 3) && !or.is_empty() {
            where_.push("item_fts MATCH ?".into());
            args.push(or);
        } else {
            // Trigram can't index < 3 chars: fall back to LIKE on the small FTS table.
            let mut likes = Vec::new();
            for t in &tokens { likes.push("(f.name LIKE ? OR f.tags LIKE ?)".to_string()); args.push(format!("%{t}%")); args.push(format!("%{t}%")); }
            where_.push(likes.join(" AND "));
        }
    }
    for t in tags {
        where_.push("EXISTS (SELECT 1 FROM item_tag it JOIN tag tg ON tg.id = it.tag_id WHERE it.item_id = s.id AND tg.name = ?)".into());
        args.push(t.to_lowercase());
    }
    match status {
        Some(Status::InUse) => where_.push("s.in_use > 0".into()),
        Some(Status::Low) => where_.push("s.is_low".into()),
        Some(Status::UsedUp) => where_.push("s.is_used_up".into()),
        None => {}
    }
    let sql = format!(
        "SELECT s.id, s.name, f.path, s.qty, s.unit, s.in_use, s.is_low, s.is_used_up,
                f.name || ' ' || f.notes || ' ' || f.tags || ' ' || f.path
         FROM item_fts f JOIN item_status s ON s.id = f.rowid
         {} LIMIT 400",
        if where_.is_empty() { String::new() } else { format!("WHERE {}", where_.join(" AND ")) });

    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| Ok((
        Hit { id: r.get(0)?, name: r.get(1)?, path: r.get(2)?, qty: r.get(3)?, unit: r.get(4)?,
              in_use: r.get(5)?, is_low: r.get(6)?, is_used_up: r.get(7)?, score: 0 },
        r.get::<_, String>(8)?,
    )))?;

    let mut hits = Vec::new();
    for row in rows {
        let (mut hit, hay) = row?;
        let hay = hay.to_lowercase();
        let words: Vec<&str> = hay.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let mut total = 0;
        let mut ok = true;
        for t in &tokens {
            let s = score_token(t, &words);
            if s == 0 { ok = false; break; }
            total += s;
        }
        if ok { hit.score = total; hits.push(hit); }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    hits.truncate(limit);
    Ok(hits)
}
