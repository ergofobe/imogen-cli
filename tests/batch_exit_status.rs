//! What a script sees when a batch partly fails: the exit status, and the one document
//! `--json` promises. ergofobe/imogen-cli#27.
//!
//! These run the real program against a stub that answers the few requests a batch makes,
//! because the two things under test — `$?` and the bytes on stdout — only exist once
//! there is a process. A second JSON document is invisible to a grep and obvious to a
//! parser, so stdout is parsed as a stream and the documents are counted.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Complete success, every item refused, and some of each. The statuses the program hands
/// back for the last two are what a script reads.
const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_PARTIAL: i32 = 3;

// --- the stub -------------------------------------------------------------------------

struct Server {
    base: String,
}

type Answer = dyn Fn(&str, &str) -> (u16, String) + Send + Sync + 'static;

impl Server {
    /// Answers every request with whatever `answer` makes of its method and path, on a
    /// port the operating system picks. One request per connection, closed afterwards, so
    /// the stub never has to understand keep-alive.
    fn start(answer: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let answer: Arc<Answer> = Arc::new(answer);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let answer = answer.clone();
                std::thread::spawn(move || handle(stream, answer.as_ref()));
            }
        });
        Self { base }
    }
}

fn handle(stream: TcpStream, answer: &Answer) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut words = request_line.split_whitespace();
    let method = words.next().unwrap_or_default().to_string();
    let target = words.next().unwrap_or_default().to_string();

    let mut length = 0usize;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "transfer-encoding" => chunked = value.trim().eq_ignore_ascii_case("chunked"),
                _ => {}
            }
        }
    }

    // The body is drained before answering, so an upload is never writing into a socket
    // that has already been closed underneath it.
    if chunked {
        drain_chunked(&mut reader);
    } else if length > 0 {
        let mut body = vec![0u8; length];
        let _ = reader.read_exact(&mut body);
    }

    let path = target.split('?').next().unwrap_or(&target);
    let (status, payload) = answer(&method, path);
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} \r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
}

fn drain_chunked(reader: &mut BufReader<TcpStream>) {
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            return;
        }
        let size = usize::from_str_radix(header.trim(), 16).unwrap_or(0);
        let mut chunk = vec![0u8; size + 2];
        if reader.read_exact(&mut chunk).is_err() || size == 0 {
            return;
        }
    }
}

// --- what the stub says ---------------------------------------------------------------

fn rejected() -> (u16, String) {
    (
        400,
        r#"{"error":{"code":"validation_failed","message":"The request did not match what this endpoint expects","details":{"capturedAt":["Invalid date"]}}}"#
            .to_string(),
    )
}

fn not_found() -> (u16, String) {
    (
        404,
        r#"{"error":{"code":"not_found","message":"No such photograph"}}"#.to_string(),
    )
}

fn asset(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","ownerId":"owner-1","type":"image","status":"ready",
            "originalFilename":"harbour.jpg","mimeType":"image/jpeg","checksum":"abc",
            "sizeBytes":12,"width":null,"height":null,"duration":null,
            "capturedAt":"2024-06-01T09:30:00.000Z","capturedAtIsExact":true,
            "capturedAtOriginal":null,"capturedAtOriginalIsExact":null,
            "createdAt":"2024-06-01T09:30:00.000Z","updatedAt":"2024-06-01T09:30:00.000Z",
            "deletedAt":null,"favorite":false,"archived":false,"description":null,
            "exif":null,"location":null,"placeholderColor":null,"livePhotoVideoId":null,
            "deviceAssetId":null}}"#
    )
}

fn uploaded(id: &str) -> (u16, String) {
    (
        201,
        format!(r#"{{"asset":{},"duplicate":false}}"#, asset(id)),
    )
}

// --- running the program ---------------------------------------------------------------

fn imogen(server: &str, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_imogen"))
        .env("IMOGEN_CONFIG", home.join("config.json"))
        .env("NO_COLOR", "1")
        .env_remove("IMOGEN_PROFILE")
        .arg("--server")
        .arg(server)
        .arg("--token")
        .arg("test-token")
        .args(args)
        .output()
        .expect("the program runs")
}

/// Everything on stdout, read the way `jq` reads it. A run that wrote a summary and then
/// an error wrote two documents, and this is where that shows up.
fn documents(output: &Output) -> Vec<serde_json::Value> {
    serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<serde_json::Value>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| {
            panic!(
                "stdout is not a stream of JSON documents: {error}\n{}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
}

fn photograph(directory: &Path, name: &str) -> String {
    let path = directory.join(name);
    std::fs::write(&path, b"not really a photograph").expect("a file");
    path.display().to_string()
}

// --- the tests --------------------------------------------------------------------------

/// An edit the server refused every part of used to print "Edited 0 photographs." and
/// exit 0, so a script that submitted a hundred edits and had a hundred refused saw
/// success.
#[test]
fn an_edit_the_server_refused_entirely_fails() {
    let home = tempfile::tempdir().expect("a directory");
    let server = Server::start(|_method, _path| rejected());

    let prose = imogen(
        &server.base,
        home.path(),
        &["edit", "asset-1", "asset-2", "--favorite"],
    );
    assert_eq!(prose.status.code(), Some(EXIT_FAILED));
    // Nothing was edited, so nothing closes the run by saying how much was.
    let commentary = String::from_utf8_lossy(&prose.stderr);
    assert!(!commentary.contains("Edited 0"), "{commentary}");
    assert!(
        commentary.contains("capturedAt: Invalid date"),
        "{commentary}"
    );

    let json = imogen(
        &server.base,
        home.path(),
        &["--json", "edit", "asset-1", "asset-2", "--favorite"],
    );
    assert_eq!(json.status.code(), Some(EXIT_FAILED));
    let documents = documents(&json);
    assert_eq!(documents.len(), 1, "{documents:?}");
    assert_eq!(documents[0]["failed"], 2);
    assert_eq!(documents[0]["updated"], 0);
}

/// `upload --json` wrote its summary and then let the failure write a second document to
/// the same stdout, which no ordinary parser will read.
#[test]
fn a_partly_refused_upload_writes_one_document() {
    let home = tempfile::tempdir().expect("a directory");
    let uploads = Arc::new(AtomicUsize::new(0));
    let server = Server::start(move |method, path| {
        // One file through, one refused — whichever order the concurrent uploads arrive
        // in. Counted by endpoint rather than by request, so a preflight the program
        // learns to make one day does not quietly become the refused one.
        let upload = method == "POST" && path == "/api/v1/assets";
        if upload && uploads.fetch_add(1, Ordering::SeqCst) == 0 {
            return rejected();
        }
        uploaded("asset-1")
    });

    let first = photograph(home.path(), "one.jpg");
    let second = photograph(home.path(), "two.jpg");
    let output = imogen(
        &server.base,
        home.path(),
        &["--json", "upload", &first, &second],
    );

    let documents = documents(&output);
    assert_eq!(documents.len(), 1, "{documents:?}");
    assert_eq!(documents[0]["failed"], 1);
    assert_eq!(documents[0]["uploaded"], 1);
    assert_eq!(
        documents[0]["failures"][0]["details"]["capturedAt"][0],
        "Invalid date"
    );
    assert_eq!(output.status.code(), Some(EXIT_PARTIAL));
}

/// The same run reported differently depending on which audience asked: the JSON branch
/// returned before the failure was raised, so `--json` exited 0 where prose exited 1.
#[test]
fn a_download_reports_the_same_failure_to_both_audiences() {
    let home = tempfile::tempdir().expect("a directory");
    let server = Server::start(|_method, path| {
        if path.ends_with("/download") {
            return not_found();
        }
        (200, asset("asset-1"))
    });
    let out = home.path().join("out");

    let prose = imogen(
        &server.base,
        home.path(),
        &["download", "asset-1", "-o", &out.display().to_string()],
    );
    let json = imogen(
        &server.base,
        home.path(),
        &[
            "--json",
            "download",
            "asset-1",
            "-o",
            &out.display().to_string(),
        ],
    );

    assert_eq!(json.status.code(), prose.status.code());
    assert_eq!(json.status.code(), Some(EXIT_FAILED));
    let documents = documents(&json);
    assert_eq!(documents.len(), 1, "{documents:?}");
    assert_eq!(documents[0]["failed"], 1);
}

/// The rule only earns its keep if a run that worked still says so.
#[test]
fn a_batch_that_worked_still_exits_zero() {
    let home = tempfile::tempdir().expect("a directory");
    let server = Server::start(|_method, _path| uploaded("asset-1"));

    let only = photograph(home.path(), "one.jpg");
    let output = imogen(&server.base, home.path(), &["--json", "upload", &only]);

    assert_eq!(output.status.code(), Some(EXIT_OK));
    let documents = documents(&output);
    assert_eq!(documents.len(), 1, "{documents:?}");
    assert_eq!(documents[0]["failed"], 0);
}
