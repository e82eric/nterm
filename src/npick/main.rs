//! npick: open an nterm picker from the shell and print the selection.
//!
//!   npick [OPTIONS] NAME [KEY=VALUE...]   open a named picker from nterm.lua
//!   COMMAND | npick [OPTIONS]             pick one of the lines on stdin
//!   COMMAND | npick --stdin NAME [KEY=VALUE...]
//!                                         stream stdin into a named Items picker
//!
//! Exits 0 with the selected value on stdout, 130 when cancelled, and 2 on
//! errors, so `sel=$(npick ...) || return` works in scripts.

// Shared with the terminal's server; verify_proof is only used there.
#[cfg(unix)]
#[allow(dead_code)]
#[path = "../picker/pipe/protocol.rs"]
mod protocol;

#[cfg(unix)]
mod client;

#[cfg(not(unix))]
fn main() {
    eprintln!("npick: only supported on Unix; use Invoke-NtermPicker on Windows");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() {
    let code = match unix::main(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("npick: {message}");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(unix)]
mod unix {
    use super::client::{self, Item, Outcome};
    use serde_json::{Map, Value, json};
    use std::io::{BufRead, IsTerminal, Write};

    const USAGE: &str = "\
Usage: npick [OPTIONS] NAME [KEY=VALUE...]
       COMMAND | npick [OPTIONS]
       COMMAND | npick --stdin [OPTIONS] NAME [KEY=VALUE...]

Opens a picker in the nterm pane this shell runs in and prints the
selected value. NAME opens a picker from nterm.lua; it must be listed in
picker_pipe.named. Without NAME, each line of stdin is a candidate
(picker_pipe.allow_stream must be true). With --stdin and NAME, stdin
fills that named Items picker, which keeps its configured preview.

Options:
  -q, --query TEXT       initial query
      --position POS     Top, Center, Bottom, FullPane or Cursor
      --rows N           result rows (1-256)
  -d, --delimiter TEXT   field separator for --display/--value (default: tab)
      --display N        show field N (1-based) instead of the whole line
      --value N          return field N instead of the whole line
      --json             each line is {\"text\": ..., \"value\": ...}
      --stdin            stream stdin into the named picker NAME
      --preview TEXT     show a text preview; {item} is the item's value
  -h, --help             show this help

Exit status: 0 selected, 130 cancelled, 2 error.";

    #[derive(Debug, Default, PartialEq)]
    pub(crate) struct Args {
        pub(crate) query: Option<String>,
        pub(crate) position: Option<String>,
        pub(crate) rows: Option<usize>,
        pub(crate) delimiter: Option<String>,
        pub(crate) display: Option<usize>,
        pub(crate) value: Option<usize>,
        pub(crate) json: bool,
        pub(crate) stdin: bool,
        pub(crate) preview: Option<String>,
        pub(crate) name: Option<String>,
        pub(crate) params: Vec<(String, String)>,
        pub(crate) help: bool,
    }

    fn field(option: &str, value: Option<String>) -> Result<usize, String> {
        value
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|&n| n >= 1)
            .ok_or_else(|| format!("{option} needs a field number starting at 1"))
    }

    pub(crate) fn parse(args: Vec<String>) -> Result<Args, String> {
        let mut parsed = Args::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let (option, inline) = match arg.split_once('=') {
                Some((option, value)) if option.starts_with("--") => {
                    (option.to_owned(), Some(value.to_owned()))
                }
                _ => (arg.clone(), None),
            };
            let mut value = || inline.clone().or_else(|| args.next());
            match option.as_str() {
                "-h" | "--help" => parsed.help = true,
                "-q" | "--query" => parsed.query = Some(value().ok_or("--query needs a value")?),
                "--position" => parsed.position = Some(value().ok_or("--position needs a value")?),
                "--rows" => {
                    parsed.rows = Some(
                        value()
                            .and_then(|v| v.parse().ok())
                            .filter(|n| (1..=256).contains(n))
                            .ok_or("--rows needs a number from 1 to 256")?,
                    )
                }
                "-d" | "--delimiter" => {
                    let delimiter = value().ok_or("--delimiter needs a value")?;
                    if delimiter.is_empty() {
                        return Err("--delimiter cannot be empty".into());
                    }
                    parsed.delimiter = Some(delimiter);
                }
                "--display" => parsed.display = Some(field("--display", value())?),
                "--value" => parsed.value = Some(field("--value", value())?),
                "--json" => parsed.json = true,
                "--stdin" => parsed.stdin = true,
                "--preview" => parsed.preview = Some(value().ok_or("--preview needs a value")?),
                "--" => {
                    for rest in args.by_ref() {
                        positional(&mut parsed, rest)?;
                    }
                }
                _ if arg.starts_with('-') && arg.len() > 1 => {
                    return Err(format!("unknown option {arg}; see npick --help"));
                }
                _ => positional(&mut parsed, arg)?,
            }
        }
        if parsed.json && (parsed.display.is_some() || parsed.value.is_some()) {
            return Err("--json cannot be combined with --display or --value".into());
        }
        if parsed.stdin && parsed.name.is_none() {
            return Err("--stdin needs a picker NAME".into());
        }
        Ok(parsed)
    }

    fn positional(parsed: &mut Args, arg: String) -> Result<(), String> {
        if parsed.name.is_none() {
            parsed.name = Some(arg);
        } else {
            let (key, value) = arg
                .split_once('=')
                .ok_or_else(|| format!("expected KEY=VALUE, got {arg:?}"))?;
            parsed.params.push((key.to_owned(), value.to_owned()));
        }
        Ok(())
    }

    /// Turn one input line into a candidate, or `None` to skip it.
    pub(crate) fn item(args: &Args, line: &str) -> Result<Option<Item>, String> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            return Ok(None);
        }
        if args.json {
            let parsed: Value =
                serde_json::from_str(line).map_err(|e| format!("invalid JSON line: {e}"))?;
            let text = parsed["text"]
                .as_str()
                .ok_or("JSON lines need a string \"text\" field")?
                .to_owned();
            let value = match &parsed["value"] {
                Value::Null => text.clone(),
                Value::String(value) => value.clone(),
                other => other.to_string(),
            };
            return Ok(Some(Item { text, value }));
        }
        let delimiter = args.delimiter.as_deref().unwrap_or("\t");
        let pick = |n: Option<usize>| match n {
            Some(n) => line.split(delimiter).nth(n - 1).unwrap_or("").to_owned(),
            None => line.to_owned(),
        };
        Ok(Some(Item {
            text: pick(args.display),
            value: pick(args.value),
        }))
    }

    pub(crate) fn request(args: &Args) -> Value {
        if let Some(name) = &args.name {
            let mut params: Map<String, Value> = args
                .params
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            // Named pickers run commands relative to the caller's directory.
            if let (false, Ok(cwd)) = (params.contains_key("cwd"), std::env::current_dir()) {
                if let Some(cwd) = cwd.to_str() {
                    params.insert("cwd".into(), cwd.into());
                }
            }
            if let Some(query) = &args.query {
                params
                    .entry("query")
                    .or_insert_with(|| query.clone().into());
            }
            if args.stdin {
                return json!({"kind": "named", "name": name, "params": params, "stream": true});
            }
            return json!({"kind": "named", "name": name, "params": params});
        }
        let mut options = Map::new();
        if let Some(query) = &args.query {
            options.insert("initial_query".into(), query.clone().into());
        }
        if let Some(position) = &args.position {
            options.insert("position".into(), position.clone().into());
        }
        if let Some(rows) = args.rows {
            options.insert("result_rows".into(), rows.into());
        }
        if let Some(preview) = &args.preview {
            options.insert("preview".into(), json!({"type": "Text", "text": preview}));
            options.insert("preview_visible".into(), true.into());
        }
        json!({"kind": "stream", "options": options})
    }

    pub(super) fn main(args: Vec<String>) -> Result<i32, String> {
        let args = parse(args)?;
        if args.help {
            println!("{USAGE}");
            return Ok(0);
        }
        let endpoint = std::env::var("NTERM_PICKER_ENDPOINT")
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or("not running in an nterm pane (NTERM_PICKER_ENDPOINT is not set)")?;
        let token = std::env::var("NTERM_PICKER_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or("NTERM_PICKER_TOKEN is not set")?;
        let request = request(&args);
        let items = if args.name.is_some() && !args.stdin {
            None
        } else {
            if std::io::stdin().is_terminal() {
                return Err(
                    "pass a picker NAME or pipe candidates on stdin; see npick --help".into(),
                );
            }
            Some(read_stdin(args)?)
        };
        match client::run(&endpoint, &token, request, items)? {
            Outcome::Accepted(value) => {
                let mut stdout = std::io::stdout().lock();
                let _ = match value {
                    Value::String(value) => writeln!(stdout, "{value}"),
                    other => writeln!(stdout, "{other}"),
                };
                Ok(0)
            }
            Outcome::Cancelled => Ok(130),
        }
    }

    /// Read candidates on a background thread so the picker can open, and be
    /// accepted, while a slow producer is still writing.
    fn read_stdin(args: Args) -> Result<crossbeam_channel::Receiver<Item>, String> {
        let (tx, rx) = crossbeam_channel::bounded(4096);
        std::thread::Builder::new()
            .name("npick stdin".into())
            .spawn(move || {
                let mut stdin = std::io::stdin().lock();
                let mut line = Vec::new();
                loop {
                    line.clear();
                    match stdin.read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    match item(&args, &String::from_utf8_lossy(&line)) {
                        Ok(Some(item)) => {
                            if tx.send(item).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(message) => eprintln!("npick: {message}"),
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(rx)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn args(list: &[&str]) -> Args {
            parse(list.iter().map(|s| s.to_string()).collect()).unwrap()
        }

        #[test]
        fn parses_named_requests_with_parameters() {
            let parsed = args(&["-q", "src", "files", "pattern=TODO", "cwd=/tmp"]);
            assert_eq!(parsed.name.as_deref(), Some("files"));
            assert_eq!(parsed.query.as_deref(), Some("src"));
            let request = request(&parsed);
            assert_eq!(request["kind"], "named");
            assert_eq!(request["params"]["pattern"], "TODO");
            assert_eq!(request["params"]["cwd"], "/tmp");
            assert_eq!(request["params"]["query"], "src");
        }

        #[test]
        fn named_requests_default_cwd_to_the_current_directory() {
            let request = request(&args(&["files"]));
            let cwd = std::env::current_dir().unwrap();
            assert_eq!(request["params"]["cwd"], cwd.to_str().unwrap());
        }

        #[test]
        fn parses_stream_options() {
            let parsed = args(&["--position=Center", "--rows", "10", "--preview", "{item}"]);
            assert!(parsed.name.is_none());
            let request = request(&parsed);
            assert_eq!(request["kind"], "stream");
            assert_eq!(request["options"]["position"], "Center");
            assert_eq!(request["options"]["result_rows"], 10);
            assert_eq!(request["options"]["preview"]["type"], "Text");
            assert_eq!(request["options"]["preview_visible"], true);
        }

        #[test]
        fn stdin_streams_into_a_named_picker() {
            let parsed = args(&["--stdin", "--display", "1", "completions", "commandName=ls"]);
            let streamed = request(&parsed);
            assert_eq!(streamed["kind"], "named");
            assert_eq!(streamed["name"], "completions");
            assert_eq!(streamed["params"]["commandName"], "ls");
            assert_eq!(streamed["stream"], true);
            assert!(request(&args(&["completions"])).get("stream").is_none());
        }

        #[test]
        fn rejects_invalid_arguments() {
            for list in [
                &["--stdin"][..],
                &["--bogus"][..],
                &["--rows", "0"],
                &["--display", "0"],
                &["--json", "--value", "2"],
                &["files", "not-a-pair"],
                &["--delimiter", ""],
            ] {
                assert!(
                    parse(list.iter().map(|s| s.to_string()).collect()).is_err(),
                    "{list:?}"
                );
            }
        }

        #[test]
        fn splits_lines_into_display_and_value_fields() {
            let whole = args(&[]);
            assert_eq!(
                item(&whole, "a b\r").unwrap(),
                Some(Item {
                    text: "a b".into(),
                    value: "a b".into()
                })
            );
            assert_eq!(item(&whole, "").unwrap(), None);
            let fields = args(&["--display", "2", "--value", "1"]);
            assert_eq!(
                item(&fields, "/x/readme\treadme").unwrap(),
                Some(Item {
                    text: "readme".into(),
                    value: "/x/readme".into()
                })
            );
            let custom = args(&["-d", ":", "--value", "3"]);
            assert_eq!(item(&custom, "a:b").unwrap().unwrap().value, "");
        }

        #[test]
        fn reads_json_lines() {
            let json = args(&["--json"]);
            assert_eq!(
                item(&json, r#"{"text":"shown","value":"kept"}"#).unwrap(),
                Some(Item {
                    text: "shown".into(),
                    value: "kept".into()
                })
            );
            assert_eq!(
                item(&json, r#"{"text":"n","value":{"a":1}}"#)
                    .unwrap()
                    .unwrap()
                    .value,
                r#"{"a":1}"#
            );
            assert!(item(&json, r#"{"value":"x"}"#).is_err());
        }
    }
}
