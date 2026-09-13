//! `jan-klod-gui` — a Tauri window around the web client the core already serves.
//!
//! Usage:
//!   `jan-klod-gui --url <url> [--title <title>]`
//!
//! This binary is **only a window**. It does not spawn a gateway, resolve an
//! address, or know what a session is — `jan-klod --gui` does all of that and
//! then launches this with a URL that is already answering. That split is
//! deliberate: `src/tui` already owns spawn-or-attach, and a second copy of
//! that rule would eventually find a different gateway than the REST path does.
//!
//! ## The one thing here that is not a window
//!
//! The core's token lives in `sessionStorage` under `jan-klod-token`, where the
//! browser client puts it after prompting (`src/web/src/api.ts`). A window that
//! prompted the user for a token it was *already given* would be theatre, so
//! this seeds it — and seeding a credential into a web page is a boundary, not
//! a detail:
//!
//! * The seed runs **only on the core's own origin**. The initialization script
//!   is injected into every frame the webview loads, so it guards on
//!   `location.origin` before touching storage — otherwise a page from anywhere
//!   else would be handed the token.
//! * Navigation **off that origin is refused** and handed to the system browser
//!   instead, so the guard above is a second line rather than the only one.
//!
//! Both are pinned by `tests::` below and cited from
//! `docs/concepts/security-model.md`.

use std::process::{Command, ExitCode, Stdio};

use tauri::{Url, WebviewUrl, WebviewWindowBuilder};

/// Where the web client keeps the gateway token. Must match `TOKEN_KEY` in
/// `src/web/src/api.ts` — if the two drift, the window silently prompts.
const TOKEN_KEY: &str = "jan-klod-token";

/// The environment variable the whole repository already uses for the token:
/// the gateway reads it to decide whether to require one, and every client
/// reads it to send one.
const TOKEN_ENV: &str = "JAN_KLOD_TOKEN";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let token = std::env::var(TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty());
    let origin = origin_of(&parsed.url);
    let script = seed_token_script(&origin, token.as_deref());

    let app = match tauri::Builder::default().build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(err) => {
            eprintln!("jan-klod-gui: could not start the webview: {err}");
            return ExitCode::FAILURE;
        }
    };

    let built = WebviewWindowBuilder::new(&app, "main", WebviewUrl::External(parsed.url.clone()))
        .title(parsed.title)
        .inner_size(1100.0, 800.0)
        .initialization_script(&script)
        // Returning `false` cancels the navigation. The window is a shell for
        // one origin; a link in a transcript pointing anywhere else belongs in
        // the user's browser, not in the frame holding their token.
        .on_navigation(move |url| {
            if origin_of(url) == origin {
                return true;
            }
            open_externally(url.as_str());
            false
        })
        .build();

    if let Err(err) = built {
        eprintln!(
            "jan-klod-gui: could not open a window at {}: {err}",
            parsed.url
        );
        return ExitCode::FAILURE;
    }

    app.run(|_, _| {});
    ExitCode::SUCCESS
}

/// What the arguments asked for.
#[derive(Debug)]
struct Args {
    /// The page to open. Required: this binary has no default worth guessing,
    /// because the launcher always knows the address it just reached.
    url: Url,
    /// The window title.
    title: String,
}

/// Read `--url <url>` and `--title <title>`.
///
/// Hand-rolled for the same reason `src/tui` hand-rolls its own: two flags
/// do not justify an argument-parsing dependency in a tree this large already.
fn parse_args(args: &[String]) -> Result<Args, String> {
    const USAGE: &str = "usage: jan-klod-gui --url <url> [--title <title>]";

    let mut url = None;
    let mut title = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if let Some(value) = arg.strip_prefix("--url=") {
            url = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--title=") {
            title = Some(value.to_string());
        } else if arg == "--url" {
            url = Some(next_value(&mut rest, "--url", USAGE)?);
        } else if arg == "--title" {
            title = Some(next_value(&mut rest, "--title", USAGE)?);
        } else {
            return Err(format!(
                "jan-klod-gui: unexpected argument `{arg}`. {USAGE}"
            ));
        }
    }

    let url = url.ok_or_else(|| format!("jan-klod-gui: --url is required. {USAGE}"))?;
    let url = Url::parse(&url)
        .map_err(|err| format!("jan-klod-gui: `{url}` is not a URL ({err}). {USAGE}"))?;

    Ok(Args {
        url,
        title: title.unwrap_or_else(|| "jan-klod".to_string()),
    })
}

/// The value after a flag, or an error naming the flag that wanted one.
fn next_value<'a>(
    rest: &mut impl Iterator<Item = &'a String>,
    flag: &str,
    usage: &str,
) -> Result<String, String> {
    rest.next()
        .cloned()
        .ok_or_else(|| format!("jan-klod-gui: `{flag}` needs a value. {usage}"))
}

/// The origin of a URL, in the form `location.origin` produces in a page:
/// `scheme://host[:port]`, with no path and no trailing slash.
///
/// Compared as a string rather than with `Url::origin` so that the Rust side
/// and the JavaScript side are literally comparing the same spelling — an
/// origin check that is *nearly* the same on both sides is the kind that holds
/// until the day a default port shows up on one of them.
fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// The script that seeds the token into `sessionStorage`, or a script that does
/// nothing when there is no token to seed.
///
/// Guarded on `location.origin`: Tauri injects an initialization script into
/// **every** frame the webview loads, so an unguarded version would hand the
/// token to whatever a page embedded. `serde_json` does the escaping, because
/// both values end up inside JavaScript string literals and a token containing
/// a quote is otherwise an injection.
fn seed_token_script(origin: &str, token: Option<&str>) -> String {
    let Some(token) = token else {
        // Still a valid script, so the caller has no branch: with no token the
        // gateway is open (it only demands one when JAN_KLOD_TOKEN is set), and
        // the page's own prompt stays the fallback if that ever stops being true.
        return "/* jan-klod: no token to seed */".to_string();
    };
    let origin = serde_json::to_string(origin).expect("a string serialises");
    let key = serde_json::to_string(TOKEN_KEY).expect("a string serialises");
    let token = serde_json::to_string(token).expect("a string serialises");
    format!(
        "(function () {{ \
           if (window.location.origin !== {origin}) {{ return; }} \
           try {{ window.sessionStorage.setItem({key}, {token}); }} catch (e) {{}} \
         }})();"
    )
}

/// Hand a URL to the system browser. Best-effort: a link that fails to open is
/// not worth taking the window down for, and the alternative — navigating to it
/// in the frame holding the token — is the thing being avoided.
fn open_externally(url: &str) {
    let (program, leading): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        // `start` is a cmd builtin, and its first quoted argument is the window
        // title — hence the empty one before the URL.
        ("cmd", &["/C", "start", ""])
    } else {
        ("xdg-open", &[])
    };
    let _ = Command::new(program)
        .args(leading)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::{origin_of, parse_args, seed_token_script, TOKEN_KEY};
    use tauri::Url;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("a test URL")
    }

    #[test]
    fn url_is_required_and_must_be_one() {
        let err = parse_args(&args(&[])).expect_err("no --url");
        assert!(err.contains("--url is required"), "{err}");

        let err = parse_args(&args(&["--url", "127.0.0.1:8787"])).expect_err("not a URL");
        assert!(err.contains("is not a URL"), "{err}");

        let err = parse_args(&args(&["--url"])).expect_err("no value");
        assert!(err.contains("needs a value"), "{err}");
    }

    #[test]
    fn the_title_defaults_and_can_be_set() {
        let parsed = parse_args(&args(&["--url", "http://127.0.0.1:8787/"])).expect("parses");
        assert_eq!(parsed.title, "jan-klod");

        let parsed = parse_args(&args(&["--url=http://127.0.0.1:8787/", "--title", "klod"]))
            .expect("parses");
        assert_eq!(parsed.title, "klod");
        assert_eq!(parsed.url.as_str(), "http://127.0.0.1:8787/");
    }

    #[test]
    fn an_unknown_argument_is_refused_rather_than_ignored() {
        let err = parse_args(&args(&["--gui"])).expect_err("not our flag");
        assert!(err.contains("--gui"), "{err}");
    }

    /// The Rust origin has to be spelled the way `location.origin` spells it, or
    /// the guard in the script never matches and the token is never seeded.
    #[test]
    fn origin_drops_the_path_and_keeps_a_non_default_port() {
        assert_eq!(
            origin_of(&url("http://127.0.0.1:8787/some/path?q=1")),
            "http://127.0.0.1:8787"
        );
        // A default port is elided, exactly as a browser elides it.
        assert_eq!(origin_of(&url("http://example.com/")), "http://example.com");
        assert_eq!(
            origin_of(&url("https://example.com:8443/")),
            "https://example.com:8443"
        );
    }

    /// The security boundary this crate exists to get right: the token is only
    /// written when the page's own origin is the core's.
    #[test]
    fn the_token_is_seeded_only_on_the_cores_own_origin() {
        let script = seed_token_script("http://127.0.0.1:8787", Some("s3cret"));
        assert!(
            script.contains("window.location.origin !== \"http://127.0.0.1:8787\""),
            "the script must guard on the origin: {script}"
        );
        assert!(script.contains(TOKEN_KEY), "{script}");
        assert!(script.contains("s3cret"), "{script}");
        // The guard has to come before the write, or it guards nothing.
        let guard = script.find("location.origin").expect("a guard");
        let write = script.find("setItem").expect("a write");
        assert!(guard < write, "the origin guard must precede the write");
    }

    /// No token means no script that touches storage at all — not a script that
    /// writes an empty string, which the page would read as a real token and
    /// send as `Authorization: Bearer `.
    #[test]
    fn no_token_means_nothing_is_written() {
        let script = seed_token_script("http://127.0.0.1:8787", None);
        assert!(!script.contains("setItem"), "{script}");
    }

    /// A token is a credential from the environment, and it lands inside a
    /// JavaScript string literal. Quotes and backslashes have to survive that.
    #[test]
    fn a_token_with_javascript_metacharacters_is_escaped() {
        let nasty = "a\"b\\c\"; window.alert(1); //";
        let script = seed_token_script("http://127.0.0.1:8787", Some(nasty));
        // The payload's own text may appear — it is *inside* the literal, which
        // is the point. What must not appear is the literal byte sequence, since
        // that is only possible if the quote closed the string early.
        assert!(
            !script.contains(nasty),
            "an unescaped token escaped its literal: {script}"
        );
        assert!(script.contains(r#"a\"b\\c\""#), "{script}");
        // A single line by construction, so a newline in a token cannot comment
        // out the rest of the script with that trailing `//`.
        assert_eq!(script.lines().count(), 1, "{script}");
    }
}
