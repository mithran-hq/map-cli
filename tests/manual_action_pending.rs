//! End-to-end tests for canonical manual action results (mithran-control-plane#219).
//!
//! Every test drives the real `map` binary against an owned loopback HTTP server
//! with a synthetic bearer, so the command, output and exit-code path is the one a
//! caller actually observes. The server never reaches a public API or cloud.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TOKEN: &str = "synthetic-bearer-token";
const INTENT_ID: &str = "0123456789abcdef0123456789abcdef";
/// 32-lowercase-hex target identity per the control-plane `OwnerAdmission`.
const TARGET_ID: &str = "fedcba9876543210fedcba9876543210";
const ROUTE_POINTER: &str = "route-pointer://sandbox/production/app:gtd-tracker";
/// A credential string the server must never reflect into caller-visible output.
const CREDENTIAL: &str = "Bearer synthetic-bearer-token";

const PENDING_COMMIT_UNKNOWN: &str = r#"{"status":"pending","action":"publish","target_id":"fedcba9876543210fedcba9876543210","route_pointer_ref":"route-pointer://sandbox/production/app:gtd-tracker","intent_id":"0123456789abcdef0123456789abcdef","reason":"commit_unknown"}"#;

#[derive(Clone, Debug)]
struct Captured {
    request_line: String,
    headers: Vec<String>,
    body: String,
}

struct FakeServer {
    endpoint: String,
    captured: Arc<Mutex<Vec<Captured>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeServer {
    fn start(status: u16, reason: &str, body: &str) -> Self {
        Self::start_with_deadline(status, reason, body, Duration::from_secs(10))
    }

    /// A live loopback listener that refuses no one but must receive nothing.
    /// Used to prove an invalid resume argument performs no HTTP request.
    fn start_idle() -> Self {
        Self::start_with_deadline(200, "OK", "", Duration::from_millis(600))
    }

    fn start_with_deadline(status: u16, reason: &str, body: &str, window: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind owned loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking loopback listener");
        let address = listener.local_addr().expect("loopback address");
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let handle = thread::spawn(move || {
            let expiry = Instant::now() + window;
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking accepted stream");
                        serve(stream, &sink, &response);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= expiry {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            endpoint: format!("http://{address}"),
            captured,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> Vec<Captured> {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.captured.lock().expect("captured lock").clone()
    }
}

fn serve(mut stream: TcpStream, sink: &Arc<Mutex<Vec<Captured>>>, response: &str) {
    let clone = stream.try_clone().expect("clone stream");
    let mut reader = BufReader::new(clone);
    let mut request_line = String::new();
    let _ = reader.read_line(&mut request_line);
    let mut content_length = 0usize;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line == "\r\n" {
            break;
        }
        let trimmed = line.trim_end().to_string();
        if let Some(value) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        headers.push(trimmed);
    }
    let mut buffer = vec![0u8; content_length];
    let _ = reader.read_exact(&mut buffer);
    sink.lock().expect("captured lock").push(Captured {
        request_line: request_line.trim_end().to_string(),
        headers,
        body: String::from_utf8_lossy(&buffer).to_string(),
    });
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn run_map(endpoint: &str, json: bool, command: &[&str]) -> Output {
    let mut args: Vec<&str> = vec!["--endpoint", endpoint, "--token", TOKEN];
    if json {
        args.push("--json");
    }
    args.extend_from_slice(command);
    Command::new(env!("CARGO_BIN_EXE_map"))
        .args(&args)
        .output()
        .expect("map binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn publish_args() -> Vec<&'static str> {
    vec![
        "publish",
        "gtd-tracker",
        "--deployment-ref",
        "deployment://sandbox/production/gtd-1",
    ]
}

fn canary_start_args() -> Vec<&'static str> {
    vec![
        "canary",
        "start",
        "gtd-tracker",
        "--deployment-ref",
        "deployment://sandbox/production/gtd-2",
        "--weight",
        "20",
    ]
}

fn rollback_args() -> Vec<&'static str> {
    vec!["rollback", "deployment://sandbox/production/gtd-1"]
}

fn request_body(captured: &[Captured]) -> Value {
    let request = captured.first().expect("one captured request");
    serde_json::from_str(&request.body).expect("request body is JSON")
}

#[test]
fn publish_http200_pending_human_reports_pending_and_exits_nonzero() {
    let server = FakeServer::start(200, "OK", PENDING_COMMIT_UNKNOWN);
    let output = run_map(&server.endpoint, false, &publish_args());
    let captured = server.finish();

    assert!(!output.status.success(), "pending must exit nonzero");
    let text = stdout(&output);
    assert!(text.contains("status: pending"), "{text}");
    assert!(text.contains("reason: commit_unknown"), "{text}");
    assert!(text.contains(&format!("intent_id: {INTENT_ID}")), "{text}");
    assert!(!text.lines().any(|line| line.trim() == "ok"), "{text}");
    assert!(!text.contains("published http"), "{text}");
    assert!(stderr(&output).contains("pending"), "{}", stderr(&output));
    assert!(!stdout(&output).contains(TOKEN));
    assert!(!stderr(&output).contains(TOKEN));
    assert_eq!(
        request_body(&captured)["deployment_ref"],
        "deployment://sandbox/production/gtd-1"
    );
}

#[test]
fn publish_http200_pending_json_is_single_parseable_pending_and_exits_nonzero() {
    let server = FakeServer::start(200, "OK", PENDING_COMMIT_UNKNOWN);
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success(), "pending must exit nonzero");
    let value: Value = serde_json::from_str(stdout(&output).trim())
        .expect("JSON stdout is one parseable document");
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], "pending");
    assert_eq!(value["reason"], "commit_unknown");
    assert_eq!(value["intent_id"], INTENT_ID);
    assert_eq!(value["action"], "publish");
    assert!(!stdout(&output).contains(TOKEN));
}

#[test]
fn canary_http200_pending_human_reports_pending_and_exits_nonzero() {
    let body = r#"{"status":"pending","action":"canary-start","target_id":"fedcba9876543210fedcba9876543210","route_pointer_ref":"route-pointer://x","intent_id":"0123456789abcdef0123456789abcdef","reason":"delivery_unknown"}"#;
    let server = FakeServer::start(200, "OK", body);
    let output = run_map(&server.endpoint, false, &canary_start_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let text = stdout(&output);
    assert!(text.contains("status: pending"), "{text}");
    assert!(text.contains("reason: delivery_unknown"), "{text}");
    assert!(!text.lines().any(|line| line.trim() == "ok"), "{text}");
    assert!(!text.contains("result:"), "{text}");
}

#[test]
fn rollback_http200_pending_json_reports_pending_and_exits_nonzero() {
    let server = FakeServer::start(200, "OK", PENDING_COMMIT_UNKNOWN);
    let output = run_map(&server.endpoint, true, &rollback_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], "pending");
    assert_eq!(value["reason"], "commit_unknown");
}

#[test]
fn pending_json_preserves_attempted_state_version() {
    let body = r#"{"status":"pending","action":"publish","target_id":"fedcba9876543210fedcba9876543210","route_pointer_ref":"route-pointer://x","intent_id":"0123456789abcdef0123456789abcdef","reason":"commit_unknown","attempted_state_version":"7"}"#;
    let server = FakeServer::start(200, "OK", body);
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["attempted_state_version"], "7");
}

#[test]
fn publish_http200_ok_human_reports_published_and_exits_zero() {
    let body = r#"{"status":"ok","action":"publish","published":{"hostname":"gtd-tracker.apps.mithran.cloud"}}"#;
    let server = FakeServer::start(200, "OK", body);
    let output = run_map(&server.endpoint, false, &publish_args());
    let _ = server.finish();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "published https://gtd-tracker.apps.mithran.cloud\n"
    );
}

#[test]
fn publish_http200_ok_json_preserves_server_shape_and_exits_zero() {
    let body = r#"{"status":"ok","action":"publish","published":{"hostname":"gtd-tracker.apps.mithran.cloud"},"server_extra":{"kept":true}}"#;
    let server = FakeServer::start(200, "OK", body);
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();

    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["server_extra"]["kept"], true);
}

#[test]
fn rollback_http200_ok_json_exits_zero() {
    let body = r#"{"status":"ok","action":"rollback"}"#;
    let server = FakeServer::start(200, "OK", body);
    let output = run_map(&server.endpoint, true, &rollback_args());
    let _ = server.finish();

    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["status"], "ok");
}

#[test]
fn publish_forbidden_403_is_not_reported_as_success_or_pending() {
    let server = FakeServer::start(403, "Forbidden", r#"{"error":"forbidden"}"#);
    let output = run_map(&server.endpoint, false, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let combined = format!("{}{}", stdout(&output), stderr(&output));
    assert!(!combined.contains("published http"), "{combined}");
    assert!(combined.contains("403"), "{combined}");
}

#[test]
fn publish_conflict_409_keeps_stale_guidance() {
    let server = FakeServer::start(409, "Conflict", r#"{"error":"stale"}"#);
    let output = run_map(&server.endpoint, false, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("stale: the reviewed source moved"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn publish_manual_response_missing_status_json_errors_nonzero() {
    let server = FakeServer::start(200, "OK", r#"{"action":"publish"}"#);
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["ok"], false);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("missing a string status"),
        "{value}"
    );
}

#[test]
fn publish_manual_response_unknown_status_json_errors_nonzero() {
    let server = FakeServer::start(200, "OK", r#"{"status":"accepted"}"#);
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["ok"], false);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown status"),
        "{value}"
    );
}

#[test]
fn publish_manual_response_malformed_json_errors_nonzero() {
    let server = FakeServer::start(200, "OK", "not-json");
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["ok"], false);
}

#[test]
fn publish_pending_unknown_reason_errors_nonzero() {
    let body = r#"{"status":"pending","action":"publish","target_id":"fedcba9876543210fedcba9876543210","route_pointer_ref":"route-pointer://x","intent_id":"0123456789abcdef0123456789abcdef","reason":"made_up"}"#;
    let server = FakeServer::start(200, "OK", body);
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["ok"], false);
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown reason"),
        "{value}"
    );
}

#[test]
fn publish_pending_missing_intent_id_errors_nonzero() {
    let body = r#"{"status":"pending","action":"publish","target_id":"fedcba9876543210fedcba9876543210","route_pointer_ref":"route-pointer://x","reason":"commit_unknown"}"#;
    let server = FakeServer::start(200, "OK", body);
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();

    assert!(!output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["ok"], false);
}

#[test]
fn publish_intent_id_and_attempted_state_version_body_echoes_exact_inputs() {
    let okay = r#"{"status":"ok","action":"publish","published":{"hostname":"gtd-tracker.apps.mithran.cloud"}}"#;
    let server = FakeServer::start(200, "OK", okay);
    let mut args = publish_args();
    args.extend_from_slice(&[
        "--intent-id",
        INTENT_ID,
        "--attempted-state-version",
        "3",
        "--expected-sha",
        "0123456789abcdef0123456789abcdef01234567",
        "--actor",
        "actor://user/b@mithran.ai",
    ]);
    let output = run_map(&server.endpoint, false, &args);
    let captured = server.finish();

    assert!(output.status.success(), "{}", stderr(&output));
    let request = captured.first().expect("one request");
    assert_eq!(
        request.request_line,
        "POST /v1/map-control/deploy/publish HTTP/1.1"
    );
    let body = request_body(&captured);
    assert_eq!(body["app_ref"], "app:gtd-tracker");
    assert_eq!(
        body["deployment_ref"],
        "deployment://sandbox/production/gtd-1"
    );
    assert_eq!(
        body["expected_source_sha"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(body["actor_ref"], "actor://user/b@mithran.ai");
    assert_eq!(body["intent_id"], INTENT_ID);
    assert_eq!(body["attempted_state_version"], "3");
    assert!(request
        .headers
        .iter()
        .any(|header| header.to_ascii_lowercase() == format!("authorization: bearer {TOKEN}")));
}

#[test]
fn canary_intent_id_body_echoes_exact_inputs() {
    let okay = r#"{"status":"ok","action":"canary-start","alias":{}}"#;
    let server = FakeServer::start(200, "OK", okay);
    let mut args = canary_start_args();
    args.extend_from_slice(&["--intent-id", INTENT_ID]);
    let output = run_map(&server.endpoint, false, &args);
    let captured = server.finish();

    assert!(output.status.success(), "{}", stderr(&output));
    let body = request_body(&captured);
    assert_eq!(body["canary_action"], "start");
    assert_eq!(
        body["canary_deployment_ref"],
        "deployment://sandbox/production/gtd-2"
    );
    assert_eq!(body["weight_pct"], 20);
    assert_eq!(body["intent_id"], INTENT_ID);
}

#[test]
fn rollback_intent_id_body_echoes_exact_inputs() {
    let okay = r#"{"status":"ok","action":"rollback"}"#;
    let server = FakeServer::start(200, "OK", okay);
    let mut args = rollback_args();
    args.extend_from_slice(&["--intent-id", INTENT_ID, "--evidence-ref", "evidence://x"]);
    let output = run_map(&server.endpoint, false, &args);
    let captured = server.finish();

    assert!(output.status.success(), "{}", stderr(&output));
    let body = request_body(&captured);
    assert_eq!(
        body["deployment_ref"],
        "deployment://sandbox/production/gtd-1"
    );
    assert_eq!(body["authority_evidence_ref"], "evidence://x");
    assert_eq!(body["intent_id"], INTENT_ID);
}

#[test]
fn attempted_state_version_without_intent_id_fails_before_http() {
    let mut args = publish_args();
    args.extend_from_slice(&["--attempted-state-version", "3"]);
    let output = run_map("http://127.0.0.1:1", false, &args);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--attempted-state-version requires --intent-id"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn invalid_intent_id_fails_before_http() {
    let mut args = publish_args();
    args.extend_from_slice(&["--intent-id", "not-a-hex-identifier"]);
    let output = run_map("http://127.0.0.1:1", false, &args);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--intent-id must be a 32-character lowercase hex ID"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn invalid_attempted_state_version_fails_before_http() {
    let mut args = publish_args();
    args.extend_from_slice(&[
        "--intent-id",
        INTENT_ID,
        "--attempted-state-version",
        "later",
    ]);
    let output = run_map("http://127.0.0.1:1", false, &args);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--attempted-state-version must be a nonnegative integer"),
        "{}",
        stderr(&output)
    );
}

// ── R1 credential-value regressions (actual CLI, human + JSON) ──

fn pending_value() -> Value {
    serde_json::json!({
        "status": "pending",
        "action": "publish",
        "target_id": TARGET_ID,
        "route_pointer_ref": ROUTE_POINTER,
        "intent_id": INTENT_ID,
        "reason": "commit_unknown",
    })
}

#[test]
fn manual_pending_credential_values_never_reach_stdout_or_stderr() {
    // The review's 18 cases: seven response coordinates times two output
    // modes, plus two invalid resume arguments times two modes.
    for json in [false, true] {
        for field in [
            "action",
            "target_id",
            "route_pointer_ref",
            "intent_id",
            "attempted_state_version",
            "reason",
            "status",
        ] {
            let mut body = pending_value();
            body[field] = Value::String(CREDENTIAL.to_string());
            let text = body.to_string();
            let server = FakeServer::start(200, "OK", &text);
            let output = run_map(&server.endpoint, json, &publish_args());
            let _ = server.finish();
            let combined = format!("{}{}", stdout(&output), stderr(&output));
            assert!(
                !combined.contains(TOKEN) && !combined.contains(CREDENTIAL),
                "field {field} json={json} leaked the credential value: {combined}"
            );
            assert!(!output.status.success(), "field {field} json={json}");
        }
    }
    for json in [false, true] {
        for flag in ["--intent-id", "--attempted-state-version"] {
            let server = FakeServer::start_idle();
            let mut args = publish_args();
            if flag == "--attempted-state-version" {
                args.extend_from_slice(&["--intent-id", INTENT_ID]);
            }
            args.extend_from_slice(&[flag, CREDENTIAL]);
            let output = run_map(&server.endpoint, json, &args);
            let captured = server.finish();
            assert_eq!(output.status.code(), Some(2), "flag {flag} json={json}");
            let combined = format!("{}{}", stdout(&output), stderr(&output));
            assert!(
                !combined.contains(TOKEN) && !combined.contains(CREDENTIAL),
                "flag {flag} json={json} leaked the credential value: {combined}"
            );
            assert!(
                captured.is_empty(),
                "flag {flag} json={json} performed HTTP"
            );
        }
    }
}

#[test]
fn pending_json_retains_legitimate_resume_coordinates_exactly() {
    let server = FakeServer::start(200, "OK", PENDING_COMMIT_UNKNOWN);
    let output = run_map(&server.endpoint, true, &publish_args());
    let _ = server.finish();
    assert!(!output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
    assert_eq!(value["ok"], false);
    assert_eq!(value["action"], "publish");
    assert_eq!(value["target_id"], TARGET_ID);
    assert_eq!(value["route_pointer_ref"], ROUTE_POINTER);
    assert_eq!(value["intent_id"], INTENT_ID);
    assert_eq!(value["reason"], "commit_unknown");
}

#[test]
fn pending_human_retains_legitimate_resume_coordinates_exactly() {
    let server = FakeServer::start(200, "OK", PENDING_COMMIT_UNKNOWN);
    let output = run_map(&server.endpoint, false, &publish_args());
    let _ = server.finish();
    let text = stdout(&output);
    assert!(text.contains(&format!("target_id: {TARGET_ID}")), "{text}");
    assert!(
        text.contains(&format!("route_pointer_ref: {ROUTE_POINTER}")),
        "{text}"
    );
    assert!(text.contains(&format!("intent_id: {INTENT_ID}")), "{text}");
}

#[test]
fn pending_output_omits_unrelated_credential_fields() {
    let body = serde_json::json!({
        "status": "pending",
        "action": "publish",
        "target_id": TARGET_ID,
        "route_pointer_ref": ROUTE_POINTER,
        "intent_id": INTENT_ID,
        "reason": "commit_unknown",
        "access_token": TOKEN,
        "provider_message": format!("Bearer {TOKEN}"),
        "config": { "secret": TOKEN },
    })
    .to_string();
    for json in [false, true] {
        let server = FakeServer::start(200, "OK", &body);
        let output = run_map(&server.endpoint, json, &publish_args());
        let _ = server.finish();
        let combined = format!("{}{}", stdout(&output), stderr(&output));
        assert!(
            !combined.contains(TOKEN),
            "json={json} leaked an unknown field: {combined}"
        );
        if json {
            let value: Value =
                serde_json::from_str(stdout(&output).trim()).expect("single JSON doc");
            assert!(value.get("access_token").is_none(), "{value}");
            assert!(value.get("provider_message").is_none(), "{value}");
            assert!(value.get("config").is_none(), "{value}");
        }
    }
}

#[test]
fn pending_rejects_malformed_coordinates_value_free() {
    // The review observed these rendered as pending; the strict server
    // contract must reject them without echoing the submitted value.
    let coordinates = [
        ("intent_id", "z".repeat(32)),
        ("target_id", "target://sandbox/app:gtd-tracker".to_string()),
        ("action", "Publish Action".to_string()),
        ("route_pointer_ref", "not-a-pointer".to_string()),
        ("attempted_state_version", "9223372036854775808".to_string()),
        ("attempted_state_version", "-1".to_string()),
    ];
    for (field, value) in coordinates {
        let mut body = pending_value();
        body[field] = Value::String(value.clone());
        let text = body.to_string();
        let server = FakeServer::start(200, "OK", &text);
        let output = run_map(&server.endpoint, true, &publish_args());
        let _ = server.finish();
        let combined = format!("{}{}", stdout(&output), stderr(&output));
        assert!(!output.status.success(), "field {field}");
        assert!(
            !combined.contains(&value),
            "field {field} echoed value: {combined}"
        );
        assert!(combined.contains("malformed"), "field {field}: {combined}");
    }
}

#[test]
fn pending_never_prints_reflected_request_bearer() {
    for field in ["action", "route_pointer_ref", "intent_id", "target_id"] {
        let mut body = pending_value();
        body[field] = if field == "route_pointer_ref" {
            Value::String(format!("route-pointer://{TOKEN}"))
        } else {
            Value::String(TOKEN.to_string())
        };
        let text = body.to_string();
        for json in [false, true] {
            let server = FakeServer::start(200, "OK", &text);
            let output = run_map(&server.endpoint, json, &publish_args());
            let _ = server.finish();
            let combined = format!("{}{}", stdout(&output), stderr(&output));
            assert!(
                !combined.contains(TOKEN),
                "field {field} json={json}: {combined}"
            );
        }
    }
}

#[test]
fn manual_http_error_body_never_prints_request_bearer() {
    let body = format!(r#"{{"error":"forbidden","message":"Bearer {TOKEN}"}}"#);
    for json in [false, true] {
        let server = FakeServer::start(403, "Forbidden", &body);
        let output = run_map(&server.endpoint, json, &publish_args());
        let _ = server.finish();
        let combined = format!("{}{}", stdout(&output), stderr(&output));
        assert!(!combined.contains(TOKEN), "json={json}: {combined}");
        assert!(!output.status.success());
    }
}
