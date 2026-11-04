//! Desktop entry files (freedesktop.org Desktop Entry Specification 1.5) and the
//! GSettings string lists of GNOME Shell's enabled extensions.

use std::path::Path;

/// `path` as one `Exec` argument: always quoted; `"`, `` ` ``, `$` and `\` escaped
/// inside the quotes, `%` doubled, then backslashes escaped again for the file's
/// string syntax. Control characters (a newline would end the line) are refused.
pub fn exec_arg(path: &Path) -> Result<String, String> {
    let s = path
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))?;
    if s.chars().any(char::is_control) {
        return Err(format!("{s:?} contains a control character"));
    }
    let mut q = String::with_capacity(s.len() + 2);
    q.push('"');
    for c in s.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            _ => q.push(c),
        }
    }
    q.push('"');
    Ok(q.replace('\\', "\\\\"))
}

/// `path` as a plain string value (`TryExec`): backslashes escaped.
pub fn string_value(path: &Path) -> Result<String, String> {
    let s = path
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))?;
    if s.chars().any(char::is_control) {
        return Err(format!("{s:?} contains a control character"));
    }
    Ok(s.replace('\\', "\\\\"))
}

/// The value of `key` in the `[Desktop Entry]` group, string escapes undone.
pub fn value(text: &str, key: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with('[') {
            in_entry = line.trim_end() == "[Desktop Entry]";
        } else if in_entry
            && let Some((k, v)) = line.split_once('=')
            && k.trim_end() == key
        {
            return Some(unescape(v.trim_start()));
        }
    }
    None
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// The program an `Exec` line starts (first argument, quotes and escapes undone).
pub fn exec_program(text: &str) -> Option<String> {
    let exec = value(text, "Exec")?;
    let mut it = exec.trim_start().chars().peekable();
    let mut prog = String::new();
    if it.peek() == Some(&'"') {
        it.next();
        while let Some(c) = it.next() {
            match c {
                '\\' => prog.extend(it.next()),
                '"' => break,
                _ => prog.push(c),
            }
        }
    } else {
        prog.extend(it.take_while(|c| !c.is_whitespace()));
    }
    (!prog.is_empty()).then(|| prog.replace("%%", "%"))
}

/// `Hidden=true`: the entry counts as deleted (autostart turned off by the user).
pub fn hidden(text: &str) -> bool {
    value(text, "Hidden").is_some_and(|v| v.trim() == "true")
}

/// The app's menu entry: `template` (`packaging/linux/lumenwall-ui.desktop`) without
/// its comments, `Exec` pointing at `exe`, plus `TryExec` so the entry hides itself if
/// the program is removed.
pub fn app_entry(template: &str, exe: &Path) -> Result<String, String> {
    let exec = exec_arg(exe)?;
    let try_exec = string_value(exe)?;
    let mut out = String::new();
    for line in template.lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("Exec=") || line.starts_with("TryExec=") {
            if line.starts_with("Exec=") {
                out.push_str(&format!("TryExec={try_exec}\nExec={exec}\n"));
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

/// The session autostart entry: `lumenwall-ui --background` starts the engine (if it is
/// not running) and the tray icon.
pub fn autostart_entry(exe: &Path) -> Result<String, String> {
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Lumenwall\n\
         Comment=Starts the Lumenwall wallpaper engine and its tray icon\n\
         Comment[fr]=Démarre le moteur de fonds d'écran Lumenwall et son icône de notification\n\
         TryExec={}\n\
         Exec={} --background\n\
         Icon=lumenwall\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n\
         X-GNOME-Autostart-Delay=3\n\
         X-KDE-autostart-after=panel\n",
        string_value(exe)?,
        exec_arg(exe)?
    ))
}

/// A GVariant `as` as `gsettings get` prints it: `['a', 'b']`, `@as []`.
pub fn parse_strv(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    let s = s.strip_prefix("@as").unwrap_or(s).trim();
    let inner = s.strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    let mut it = inner.chars().peekable();
    loop {
        while it.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            it.next();
        }
        let Some(quote) = it.next() else { break };
        if quote != '\'' && quote != '"' {
            return None;
        }
        let mut item = String::new();
        loop {
            match it.next()? {
                '\\' => item.push(it.next()?),
                c if c == quote => break,
                c => item.push(c),
            }
        }
        out.push(item);
    }
    Some(out)
}

/// The reverse of [`parse_strv`], for `gsettings set`.
pub fn format_strv(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|i| format!("'{}'", i.replace('\\', "\\\\").replace('\'', "\\'")))
        .collect();
    format!("[{}]", quoted.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quoting_round_trips() {
        for p in [
            "/home/me/.local/bin/lumenwall-ui",
            "/home/me/My Apps/lumenwall-ui",
            r#"/tmp/we"ird $dir `x` 100%\lumenwall-ui"#,
        ] {
            let line = format!(
                "[Desktop Entry]\nExec={} --background\n",
                exec_arg(Path::new(p)).unwrap()
            );
            assert_eq!(exec_program(&line).as_deref(), Some(p), "{line}");
        }
        assert_eq!(
            exec_arg(Path::new("/a b/c")).unwrap(),
            "\"/a b/c\"",
            "spaces need quotes only"
        );
        assert_eq!(exec_arg(Path::new("/x$y")).unwrap(), r#""/x\\$y""#);
        assert!(exec_arg(Path::new("/a\nb")).is_err());
        assert_eq!(
            exec_program("[Desktop Entry]\nExec=lumenwall-ui %U\n").as_deref(),
            Some("lumenwall-ui")
        );
        assert_eq!(
            exec_program("[Other]\nExec=x\n[Desktop Entry]\nName=y\n"),
            None,
            "only the [Desktop Entry] group counts"
        );
    }

    #[test]
    fn entries_point_at_the_program() {
        let exe = Path::new("/home/me/.local/bin/lumenwall-ui");
        let app = app_entry(super::super::assets::APP_ENTRY, exe).unwrap();
        assert!(!app.contains('#'), "comments are dropped: {app}");
        assert_eq!(exec_program(&app).as_deref(), exe.to_str());
        assert_eq!(value(&app, "TryExec").as_deref(), exe.to_str());
        assert_eq!(value(&app, "Icon").as_deref(), Some("lumenwall"));
        assert_eq!(
            app.matches("Exec=").count(),
            2,
            "Exec and TryExec once each"
        );
        let auto = autostart_entry(exe).unwrap();
        assert_eq!(exec_program(&auto).as_deref(), exe.to_str());
        assert!(value(&auto, "Exec").unwrap().ends_with(" --background"));
        assert!(!hidden(&auto));
        assert!(hidden("[Desktop Entry]\nHidden=true\n"));
    }

    #[test]
    fn gsettings_lists() {
        assert_eq!(parse_strv("@as []"), Some(vec![]));
        assert_eq!(parse_strv("[]\n"), Some(vec![]));
        assert_eq!(
            parse_strv("['a@b.c', \"d@e\", 'it\\'s']"),
            Some(vec!["a@b.c".into(), "d@e".into(), "it's".into()])
        );
        assert_eq!(parse_strv("nonsense"), None);
        let v = vec!["x@y".to_owned(), "it's".to_owned()];
        assert_eq!(parse_strv(&format_strv(&v)), Some(v));
        assert_eq!(format_strv(&[]), "[]");
    }
}
