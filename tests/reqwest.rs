//! Runtime coverage for the standalone `reqwest` feature.
//!
//! This target is run with `--no-default-features --features reqwest`
//! so it proves pklr can use a configured reqwest client without linking ureq.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

// The CA and leaf are deliberately backdated and valid through 2036 so this
// regression test does not become flaky when it is built on a new runner.
const PRIVATE_CA_CERTIFICATE: &str = r#"-----BEGIN CERTIFICATE-----
MIIDHTCCAgWgAwIBAgICEAAwDQYJKoZIhvcNAQELBQAwHzEdMBsGA1UEAwwUcGts
ciB0ZXN0IHByaXZhdGUgQ0EwHhcNMjAwMTAxMDAwMDAwWhcNMzYwMTAxMDAwMDAw
WjAfMR0wGwYDVQQDDBRwa2xyIHRlc3QgcHJpdmF0ZSBDQTCCASIwDQYJKoZIhvcN
AQEBBQADggEPADCCAQoCggEBALKKoL3ieuLxFQfqMeiaq2EQ5hPZmUfhFI2Gmb7Z
D+3VPEejoWZm0Vaqheuaon2bWera/Ls/TEfpbeDJr8Rsa+VNyF13zQcaeIqtIlZM
c6+f4ZJ/tyTdXEWgPkr+/YWrInqUcx7hJGJGWaRzdFh+aFzmCC02S/DJjC/S1ueg
nAn+qqLPaK4TXikI9vFUlK94AI48UkXE/60YLRX8avgqJgNkviKqfUQOGbEab7fR
A5TSL/0BuLixzbgklO4GXupZgzNKy/wwt7B6TA5C8SMMkQZHgMdqObM0SSpvo+AW
nARi8A+GcxKMP7IyfVsmIW8BFlQf7Vd7aGdapJk9bzV+gQ0CAwEAAaNjMGEwDwYD
VR0TAQH/BAUwAwEB/zAOBgNVHQ8BAf8EBAMCAQYwHQYDVR0OBBYEFCth3mKa7W2S
ELkMa7SCOE3maGG5MB8GA1UdIwQYMBaAFCth3mKa7W2SELkMa7SCOE3maGG5MA0G
CSqGSIb3DQEBCwUAA4IBAQCOfF2rPbQ8aB0RPxaaD8vPz87jZ1z0C9JcppXowkcX
+eJiE9OeCuoGEMJhkLDK41VftLv7N5yOEkWypfkyCU6j9fTKGz3JszT2KgTuTJaO
jnbGsxLQXUwaQK0Iph4gCSDRyfMup1VSw6rcyAADMnCW6R3tnJxW7g9zYNgQuEfu
NlazeRp638Xlezl4gY4WI7XaBp0sA9p8zow7q1Bi3N+PPMckzb42F0NkTiMkO5XT
Slo5ybEO/1ciKEZ0WQK9HdempQ3mjXS8GZnf3VbQc6OtlbVAJF2PnWLGqoxfTmu8
iigGF9tCan7Sdy6jXdBQvgIL0/hRFCs1LHiWxwsi7YtY
-----END CERTIFICATE-----
"#;

const PRIVATE_CA_SERVER_CERTIFICATE: &str = r#"-----BEGIN CERTIFICATE-----
MIIDNzCCAh+gAwIBAgICEAEwDQYJKoZIhvcNAQELBQAwHzEdMBsGA1UEAwwUcGts
ciB0ZXN0IHByaXZhdGUgQ0EwHhcNMjAwMTAxMDAwMDAwWhcNMzYwMTAxMDAwMDAw
WjAUMRIwEAYDVQQDDAkxMjcuMC4wLjEwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAw
ggEKAoIBAQCkYTQkPZS/SOWsB8Yl5mAQEBNNcr+e+MA4pi2yP5SRE+iruE3E2Ccl
6t9GimbPAohjRaCeqNpoWncN5wH6XXXkADQqA92gbl5xKwTI1ZZMMgcGOfNeh0ga
zI3ObU9kErHH0Gq5zyG1C7HrU2ZAS6kH/BidkvI6WETUD6HEE76ZDIGgU4PLreAc
LC36pqAqgKqtY3hXxsMzeMnFI892bl8ySY7jEyY41Qokoxt7RvxZTKaIqd/XFJNO
Z32VHRYvRfCLC2SCVR2GbTuHdZgu80594GK+g+w4y/bcfCw45Q5t+eHFFgYzid3+
FdsTCFDnkEG3jnSbncPhxK4Obed8bW9DAgMBAAGjgYcwgYQwDAYDVR0TAQH/BAIw
ADAOBgNVHQ8BAf8EBAMCBaAwEwYDVR0lBAwwCgYIKwYBBQUHAwEwDwYDVR0RBAgw
BocEfwAAATAdBgNVHQ4EFgQUvh/A8S3aJgB4cRr1N3o4bMc4KtcwHwYDVR0jBBgw
FoAUK2HeYprtbZIQuQxrtII4TeZoYbkwDQYJKoZIhvcNAQELBQADggEBAHGxhIn6
xlKZMmrtcSlPQRq45qaHl/UGl7Lh9RugzGU1kebLykGhOn35y9xlU70QE20ZCjDr
XDkJt0MTOahSZxZMPghkR7ooWcp1IxoVPoygVfoz6oQi25ST4pFduaexqmTIPiCZ
FDLw3HQddAGHDNrjMZcR52YJmjoWlOHLzHn6uvWW+AWEm4Uipzyhz+ioaIX1RvqG
HSD+RfDAMP9cedpXNugDvRwisnoQlpBnwx1kWSUwDvJn1Mr6eVauAYkkumBIS4pW
yS+sQeVI3CccYwjFUUF/j1bcTGvquIaL21xE/2zFxbPykHNFgxvB/mzCMfbLsC/S
CWImHV1JuStVRvc=
-----END CERTIFICATE-----
"#;

const PRIVATE_CA_SERVER_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCkYTQkPZS/SOWs
B8Yl5mAQEBNNcr+e+MA4pi2yP5SRE+iruE3E2Ccl6t9GimbPAohjRaCeqNpoWncN
5wH6XXXkADQqA92gbl5xKwTI1ZZMMgcGOfNeh0gazI3ObU9kErHH0Gq5zyG1C7Hr
U2ZAS6kH/BidkvI6WETUD6HEE76ZDIGgU4PLreAcLC36pqAqgKqtY3hXxsMzeMnF
I892bl8ySY7jEyY41Qokoxt7RvxZTKaIqd/XFJNOZ32VHRYvRfCLC2SCVR2GbTuH
dZgu80594GK+g+w4y/bcfCw45Q5t+eHFFgYzid3+FdsTCFDnkEG3jnSbncPhxK4O
bed8bW9DAgMBAAECggEABAiZ7HJnENiQ64tB9VJ1X1Tdpw6z8rUyQPxG7nL5SiQT
F1+i55Em83Bo1oTKkrWCXkPqSz+5IvVmqtHVLm0WWagYSkL6WxefkVGtFjaI2nQh
0FPMtoWa+IQLeMTIPXGTU464MmNe9YqEwlGza8wk04bVSHc/eBIMF6i4ifoIDnOs
2lgmiKtYvxDQYkTzB6FWmaqzJOKL3jqt8NrHhzbuzLHo0YiCiZ5OsIFN3JEO3yJy
TYJO2FEwzmDgQ+hTOigzP7elb/SWDU+bU2JMhhu3mUoWxiDx7HLwce7L/gO+YB5o
z9/sWPCs+nrYJaLJqm3wsTPXjGD5essOhUA0HYYokQKBgQDcAVVfhMdSwFQPLJRm
wvp6Aiiu+jSdLvdzYXHptnaS0koHEVrSOVwOzuVJXAwSWexZ2zIWmAhHlLtTdmKF
2SkQgUSByxORkF3HnWApjJji7QMokMcuFlgfwAi3uF3EMsZ9Nsl83J/dBt5Munjx
gF/5RlYY+/8DHDCDvskqa7EFswKBgQC/Rg84fvQfgb9tgolodUnSRRbySiw6begD
ScPuSzyXc4uB29sSoYzUydETwRwsljw6HOGrRcgC4tHsEkVu9oF4VzY2bqmuQYQ4
uYL+jfBXgdgYvb2VWK17pto/wuzXsPRCawBiCr07Y6bjMi4CDuAxOzJ0XlmAoTLa
DuXrq8tIMQKBgAUaQtCkU7snmst/TTHU89pAkpD8XJwIqtSSPgIdqUJefjkLvf+C
NRBi3A6HhAAo9cJfwxmjDQ4b9PxKkp5oGvu3A8++1gVaQ9KNY92S1TjuJlSahwQa
oJCb85fPPt1+D/x3eNTciRinQCCncoanY5J0fyq1LYT08msb0a6aMNDhAoGAPMFz
Oj3RK2TaOl25ac2/qiO5+zImRFT+2nSG4N1THMRd7ty4BH3+LuUAHWc8nMkHzmm4
IOAkfQ4xIexX07xHOcNx++5AxZIX/rCmdFb/nbwnuQwj+RlW2a0RLCmtc4HIxIQa
dgn1O7UWoJoi1RKGkfy8tQv3IA2UCoGq9KX4BzECgYEAiTtAgmdhMFk8t/gCUPWg
VkJ74Ctt+Xf69Giv/fma6+nZUQ/AsoTTL8W9IIAprHDHlbp7cI+XasdU9fLZlIpe
gCiRq/i/a8Qk/9L88sXzS50GvQBqhdg0vNEwp4nCbzlF0VFT6R3UvPbyHiHY+zlV
ulUER3+Oe8VNqd0+BTwNa1Y=
-----END PRIVATE KEY-----
"#;

#[test]
fn reqwest_only_fetches_remote_imports() {
    let (routes, expected) = common::fan_out_routes(3);
    let server = common::DelayedServer::start(&common::borrow_routes(&routes), common::DELAY);
    let path = common::write_entry(
        "reqwest_only_remote_import",
        "main.pkl",
        "import \"https://example.test/Main.pkl\" as Main\nresult = Main.total\n",
    );

    let json = pklr::EvaluatorBuilder::new()
        .http_client(pklr::reqwest::Client::new())
        .http_rewrites([format!("https://example.test/={}/", server.base)])
        .eval_to_json(&path)
        .unwrap();

    assert_eq!(json["result"], expected);
    assert_eq!(server.requests(), routes.len());
}

#[test]
fn reqwest_only_uses_the_configured_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let request_target = Arc::new(Mutex::new(None));
    let seen_target = request_target.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        *seen_target.lock().unwrap() = request_line.split_whitespace().nth(1).map(str::to_owned);
        drain_headers(&mut reader);
        write_response(&mut stream, "value = 42\n");
    });
    let path = common::write_entry(
        "reqwest_only_proxy",
        "main.pkl",
        "import \"http://origin.invalid/Imported.pkl\" as Imported\nresult = Imported.value\n",
    );
    let client = pklr::reqwest::Client::builder()
        .proxy(pklr::reqwest::Proxy::all(proxy).unwrap())
        .build()
        .unwrap();

    let json = pklr::EvaluatorBuilder::new()
        .http_client(client)
        .eval_to_json(&path)
        .unwrap();
    server.join().unwrap();

    assert_eq!(json["result"], 42);
    assert_eq!(
        request_target.lock().unwrap().as_deref(),
        Some("http://origin.invalid/Imported.pkl")
    );
}

#[test]
fn reqwest_only_accepts_a_configured_private_ca() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("https://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let certificate =
            CertificateDer::from_pem_slice(PRIVATE_CA_SERVER_CERTIFICATE.as_bytes()).unwrap();
        let key = PrivateKeyDer::from_pem_slice(PRIVATE_CA_SERVER_KEY.as_bytes()).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap();
        let (stream, _) = listener.accept().unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut reader = BufReader::new(rustls::StreamOwned::new(connection, stream));
        drain_headers(&mut reader);
        let mut stream = reader.into_inner();
        write_response(&mut stream, "value = 42\n");
    });
    let path = common::write_entry(
        "reqwest_only_private_ca",
        "main.pkl",
        &format!("import \"{endpoint}/Imported.pkl\" as Imported\nresult = Imported.value\n"),
    );
    let client = pklr::reqwest::Client::builder()
        .tls_certs_only([
            pklr::reqwest::Certificate::from_pem(PRIVATE_CA_CERTIFICATE.as_bytes()).unwrap(),
        ])
        .build()
        .unwrap();

    let json = pklr::EvaluatorBuilder::new()
        .http_client(client)
        .eval_to_json(&path)
        .unwrap();
    server.join().unwrap();

    assert_eq!(json["result"], 42);
}

fn drain_headers(reader: &mut impl BufRead) {
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line == "\r\n" => return,
            Ok(_) => {}
        }
    }
}

fn write_response(stream: &mut impl Write, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
}
