//! Minimal command-line parsing: `knxide <command> [--flag value]... [positional]...`.

use std::collections::HashMap;

/// Parsed arguments of one command.
#[derive(Debug, Default)]
pub struct Args {
    pub positional: Vec<String>,
    flags: HashMap<String, Vec<String>>,
    switches: Vec<String>,
}

/// Flags that take no value.
const SWITCHES: &[&str] = &["help", "no-nat", "busmonitor", "routing", "tcp", "yes"];

impl Args {
    pub fn parse(raw: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut args = Self::default();
        let mut raw = raw.into_iter();
        while let Some(arg) = raw.next() {
            if arg == "--" {
                args.positional.extend(raw.by_ref());
                break;
            }
            if let Some(name) = arg.strip_prefix("--") {
                let (name, inline) = match name.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (name.to_string(), None),
                };
                if SWITCHES.contains(&name.as_str()) {
                    args.switches.push(name);
                    continue;
                }
                let value = match inline {
                    Some(v) => v,
                    None => raw
                        .next()
                        .ok_or_else(|| format!("--{name} needs a value"))?,
                };
                args.flags.entry(name).or_default().push(value);
            } else if arg == "-h" {
                args.switches.push("help".to_string());
            } else {
                args.positional.push(arg);
            }
        }
        Ok(args)
    }

    pub fn switch(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .get(name)
            .and_then(|v| v.last())
            .map(String::as_str)
    }

    pub fn flags(&self, name: &str) -> &[String] {
        self.flags.get(name).map_or(&[], Vec::as_slice)
    }

    /// Fail on flags the command does not know.
    pub fn only(&self, known: &[&str]) -> Result<(), String> {
        for name in self.flags.keys().chain(self.switches.iter()) {
            if !known.contains(&name.as_str()) && name != "help" {
                return Err(format!("unknown option --{name}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Args {
        Args::parse(s.split_whitespace().map(String::from)).unwrap()
    }

    #[test]
    fn flags_switches_and_positionals() {
        let a = parse("1/2/3 --dpt 9.001 --gateway=10.0.0.1 21.5 --no-nat --dpt 1.001");
        assert_eq!(a.positional, ["1/2/3", "21.5"]);
        assert_eq!(a.flag("dpt"), Some("1.001"));
        assert_eq!(a.flags("dpt").len(), 2);
        assert_eq!(a.flag("gateway"), Some("10.0.0.1"));
        assert!(a.switch("no-nat"));
        assert!(a.only(&["dpt", "gateway", "no-nat"]).is_ok());
        assert!(a.only(&["dpt"]).is_err());
    }

    #[test]
    fn negative_values_after_double_dash() {
        let a = parse("1/2/3 --dpt 9 -- -5");
        assert_eq!(a.positional, ["1/2/3", "-5"]);
        assert!(Args::parse(["--dpt".to_string()]).is_err());
    }
}
