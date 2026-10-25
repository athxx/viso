//! A Linux desktop app's launcher entry, as the freedesktop.org desktop
//! entry specification writes it.

/// The `.desktop` file of the app `name` that runs `exec` with the icon
/// `icon`, in the directory it is unpacked in, and handles the URLs of
/// `schemes`: each passed as the program's argument.
pub fn desktop_entry(name: &str, exec: &str, icon: Option<&str>, schemes: &[String]) -> String {
    let url = if schemes.is_empty() { "" } else { " %u" };
    let mut entry = format!(
        "[Desktop Entry]\nType=Application\nVersion=1.5\nName={}\nExec={}{url}\n\
         Terminal=false\n",
        value(name),
        exec_quoted(exec)
    );
    if let Some(icon) = icon {
        entry.push_str(&format!("Icon={}\n", value(icon)));
    }
    entry.push_str("Categories=Utility;\n");
    if !schemes.is_empty() {
        entry.push_str("MimeType=");
        for scheme in schemes {
            entry.push_str(&format!("x-scheme-handler/{};", value(scheme)));
        }
        entry.push('\n');
    }
    entry
}

/// A string value: no line breaks, `\` escaped.
fn value(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// The program of an `Exec` key, quoted when it has a reserved character.
fn exec_quoted(program: &str) -> String {
    let reserved = |c: char| " \t\n\"'\\><~|&;$*?#()`".contains(c);
    if program.contains(reserved) {
        // Escaped for the quotes, then again as any string value is.
        let inner = program
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$");
        value(&format!("\"{inner}\""))
    } else {
        value(program)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_names_the_app_its_program_and_icon() {
        let entry = desktop_entry("Notes", "notes", Some("notes"), &[]);
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(entry.contains("\nName=Notes\nExec=notes\n"));
        assert!(entry.contains("\nIcon=notes\n"));
        assert!(!entry.contains("MimeType"));
        assert!(!desktop_entry("A", "a", None, &[]).contains("Icon="));
        assert!(desktop_entry("A", "my app", None, &[]).contains("Exec=\"my app\"\n"));
    }

    #[test]
    fn the_entry_hands_the_app_the_urls_of_its_schemes() {
        let schemes = ["notes".to_owned(), "x-notes".to_owned()];
        let entry = desktop_entry("Notes", "notes", None, &schemes);
        assert!(entry.contains("\nExec=notes %u\n"), "{entry}");
        assert!(entry.ends_with("\nMimeType=x-scheme-handler/notes;x-scheme-handler/x-notes;\n"));
        let spaced = desktop_entry("A", "my app", None, &schemes);
        assert!(spaced.contains("\nExec=\"my app\" %u\n"));
    }
}
