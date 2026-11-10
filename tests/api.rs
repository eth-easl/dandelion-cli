//! End-to-end tests: run the binary against an httpmock server and check the exact BSON bodies.

use std::path::Path;
use std::process::{Command, Output};

use bson::spec::BinarySubtype;
use bson::{Binary, Bson, Document, doc};
use httpmock::prelude::*;

fn bin(bytes: &[u8]) -> Bson {
    Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: bytes.to_vec() })
}

/// Runs `dandelion` with an isolated config file and no DANDELION_* environment.
fn run(config: &Path, args: &[&str]) -> Output {
    run_with_stdin(config, args, None)
}

fn run_with_stdin(config: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    use std::io::Write;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dandelion"));
    cmd.args(args)
        .env("DANDELION_CONFIG", config)
        .env_remove("DANDELION_URL")
        .env_remove("DANDELION_PROFILE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        let mut pipe = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            pipe.write_all(s.as_bytes()).unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A mock that only matches the exact BSON body and content type.
fn expect_bson<'a>(server: &'a MockServer, path: &str, expected: Document, status: u16, body: Vec<u8>) -> httpmock::Mock<'a> {
    let expected = expected.to_vec().unwrap();
    server.mock(move |when, then| {
        when.method(POST)
            .path(path)
            .header("content-type", "application/bson")
            .is_true(move |req| req.body_ref() == expected.as_slice());
        then.status(status).body(body);
    })
}

#[test]
fn register_function_with_flags() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let binary = dir.path().join("matmul.elf");
    std::fs::write(&binary, b"\x7fELF").unwrap();
    let params = dir.path().join("params.bin");
    std::fs::write(&params, b"\x01\x02").unwrap();

    let server = MockServer::start();
    let expected = doc! {
        "name": "matmul",
        "context_size": 64_i64 * 1024 * 1024,
        "engine_type": "Kvm",
        "local_path": "",
        "binary": bin(b"\x7fELF"),
        "input_sets": [ ["matrices", Bson::Null], ["config", [["params", bin(b"\x01\x02")]]] ],
        "output_sets": ["result"],
        "min_set_bytes": [Bson::Int64(4096)],
    };
    let m = expect_bson(&server, "/register/function", expected, 200, b"Function registered".to_vec());

    let static_arg = format!("config:params=@{}", params.display());
    let out = run(&cfg, &[
        "--url", &server.base_url(),
        "function", "register", binary.to_str().unwrap(),
        "--engine", "kvm", "--context-size", "64M",
        "--input-set", "matrices", "--input-set", "config",
        "--static", &static_arg,
        "--output-set", "result", "--min-set-bytes", "4096",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    m.assert();
    assert_eq!(stdout(&out).trim(), "Function registered");
}

#[test]
fn register_function_uses_profile_defaults_and_server_path() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let server = MockServer::start();
    std::fs::write(
        &cfg,
        format!("active_profile = \"t\"\n[profiles.t]\nurl = \"{}\"\nengine = \"Process\"\ncontext_size = \"1M\"\n", server.base_url()),
    )
    .unwrap();

    let expected = doc! {
        "name": "f",
        "context_size": 1_i64 << 20,
        "engine_type": "Process",
        "local_path": "/opt/fns/f",
        "binary": bin(b""),
        "input_sets": [],
        "output_sets": [],
        "min_set_bytes": [],
    };
    let m = expect_bson(&server, "/register/function", expected, 200, b"Function registered".to_vec());
    let out = run(&cfg, &["function", "register", "--server-path", "/opt/fns/f"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    m.assert();
}

#[test]
fn register_composition_from_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let server = MockServer::start();
    let text = "(:composition C (in) -> (out) ())\n";
    let m = expect_bson(&server, "/register/composition", doc! { "composition": text }, 200, b"Function registered".to_vec());
    let out = run_with_stdin(&cfg, &["--url", &server.base_url(), "composition", "register", "-"], Some(text));
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    m.assert();
}

fn sample_response() -> Vec<u8> {
    doc! {
        "sets": [
            { "items": [
                { "identifier": "msg", "key": 0_i64, "data": bin(b"hello world") },
                { "identifier": "blob", "key": 7_i32, "data": bin(&[0, 255, 1]) },
            ] },
        ],
        "timestamps": "{\"start\":10,\"end\":42}",
    }
    .to_vec()
    .unwrap()
}

#[test]
fn invoke_by_name_with_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let f = dir.path().join("a.bin");
    std::fs::write(&f, b"AAA").unwrap();

    let server = MockServer::start();
    let expected = doc! {
        "name": "matmul",
        "sets": [
            { "items": [
                { "identifier": "a", "key": 0_i64, "data": bin(b"AAA") },
                { "identifier": "b", "key": 5_i64, "data": bin(b"lit") },
            ] },
            { "items": [ { "identifier": "c", "key": 0_i64, "data": bin(b"x") } ] },
        ],
    };
    let m = expect_bson(&server, "/hot/matmul", expected, 200, sample_response());
    let a = format!("m:a=@{}", f.display());
    let out = run(&cfg, &[
        "--url", &server.base_url(), "invoke", "matmul",
        "--input", &a, "--input", "cfg:c=x", "--input", "m:b#5=lit",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    m.assert();
    let s = stdout(&out);
    assert!(s.contains("set 0: 2 items"), "{s}");
    assert!(s.contains("| hello world"), "{s}");
    assert!(s.contains("\"blob\"  key=7  size=3 B"), "{s}");
    assert!(s.contains("\"end\": 42"), "{s}");
}

#[test]
fn invoke_inline_composition_cold_json() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let comp = dir.path().join("c.comp");
    std::fs::write(&comp, "COMP").unwrap();

    let server = MockServer::start();
    let m = expect_bson(&server, "/cold/matmul", doc! { "composition": "COMP", "sets": [] }, 200, sample_response());
    let out = run(&cfg, &["--url", &server.base_url(), "invoke", "--composition", comp.to_str().unwrap(), "--no-cache", "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    m.assert();
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(j["sets"][0]["items"][0]["identifier"], "msg");
    assert_eq!(j["sets"][0]["items"][1]["key"], 7);
    assert_eq!(j["sets"][0]["items"][1]["data"], "AP8B");
    assert_eq!(j["timestamps"]["start"], 10);
}

#[test]
fn invoke_raw_and_out_dir_and_custom_hot_path() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let server = MockServer::start();
    std::fs::write(&cfg, "active_profile = \"t\"\n[profiles.t]\nhot_path = \"/hot/v2\"\n").unwrap();
    let m = server.mock(|when, then| {
        when.method(POST).path("/hot/v2");
        then.status(200).body(sample_response());
    });

    let out = run(&cfg, &["--url", &server.base_url(), "invoke", "f", "--raw", "0:blob"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(out.stdout, [0, 255, 1]);

    let od = dir.path().join("out");
    let out = run(&cfg, &["--url", &server.base_url(), "invoke", "f", "--out-dir", od.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read(od.join("0/msg")).unwrap(), b"hello world");
    assert_eq!(std::fs::read_to_string(od.join("timestamps.json")).unwrap(), "{\"start\":10,\"end\":42}");
    m.assert_calls(2);
}

#[test]
fn exit_codes_for_server_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    for (status, body, code) in [
        (400, "Failed to serve request: no such function", 3),
        (500, "Failed to serve request: boom", 4),
        (503, "Server is shutting down", 5),
    ] {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/hot/matmul");
            then.status(status).body(body);
        });
        let out = run(&cfg, &["--url", &server.base_url(), "invoke", "nope"]);
        assert_eq!(out.status.code(), Some(code), "status {status}");
        assert!(stderr(&out).contains(body), "stderr: {}", stderr(&out));
    }
}

#[test]
fn exit_code_for_connection_failure_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    // Port 9 (discard) on localhost is essentially never listening.
    let out = run(&cfg, &["--url", "http://127.0.0.1:9", "ping"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));

    let out = run(&cfg, &["invoke"]);
    assert_eq!(out.status.code(), Some(1));
    let out = run(&cfg, &["invoke", "f", "--input", "bad"]);
    assert_eq!(out.status.code(), Some(1));
    let out = run(&cfg, &["--help"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn timeout_gives_exit_code_2() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let server = MockServer::start();
    server.mock(|when, then| {
        when.any_request();
        then.status(200).delay(std::time::Duration::from_secs(3)).body(sample_response());
    });
    let out = run(&cfg, &["--url", &server.base_url(), "--timeout", "200ms", "invoke", "f"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("timed out"), "stderr: {}", stderr(&out));
}

#[test]
fn timeout_from_config() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let server = MockServer::start();
    server.mock(|when, then| {
        when.any_request();
        then.status(200).delay(std::time::Duration::from_secs(3)).body(sample_response());
    });
    assert!(run(&cfg, &["config", "set", "timeout", "200ms"]).status.success());
    let out = run(&cfg, &["--url", &server.base_url(), "invoke", "f"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("timed out"), "stderr: {}", stderr(&out));
}

#[test]
fn ping_reports_status() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(GET).path("/");
        then.status(200).body("Hello, World\n");
    });
    let out = run(&cfg, &["--url", &server.base_url(), "ping", "-v"]);
    assert!(out.status.success());
    m.assert();
    assert!(stdout(&out).contains("200 OK"));
    assert!(stderr(&out).contains("GET "), "verbose output: {}", stderr(&out));
}

#[test]
fn config_commands() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("sub/config.toml");

    let out = run(&cfg, &["config", "path"]);
    assert_eq!(stdout(&out).trim(), cfg.to_str().unwrap());

    assert!(run(&cfg, &["config", "profile", "add", "local", "--url", "http://localhost:8080"]).status.success());
    assert!(run(&cfg, &["config", "profile", "add", "remote", "--url", "http://h:8080"]).status.success());
    assert!(run(&cfg, &["config", "set", "engine", "kvm"]).status.success());
    assert!(run(&cfg, &["config", "set", "engine", "wasm"]).status.code() == Some(1));
    assert!(run(&cfg, &["--profile", "remote", "config", "set", "context_size", "1G"]).status.success());

    assert_eq!(stdout(&run(&cfg, &["config", "get", "engine"])).trim(), "Kvm");
    assert_eq!(stdout(&run(&cfg, &["config", "get", "hot_path"])).trim(), "/hot/matmul");
    assert_eq!(run(&cfg, &["config", "get", "timeout"]).status.code(), Some(1));
    assert_eq!(stdout(&run(&cfg, &["--profile", "remote", "config", "get", "context_size"])).trim(), "1G");

    let list = stdout(&run(&cfg, &["config", "profile", "list"]));
    assert_eq!(list, "* local\n  remote\n");
    assert!(run(&cfg, &["config", "profile", "use", "remote"]).status.success());
    assert_eq!(stdout(&run(&cfg, &["config", "get", "url"])).trim(), "http://h:8080");
    assert!(run(&cfg, &["config", "unset", "url"]).status.success());
    assert_eq!(stdout(&run(&cfg, &["config", "get", "url"])).trim(), "http://localhost:8080");

    assert!(run(&cfg, &["config", "profile", "remove", "remote"]).status.success());
    assert_eq!(stdout(&run(&cfg, &["config", "profile", "list"])), "  local\n");
    assert_eq!(run(&cfg, &["config", "profile", "use", "nope"]).status.code(), Some(1));
}

#[test]
fn completions_generate() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(&dir.path().join("c.toml"), &["completions", "zsh"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("dandelion"));
}
