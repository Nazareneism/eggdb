use crate::YqlError;

/// A token produced by the YQL tokenizer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Commands
    Show,
    Find,
    List,
    Count,
    Sum,
    Avg,
    Min,
    Max,

    // Clauses
    Where,
    By,
    Sort,
    Limit,
    And,
    Between,
    In,
    Like,
    Asc,
    Desc,

    // Functions
    Month,
    Year,
    Day,
    Date,
    Lower,
    Upper,
    Length,

    // Literals & identifiers
    Ident(String),
    StringLit(String),
    NumberLit(f64),
    Null,

    // Punctuation
    Dot,
    Comma,
    LParen,
    RParen,

    // Operators
    Eq,
    Ne,
    Gt,
    Lt,
    Gte,
    Lte,
}

/// Tokenize a YQL input string into a sequence of tokens.
pub fn tokenize(input: &str) -> Result<Vec<Token>, YqlError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];

        // Skip whitespace
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Punctuation
        match c {
            '.' => {
                tokens.push(Token::Dot);
                i += 1;
                continue;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
                continue;
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
                continue;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
                continue;
            }
            _ => {}
        }

        // Operators
        if c == '!' && i + 1 < len && chars[i + 1] == '=' {
            tokens.push(Token::Ne);
            i += 2;
            continue;
        }
        if c == '>' && i + 1 < len && chars[i + 1] == '=' {
            tokens.push(Token::Gte);
            i += 2;
            continue;
        }
        if c == '<' && i + 1 < len && chars[i + 1] == '=' {
            tokens.push(Token::Lte);
            i += 2;
            continue;
        }
        if c == '=' {
            tokens.push(Token::Eq);
            i += 1;
            continue;
        }
        if c == '>' {
            tokens.push(Token::Gt);
            i += 1;
            continue;
        }
        if c == '<' {
            tokens.push(Token::Lt);
            i += 1;
            continue;
        }

        // Quoted strings (single or double)
        if c == '\'' || c == '"' {
            let quote = c;
            i += 1;
            let start = i;
            while i < len && chars[i] != quote {
                i += 1;
            }
            if i >= len {
                return Err(YqlError::UnterminatedString(start - 1));
            }
            let s: String = chars[start..i].iter().collect();
            tokens.push(Token::StringLit(s));
            i += 1; // skip closing quote
            continue;
        }

        // Numbers (including negative via leading -)
        if c.is_ascii_digit() || (c == '-' && i + 1 < len && chars[i + 1].is_ascii_digit()) {
            let start = i;
            if c == '-' {
                i += 1;
            }
            while i < len && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i < len && chars[i] == '.' && i + 1 < len && chars[i + 1].is_ascii_digit() {
                i += 1; // skip decimal point
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let num_str: String = chars[start..i].iter().collect();
            let num = num_str
                .parse::<f64>()
                .map_err(|_| YqlError::InvalidNumber(num_str))?;
            tokens.push(Token::NumberLit(num));
            continue;
        }

        // Identifiers and keywords
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let token = match word.to_ascii_lowercase().as_str() {
                "show" => Token::Show,
                "find" => Token::Find,
                "list" => Token::List,
                "count" => Token::Count,
                "sum" => Token::Sum,
                "avg" => Token::Avg,
                "min" => Token::Min,
                "max" => Token::Max,
                "where" => Token::Where,
                "by" => Token::By,
                "sort" => Token::Sort,
                "limit" => Token::Limit,
                "and" => Token::And,
                "between" => Token::Between,
                "in" => Token::In,
                "like" => Token::Like,
                "asc" => Token::Asc,
                "desc" => Token::Desc,
                "month" => Token::Month,
                "year" => Token::Year,
                "day" => Token::Day,
                "date" => Token::Date,
                "lower" => Token::Lower,
                "upper" => Token::Upper,
                "length" => Token::Length,
                "null" => Token::Null,
                _ => Token::Ident(word),
            };
            tokens.push(token);
            continue;
        }

        return Err(YqlError::UnexpectedChar(c, i));
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_simple_query() {
        let tokens = tokenize("show users where x = 1 sort name limit 10").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Show,
                Token::Ident("users".into()),
                Token::Where,
                Token::Ident("x".into()),
                Token::Eq,
                Token::NumberLit(1.0),
                Token::Sort,
                Token::Ident("name".into()),
                Token::Limit,
                Token::NumberLit(10.0),
            ]
        );
    }

    #[test]
    fn tokenize_operators() {
        let tokens = tokenize("a != b >= c <= d > e < f").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::Ne,
                Token::Ident("b".into()),
                Token::Gte,
                Token::Ident("c".into()),
                Token::Lte,
                Token::Ident("d".into()),
                Token::Gt,
                Token::Ident("e".into()),
                Token::Lt,
                Token::Ident("f".into()),
            ]
        );
    }

    #[test]
    fn tokenize_strings() {
        let tokens = tokenize(r#"where name = "Alice" and city = 'NYC'"#).unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Where,
                Token::Ident("name".into()),
                Token::Eq,
                Token::StringLit("Alice".into()),
                Token::And,
                Token::Ident("city".into()),
                Token::Eq,
                Token::StringLit("NYC".into()),
            ]
        );
    }

    #[test]
    fn tokenize_dotted() {
        let tokens = tokenize("users.name").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Ident("users".into()),
                Token::Dot,
                Token::Ident("name".into()),
            ]
        );
    }

    #[test]
    fn tokenize_function_call() {
        let tokens = tokenize("month(created_at)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Month,
                Token::LParen,
                Token::Ident("created_at".into()),
                Token::RParen,
            ]
        );
    }

    #[test]
    fn tokenize_negative_number() {
        let tokens = tokenize("-42").unwrap();
        assert_eq!(tokens, vec![Token::NumberLit(-42.0)]);
    }

    #[test]
    fn tokenize_decimal_number() {
        let tokens = tokenize("3.14").unwrap();
        assert_eq!(tokens, vec![Token::NumberLit(3.14)]);
    }

    #[test]
    fn unterminated_string() {
        let result = tokenize("name = 'hello");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            YqlError::UnterminatedString(_)
        ));
    }

    #[test]
    fn unexpected_char() {
        let result = tokenize("name @ value");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            YqlError::UnexpectedChar('@', 5)
        ));
    }

    #[test]
    fn tokenize_between() {
        let tokens = tokenize("between 10 and 50").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Between,
                Token::NumberLit(10.0),
                Token::And,
                Token::NumberLit(50.0),
            ]
        );
    }

    #[test]
    fn tokenize_in_list() {
        let tokens = tokenize(r#"in ("US", "UK")"#).unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::In,
                Token::LParen,
                Token::StringLit("US".into()),
                Token::Comma,
                Token::StringLit("UK".into()),
                Token::RParen,
            ]
        );
    }

    #[test]
    fn tokenize_null() {
        let tokens = tokenize("null").unwrap();
        assert_eq!(tokens, vec![Token::Null]);
    }

    #[test]
    fn case_insensitive_keywords() {
        let tokens = tokenize("SHOW Users WHERE X = 1").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Show,
                Token::Ident("Users".into()),
                Token::Where,
                Token::Ident("X".into()),
                Token::Eq,
                Token::NumberLit(1.0),
            ]
        );
    }
}
