//! Sample Rust library whose public functions span the executable subset (expressions, if/let/
//! match, iterator chains, an accumulator for-loop, tuples, Option) AND carry doc-tests, so
//! `nl-ingest --v2 --emit-dir` produces records with real examples and runnable body ASTs that
//! `nl-validator run` can execute. Exercised in-process by the `executable_corpus_runs` test.

/// Double a number.
/// ```
/// assert_eq!(double(5), 10);
/// ```
pub fn double(n: i64) -> i64 {
    n * 2
}

/// The sign of a number: -1, 0, or 1 (nested if/else).
/// ```
/// assert_eq!(sign(5), 1);
/// assert_eq!(sign(-3), -1);
/// assert_eq!(sign(0), 0);
/// ```
pub fn sign(n: i64) -> i64 {
    if n > 0 {
        1
    } else if n < 0 {
        -1
    } else {
        0
    }
}

/// Absolute difference via a local binding.
/// ```
/// assert_eq!(abs_diff(3, 7), 4);
/// ```
pub fn abs_diff(a: i64, b: i64) -> i64 {
    let d = a - b;
    if d < 0 {
        -d
    } else {
        d
    }
}

/// The value of an option, or a default (match on an Option).
/// ```
/// assert_eq!(unwrap_or(Some(7), 0), 7);
/// assert_eq!(unwrap_or(None, 0), 0);
/// ```
pub fn unwrap_or(o: Option<i64>, d: i64) -> i64 {
    match o {
        Some(x) => x,
        None => d,
    }
}

/// Sum of the squares (iterator chain).
/// ```
/// assert_eq!(sum_squares(vec![1, 2, 3]), 14);
/// ```
pub fn sum_squares(xs: Vec<i64>) -> i64 {
    xs.iter().map(|x| x * x).sum()
}

/// Keep the positive numbers (iterator filter).
/// ```
/// assert_eq!(positives(vec![1, -2, 3]), vec![1, 3]);
/// ```
pub fn positives(xs: Vec<i64>) -> Vec<i64> {
    xs.iter().filter(|&x| x > 0).cloned().collect()
}

/// Total via an imperative accumulator for-loop.
/// ```
/// assert_eq!(total(vec![1, 2, 3, 4]), 10);
/// ```
pub fn total(xs: Vec<i64>) -> i64 {
    let mut acc = 0;
    for x in xs {
        acc += x;
    }
    acc
}

/// Both the sum and the difference (tuple result).
/// ```
/// assert_eq!(add_sub(5, 3), (8, 2));
/// ```
pub fn add_sub(a: i64, b: i64) -> (i64, i64) {
    (a + b, a - b)
}

/// The successor wrapped in an option.
/// ```
/// assert_eq!(bump(4), Some(5));
/// ```
pub fn bump(n: i64) -> Option<i64> {
    Some(n + 1)
}
