use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// Runtime representation of a single cell value.
///
/// Maps 1:1 with SQLite's type affinity system. `Null` is a first-class
/// variant rather than `Option<Value>` because null bitmaps are handled
/// externally — encoders never see `Null` in their input slice.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Value {
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    Null,
}

impl Value {
    /// Returns `true` if this value is `Null`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Returns the SQLite type affinity, or `None` for `Null`.
    #[must_use]
    pub fn sqlite_type(&self) -> Option<SqliteType> {
        match self {
            Value::Integer(_) => Some(SqliteType::Integer),
            Value::Real(_) => Some(SqliteType::Real),
            Value::Text(_) => Some(SqliteType::Text),
            Value::Blob(_) => Some(SqliteType::Blob),
            Value::Null => None,
        }
    }
}

/// Total ordering for storage purposes.
///
/// Ordering: Null < Integer < Real < Text < Blob.
/// Within Real, NaN == NaN and NaN sorts after all finite/infinite values.
/// This is a *storage* ordering, not SQL semantics (where NULL is incomparable).
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Integer(a), Value::Integer(b)) => a == b,
            (Value::Real(a), Value::Real(b)) => float_total_cmp(*a, *b) == Ordering::Equal,
            (Value::Text(a), Value::Text(b)) => a == b,
            (Value::Blob(a), Value::Blob(b)) => a == b,
            (Value::Null, Value::Null) => true,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,

            (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
            (Value::Integer(_), _) => Ordering::Less,
            (_, Value::Integer(_)) => Ordering::Greater,

            (Value::Real(a), Value::Real(b)) => float_total_cmp(*a, *b),
            (Value::Real(_), _) => Ordering::Less,
            (_, Value::Real(_)) => Ordering::Greater,

            (Value::Text(a), Value::Text(b)) => a.cmp(b),
            (Value::Text(_), _) => Ordering::Less,
            (_, Value::Text(_)) => Ordering::Greater,

            (Value::Blob(a), Value::Blob(b)) => a.cmp(b),
        }
    }
}

/// Total ordering for f64 that handles NaN deterministically.
/// NaN == NaN, NaN sorts after everything else (including +inf).
fn float_total_cmp(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Integer(v) => v.hash(state),
            Value::Real(v) => v.to_bits().hash(state),
            Value::Text(v) => v.hash(state),
            Value::Blob(v) => v.hash(state),
            Value::Null => {}
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Integer(v) => write!(f, "{v}"),
            Value::Real(v) => write!(f, "{v}"),
            Value::Text(v) => write!(f, "{v}"),
            Value::Blob(v) => write!(f, "<blob {} bytes>", v.len()),
            Value::Null => write!(f, "NULL"),
        }
    }
}

/// SQLite storage class / column type affinity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqliteType {
    Integer,
    Real,
    Text,
    Blob,
}

impl fmt::Display for SqliteType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SqliteType::Integer => write!(f, "INTEGER"),
            SqliteType::Real => write!(f, "REAL"),
            SqliteType::Text => write!(f, "TEXT"),
            SqliteType::Blob => write!(f, "BLOB"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_sorts_first() {
        assert!(Value::Null < Value::Integer(0));
        assert!(Value::Null < Value::Real(0.0));
        assert!(Value::Null < Value::Text(String::new()));
        assert!(Value::Null < Value::Blob(vec![]));
    }

    #[test]
    fn type_ordering() {
        // Null < Integer < Real < Text < Blob
        let null = Value::Null;
        let int = Value::Integer(100);
        let real = Value::Real(1.0);
        let text = Value::Text("z".into());
        let blob = Value::Blob(vec![0xff]);
        assert!(null < int);
        assert!(int < real);
        assert!(real < text);
        assert!(text < blob);
    }

    #[test]
    fn nan_equality() {
        let nan1 = Value::Real(f64::NAN);
        let nan2 = Value::Real(f64::NAN);
        assert_eq!(nan1, nan2);
    }

    #[test]
    fn nan_sorts_after_infinity() {
        let nan = Value::Real(f64::NAN);
        let inf = Value::Real(f64::INFINITY);
        assert!(inf < nan);
    }

    #[test]
    fn integer_ordering() {
        assert!(Value::Integer(-1) < Value::Integer(0));
        assert!(Value::Integer(0) < Value::Integer(1));
    }

    #[test]
    fn display() {
        assert_eq!(Value::Integer(42).to_string(), "42");
        assert_eq!(Value::Real(3.125).to_string(), "3.125");
        assert_eq!(Value::Text("hello".into()).to_string(), "hello");
        assert_eq!(Value::Blob(vec![1, 2, 3]).to_string(), "<blob 3 bytes>");
        assert_eq!(Value::Null.to_string(), "NULL");
    }

    #[test]
    fn hash_consistency() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Value::Integer(1));
        set.insert(Value::Integer(1));
        assert_eq!(set.len(), 1);

        set.insert(Value::Real(f64::NAN));
        set.insert(Value::Real(f64::NAN));
        // NaN == NaN for storage, so only one entry
        assert_eq!(set.len(), 2);
    }
}
