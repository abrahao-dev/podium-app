//! Minimal argv parser: positional arguments plus `--flag` / `--key value`.

use std::collections::HashMap;

/// Flags that never take a value.
const SWITCHES: &[&str] = &[
    "--stdin", "--no-wait", "--raw", "--all", "--batch-json", "--wait", "--keep",
];

pub struct Args {
    pub positional: Vec<String>,
    options: HashMap<String, String>,
    switches: Vec<String>,
}

impl Args {
    pub fn parse(argv: &[String]) -> Self {
        let mut positional = Vec::new();
        let mut options = HashMap::new();
        let mut switches = Vec::new();
        let mut iter = argv.iter().peekable();
        let mut literal = false;
        while let Some(arg) = iter.next() {
            if literal {
                positional.push(arg.clone());
            } else if arg == "--" {
                literal = true;
            } else if let Some(name) = arg.strip_prefix("--").filter(|n| !n.is_empty()) {
                if let Some((key, value)) = name.split_once('=') {
                    options.insert(key.to_owned(), value.to_owned());
                } else if SWITCHES.contains(&arg.as_str()) {
                    switches.push(name.to_owned());
                } else if let Some(value) = iter.next() {
                    options.insert(name.to_owned(), value.clone());
                } else {
                    switches.push(name.to_owned());
                }
            } else {
                positional.push(arg.clone());
            }
        }
        Args {
            positional,
            options,
            switches,
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    pub fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name) || self.options.contains_key(name)
    }

    pub fn pos(&self, i: usize) -> Option<&str> {
        self.positional.get(i).map(String::as_str)
    }

    pub fn number(&self, name: &str) -> Option<u64> {
        self.get(name).and_then(|v| v.trim().parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_arguments() {
        let argv: Vec<String> = ["Worker", "--timeout", "30", "--no-wait", "hello", "--name=x", "--", "--raw"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = Args::parse(&argv);
        assert_eq!(args.positional, vec!["Worker", "hello", "--raw"]);
        assert_eq!(args.number("timeout"), Some(30));
        assert!(args.has("no-wait"));
        assert_eq!(args.get("name"), Some("x"));
    }
}
