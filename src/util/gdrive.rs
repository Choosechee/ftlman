use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;
use ureq::{Body, http};

use crate::util::{check_response_status, content_type_no_charset};

static UUID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""([0-9a-z]{8}-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{12})""#).unwrap());

pub fn request_google_drive_download(file_id: &str) -> Result<http::Response<Body>> {
    log::debug!("Downloading file {file_id} from Google Drive");
    let initial_response = crate::AGENT
        .get("https://drive.google.com/uc?export=download")
        .query("id", file_id)
        .call()?;

    // Response is a virus check
    let data_response = if content_type_no_charset(&initial_response) == "text/html" {
        let text = initial_response.into_body().read_to_string()?;
        let Some(matched) = UUID_REGEX.captures(&text) else {
            bail!("Virus check HTML contained no UUID");
        };

        let uuid = matched.get(1).unwrap().as_str();

        crate::AGENT
            .get("https://drive.usercontent.google.com/download?export=download&confirm=t")
            .query("id", file_id)
            .query("uuid", uuid)
            .call()?
    } else {
        initial_response
    };

    check_response_status(&data_response)?;

    Ok(data_response)
}
