use std::{io::Cursor, path::Path};

use anyhow::{Context, Result};
use lazy_static::lazy_static;
use log::trace;
use regex::{Regex, RegexBuilder};
use zip::ZipArchive;

use super::Installer;

lazy_static! {
    static ref LD_LIBRARY_PATH_REGEX: Regex = RegexBuilder::new(r#"^export LD_LIBRARY_PATH=(.*?)$"#)
        .multi_line(true)
        .build()
        .unwrap();
    static ref LD_PRELOAD_REGEX: Regex = RegexBuilder::new("^export LD_PRELOAD=(.*?)\n")
        .multi_line(true)
        .build()
        .unwrap();
    static ref EXEC_REGEX: Regex = RegexBuilder::new(r#"^exec "[^"]*" .*?$"#)
        .multi_line(true)
        .build()
        .unwrap();
    static ref HYPERSPACE_SO_REGEX: Regex = Regex::new(r#"^Hyperspace(\.\d+)*.amd64.so$"#).unwrap();
    static ref ZIP_SO_REGEX: Regex = Regex::new(r#"^Linux/[^/]+(\.[^.]+)*\.so(\.[^.]+)*$"#).unwrap();
}

pub struct LinuxInstaller;

impl Installer for LinuxInstaller {
    fn supported(&self, ftl: &Path) -> Result<Result<&dyn Installer, String>> {
        // TODO: Add FTL 1.16.12 size here
        // TODO: Is it worth it to use hashes here?
        match ftl.join("FTL.amd64").metadata() {
            Ok(x) => {
                if x.len() == 72443660 {
                    Ok(Ok(self))
                } else {
                    Ok(Err(format!("Unrecognised FTL binary size: {}", x.len())))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Err("FTL binary not found".to_string())),
            Err(e) => Err(e)?,
        }
    }

    fn install(&self, ftl: &Path, zip: &mut ZipArchive<Cursor<Vec<u8>>>) -> Result<()> {
        let shared_objects = zip
            .file_names()
            .filter(|name| ZIP_SO_REGEX.is_match(name))
            .map(|s| s.to_owned())
            .collect::<Vec<_>>();

        trace!("Copying Hyperspace shared objects");
        for obj in shared_objects.iter() {
            let dst = obj.strip_prefix("Linux/").unwrap();
            trace!("    {obj} -> {dst}");
            let mut input = zip
                .by_name(obj)
                .with_context(|| format!("Could not open {obj} from Hyperspace zip"))?;
            let mut output = std::fs::File::create(ftl.join(dst))?;

            std::io::copy(&mut input, &mut output)
                .with_context(|| format!("Could not copy {obj} from Hyperspace zip"))?;
        }

        trace!("Patching FTL start script");

        let script_path = ftl.join("FTL");
        let mut script = std::fs::read_to_string(&script_path).context("Could not open FTL start script")?;

        let exec_range = EXEC_REGEX.find(&script).map(|m| m.range());

        if let Some(range) = exec_range {
            let s = "export LD_LIBRARY_PATH=\"$here:$LD_LIBRARY_PATH\"\n";
            let s_no_nl = &s[..s.len() - 1];
            if let Some(m) = LD_LIBRARY_PATH_REGEX.find(&script) {
                if m.as_str() != s_no_nl {
                    trace!("   Already modified LD_LIBRARY_PATH export found, ignoring")
                }
            } else {
                trace!("    Adding LD_LIBRARY_PATH");
                script.insert_str(range.start, s);
            }

            // Hopefully the two FTL version have different sizes...
            let obj = if std::fs::metadata(ftl.join("FTL.amd64"))?.len() == 72443660 {
                "Hyperspace.1.6.13.amd64.so"
            } else {
                "Hyperspace.1.6.12.amd64.so"
            };
            let s = format!("export LD_PRELOAD={obj}\n");
            if let Some(m) = LD_PRELOAD_REGEX.captures(&script) {
                let group = m.get(1).unwrap();
                if HYPERSPACE_SO_REGEX.is_match(group.as_str().trim_matches(['\'', '\"'].as_slice())) {
                    script.replace_range(group.range(), obj);
                } else {
                    trace!("   Already modified LD_PRELOAD export found, ignoring")
                }
            } else {
                trace!("    Adding LD_PRELOAD");
                script.insert_str(range.start, &s);
            }
        } else {
            trace!("FTL start script seems modified, no changes will be made");
        }

        std::fs::write(script_path, script).context("Could not write new FTL start script")?;

        Ok(())
    }

    fn disable(&self, ftl: &Path) -> Result<()> {
        let script_path = ftl.join("FTL");
        let script = std::fs::read_to_string(&script_path).context("Could not open FTL start script")?;

        // TODO: Only remove matches that match HYPERSPACE_SO_REGEX
        if LD_PRELOAD_REGEX.find(&script).is_some() {
            std::fs::write(script_path, LD_PRELOAD_REGEX.replace_all(&script, "").as_bytes())
                .context("Failed to write FTL start script")?;
        }

        Ok(())
    }
}
