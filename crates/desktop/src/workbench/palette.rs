//! Command-palette matching (pure) and state.

/// Fuzzy-match `query` against `text`. `None` when `query` is not a case-insensitive subsequence
/// of `text`; otherwise a score where higher is better: consecutive characters and matches at
/// word starts score more, long gaps and a late first match score less.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase()).collect();
    // `to_lowercase` can change the length for exotic characters; fall back to a plain
    // positional match so indices stay aligned.
    let lower = if lower.len() == chars.len() {
        lower
    } else {
        chars
            .iter()
            .map(|c| c.to_lowercase().next().unwrap_or(*c))
            .collect()
    };

    let mut score = 0i32;
    let mut qi = 0;
    let mut prev_match: Option<usize> = None;
    for (i, c) in lower.iter().enumerate() {
        if qi == query.len() {
            break;
        }
        if *c != query[qi] {
            continue;
        }
        let word_start = i == 0
            || !chars[i - 1].is_alphanumeric()
            || (chars[i - 1].is_lowercase() && chars[i].is_uppercase());
        score += 10;
        if word_start {
            score += 8;
        }
        match prev_match {
            Some(p) if p + 1 == i => score += 12,
            Some(p) => score -= ((i - p - 1) as i32).min(6),
            None => score -= (i as i32).min(10),
        }
        prev_match = Some(i);
        qi += 1;
    }
    (qi == query.len()).then_some(score)
}

/// Indices into `titles` that match `query`, best first; ties keep original order.
pub fn filter_indices<'a>(query: &str, titles: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = titles
        .into_iter()
        .enumerate()
        .filter_map(|(i, t)| fuzzy_score(query, t).map(|s| (s, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// An open palette: the query, the filtered entries (indices into the command list) and the
/// highlighted row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteState {
    pub query: String,
    pub matches: Vec<usize>,
    pub selected: usize,
}

impl PaletteState {
    pub fn refilter<'a>(&mut self, titles: impl IntoIterator<Item = &'a str>) {
        self.matches = filter_indices(&self.query, titles);
        self.selected = 0;
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
        }
    }

    pub fn selected_command(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_required() {
        assert!(fuzzy_score("sr", "Split Right").is_some());
        assert!(fuzzy_score("xyz", "Split Right").is_none());
        assert!(fuzzy_score("tr", "Split Right").is_some());
        assert!(
            fuzzy_score("rs", "Split Right").is_none(),
            "order matters: no `s` after the `r`"
        );
    }

    #[test]
    fn empty_query_matches_everything_in_order() {
        assert_eq!(filter_indices("", ["b", "a", "c"]), vec![0, 1, 2]);
    }

    #[test]
    fn case_insensitive_and_ignores_spaces() {
        assert!(fuzzy_score("SPLIT R", "split right").is_some());
    }

    #[test]
    fn prefers_consecutive_and_word_start_matches() {
        let titles = [
            "View: Toggle Left Dock",
            "Layout: Split Right",
            "Panel: Open Sessions",
        ];
        assert_eq!(filter_indices("split", titles)[0], 1);
        assert_eq!(filter_indices("tld", titles)[0], 0);
        let ranked = filter_indices("sess", titles);
        assert_eq!(ranked[0], 2);
    }

    #[test]
    fn selection_wraps() {
        let mut p = PaletteState::default();
        p.refilter(["a", "b", "c"]);
        p.select_prev();
        assert_eq!(p.selected, 2);
        p.select_next();
        assert_eq!(p.selected, 0);
        let mut empty = PaletteState {
            query: "zzz".into(),
            ..PaletteState::default()
        };
        empty.refilter(["a"]);
        empty.select_next();
        assert_eq!(empty.selected_command(), None);
    }
}
