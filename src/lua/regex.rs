use std::{cmp, error::Error, fmt::Display, sync::Arc};

use case_conv_macros::identifier_to_camel;
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
            RegexOptionError::BadLineTerminatorString { string,
                                                        byte_count } => {
                write!(f, "lineTerminator '{string}' was {byte_count} bytes long; it must be 1 byte")
            },
            RegexOptionError::BadLineTerminatorByte { num } => {
                if *num >= -128 && *num <= 255 {
                    write!(f, "lineTerminator byte value 0x{:X} is not a valid ASCII character", *num as u8)
                }
                else {
                    write!(f, "lineTerminator '{num}' cannot be stored in a single byte")
                }
            },
            RegexOptionError::BadLineTerminatorType { actual_type } => {
                write!(f, "lineTerminator must be a string or integer, but it was a {actual_type}")
            }
        }
    }
}

impl Error for RegexOptionError {}

static RECOGNIZED_REGEX_OPTIONS: [&'static str; 9] = [
    "unicode",
    "caseInsensitive",
    "multiLine",
    "dotMatchesNewLine",
    "crlf",
    "lineTerminator",
    "swapGreed",
    "ignoreWhitespace",
    "octal"
];

fn validate_regex_options_keys(options: &LuaTable) -> LuaResult<()> {
    for pair in options.pairs::<LuaValue, LuaValue>() {
        let (key, _) = pair?;
        if let LuaValue::String(key) = key {
            let key = key.to_string_lossy();
            if !RECOGNIZED_REGEX_OPTIONS.contains(&key.as_str()) {
                return Err(LuaError::ExternalError(Arc::new(
                    RegexOptionError::UnknownOption {
                        option: key.into_boxed_str()
                    }
                )));
            }
        }
        else {
            return Err(LuaError::ExternalError(Arc::new(
                RegexOptionError::UnknownOption {
                    option: key.to_string().unwrap_or_default().into_boxed_str()
                }
            )));
        }
    }

    Ok(())
}

fn set_line_terminator<'a>(builder: &'a mut RegexBuilder,
                           options: &LuaTable) -> LuaResult<&'a mut RegexBuilder> {
    let line_terminator: LuaValue = options.get("lineTerminator")?;
    let mut line_terminator_specified = true;
    let builder = match line_terminator {
        LuaValue::String(char) => {
            let bytes = char.as_bytes();
            if bytes.len() != 1 {
                Err(LuaError::ExternalError(Arc::new(
                    RegexOptionError::BadLineTerminatorString {
                        string: char.to_string_lossy().into_boxed_str(),
                        byte_count: bytes.len()
                    }
                )))
            }
            else if bytes[0] > 127 {
                Err(LuaError::ExternalError(Arc::new(
                    RegexOptionError::BadLineTerminatorByte {
                        num: bytes[0] as i64
                    }
                )))
            }
            else {
                Ok(builder.line_terminator(bytes[0]))
            }
        },
        LuaValue::Integer(num) => {
            let byte = u8::try_from(num).map_err(|_| ()).and_then(|byte| {
                if byte <= 127 {
                    Ok(byte)
                }
                else {
                    Err(())
                }
            });
            match byte {
                Ok(byte) => Ok(builder.line_terminator(byte)),
                Err(_) => Err(LuaError::ExternalError(Arc::new(
                    RegexOptionError::BadLineTerminatorByte { num }
                )))
            }
        },
        LuaValue::Number(_) => Err(LuaError::ExternalError(Arc::new(
            RegexOptionError::BadLineTerminatorType {
                actual_type: "float"
            }
        ))),
        LuaValue::Nil => {
            line_terminator_specified = false;
            Ok(builder)
        },
        value => Err(LuaError::ExternalError(Arc::new(
            RegexOptionError::BadLineTerminatorType {
                actual_type: value.type_name()
            }
        )))
    }?;
    
    if line_terminator_specified {
        // Turn off crlf because it takes precedence over line_terminator
        Ok(builder.crlf(false))
    }
    else {
        Ok(builder)
    }
}

// TODO: Cache compiled pattern, will need to figure out how to hash a LuaTable
// by value
fn regex_from_options(pattern: &str,
                      options: Option<LuaTable>) -> Result<Regex, LuaError> {
    let mut builder = RegexBuilder::new(pattern);
    builder.crlf(true);
    builder.size_limit(usize::MAX);

    if let Some(options) = options {
        validate_regex_options_keys(&options)?;

        macro_rules! set_bool_option {
            ($option:ident) => {
                let $option: LuaValue = options.get(identifier_to_camel!($option))?;
                match $option {
                    LuaValue::Boolean(bool) => builder.$option(bool),
                    LuaValue::Nil => &builder,
                    _ => builder.$option(true)
                }
            };
        }

        set_bool_option!(unicode);
        set_bool_option!(case_insensitive);
        set_bool_option!(multi_line);
        set_bool_option!(dot_matches_new_line);
        // Above crlf because specifying lineTerminator turns off crlf
        set_line_terminator(&mut builder, &options)?;
        set_bool_option!(crlf);
        set_bool_option!(swap_greed);
        set_bool_option!(ignore_whitespace);
        set_bool_option!(octal);
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
        lua.create_function(|lua, (pattern, haystack,
                                   start, options): (LuaString, LuaString,
                                                     Option<LuaInteger>,
                                                     Option<LuaTable>)| {
            let start = deluaify_index(lua, start.unwrap_or(1), &haystack);
            let regex = regex_from_options(&pattern.to_string_lossy(), options)?;
            
            Ok(regex.is_match_at(&haystack.as_bytes(), start))
        })?,
    )?;

    Ok(table)
}