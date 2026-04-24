use std::{
    cmp, error::Error, fmt::Display,hash::Hash, mem::MaybeUninit,
    ptr, sync::{Arc, Mutex, RwLock, atomic::{AtomicUsize,
                                             Ordering::Relaxed as SafeOrd}}
};

use mlua::prelude::*;
// using bytes version because Lua strings can be invalid UTF-8
use regex::bytes::{Regex, RegexBuilder};

#[derive(Debug)]
enum RegexOptionError {
    UnknownOption { option: Box<str> },
    BadLineTerminatorString { string: Box<str>, byte_count: usize },
    BadLineTerminatorByte { num: i64 },
    BadLineTerminatorType { actual_type: &'static str },
    UnicodeButNonAsciiLineTerminator { byte: u8 },
    Other(Box<dyn Error + Send + Sync>)
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
                write!(f, "lineTerminator '{num}' cannot be stored in a single byte")
            },
            RegexOptionError::BadLineTerminatorType { actual_type } => {
                write!(f, "lineTerminator must be a string or integer, but it was a {actual_type}")
            },
            RegexOptionError::UnicodeButNonAsciiLineTerminator { byte } => {
                write!(f, "lineTerminator was 0x{:X}, but it must be an ASCII byte when unicode option is enabled", byte)
            },
            RegexOptionError::Other(error) => {
                write!(f, "{}", error)
            }
        }
    }
}

impl Error for RegexOptionError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RegexOptions {
    bool_options: u8,
    pub line_terminator: u8
}

impl RegexOptions {
    fn new() -> Self {
        // bool_options corresponds to:
        // unicode = true
        // case_insensitive = false
        // multi_line = false
        // dot_matches_new_line = false
        // crlf = true (not standard for RegexBuilder)
        // swap_greed = false
        // ignore_whitespace = false
        // octal = false
        Self { bool_options: 0b00010001, line_terminator: b'\n' }
    }

    fn unicode(&self) -> bool {
        self.bool_options & 0b00000001 != 0
    }

    fn set_unicode(&mut self, value: bool) {
        if value {
            self.bool_options |= 0b00000001;
        }
        else {
            self.bool_options &= !0b00000001;
        }
    }

    fn case_insensitive(&self) -> bool {
        self.bool_options & 0b00000010 != 0
    }

    fn set_case_insensitive(&mut self, value: bool) {
        if value {
            self.bool_options |= 0b00000010;
        }
        else {
            self.bool_options &= !0b00000010;
        }
    }

    fn multi_line(&self) -> bool {
        self.bool_options & 0b00000100 != 0
    }

    fn set_multi_line(&mut self, value: bool) {
        if value {
            self.bool_options |= 0b00000100;
        }
        else {
            self.bool_options &= !0b00000100;
        }
    }

    fn dot_matches_new_line(&self) -> bool {
        self.bool_options & 0b00001000 != 0
    }

    fn set_dot_matches_new_line(&mut self, value: bool) {
        if value {
            self.bool_options |= 0b00001000;
        }
        else {
            self.bool_options &= !0b00001000;
        }
    }

    fn crlf(&self) -> bool {
        self.bool_options & 0b00010000 != 0
    }

    fn set_crlf(&mut self, value: bool) {
        if value {
            self.bool_options |= 0b00010000;
        }
        else {
            self.bool_options &= !0b00010000;
        }
    }

    fn swap_greed(&self) -> bool {
        self.bool_options & 0b00100000 != 0
    }

    fn set_swap_greed(&mut self, value: bool) {
        if value {
            self.bool_options |= 0b00100000;
        }
        else {
            self.bool_options &= !0b00100000;
        }
    }

    fn ignore_whitespace(&self) -> bool {
        self.bool_options & 0b01000000 != 0
    }

    fn set_ignore_whitespace(&mut self, value: bool) {
        if value {
            self.bool_options |= 0b01000000;
        }
        else {
            self.bool_options &= !0b01000000;
        }
    }

    fn octal(&self) -> bool {
        self.bool_options & 0b10000000 != 0
    }

    fn set_octal(&mut self, value: bool) {
        if value {
            self.bool_options |= 0b10000000;
        }
        else {
            self.bool_options &= !0b10000000;
        }
    }

    fn apply_to_builder<'a>(&self, builder: &'a mut RegexBuilder) -> Result<&'a mut RegexBuilder, RegexOptionError> {
        builder.unicode(self.unicode())
               .case_insensitive(self.case_insensitive())
               .multi_line(self.multi_line())
               .dot_matches_new_line(self.dot_matches_new_line())
               .crlf(self.crlf())
               .swap_greed(self.swap_greed())
               .ignore_whitespace(self.ignore_whitespace())
               .octal(self.octal());
        
        if !self.unicode() || self.line_terminator <= 127 {
            Ok(builder.line_terminator(self.line_terminator))
        }
        else {
            Err(RegexOptionError::UnicodeButNonAsciiLineTerminator {
                byte: self.line_terminator
            })
        }
    }
}

fn lua_value_truth(value: &LuaValue) -> bool {
    match value {
        LuaValue::Nil => false,
        LuaValue::Boolean(bool) => *bool,
        _ => true
    }
}

fn set_line_terminator(options: &mut RegexOptions,
                       line_terminator: &LuaValue) -> Result<(), RegexOptionError> {
    let line_terminator = match line_terminator {
        LuaValue::String(char) => {
            let bytes = char.as_bytes();
            if bytes.len() != 1 {
                Err(RegexOptionError::BadLineTerminatorString {
                    string: char.to_string_lossy().into_boxed_str(),
                    byte_count: bytes.len()
                })
            }
            else {
                Ok(bytes[0])
            }
        },
        LuaValue::Integer(num) => {
            if *num >= 0 {
                let byte = u8::try_from(*num);
                match byte {
                    Ok(byte) => Ok(byte),
                    Err(_) => Err(RegexOptionError::BadLineTerminatorByte {
                        num: *num 
                    })
                }
            }
            else {
                let byte = i8::try_from(*num);
                match byte {
                    Ok(byte) => Ok(byte as u8),
                    Err(_) => Err(RegexOptionError::BadLineTerminatorByte {
                        num: *num
                    })
                }
            }
        },
        LuaValue::Number(_) => Err(RegexOptionError::BadLineTerminatorType {
            actual_type: "float"
        }),
        value => Err(RegexOptionError::BadLineTerminatorType {
            actual_type: value.type_name()
        }),
    }?;

    options.line_terminator = line_terminator;
    Ok(())
}

impl TryFrom<LuaTable> for RegexOptions {
    type Error = RegexOptionError;

    fn try_from(value: LuaTable) -> Result<Self, Self::Error> {
        let mut options = Self::new();
        let mut set_crlf_explicitly = false;

        for pair in value.pairs::<LuaValue, LuaValue>() {
            let (key, value) = pair.map_err(|error| RegexOptionError::Other(Box::new(error)))?;
            let key = key.as_string_lossy()
                         .map(|string| string.into_boxed_str())
                         .or_else(|| key.to_string().ok()
                                        .map(|string| string.into_boxed_str()))
                         .unwrap_or_else(|| Box::from(key.type_name()));
            
            match key.as_ref() {
                "unicode" => options.set_unicode(lua_value_truth(&value)),
                "caseInsensitive" => options.set_case_insensitive(lua_value_truth(&value)),
                "multiLine" => options.set_multi_line(lua_value_truth(&value)),
                "dotMatchesNewLine" => options.set_dot_matches_new_line(lua_value_truth(&value)),
                "crlf" => {
                    options.set_crlf(lua_value_truth(&value));
                    set_crlf_explicitly = true;
                }
                "lineTerminator" => {
                    set_line_terminator(&mut options, &value)?;
                    if !set_crlf_explicitly {
                        // crlf overrides lineTerminator and is on by default
                        // for us, so if lineTerminator is specified without
                        // crlf, turn off crlf
                        options.set_crlf(false);
                    }
                },
                "swapGreed" => options.set_swap_greed(lua_value_truth(&value)),
                "ignoreWhitespace" => options.set_ignore_whitespace(lua_value_truth(&value)),
                "octal" => options.set_octal(lua_value_truth(&value)),
                _ => return Err(RegexOptionError::UnknownOption { option: key })
            }
        }

        Ok(options)
    }
}

const MAX_REGEX_CACHE_SIZE: usize = 16;
static REGEX_CACHE: RwLock<[MaybeUninit<((Box<str>, RegexOptions),
                                         Arc<Mutex<Option<Arc<Regex>>>>)>;
                                         MAX_REGEX_CACHE_SIZE]> =
    RwLock::new([const { MaybeUninit::uninit() }; MAX_REGEX_CACHE_SIZE]);
static CACHED_REGEXES: AtomicUsize = AtomicUsize::new(0);

// if multithreading is ever possible, make the following changes:
// - change the variant SafeOrd aliases to SeqCst
// - uncomment let cache_len = CACHED_REGEXES.load(SafeOrd);
// - search cache again after acquiring write lock in case another thread
//   inserted a matching entry
fn try_regex_cache_hit(pattern: &str, options: RegexOptions)
-> Arc<Mutex<Option<Arc<Regex>>>> {
    let cache = REGEX_CACHE.read().unwrap();
    let cache_len = CACHED_REGEXES.load(SafeOrd);

    for item in cache[..cache_len].iter() {
        let (key, regex) = unsafe { item.assume_init_ref() };
        if pattern == key.0.as_ref() && options == key.1 {
            return regex.clone();
        }
    }

    // insert spot for new item in cache, which will be filled in by caller
    drop(cache); // release read lock before acquiring write lock
    let mut cache = REGEX_CACHE.write().unwrap();
    // let cache_len = CACHED_REGEXES.load(SafeOrd);
    let new_item = ((pattern.into(), options), Arc::new(Mutex::new(None)));

    for i in (1..(cmp::min(cache_len, MAX_REGEX_CACHE_SIZE - 1) + 1)).rev() {
        let prev_item = unsafe { ptr::read(&cache[i - 1]).assume_init() };
        if i == MAX_REGEX_CACHE_SIZE - 1 && cache_len == MAX_REGEX_CACHE_SIZE {
            // cache is full, drop last item
            unsafe { cache[i].assume_init_drop() };
        }
        cache[i].write(prev_item);
    }
    cache[0].write(new_item);

    if cache_len < MAX_REGEX_CACHE_SIZE {
        CACHED_REGEXES.fetch_add(1, SafeOrd);
    }
    unsafe { cache[0].assume_init_ref().1.clone() }
}

// removes entries that were not filled in by caller due to errors
// not strictly necessary, but prevents performance degradation
fn repair_regex_cache() {
    // I got lazy
    let mut cache_write_lock = REGEX_CACHE.write().unwrap();
    let cache_ptr = cache_write_lock.as_mut_ptr()
                    as *mut ((Box<str>, RegexOptions), Arc<Mutex<Option<Arc<Regex>>>>);
    let cache_len = CACHED_REGEXES.load(SafeOrd);

    let mut cache = unsafe { Vec::from_raw_parts(cache_ptr, cache_len,
                                                 MAX_REGEX_CACHE_SIZE) };
    cache.retain_mut(|entry| entry.1.lock().unwrap().is_some());

    // this also prvents cache_ptr from being deallocated
    let (_, cache_len, _) = cache.into_raw_parts();
    CACHED_REGEXES.store(cache_len, SafeOrd);
}

// TODO: require pattern to be valid UTF-8
fn regex_from_options(pattern: &str,
                      options: Option<LuaTable>) -> Result<Arc<Regex>, LuaError> {
    let options = options.map(RegexOptions::try_from)
                         .transpose()
                         .map_err(|error| LuaError::ExternalError(Arc::new(error)))?
                         .unwrap_or(RegexOptions::new());
    
    let maybe_regex_mutex = try_regex_cache_hit(pattern, options);
    let mut maybe_regex = maybe_regex_mutex.lock().unwrap();
    if let Some(regex) = &*maybe_regex {
        return Ok(regex.clone());
    }

    let mut builder = RegexBuilder::new(pattern);
    match options.apply_to_builder(&mut builder) {
        Ok(_) => (),
        Err(error) => {
            repair_regex_cache();
            return Err(LuaError::ExternalError(Arc::new(error)));
        },
    }
    // 64 MiB
    builder.size_limit(64 * (1 << 20));

    let regex = match builder.build() {
        Ok(regex) => Arc::new(regex),
        Err(error) => {
            repair_regex_cache();
            return Err(LuaError::ExternalError(Arc::new(error)));
        }
    };

    Ok(maybe_regex.insert(regex).clone())
}

fn deluaify_index(index: i64, string: &LuaString) -> usize {
    let string_len = string.as_bytes().len() as i64;
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
        lua.create_function(|_lua, (pattern, haystack,
                                    start, options): (LuaString, LuaString,
                                                      Option<LuaInteger>,
                                                      Option<LuaTable>)| {
            let start = deluaify_index(start.unwrap_or(1), &haystack);
            let regex = regex_from_options(&pattern.to_string_lossy(), options)?;
            
            Ok(regex.is_match_at(&haystack.as_bytes(), start))
        })?,
    )?;

    // DEBUG FROM NOW ON. REMOVE LATER
    table.raw_set(
        "print_cache",
        lua.create_function(|_lua, _: ()| {
            let cache = REGEX_CACHE.read().unwrap();
            let cache_len = CACHED_REGEXES.load(SafeOrd);

            unsafe {
                let regexes_str = cache[0..cache_len].iter().map(|kv| {
                    let maybe_r = kv.assume_init_ref().1.lock().unwrap();
                    maybe_r.clone().map(|r| r.as_str() as *const str)
                                   .unwrap_or("⧘None⧙")
                }).fold(String::new(), |a, b| a + b.as_ref_unchecked() + ", ");
                println!("{}", &regexes_str[..(regexes_str.len() - 2)]);
            }

            Ok(())
        })?
    )?;

    table.raw_set(
        "clear_cache",
        lua.create_function(|_lua, _: ()| {
            let mut cache = REGEX_CACHE.write().unwrap();
            let cache_len = CACHED_REGEXES.load(SafeOrd);

            for entry in cache[..cache_len].iter_mut() {
                unsafe { entry.assume_init_drop(); }
            }
            CACHED_REGEXES.store(0, SafeOrd);

            Ok(())
        })?
    )?;

    Ok(table)
}