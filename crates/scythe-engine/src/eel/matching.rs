//! EEL2's `match`/`matchi`: `*` matches any run of characters, `?` one, `+` one or more, and
//! `%d`, `%f`, `%x`, `%s` … match a field that is captured into the next argument.

/// A captured field: a number, or text for `%s`.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Field {
    Num(f64),
    Text(String),
}

/// The fields `pattern` captures from `text` when it matches the whole of it.
pub(super) fn matches(pattern: &str, text: &str, fold: bool) -> Option<Vec<Field>> {
    let chars = |s: &str| -> Vec<char> { s.chars().map(|c| if fold { c.to_ascii_lowercase() } else { c }).collect() };
    let (p, t) = (chars(pattern), chars(text));
    let original: Vec<char> = text.chars().collect();
    let mut fields = Vec::new();
    step(&p, &t, &original, 0, &mut fields).then_some(fields)
}

fn field(conv: char, s: &[char]) -> Option<Field> {
    let s: String = s.iter().collect();
    let n = s.trim();
    let num = match conv {
        's' => return Some(Field::Text(s)),
        'c' => s.chars().next()? as u32 as f64,
        'x' | 'X' => i64::from_str_radix(n, 16).ok()? as f64,
        'd' | 'i' | 'u' => n.parse::<i64>().ok()? as f64,
        _ => n.parse::<f64>().ok()?,
    };
    Some(Field::Num(num))
}

fn step(p: &[char], t: &[char], original: &[char], at: usize, fields: &mut Vec<Field>) -> bool {
    let Some(&c) = p.first() else { return t.is_empty() };
    let rest = &p[1..];
    match c {
        '*' | '+' => {
            let min = (c == '+') as usize;
            (min..=t.len()).rev().any(|n| step(rest, &t[n..], original, at + n, fields))
        }
        '?' => !t.is_empty() && step(rest, &t[1..], original, at + 1, fields),
        '%' if rest.first() == Some(&'%') => {
            t.first() == Some(&'%') && step(&rest[1..], &t[1..], original, at + 1, fields)
        }
        '%' => {
            let spec = rest.iter().position(|ch| ch.is_ascii_alphabetic());
            let Some(k) = spec else { return false };
            let conv = rest[k];
            let after = &rest[k + 1..];
            let max = if conv == 'c' { 1.min(t.len()) } else { t.len() };
            (1..=max).rev().any(|n| {
                let Some(f) = field(conv, &original[at..at + n]) else { return false };
                let mark = fields.len();
                fields.push(f);
                step(after, &t[n..], original, at + n, fields) || {
                    fields.truncate(mark);
                    false
                }
            })
        }
        _ => t.first() == Some(&c) && step(rest, &t[1..], original, at + 1, fields),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_and_fields() {
        assert!(matches("a*c", "abbbc", false).is_some());
        assert!(matches("a+c", "ac", false).is_none());
        assert!(matches("A?C", "abc", true).is_some());
        assert_eq!(
            matches("%-3d\t%s\r*", "60 \tMiddle C\r\n", false),
            Some(vec![Field::Num(60.0), Field::Text("Middle C".into())])
        );
        assert_eq!(matches("%d %s", "x y", false), None);
    }
}
