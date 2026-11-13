//! Shipped Barista procedures. They are versioned with the service and are part of the
//! assistant evaluation identity; they never grant authority by themselves.

use serde_json::{Value, json};

pub(super) struct Skill {
    pub topic: &'static str,
    pub version: u32,
    pub title: &'static str,
    pub summary: &'static str,
    pub body: &'static str,
}

macro_rules! skills {
    ($($file:literal),* $(,)?) => {
        const SOURCES: &[&str] = &[$(include_str!(concat!("skills/", $file, ".md"))),*];
    };
}
skills!(
    "setup-voice",
    "find-and-run-model",
    "increase-context",
    "diagnose-slow",
    "free-disk",
    "connect-client",
    "cache-check",
    "engine-choice",
    "compare-variants",
    "how-to",
);

fn parse(source: &'static str) -> Skill {
    let rest = source
        .strip_prefix("---\n")
        .expect("skill front matter opens");
    let (header, body) = rest
        .split_once("\n---\n")
        .expect("skill front matter closes");
    let field = |name: &str| {
        header
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
            .unwrap_or_else(|| panic!("skill field {name}"))
    };
    Skill {
        topic: field("topic"),
        version: field("version").parse().expect("skill version"),
        title: field("title"),
        summary: field("summary"),
        body: body.trim(),
    }
}

pub(super) fn all() -> impl Iterator<Item = Skill> {
    SOURCES.iter().map(|source| parse(source))
}

pub(super) fn find(topic: &str) -> Option<Skill> {
    all().find(|skill| skill.topic == topic)
}

pub(super) fn index() -> Vec<Value> {
    all()
        .map(|skill| json!({"topic":skill.topic,"title":skill.title,"summary":skill.summary,"version":skill.version}))
        .collect()
}

pub(super) fn topics() -> Vec<&'static str> {
    all().map(|skill| skill.topic).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_skill_parses_and_names_only_listed_tools() {
        let names: Vec<&str> = super::super::tools::catalog()
            .map(|tool| tool.name)
            .collect();
        let mut topics = std::collections::BTreeSet::new();
        for skill in all() {
            assert!(topics.insert(skill.topic), "{}", skill.topic);
            assert!(skill.version >= 1);
            assert!(skill.body.starts_with("# "), "{}", skill.topic);
            assert!(skill.body.len() < 4096, "{}", skill.topic);
            for word in skill.body.split('`').skip(1).step_by(2) {
                if word.starts_with("juiced_") {
                    assert!(
                        names.contains(&word),
                        "{} names unknown tool {word}",
                        skill.topic
                    );
                }
            }
        }
        assert_eq!(topics.len(), SOURCES.len());
    }
}
