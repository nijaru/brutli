use std::io::Write;
use std::process::{Command, Stdio};

fn brotli_cli_available() -> bool {
    Command::new("brotli")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_brotli(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("brotli")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn brotli CLI");

    {
        let mut stdin = child.stdin.take().expect("brotli stdin");
        stdin.write_all(input).expect("write brotli CLI input");
    }

    let output = child.wait_with_output().expect("wait for brotli CLI");
    assert!(
        output.status.success(),
        "brotli {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn c_cli_reference_round_trips() {
    if !brotli_cli_available() {
        eprintln!("skipping C reference test: brotli CLI not found");
        return;
    }

    let sources = [
        (
            "text",
            b"Brotli combines LZ77, Huffman coding, and a static dictionary. ".repeat(256),
        ),
        ("repetitive", b"abc123abc123abc123abc123".repeat(1024)),
        (
            "binary",
            (0..=255_u8).cycle().take(16 * 1024).collect::<Vec<_>>(),
        ),
        ("empty", Vec::new()),
    ];

    for (name, source) in sources {
        for quality in [0, 1, 5, 9, 11] {
            let ours = brutli::compress_with_quality(&source, quality)
                .unwrap_or_else(|error| panic!("{name} q{quality}: {error}"));
            let c_decoded = run_brotli(&["-d", "-c"], &ours);
            assert_eq!(
                c_decoded, source,
                "{name} q{quality}: C reference decoded Brutli output incorrectly"
            );

            let quality_arg = quality.to_string();
            let c_compressed = run_brotli(&["-c", "-q", &quality_arg], &source);
            let our_decoded = brutli::decompress(&c_compressed, source.len())
                .unwrap_or_else(|error| panic!("{name} q{quality}: {error}"));
            assert_eq!(
                our_decoded, source,
                "{name} q{quality}: Brutli decoded C reference output incorrectly"
            );
        }
    }
}
