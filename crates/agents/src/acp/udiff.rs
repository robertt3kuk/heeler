//! A unified diff of two texts, for ACP `diff` content (which carries the file's old and new
//! content, where the event model carries patches).
//!
//! Plain line diff by longest common subsequence over the lines between the common prefix and
//! suffix, three lines of context. Texts whose changed middle is too large for the quadratic
//! table are shown as one block of removed lines followed by one of added lines: still a
//! correct patch, just not a minimal one.

const CONTEXT: usize = 3;
/// Cells of the LCS table the diff of the changed middle may use.
const MAX_TABLE_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Keep,
    Remove,
    Add,
}

/// Lines of `text`, without their terminators. An empty text has no lines.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n')
        .map(|line| line.strip_suffix('\n').unwrap_or(line))
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// `(op, index into old or new)` for every line of the edit script.
fn script(old: &[&str], new: &[&str]) -> Vec<(Op, usize)> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_mid = &old[prefix..old.len() - suffix];
    let new_mid = &new[prefix..new.len() - suffix];

    let mut ops: Vec<(Op, usize)> = (0..prefix).map(|i| (Op::Keep, i)).collect();
    let (n, m) = (old_mid.len(), new_mid.len());
    if n.saturating_mul(m) > MAX_TABLE_CELLS {
        ops.extend((0..n).map(|i| (Op::Remove, prefix + i)));
        ops.extend((0..m).map(|j| (Op::Add, prefix + j)));
    } else {
        // lcs[i][j]: the length of the LCS of old_mid[i..] and new_mid[j..].
        let width = m + 1;
        let mut lcs = vec![0_u32; (n + 1) * width];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * width + j] = if old_mid[i] == new_mid[j] {
                    lcs[(i + 1) * width + j + 1] + 1
                } else {
                    lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && old_mid[i] == new_mid[j] {
                ops.push((Op::Keep, prefix + i));
                i += 1;
                j += 1;
            } else if i < n && (j == m || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1]) {
                // On a tie the removal comes first, as in `diff`.
                ops.push((Op::Remove, prefix + i));
                i += 1;
            } else {
                ops.push((Op::Add, prefix + j));
                j += 1;
            }
        }
    }
    ops.extend((0..suffix).map(|k| (Op::Keep, old.len() - suffix + k)));
    ops
}

/// The unified diff from `old` to `new` for `path`; `None` when the texts are equal.
pub fn unified(path: &str, old: &str, new: &str) -> Option<String> {
    if old == new {
        return None;
    }
    let (old_lines, new_lines) = (lines(old), lines(new));
    let ops = script(&old_lines, &new_lines);

    // Hunks are runs of changed script positions with CONTEXT lines around, merged when the
    // context overlaps.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for at in ops
        .iter()
        .enumerate()
        .filter(|(_, (op, _))| *op != Op::Keep)
        .map(|(at, _)| at)
    {
        let from = at.saturating_sub(CONTEXT);
        let to = (at + CONTEXT + 1).min(ops.len());
        match hunks.last_mut() {
            Some(last) if from <= last.1 => last.1 = to,
            _ => hunks.push((from, to)),
        }
    }

    // `a/` and `b/` as git writes them, for a path relative to the repository; an absolute path
    // is shown as it is.
    let (old_name, new_name) = if path.starts_with('/') {
        (path.to_owned(), path.to_owned())
    } else {
        (format!("a/{path}"), format!("b/{path}"))
    };
    let mut out = format!("--- {old_name}\n+++ {new_name}\n");
    for (from, to) in hunks {
        let count =
            |slice: &[(Op, usize)], skip: Op| slice.iter().filter(|(op, _)| *op != skip).count();
        // 1-based first line of the hunk in the old and the new text.
        let old_start = count(&ops[..from], Op::Add) + 1;
        let new_start = count(&ops[..from], Op::Remove) + 1;
        let old_count = count(&ops[from..to], Op::Add);
        let new_count = count(&ops[from..to], Op::Remove);
        // An empty side starts "before" its first line.
        let shown = |count: usize, start: usize| if count == 0 { start - 1 } else { start };
        out.push_str(&format!(
            "@@ -{},{old_count} +{},{new_count} @@\n",
            shown(old_count, old_start),
            shown(new_count, new_start),
        ));
        for (op, index) in &ops[from..to] {
            let (sign, text) = match op {
                Op::Keep => (' ', old_lines[*index]),
                Op::Remove => ('-', old_lines[*index]),
                Op::Add => ('+', new_lines[*index]),
            };
            out.push(sign);
            out.push_str(text);
            out.push('\n');
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_texts_have_no_diff() {
        assert_eq!(unified("a.txt", "x\ny\n", "x\ny\n"), None);
        assert_eq!(unified("a.txt", "", ""), None);
    }

    #[test]
    fn a_changed_line_is_one_hunk_with_context() {
        let diff = unified("src/a.rs", "one\ntwo\nthree\n", "one\n2\nthree\n").unwrap();
        assert_eq!(
            diff,
            "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
        );
    }

    #[test]
    fn distant_changes_make_two_hunks() {
        let old: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let new = old
            .replace("line 2\n", "second\n")
            .replace("line 19\n", "nineteenth\n");
        let diff = unified("f", &old, &new).unwrap();
        assert_eq!(diff.matches("@@ ").count(), 2, "{diff}");
        assert!(diff.contains("-line 2\n+second\n"));
        assert!(diff.contains("-line 19\n+nineteenth\n"));
        assert!(diff.contains("@@ -16,5 +16,5 @@"), "{diff}");
    }

    #[test]
    fn an_absolute_path_is_shown_as_it_is() {
        let diff = unified("/tmp/x/a.txt", "a\n", "b\n").unwrap();
        assert!(
            diff.starts_with("--- /tmp/x/a.txt\n+++ /tmp/x/a.txt\n"),
            "{diff}"
        );
    }

    #[test]
    fn a_new_file_is_all_additions() {
        let diff = unified("n.txt", "", "a\nb\n").unwrap();
        assert_eq!(diff, "--- a/n.txt\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n");
    }

    #[test]
    fn a_huge_change_falls_back_to_remove_then_add() {
        let old: String = (0..3000).map(|n| format!("old {n}\n")).collect();
        let new: String = (0..3000).map(|n| format!("new {n}\n")).collect();
        let diff = unified("big", &old, &new).unwrap();
        assert!(diff.contains("@@ -1,3000 +1,3000 @@"));
        assert_eq!(diff.matches("\n-old").count(), 3000);
        assert_eq!(diff.matches("\n+new").count(), 3000);
    }
}
