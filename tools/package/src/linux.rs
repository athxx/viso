//! A Linux desktop app's launcher entry, as the freedesktop.org desktop
//! entry specification writes it.

/// The `.desktop` file of the app `name` that runs `exec` with the icon
/// `icon`, in the directory it is unpacked in.
pub fn desktop_entry(name: &str, exec: &str, icon: Option<&str>) -> String {
    let mut entry = format!(
        "[Desktop Entry]\nType=Application\nVersion=1.5\nName={}\nExec={}\nTerminal=false\n",
        value(name),
        exec_quoted(exec)
    );
    if let Some(icon) = icon {
        entry.push_str(&format!("Icon={}\n", value(icon)));
    }
    entry.push_str("Categories=Utility;\n");
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
        let entry = desktop_entry("Notes", "notes", Some("notes"));
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(entry.contains("\nName=Notes\nExec=notes\n"));
        assert!(entry.contains("\nIcon=notes\n"));
        assert!(!desktop_entry("A", "a", None).contains("Icon="));
        assert!(desktop_entry("A", "my app", None).contains("Exec=\"my app\"\n"));
    }
}
