//! Parsing what `wsl.exe` prints. It writes UTF-16LE.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro {
    pub name: String,
    pub running: bool,
    pub version: u32,
    pub is_default: bool,
}

/// UTF-16LE bytes (BOM optional) to a String; plain UTF-8 passes through, so a fake probe can use either.
pub fn decode(bytes: &[u8]) -> String {
    let bom = bytes.starts_with(&[0xff, 0xfe]);
    let nul_pairs = bytes.len() >= 4 && bytes[1] == 0 && bytes[3] == 0;
    if !bom && !nul_pairs {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units).trim_start_matches('\u{feff}').to_string()
}

/// The table `wsl -l -v` prints: `  NAME  STATE  VERSION` then one row each, the default marked `*`.
pub fn parse_list_verbose(text: &str) -> Vec<Distro> {
    let mut out = Vec::new();
    for line in text.lines().map(|l| l.trim_end_matches('\r')) {
        let is_default = line.trim_start().starts_with('*');
        let cols: Vec<&str> = line.trim_start().trim_start_matches('*').split_whitespace().collect();
        if cols.len() < 3 || cols[0].eq_ignore_ascii_case("NAME") {
            continue;
        }
        let Ok(version) = cols[cols.len() - 1].parse::<u32>() else { continue };
        out.push(Distro { name: cols[0].to_string(), running: cols[1].eq_ignore_ascii_case("running"), version, is_default });
    }
    out
}

/// A distro we can install into: WSL2, and Debian-family by its name (the installer then confirms with /etc/os-release).
/// Microsoft's helper distros (docker-desktop) are never candidates.
pub fn is_candidate(d: &Distro) -> bool {
    let n = d.name.to_ascii_lowercase();
    d.version == 2 && !n.starts_with("docker-desktop") && (n.starts_with("ubuntu") || n.starts_with("debian") || n.contains("kali"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        let mut v = vec![0xff, 0xfe];
        for u in s.encode_utf16() {
            v.extend(u.to_le_bytes());
        }
        v
    }

    #[test]
    fn decodes_and_parses() {
        let t = "  NAME            STATE     VERSION\r\n* Ubuntu          Running   2\r\n  docker-desktop  Stopped   2\r\n  Debian          Stopped   1\r\n";
        let ds = parse_list_verbose(&decode(&utf16(t)));
        assert_eq!(ds.len(), 3);
        assert!(ds[0].is_default && ds[0].running && ds[0].version == 2);
        assert!(is_candidate(&ds[0]));
        assert!(!is_candidate(&ds[1]));
        assert!(!is_candidate(&ds[2]));
        assert_eq!(decode(b"plain ascii text"), "plain ascii text");
    }

    #[test]
    fn empty_is_empty() {
        assert!(parse_list_verbose("").is_empty());
        assert!(parse_list_verbose(&decode(&utf16("  NAME STATE VERSION\r\n"))).is_empty());
    }
}
