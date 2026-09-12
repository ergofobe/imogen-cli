//! What a failure carries, and how it is shown.
//!
//! A rejection names the fields it refused. A command that records only the sentence
//! throws that away, so the map travels with the failure — down the `anyhow` chain for a
//! whole command, and beside each file's message for a batch.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

/// The API's own `field -> complaints` map, unrenamed.
pub type Details = BTreeMap<String, Vec<String>>;

/// The map the server sent with a rejection, looked for all the way down the chain: a
/// command that added its own context leaves the SDK error underneath it.
pub fn details_in(error: &anyhow::Error) -> Option<&Details> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<imogen_sdk::Error>())
        .and_then(imogen_sdk::Error::details)
}

/// One line per complaint rather than per field, so a field the server faults twice reads
/// as two statements instead of one run-on.
pub fn detail_lines(details: &Details) -> Vec<String> {
    details
        .iter()
        .flat_map(|(path, messages)| {
            messages
                .iter()
                .map(move |message| format!("{path}: {message}"))
        })
        .collect()
}

/// One file's failure within a batch, in the API's own shape. The key is absent rather
/// than empty when the server named no fields, because `details` is the API's own
/// optional map and not something this program invented.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub path: String,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Details>,
}

impl Failure {
    pub fn new(path: &Path, error: &imogen_sdk::Error) -> Self {
        Self {
            id: None,
            path: path.display().to_string(),
            error: error.to_string(),
            details: error.details().cloned(),
        }
    }

    /// A download knows which asset it was fetching; an upload only has the file.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// What a person is told: whichever of the two names the command was working from,
    /// the server's sentence, and under it the fields it refused. Told only that three
    /// files failed, nobody can tell which field of which file was wrong.
    pub fn message(&self) -> String {
        let subject = self.id.as_deref().unwrap_or(&self.path);
        let head = format!("{subject}: {}", self.error);
        let Some(details) = &self.details else {
            return head;
        };
        let named: Vec<String> = detail_lines(details)
            .into_iter()
            .map(|line| format!("  {line}"))
            .collect();
        format!("{head}\n{}", named.join("\n"))
    }
}

/// A rejection as the server sends one, for the tests of everything that renders it.
#[cfg(test)]
pub fn rejection(details: &[(&str, &[&str])]) -> imogen_sdk::Error {
    imogen_sdk::Error::Api {
        status: 400,
        code: "validation_failed".to_string(),
        message: "The request did not match what this endpoint expects".to_string(),
        details: Some(
            details
                .iter()
                .map(|(field, messages)| {
                    (
                        field.to_string(),
                        messages.iter().map(|m| m.to_string()).collect(),
                    )
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_validation_failure_names_every_field_it_rejected() {
        let error = anyhow::Error::new(rejection(&[
            ("assetIds.3", &["Invalid UUID"]),
            ("limit", &["Too large", "Must be an integer"]),
        ]));
        let details = details_in(&error).expect("the server named the fields");
        assert_eq!(
            detail_lines(details),
            vec![
                "assetIds.3: Invalid UUID",
                "limit: Too large",
                "limit: Must be an integer",
            ]
        );
    }

    /// A command that adds its own context leaves the SDK error further down the chain,
    /// so the map has to be looked for there rather than only at the top.
    #[test]
    fn the_map_survives_a_commands_own_context() {
        let error = anyhow::Error::new(rejection(&[("assetIds.3", &["Invalid UUID"])]))
            .context("could not empty the trash");
        assert!(details_in(&error).is_some());
    }

    /// The summary a person reads has to name the field, not only the envelope sentence:
    /// "3 files failed" with no field is the whole of ergofobe/imogen-cli#20.
    #[test]
    fn a_batch_failure_names_the_field_the_server_refused() {
        let error = rejection(&[("capturedAt", &["Invalid date"])]);
        let message = Failure::new(Path::new("/photos/harbour.jpg"), &error).message();
        assert!(message.contains("/photos/harbour.jpg"), "{message}");
        assert!(message.contains("capturedAt: Invalid date"), "{message}");
    }

    /// The recorded failure carries the map itself, so `--json` and the report hand on the
    /// API's own payload rather than a sentence somebody has to parse.
    #[test]
    fn a_recorded_failure_carries_the_map_unrenamed() {
        let error = rejection(&[("capturedAt", &["Invalid date"])]);
        let value = serde_json::json!(Failure::new(Path::new("/photos/harbour.jpg"), &error));
        assert_eq!(value["details"]["capturedAt"][0], "Invalid date");
    }

    /// The key is absent rather than empty when the server named no fields, because
    /// `details` is the API's own optional map and not something this program invented.
    #[test]
    fn a_failure_the_server_did_not_describe_is_recorded_without_the_key() {
        let error = imogen_sdk::Error::Invalid("nothing to send".to_string());
        let failure = Failure::new(Path::new("/photos/harbour.jpg"), &error);
        assert!(serde_json::json!(failure).get("details").is_none());
        assert_eq!(failure.message(), "/photos/harbour.jpg: nothing to send");
    }

    #[test]
    fn a_download_failure_is_named_by_the_asset_it_was_fetching() {
        let error = rejection(&[("variant", &["Unknown variant"])]);
        let failure = Failure::new(Path::new("/out/harbour.jpg"), &error).with_id("asset-7");
        assert!(failure.message().starts_with("asset-7: "));
        assert!(failure.message().contains("variant: Unknown variant"));
        assert_eq!(serde_json::json!(failure)["path"], "/out/harbour.jpg");
    }
}
