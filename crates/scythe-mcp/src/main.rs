use std::fs::File;
use std::io::{self, BufWriter};
use std::path::PathBuf;

use scythe_mcp::session::InProcess;
use scythe_mcp::{McpServer, Remote, Selection, stdio};

const USAGE: &str = "\
usage: scythe-mcp [--url URL] [--token TOKEN] [--tools DOMAINS]
       scythe-mcp --headless [project.RPP] [--null-audio] [--tools DOMAINS]

An MCP server on stdin and stdout for AI agents to drive Scythe.

  --url URL        the control API of a running Scythe (default http://127.0.0.1:8770)
  --token TOKEN    the token it requires (also SCYTHE_CONTROL_TOKEN)
  --headless       run a session in this process instead, without a window
  --null-audio     with --headless: open no audio device
  --tools DOMAINS  the tools to list: `default` (a compact set), `all`, or domains such as
                   `default,mix,setlist` (comma-separated); scythe_command reaches the rest
";

/// How often the bridge tells Scythe its client is still there, and how long Scythe keeps listing
/// it after the last time: an agent that quits shows as gone within seconds.
const KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(5);
const LISTED_FOR: std::time::Duration = std::time::Duration::from_secs(20);

struct Options {
    url: String,
    token: Option<String>,
    headless: bool,
    project: Option<PathBuf>,
    null_audio: bool,
    tools: Selection,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        url: "http://127.0.0.1:8770".into(),
        token: std::env::var("SCYTHE_CONTROL_TOKEN").ok().filter(|t| !t.is_empty()),
        headless: false,
        project: None,
        null_audio: false,
        tools: Selection::default(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next().cloned()).ok_or_else(|| format!("{name} needs a value"));
        match name {
            "--url" => o.url = value()?,
            "--token" => o.token = Some(value()?).filter(|t| !t.is_empty()),
            "--headless" => o.headless = true,
            "--null-audio" => o.null_audio = true,
            "--tools" => o.tools = Selection::parse(&value()?)?,
            n if n.starts_with("--") => return Err(format!("unknown option {n}")),
            _ if o.project.is_none() => o.project = Some(PathBuf::from(arg)),
            _ => return Err("give at most one project".into()),
        }
    }
    if o.project.is_some() && !o.headless {
        return Err("a project is opened with --headless; a running Scythe opens it with project_open".into());
    }
    Ok(o)
}

/// Plugins and libraries may print to stdout, which would corrupt the protocol: the protocol
/// gets a duplicate of stdout and everything else written there goes to stderr.
#[cfg(unix)]
fn protocol_output() -> io::Result<File> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
    if fd < 0 || unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn run(options: Options) -> Result<(), String> {
    let stdin = io::stdin().lock();
    if options.headless {
        let output = protocol_output().map_err(|e| format!("cannot set up stdout: {e}"))?;
        let session = InProcess::start(options.project, options.null_audio)?;
        let server = McpServer::with_tools(session.backend(), options.tools);
        server.keep_alive(KEEP_ALIVE, LISTED_FOR);
        eprintln!("scythe-mcp: serving an in-process session on stdio");
        stdio::serve(&server, stdin, BufWriter::new(output)).map_err(|e| e.to_string())
    } else {
        let remote = Remote::new(&options.url, options.token);
        match remote.health() {
            Ok(()) => eprintln!("scythe-mcp: serving Scythe at {} on stdio", remote.url()),
            Err(e) => eprintln!("scythe-mcp: {e}; tools fail until Scythe runs with its control API"),
        }
        let server = McpServer::with_tools(remote, options.tools);
        server.keep_alive(KEEP_ALIVE, LISTED_FOR);
        stdio::serve(&server, stdin, io::stdout()).map_err(|e| e.to_string())
    }
}

fn main() {
    if let Some(code) = scythe_engine::plugin_scan::helper_main(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return;
    }
    let code = match options(&args) {
        Ok(o) => match run(o) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("scythe-mcp: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            2
        }
    };
    std::process::exit(code);
}
