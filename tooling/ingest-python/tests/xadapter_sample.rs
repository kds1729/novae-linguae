//! Cross-adapter fixture (Rust side). Each function is written to translate to the SAME Nova Lingua
//! body as its Python twin in `xadapter_sample.py`, so the two adapters must agree byte-for-byte on
//! the body content-address. Consumed by `TestCrossAdapterAgreement`.

/// Double a number.
/// ```
/// assert_eq!(double(5), 10);
/// ```
pub fn double(n: i64) -> i64 {
    n + n
}

/// Twice a number (a literal operand).
/// ```
/// assert_eq!(times2(5), 10);
/// ```
pub fn times2(n: i64) -> i64 {
    n * 2
}

/// Whether a number is positive.
/// ```
/// assert_eq!(is_pos(3), true);
/// assert_eq!(is_pos(-1), false);
/// ```
pub fn is_pos(n: i64) -> bool {
    n > 0
}

/// Divide, or nothing on a zero divisor (a Maybe: None vs Some/Just).
/// ```
/// assert_eq!(safe_div(6, 2), Some(3));
/// assert_eq!(safe_div(7, 0), None);
/// ```
pub fn safe_div(a: i64, b: i64) -> Option<i64> {
    if b == 0 {
        None
    } else {
        Some(a / b)
    }
}

/// The sum and the difference (a tuple result).
/// ```
/// assert_eq!(add_sub(5, 3), (8, 2));
/// ```
pub fn add_sub(a: i64, b: i64) -> (i64, i64) {
    (a + b, a - b)
}
