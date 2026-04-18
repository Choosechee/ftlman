use std::{cmp, collections::HashSet, error::Error, fmt::Display, sync::{Arc, LazyLock}};

use mlua::prelude::*;
// using bytes version because Lua strings can be invalid UTF-8
use regex::bytes::{Regex, RegexBuilder};

#[derive(Debug)]
enum RegexOptionError {
    UnknownOption { option: Box<str> },
    BadLineTerminatorString { string: Box<str>, byte_count: usize },
    BadLineTerminatorByte { num: i64 },
    BadLineTerminatorType { actual_type: &'static str }
}

impl Display for RegexOptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegexOptionError::UnknownOption { option } => {
                write!(f, "unknown option '{option}'")
            }
            RegexOptionError::BadLineTerminatorString { string, byte_count } => {
                write!(f, "lineTerminator '{string}' was {byte_count} bytes long; it must be 1 byte")
            },
            RegexOptionError::BadLineTerminatorByte { num } => {
                write!(f, "lineTerminator '{num}' cannot be stored in a single byte")
            },
            RegexOptionError::BadLineTerminatorType { actual_type } => {
                write!(f, "lineTerminator must be a string or integer, but it was a {actual_type}")
            }
        }
    }
}

impl Error for RegexOptionError {}

static RECOGNIZED_REGEX_OPTIONS: LazyLock<HashSet<&'static str>> =
LazyLock::new(|| {
    HashSet::from([
        "unicode",
        "caseInsensitive",
        "multiLine",
        "dotMatchesNewLine",
        "crlf",
        "lineTerminator",
        "swapGreed",
        "ignoreWhitespace",
        "octal"
    ])
});

// TODO: Cache compiled pattern, will need to figure out how to hash a LuaTable
// by value
fn regex_from_options(pattern: &str, options: Option<LuaTable>) -> Result<Regex, LuaError> {
    let mut builder = RegexBuilder::new(pattern);
    builder.crlf(true);
    builder.size_limit(usize::MAX);

    if let Some(options) = options {
        for pair in options.pairs::<LuaValue, LuaValue>() {
            let (key, _) = pair?;
            if let LuaValue::String(key) = key {
                let key = key.to_string_lossy();
                if !RECOGNIZED_REGEX_OPTIONS.contains(key.as_str()) {
                    return Err(LuaError::ExternalError(Arc::new(RegexOptionError::UnknownOption { option: key.into_boxed_str() })));
                }
            }
            else {
                return Err(LuaError::ExternalError(Arc::new(RegexOptionError::UnknownOption { option: key.to_string().unwrap_or_default().into_boxed_str() })));
            }
        }
        
        let unicode: LuaValue = options.get("unicode")?;
        match unicode {
            LuaValue::Boolean(bool) => builder.unicode(bool),
            LuaValue::Nil => &builder,
            _ => builder.unicode(true)
        };

        let case_insensitive: LuaValue = options.get("caseInsensitive")?;
        match case_insensitive {
            LuaValue::Boolean(bool) => builder.case_insensitive(bool),
            LuaValue::Nil => &builder,
            _ => builder.case_insensitive(true)
        };

        let multi_line: LuaValue = options.get("multiLine")?;
        match multi_line {
            LuaValue::Boolean(bool) => builder.multi_line(bool),
            LuaValue::Nil => &builder,
            _ => builder.multi_line(true)
        };

        let dot_matches_new_line: LuaValue = options.get("dotMatchesNewLine")?;
        match dot_matches_new_line {
            LuaValue::Boolean(bool) => builder.dot_matches_new_line(bool),
            LuaValue::Nil => &builder,
            _ => builder.dot_matches_new_line(true)
        };

        let crlf: LuaValue = options.get("crlf")?;
        match crlf {
            LuaValue::Boolean(bool) => builder.crlf(bool),
            LuaValue::Nil => &builder,
            _ => builder.crlf(true)
        };

        let line_terminator: LuaValue = options.get("lineTerminator")?;
        match line_terminator {
            LuaValue::String(char) => {
                let bytes = char.as_bytes();
                if bytes.len() != 1 {
                    return Err(LuaError::ExternalError(Arc::new(RegexOptionError::BadLineTerminatorString { string: char.to_string_lossy().into_boxed_str(), byte_count: bytes.len() })));
                }

                builder.line_terminator(bytes[0])
            },
            LuaValue::Integer(num) => {
                if num >= 0 {
                    let byte = u8::try_from(num);
                    match byte {
                        Ok(byte) => builder.line_terminator(byte),
                        Err(_) => return Err(LuaError::ExternalError(Arc::new(RegexOptionError::BadLineTerminatorByte { num })))
                    }
                }
                else {
                    let byte = i8::try_from(num);
                    match byte {
                        Ok(byte) => builder.line_terminator(byte as u8),
                        Err(_) => return Err(LuaError::ExternalError(Arc::new(RegexOptionError::BadLineTerminatorByte { num })))
                    }
                }
            },
            LuaValue::Number(_) => return Err(LuaError::ExternalError(Arc::new(RegexOptionError::BadLineTerminatorType { actual_type: "float" }))),
            LuaValue::Nil => &builder,
            value => return Err(LuaError::ExternalError(Arc::new(RegexOptionError::BadLineTerminatorType { actual_type: value.type_name() })))
        };

        let swap_greed: LuaValue = options.get("swapGreed")?;
        match swap_greed {
            LuaValue::Boolean(bool) => builder.swap_greed(bool),
            LuaValue::Nil => &builder,
            _ => builder.swap_greed(true)
        };

        let ignore_whitespace: LuaValue = options.get("ignoreWhitespace")?;
        match ignore_whitespace {
            LuaValue::Boolean(bool) => builder.ignore_whitespace(bool),
            LuaValue::Nil => &builder,
            _ => builder.ignore_whitespace(true)
        };

        let octal: LuaValue = options.get("octal")?;
        match octal {
            LuaValue::Boolean(bool) => builder.octal(bool),
            LuaValue::Nil => &builder,
            _ => builder.octal(true)
        };
    }

    builder.build().map_err(|error| {
        LuaError::ExternalError(Arc::new(error))
    })
}

fn deluaify_index(lua: &Lua, index: i64, string: &LuaString) -> usize {
    let string_len = lua.globals()
                        .get::<LuaTable>("string").unwrap()
                        .get::<LuaFunction>("len").unwrap()
                        .call::<i64>(string).unwrap();
    if index >= 0 {
        cmp::min(cmp::max(index - 1, 0), string_len) as usize
    }
    else {
        cmp::max(index + string_len, 0) as usize
    }
}

pub fn create_regex_lib(lua: &Lua) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;

    table.raw_set(
        "is_match",
        lua.create_function(|lua, (pattern, haystack, start, options): (LuaString, LuaString, Option<LuaInteger>, Option<LuaTable>)| {
            let start = deluaify_index(lua, start.unwrap_or(1), &haystack);
            let regex = regex_from_options(&pattern.to_string_lossy(), options)?;
            
            Ok(regex.is_match_at(&haystack.as_bytes(), start))
        })?,
    )?;

    Ok(table)
}