#[macro_export]
macro_rules! cache {
    (read($path: expr, $key: expr) or insert $ins: block) => {{
        let __cache_path = $path;
        let __cache_key = $key;
        match ::cacache::read_sync(&__cache_path, &__cache_key) {
            Ok(data) => Ok(data),
            Err(cacache::Error::EntryNotFound(..)) => {
                let result = $ins;

                cacache::write_sync(__cache_path, __cache_key, &result)?;

                Ok(result.into())
            }
            Err(err) => Err(err).context("Failed to retrieve cached response"),
        }
    }};

    (read($path: expr, $key: expr) keepalive($alive: expr) or insert $ins: block) => {{
        let __cache_path = $path;
        let __cache_key = $key;
        let __cache_time_cached =
            ::cacache::metadata_sync(&__cache_path, &__cache_key)?
                .map(|md| {
                    ::std::time::UNIX_EPOCH
                        + ::std::time::Duration::from_millis(md.time.try_into().unwrap())
                });

        let __cache_maybe_data = match __cache_time_cached {
            Some(__cache_time)
                if ::std::time::SystemTime::now()
                    .duration_since(__cache_time)
                    .map(|x| x < $alive)
                    .unwrap_or(false) =>
            {
                match ::cacache::read_sync(&__cache_path, &__cache_key) {
                    Ok(data) => Some(data),
                    Err(cacache::Error::EntryNotFound(..)) => None,
                    Err(err) => return Err(err).context("Failed to retrieve cached response"),
                }
            }
            _ => None,
        };

        match __cache_maybe_data {
            Some(data) => data,
            None => {
                let result = $ins;

                cacache::write_sync(__cache_path, __cache_key, &result)?;

                result.into()
            }
        }
    }};
}
