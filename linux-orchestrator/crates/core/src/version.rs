use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A deterministic, comparable semantic version with three numeric components.
///
/// Runtime banners differ wildly (`Python 3.12.8`, `v22.12.0`, `1.8.0_401`,
/// `go1.23.4`), so parsing intentionally ignores prefixes and trailing tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse the first `major[.minor[.patch]]` sequence in a string.
    pub fn parse(input: &str) -> Option<Self> {
        let bytes = input.as_bytes();
        let start = bytes.iter().position(|b| b.is_ascii_digit())?;
        let mut parts = [0u32; 3];
        let mut part = 0usize;
        let mut current = String::new();
        let mut i = start;
        while i < bytes.len() {
            let b = bytes[i];
            if b.is_ascii_digit() {
                current.push(b as char);
            } else if b == b'.' {
                if part < 2 {
                    if !current.is_empty() {
                        parts[part] = current.parse().ok()?;
                        current.clear();
                        part += 1;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
            i += 1;
        }
        if !current.is_empty() && part < 3 {
            parts[part] = current.parse().ok()?;
        }
        Some(Self::new(parts[0], parts[1], parts[2]))
    }

    /// Java reports legacy versions as `1.8.0_401`; modern Java reports `21.0.5`.
    /// Normalise the legacy form to its feature-release number.
    pub fn parse_java(input: &str) -> Option<Self> {
        let parsed = Self::parse(input)?;
        if parsed.major == 1 && parsed.minor >= 5 {
            Some(Self::new(parsed.minor, parsed.patch, 0))
        } else {
            Some(parsed)
        }
    }

    pub fn major_minor(&self) -> (u32, u32) {
        (self.major, self.minor)
    }

    pub fn is_at_least(&self, other: Version) -> bool {
        *self >= other
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Version::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid version: {text}")))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
    Ne,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    pub op: Op,
    pub version: Version,
}

impl Clause {
    fn matches(&self, v: Version) -> bool {
        match self.op {
            Op::Ge => v >= self.version,
            Op::Gt => v > self.version,
            Op::Le => v <= self.version,
            Op::Lt => v < self.version,
            Op::Eq => v == self.version,
            Op::Ne => v != self.version,
        }
    }

    fn lower_bound(&self) -> Option<Version> {
        match self.op {
            Op::Ge | Op::Gt | Op::Eq => Some(self.version),
            _ => None,
        }
    }
}

/// A small parser for the version range syntaxes found in real manifests:
/// npm (`^18`, `~20.11`, `>=18 <21`), Python (`>=3.9,<3.13`, `~=3.10`) and
/// plain Maven/Gradle values.
///
/// All ranges are normalised into groups of simple comparators. `||` creates
/// alternative groups; a version matches if any group matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionReq {
    raw: String,
    groups: Vec<Vec<Clause>>,
}

impl VersionReq {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let mut groups = Vec::new();
        for group in raw.split("||") {
            let mut clauses = Vec::new();
            let mut group_is_any = false;
            let normalized = group
                .replace(',', " ")
                .replace('(', " ")
                .replace(')', " ")
                .replace(" - ", " ");
            let mut tokens = normalized.split_whitespace().peekable();
            while let Some(token) = tokens.next() {
                if token == "*" || token.eq_ignore_ascii_case("x") {
                    group_is_any = true;
                    continue;
                }
                let (op, rest) = split_operator(token);
                let rest = if rest.is_empty() {
                    tokens.next().unwrap_or_default()
                } else {
                    rest
                };
                if rest.is_empty() {
                    continue;
                }
                expand_clause(op, rest, &mut clauses)?;
            }
            if !clauses.is_empty() {
                groups.push(clauses);
            } else if group_is_any {
                groups.push(Vec::new());
            }
        }
        if groups.is_empty() {
            return None;
        }
        Some(Self {
            raw: raw.to_string(),
            groups,
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn satisfies(&self, version: Version) -> bool {
        self.groups
            .iter()
            .any(|group| group.iter().all(|clause| clause.matches(version)))
    }

    /// The lowest version that can satisfy the first alternative group, when a
    /// concrete lower bound exists. Used to select a toolchain to fetch.
    pub fn minimum(&self) -> Option<Version> {
        let first = self.groups.first()?;
        let mut minimum: Option<Version> = None;
        for clause in first {
            if let Some(bound) = clause.lower_bound() {
                minimum = Some(match minimum {
                    Some(existing) if existing >= bound => existing,
                    _ => bound,
                });
            }
        }
        if let Some(clause) = first.iter().find(|c| c.op == Op::Gt) {
            if minimum == Some(clause.version) {
                return Some(Version::new(
                    clause.version.major,
                    clause.version.minor,
                    clause.version.patch.saturating_add(1),
                ));
            }
        }
        minimum
    }

    /// Upper bound (exclusive) if one is present in the first group.
    pub fn upper_exclusive(&self) -> Option<Version> {
        self.groups
            .first()?
            .iter()
            .find(|c| c.op == Op::Lt)
            .map(|c| c.version)
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Serialize for VersionReq {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for VersionReq {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        VersionReq::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid version range: {text}")))
    }
}

fn split_operator(token: &str) -> (Op, &str) {
    if let Some(rest) = token.strip_prefix(">=") {
        (Op::Ge, rest)
    } else if let Some(rest) = token.strip_prefix("<=") {
        (Op::Le, rest)
    } else if let Some(rest) = token.strip_prefix("==") {
        (Op::Eq, rest)
    } else if let Some(rest) = token.strip_prefix("!=") {
        (Op::Ne, rest)
    } else if let Some(rest) = token.strip_prefix("~=") {
        (Op::Eq, rest)
    } else if let Some(rest) = token.strip_prefix(">") {
        (Op::Gt, rest)
    } else if let Some(rest) = token.strip_prefix("<") {
        (Op::Lt, rest)
    } else if let Some(rest) = token.strip_prefix("=") {
        (Op::Eq, rest)
    } else {
        (Op::Ge, token)
    }
}

fn parse_partial(text: &str) -> Option<(Version, usize)> {
    let cleaned = text.trim().trim_start_matches('v');
    let mut numbers = Vec::new();
    let mut wildcard = false;
    for part in cleaned.split('.') {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            if part.eq_ignore_ascii_case("x") || part == "*" {
                wildcard = true;
            }
            break;
        }
        numbers.push(digits.parse::<u32>().ok()?);
        if numbers.len() == 3 {
            break;
        }
    }
    if numbers.is_empty() {
        return None;
    }
    let count = numbers.len();
    while numbers.len() < 3 {
        numbers.push(0);
    }
    let mut version = Version::new(numbers[0], numbers[1], numbers[2]);
    // A token like "18" or "18.x" means the whole major/minor line.
    if wildcard {
        if count == 1 {
            version.minor = 0;
            version.patch = 0;
            return Some((version, 1));
        }
        if count == 2 {
            version.patch = 0;
            return Some((version, 2));
        }
    }
    Some((version, count))
}

fn expand_clause(op: Op, text: &str, out: &mut Vec<Clause>) -> Option<()> {
    let original = text.trim();
    if original.is_empty() || original == "*" || original.eq_ignore_ascii_case("x") {
        return Some(());
    }
    // npm caret range.
    if let Some(rest) = original.strip_prefix('^') {
        let (v, count) = parse_partial(rest)?;
        out.push(Clause {
            op: Op::Ge,
            version: v,
        });
        let upper = caret_upper(v, count);
        out.push(Clause {
            op: Op::Lt,
            version: upper,
        });
        return Some(());
    }
    // npm tilde range.
    if let Some(rest) = original.strip_prefix('~') {
        if let Some(compatible) = rest.strip_prefix('=') {
            // Python compatible release: ~=3.10 -> >=3.10,<4.0
            let (v, count) = parse_partial(compatible)?;
            out.push(Clause {
                op: Op::Ge,
                version: v,
            });
            let upper = if count <= 2 {
                Version::new(v.major.saturating_add(1), 0, 0)
            } else {
                Version::new(v.major, v.minor.saturating_add(1), 0)
            };
            out.push(Clause {
                op: Op::Lt,
                version: upper,
            });
            return Some(());
        }
        let (v, count) = parse_partial(rest)?;
        out.push(Clause {
            op: Op::Ge,
            version: v,
        });
        let upper = if count <= 1 {
            Version::new(v.major.saturating_add(1), 0, 0)
        } else {
            Version::new(v.major, v.minor.saturating_add(1), 0)
        };
        out.push(Clause {
            op: Op::Lt,
            version: upper,
        });
        return Some(());
    }

    let (v, count) = parse_partial(original)?;
    let wildcard = original.contains('*') || original.contains('x') || original.contains('X');
    let clause = Clause { op, version: v };
    if wildcard {
        // "18.x" / "3.12.*" -> >=18.0.0,<19.0.0 / >=3.12.0,<3.13.0
        let upper = if count <= 1 {
            Version::new(v.major.saturating_add(1), 0, 0)
        } else {
            Version::new(v.major, v.minor.saturating_add(1), 0)
        };
        out.push(Clause {
            op: Op::Ge,
            version: v,
        });
        out.push(Clause {
            op: Op::Lt,
            version: upper,
        });
    } else {
        out.push(clause);
    }
    Some(())
}

fn caret_upper(v: Version, count: usize) -> Version {
    if v.major > 0 {
        Version::new(v.major.saturating_add(1), 0, 0)
    } else if v.minor > 0 {
        Version::new(0, v.minor.saturating_add(1), 0)
    } else if count >= 3 {
        Version::new(0, 0, v.patch.saturating_add(1))
    } else {
        Version::new(0, 1, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_runtime_banners() {
        assert_eq!(
            Version::parse("Python 3.12.8"),
            Some(Version::new(3, 12, 8))
        );
        assert_eq!(Version::parse("v22.12.0"), Some(Version::new(22, 12, 0)));
        assert_eq!(Version::parse("go1.23.4"), Some(Version::new(1, 23, 4)));
        assert_eq!(
            Version::parse_java("openjdk 21.0.5 2025-01-21"),
            Some(Version::new(21, 0, 5))
        );
        assert_eq!(
            Version::parse_java("1.8.0_401"),
            Some(Version::new(8, 0, 0))
        );
        assert_eq!(Version::parse("nonsense"), None);
    }

    #[test]
    fn python_range_works() {
        let req = VersionReq::parse(">=3.9,<3.13").unwrap();
        assert!(req.satisfies(Version::new(3, 10, 0)));
        assert!(req.satisfies(Version::new(3, 12, 9)));
        assert!(!req.satisfies(Version::new(3, 13, 0)));
        assert!(!req.satisfies(Version::new(3, 8, 9)));
        assert_eq!(req.minimum(), Some(Version::new(3, 9, 0)));
        assert_eq!(req.upper_exclusive(), Some(Version::new(3, 13, 0)));
    }

    #[test]
    fn npm_ranges_work() {
        let caret = VersionReq::parse("^18.17.0").unwrap();
        assert!(caret.satisfies(Version::new(18, 20, 0)));
        assert!(!caret.satisfies(Version::new(19, 0, 0)));
        let tilde = VersionReq::parse("~20.11.1").unwrap();
        assert!(tilde.satisfies(Version::new(20, 11, 9)));
        assert!(!tilde.satisfies(Version::new(20, 12, 0)));
        let wildcard = VersionReq::parse("18.x").unwrap();
        assert!(wildcard.satisfies(Version::new(18, 99, 0)));
        assert!(!wildcard.satisfies(Version::new(19, 0, 0)));
        let any = VersionReq::parse("*").unwrap();
        assert!(any.satisfies(Version::new(1, 0, 0)));
    }

    #[test]
    fn alternatives_work() {
        let req = VersionReq::parse("^18 || ^20").unwrap();
        assert!(req.satisfies(Version::new(18, 0, 0)));
        assert!(req.satisfies(Version::new(20, 5, 0)));
        assert!(!req.satisfies(Version::new(19, 0, 0)));
    }
}